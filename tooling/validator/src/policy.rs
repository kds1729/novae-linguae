//! Reference policy engine — the receiver's *local* trust policy (spec/trust-model.md). There is no
//! central authority (principle 7): every agent decides, from its own declared policy, whom and what
//! to trust. This engine consumes a policy declaration and the [`AttestationGraph`] / delegation
//! tokens, and answers the two decisions the trust model leaves to the receiver:
//!
//! - **Trust derivation** ([`Policy::evaluate_trust`]) — is an agent (or artifact) trusted, given the
//!   attestation graph? Trust spreads outward from the policy's `trusted_roots`: a subject becomes
//!   trusted when at least `min_distinct_paths` *already-trusted* agents positively attest to it
//!   (the Sybil/sock-puppet mitigation the trust model calls for — diversity, not concentration),
//!   within `max_depth` hops, unless a trusted agent `distrusts` it (revocation overriding a positive
//!   path). This is the trust-establishment pattern made executable.
//! - **Capability authorization** ([`Policy::authorize_capability`]) — wraps the delegation-chain
//!   verifier (delegation.rs) with the policy's recognized roots, and then *enforces* the chain's
//!   accumulated `conditions`: a delegation condition the policy does not declare itself able to
//!   satisfy is grounds for refusal. The verifier surfaces conditions; the policy is what enforces them.
//!
//! The policy is a small JSON document so an agent's operator (or the agent itself) can state its
//! stance declaratively and version it. Honest scope: path *diversity* is measured as the number of
//! distinct trusted agents attesting at the final hop (not full vertex-disjoint path enumeration);
//! recency decay and confidence-weighting are left to richer policies.

use crate::attestation::AttestationGraph;
use crate::verify_delegation_chain;
use serde::Deserialize;
use serde_json::Value as J;
use std::collections::BTreeSet;

fn default_max_depth() -> usize {
    5
}
fn default_min_paths() -> usize {
    1
}
fn default_true() -> bool {
    true
}

/// A receiver's local trust policy, deserialized from JSON.
#[derive(Debug, Clone, Deserialize)]
pub struct Policy {
    /// Agents (and/or artifacts) trusted a priori — the seed of all trust derivation, and the
    /// recognized roots for delegation-chain verification.
    #[serde(default)]
    pub trusted_roots: BTreeSet<String>,
    /// Maximum attestation-chain length from a root (bounds how far trust propagates).
    #[serde(default = "default_max_depth")]
    pub max_depth: usize,
    /// How many *distinct* already-trusted agents must attest to a subject before it is trusted.
    /// `>= 2` requires diverse corroboration (Sybil/sock-puppet mitigation).
    #[serde(default = "default_min_paths")]
    pub min_distinct_paths: usize,
    /// Whether a `distrusts` edge from a trusted agent overrides positive paths to the subject.
    #[serde(default = "default_true")]
    pub allow_distrust_override: bool,
    /// Delegation conditions this policy declares itself able to satisfy. A verified chain carrying a
    /// condition outside this set is refused (conditions are free-text policy hooks; this is the hook).
    #[serde(default)]
    pub satisfied_conditions: BTreeSet<String>,
}

impl Policy {
    pub fn from_json(value: &J) -> anyhow::Result<Self> {
        serde_json::from_value(value.clone()).map_err(|e| anyhow::anyhow!("invalid policy: {e}"))
    }
}

/// The result of a trust-derivation query.
#[derive(Debug, Clone)]
pub struct TrustVerdict {
    pub trusted: bool,
    /// The trusted agents whose attestations directly support the subject (empty if it is a root).
    pub supporting: Vec<String>,
    pub reason: String,
}

/// The result of a capability-authorization query.
#[derive(Debug, Clone)]
pub struct CapabilityVerdict {
    pub authorized: bool,
    pub reason: String,
}

