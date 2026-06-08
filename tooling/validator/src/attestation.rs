//! Attestation-graph query layer — the data the reference policy engine (policy.rs) reasons over.
//!
//! An attestation (spec/trust-model.md) is a signed `assert` message whose `claim` is of kind
//! `attestation`: `<attester> <verb> <subject>`, where the attester is the message's `from`, the verb
//! is the closed trust vocabulary (`vouches-for`, `trusts-claims-about`, `distrusts`), and the subject
//! is an agent DID or an artifact's content-address. These are the *edges* of a trust graph.
//!
//! [`AttestationGraph::from_messages`] ingests a set of Nova Locutio messages and builds the graph:
//! every `assert`/attestation whose signature verifies becomes an edge; every `retract` drops the
//! attestation it targets (revocation — trust-model.md: retractions stop *ongoing* effects); and edges
//! whose `expires_at` precedes the verification instant are pruned. The graph then answers the queries
//! the trust-establishment pattern needs: attestations *about* a subject, attestations *by* an
//! attester, and the positive/negative edges out of an attester (the policy engine walks these).
//!
//! Trust is **not** decided here — that is the receiver's local policy (no central authority,
//! principle 7). This layer only verifies authenticity and answers structural queries.

use serde_json::Value as J;
use std::collections::{BTreeSet, HashMap};

/// One verified trust edge `attester --(verb[/domain])--> subject`.
#[derive(Debug, Clone, PartialEq)]
pub struct Attestation {
    pub attester: String,
    pub subject: String,
    pub verb: String,
    pub domain: Option<String>,
    pub confidence: Option<f64>,
    pub expires_at: Option<String>,
    /// The `assert` message's content-address — what a `retract` targets.
    pub hash: String,
}

impl Attestation {
    pub fn is_positive(&self) -> bool {
        self.verb == "vouches-for" || self.verb == "trusts-claims-about"
    }
    pub fn is_distrust(&self) -> bool {
        self.verb == "distrusts"
    }
    /// Does this positive edge apply when evaluating trust for `domain`? `vouches-for` is general (any
    /// domain); `trusts-claims-about` applies only to its own domain.
    pub fn applies_to_domain(&self, domain: Option<&str>) -> bool {
        match self.verb.as_str() {
            "vouches-for" => true,
            "trusts-claims-about" => match (domain, self.domain.as_deref()) {
                (Some(q), Some(d)) => q == d,
                (None, _) => true, // a general query accepts any domain-scoped trust
                _ => false,
            },
            _ => false,
        }
    }
}

/// A verified, retraction-pruned, expiry-pruned set of attestation edges, indexed for querying.
#[derive(Debug, Default, Clone)]
pub struct AttestationGraph {
    edges: Vec<Attestation>,
}

fn is_expired(expires_at: Option<&str>, at: Option<&str>) -> bool {
    matches!((expires_at, at), (Some(exp), Some(now)) if now > exp)
}

impl AttestationGraph {
    /// Build the graph from a set of messages, verifying each attestation's signature, dropping
    /// retracted attestations, and pruning those expired as of `at` (RFC 3339 UTC; `None` ignores
    /// expiry). Non-attestation messages and unverifiable ones are silently skipped.
    pub fn from_messages(messages: &[J], at: Option<&str>) -> Self {
        // First pass: collect retraction targets (`retract` bodies carry `retracts` = a message hash).
        let mut retracted: BTreeSet<String> = BTreeSet::new();
        for m in messages {
            if m.get("kind").and_then(|k| k.as_str()) == Some("retract") {
                // A retraction only counts if it is itself authentic.
                if crate::verify_signature(m).is_err() {
                    continue;
                }
                if let Some(target) = m.pointer("/body/retracts").and_then(|t| t.as_str()) {
                    retracted.insert(target.to_string());
                }
            }
        }

        let mut edges = Vec::new();
        for m in messages {
            if m.get("kind").and_then(|k| k.as_str()) != Some("assert") {
                continue;
            }
            if m.pointer("/body/claim/kind").and_then(|k| k.as_str()) != Some("attestation") {
                continue;
            }
            let hash = match m.get("hash").and_then(|h| h.as_str()) {
                Some(h) if !retracted.contains(h) => h.to_string(),
                _ => continue, // missing hash, or retracted
            };
            if crate::verify_signature(m).is_err() {
                continue;
            }
            let claim = match m.pointer("/body/claim") {
                Some(c) => c,
                None => continue,
            };
            let expires_at = claim.get("expires_at").and_then(|e| e.as_str()).map(String::from);
            if is_expired(expires_at.as_deref(), at) {
                continue;
            }
            let (Some(attester), Some(subject), Some(verb)) = (
                m.get("from").and_then(|f| f.as_str()),
                claim.get("subject").and_then(|s| s.as_str()),
                claim.get("verb").and_then(|v| v.as_str()),
            ) else {
                continue;
            };
            edges.push(Attestation {
                attester: attester.to_string(),
                subject: subject.to_string(),
                verb: verb.to_string(),
                domain: claim.get("domain").and_then(|d| d.as_str()).map(String::from),
                confidence: claim.get("confidence").and_then(|c| c.as_f64()),
                expires_at,
                hash,
            });
        }
        AttestationGraph { edges }
    }