impl Policy {
    /// Derive whether `subject` is trusted (optionally scoped to `domain`) given the attestation graph.
    ///
    /// Two tiers. **Propagation**: trust spreads transitively from `trusted_roots` — an agent becomes a
    /// trusted *voucher* once any already-trusted agent positively attests to it (web-of-trust
    /// reachability), bounded by `max_depth` and blocked by a trusted `distrusts`. **Decision gate**:
    /// the queried subject is trusted only if at least `min_distinct_paths` distinct trusted vouchers
    /// attest to it directly (the diversity requirement — set it `>= 2` to demand corroboration from
    /// independent agents, the Sybil mitigation) and no trusted agent distrusts it.
    pub fn evaluate_trust(&self, graph: &AttestationGraph, subject: &str, domain: Option<&str>) -> TrustVerdict {
        if self.trusted_roots.contains(subject) {
            return TrustVerdict { trusted: true, supporting: vec![], reason: format!("`{subject}` is a trusted root") };
        }
        let positive = graph.positive_index(domain);

        // Propagation: a single trusted voucher makes an agent a trusted voucher, transitively.
        let mut trusted = self.trusted_roots.clone();
        for _ in 0..self.max_depth {
            let mut changed = false;
            for (s, attesters) in &positive {
                if trusted.contains(s) || !attesters.iter().any(|a| trusted.contains(a)) {
                    continue;
                }
                if self.allow_distrust_override && graph.distrusters(s).iter().any(|d| trusted.contains(d)) {
                    continue;
                }
                trusted.insert(s.clone());
                changed = true;
            }
            if !changed {
                break;
            }
        }

        // Decision gate on the queried subject: distinct trusted vouchers ≥ min_distinct_paths.
        let supporting: Vec<String> = positive
            .get(subject)
            .map(|a| a.iter().filter(|x| trusted.contains(*x)).cloned().collect())
            .unwrap_or_default();
        let distrusted = self.allow_distrust_override && graph.distrusters(subject).iter().any(|d| trusted.contains(d));
        let trusted_answer = !distrusted && supporting.len() >= self.min_distinct_paths;
        let reason = if trusted_answer {
            format!("trusted via {} distinct attester(s): [{}]", supporting.len(), supporting.join(", "))
        } else if distrusted {
            format!("a trusted agent `distrusts` `{subject}`")
        } else if !supporting.is_empty() {
            format!("only {} trusted attester(s); policy requires {}", supporting.len(), self.min_distinct_paths)
        } else {
            format!("no trusted voucher attests to `{subject}` within depth {}", self.max_depth)
        };
        TrustVerdict { trusted: trusted_answer, supporting, reason }
    }

    /// Authorize a capability under this policy: verify a signed delegation chain back to one of the
    /// policy's `trusted_roots`, then enforce that every condition the chain carries is one the policy
    /// declares it can satisfy (`satisfied_conditions`).
    pub fn authorize_capability(
        &self,
        required: &str,
        grantee: &str,
        delegations: &[J],
        at: Option<&str>,
    ) -> CapabilityVerdict {
        let chain = verify_delegation_chain(required, grantee, delegations, &self.trusted_roots, at);
        if !chain.authorized {
            return CapabilityVerdict { authorized: false, reason: chain.reason };
        }
        let unmet: Vec<&String> = chain.conditions.iter().filter(|c| !self.satisfied_conditions.contains(*c)).collect();
        if !unmet.is_empty() {
            return CapabilityVerdict {
                authorized: false,
                reason: format!(
                    "chain authorized but carries condition(s) the policy cannot satisfy: [{}]",
                    unmet.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("; ")
                ),
            };
        }
        CapabilityVerdict { authorized: true, reason: chain.reason }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{did_nova_from_pubkey, sign_message, signing_key_from_seed};
    use serde_json::json;

    fn did(seed: &str) -> String {
        did_nova_from_pubkey(&signing_key_from_seed(seed).verifying_key())
    }

    fn attest(attester_seed: &str, subject: &str, verb: &str, domain: Option<&str>) -> J {
        let mut m = json!({
            "schema_version": "0.2.0", "kind": "assert", "to": null, "in_reply_to": null,
            "body": { "subject": subject, "claim": {
                "kind": "attestation", "subject": subject, "verb": verb, "domain": domain, "expires_at": null } }
        });
        sign_message(&mut m, &signing_key_from_seed(attester_seed)).unwrap();
        m
    }

    fn policy(roots: &[&str], min_paths: usize) -> Policy {
        Policy {
            trusted_roots: roots.iter().map(|s| s.to_string()).collect(),
            max_depth: 5,
            min_distinct_paths: min_paths,
            allow_distrust_override: true,
            satisfied_conditions: BTreeSet::new(),
        }
    }

    #[test]
    fn transitive_trust_from_a_root() {
        // root vouches alice; alice vouches bob. With min_paths=1, both become trusted.
        let (root, alice, bob) = (did("root"), did("alice"), did("bob"));
        let g = AttestationGraph::from_messages(
            &[attest("root", &alice, "vouches-for", None), attest("alice", &bob, "vouches-for", None)],
            None,
        );
        let p = policy(&[&root], 1);
        assert!(p.evaluate_trust(&g, &alice, None).trusted);
        assert!(p.evaluate_trust(&g, &bob, None).trusted, "trust is transitive within depth");
    }

    #[test]
    fn diversity_requirement_blocks_a_single_voucher() {
        // Only alice vouches for bob; policy requires 2 distinct trusted attesters → untrusted.
        let (root, alice, bob) = (did("root"), did("alice"), did("bob"));
        let g = AttestationGraph::from_messages(
            &[attest("root", &alice, "vouches-for", None), attest("alice", &bob, "vouches-for", None)],
            None,
        );
        let p = policy(&[&root], 2);
        assert!(!p.evaluate_trust(&g, &bob, None).trusted, "one voucher is insufficient under min_distinct_paths=2");
    }

    #[test]
    fn two_distinct_trusted_attesters_satisfy_diversity() {
        // root vouches alice AND carol (both trusted at depth 1); both vouch bob → 2 distinct paths.
        let (root, alice, carol, bob) = (did("root"), did("alice"), did("carol"), did("bob"));
        let g = AttestationGraph::from_messages(
            &[
                attest("root", &alice, "vouches-for", None),
                attest("root", &carol, "vouches-for", None),
                attest("alice", &bob, "vouches-for", None),
                attest("carol", &bob, "vouches-for", None),
            ],
            None,
        );
        let p = policy(&[&root], 2);
        let v = p.evaluate_trust(&g, &bob, None);
        assert!(v.trusted, "{}", v.reason);
        assert_eq!(v.supporting.len(), 2);
    }

    #[test]
    fn distrust_overrides_a_positive_path() {
        let (root, alice, bob) = (did("root"), did("alice"), did("bob"));
        let g = AttestationGraph::from_messages(
            &[
                attest("root", &alice, "vouches-for", None),
                attest("alice", &bob, "vouches-for", None),
                attest("root", &bob, "distrusts", None), // a trusted agent distrusts bob
            ],
            None,
        );
        let p = policy(&[&root], 1);
        assert!(!p.evaluate_trust(&g, &bob, None).trusted, "a trusted distrust overrides the positive path");
    }

    #[test]
    fn domain_scoped_trust_only_applies_in_its_domain() {
        let (root, alice) = (did("root"), did("alice"));
        let g = AttestationGraph::from_messages(
            &[attest("root", &alice, "trusts-claims-about", Some("rust_ingestion"))],
            None,
        );
        let p = policy(&[&root], 1);
        assert!(p.evaluate_trust(&g, &alice, Some("rust_ingestion")).trusted);
        assert!(!p.evaluate_trust(&g, &alice, Some("crypto")).trusted, "domain-scoped trust does not transfer domains");
    }

    #[test]
    fn capability_authorization_enforces_conditions() {
        // root delegates cap:apply/double to alice with a condition; policy must declare it satisfiable.
        let (root, alice) = (did("root"), did("alice"));
        let mut grant = json!({
            "schema_version": "0.2.0", "kind": "delegate", "to": alice, "in_reply_to": null,
            "body": { "capability": "cap:apply/double", "expires_at": null, "conditions": ["business_hours_only"] }
        });
        sign_message(&mut grant, &signing_key_from_seed("root")).unwrap();
        let tokens = vec![grant];

        // Policy that cannot satisfy the condition → refused despite a valid chain.
        let mut p = policy(&[&root], 1);
        let denied = p.authorize_capability("cap:apply/double", &alice, &tokens, None);
        assert!(!denied.authorized, "an unsatisfiable condition must block authorization");

        // Policy that declares the condition satisfiable → authorized.
        p.satisfied_conditions.insert("business_hours_only".to_string());
        let ok = p.authorize_capability("cap:apply/double", &alice, &tokens, None);
        assert!(ok.authorized, "{}", ok.reason);
    }

    #[test]
    fn policy_deserializes_from_json_with_defaults() {
        let p = Policy::from_json(&json!({ "trusted_roots": ["did:nova:abc"] })).unwrap();
        assert_eq!(p.max_depth, 5);
        assert_eq!(p.min_distinct_paths, 1);
        assert!(p.allow_distrust_override);
        assert!(p.trusted_roots.contains("did:nova:abc"));
    }
}