    pub fn len(&self) -> usize {
        self.edges.len()
    }
    pub fn is_empty(&self) -> bool {
        self.edges.is_empty()
    }

    /// All attestations about `subject`.
    pub fn about(&self, subject: &str) -> Vec<&Attestation> {
        self.edges.iter().filter(|e| e.subject == subject).collect()
    }

    /// All attestations made by `attester`.
    pub fn by(&self, attester: &str) -> Vec<&Attestation> {
        self.edges.iter().filter(|e| e.attester == attester).collect()
    }

    /// Distinct attesters who positively attest to `subject` for `domain`.
    pub fn positive_attesters(&self, subject: &str, domain: Option<&str>) -> BTreeSet<String> {
        self.edges
            .iter()
            .filter(|e| e.subject == subject && e.is_positive() && e.applies_to_domain(domain))
            .map(|e| e.attester.clone())
            .collect()
    }

    /// Attesters who `distrusts` `subject`.
    pub fn distrusters(&self, subject: &str) -> BTreeSet<String> {
        self.edges
            .iter()
            .filter(|e| e.subject == subject && e.is_distrust())
            .map(|e| e.attester.clone())
            .collect()
    }

    /// Map of subject → distinct positive attesters, for the policy engine's fixpoint.
    pub(crate) fn positive_index(&self, domain: Option<&str>) -> HashMap<String, BTreeSet<String>> {
        let mut idx: HashMap<String, BTreeSet<String>> = HashMap::new();
        for e in &self.edges {
            if e.is_positive() && e.applies_to_domain(domain) {
                idx.entry(e.subject.clone()).or_default().insert(e.attester.clone());
            }
        }
        idx
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

    /// A signed attestation `assert`: `attester_seed` makes `verb` about `subject`.
    fn attest(attester_seed: &str, subject: &str, verb: &str, domain: Option<&str>, expires: Option<&str>) -> J {
        let mut m = json!({
            "schema_version": "0.2.0", "kind": "assert", "to": null, "in_reply_to": null,
            "body": { "subject": subject, "claim": {
                "kind": "attestation", "subject": subject, "verb": verb, "domain": domain, "expires_at": expires } }
        });
        sign_message(&mut m, &signing_key_from_seed(attester_seed)).unwrap();
        m
    }

    fn retract(retractor_seed: &str, target_hash: &str) -> J {
        let mut m = json!({
            "schema_version": "0.2.0", "kind": "retract", "to": null, "in_reply_to": null,
            "body": { "retracts": target_hash, "reason": "superseded" }
        });
        sign_message(&mut m, &signing_key_from_seed(retractor_seed)).unwrap();
        m
    }

    #[test]
    fn builds_edges_from_verified_attestations() {
        let bob = did("bob");
        let g = AttestationGraph::from_messages(&[attest("alice", &bob, "vouches-for", None, None)], None);
        assert_eq!(g.len(), 1);
        assert_eq!(g.about(&bob).len(), 1);
        assert_eq!(g.about(&bob)[0].attester, did("alice"));
        assert!(g.positive_attesters(&bob, None).contains(&did("alice")));
    }

    #[test]
    fn retraction_drops_the_edge() {
        let bob = did("bob");
        let a = attest("alice", &bob, "vouches-for", None, None);
        let h = a["hash"].as_str().unwrap().to_string();
        // Alice retracts her own attestation.
        let g = AttestationGraph::from_messages(&[a, retract("alice", &h)], None);
        assert!(g.is_empty(), "a retracted attestation must not appear as an edge");
    }

    #[test]
    fn expired_attestation_is_pruned() {
        let bob = did("bob");
        let a = attest("alice", &bob, "vouches-for", None, Some("2020-01-01T00:00:00Z"));
        let live = AttestationGraph::from_messages(&[a.clone()], Some("2026-01-01T00:00:00Z"));
        assert!(live.is_empty(), "an attestation expired before `at` is pruned");
        let still = AttestationGraph::from_messages(&[a], Some("2019-01-01T00:00:00Z"));
        assert_eq!(still.len(), 1, "before expiry it stands");
    }

    #[test]
    fn tampered_attestation_is_skipped() {
        let bob = did("bob");
        let mut a = attest("alice", &bob, "vouches-for", None, None);
        a["body"]["claim"]["verb"] = json!("distrusts"); // tamper after signing
        let g = AttestationGraph::from_messages(&[a], None);
        assert!(g.is_empty(), "a tampered attestation fails signature verification and is skipped");
    }

    #[test]
    fn domain_scoping_distinguishes_verbs() {
        let bob = did("bob");
        let g = AttestationGraph::from_messages(
            &[attest("alice", &bob, "trusts-claims-about", Some("rust_ingestion"), None)],
            None,
        );
        assert!(g.positive_attesters(&bob, Some("rust_ingestion")).contains(&did("alice")));
        assert!(g.positive_attesters(&bob, Some("crypto")).is_empty(), "domain-scoped trust does not apply to another domain");
    }
}
