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
//! stance declaratively and version it. Beyond the basic distinct-attester count, three richer gates
//! are available (all opt-in, defaults preserve the simple behaviour):
//!
//! - **Confidence-weighting** (`min_confidence`) — each attestation may carry a `confidence` in [0, 1];
//!   trust scores propagate from the roots (score 1.0) by `attester_score × confidence`, combined
//!   across independent supporters by noisy-OR (`1 − ∏(1 − contribution)`). The subject is admitted
//!   only if its aggregate confidence clears the threshold.
//! - **Recency decay** (`half_life_days`) — an attestation's confidence decays by `0.5 ^ (age /
//!   half_life)` from its `issued_at` to the evaluation instant, so stale vouches count for less.
//! - **Vertex-disjoint diversity** (`min_disjoint_paths`) — the real Sybil gate: the number of
//!   *internally vertex-disjoint* paths from the roots to the subject (a max-flow over the trusted
//!   subgraph with unit vertex capacities), so corroboration that all funnels through one intermediary
//!   counts once. Strictly stronger than the distinct-final-attester count.

use crate::attestation::AttestationGraph;
use crate::verify_delegation_chain;
use serde::Deserialize;
use serde_json::Value as J;
use std::collections::{BTreeSet, HashMap, VecDeque};

fn default_max_depth() -> usize {
    5
}
fn default_min_paths() -> usize {
    1
}
fn default_true() -> bool {
    true
}

/// Days since the Unix epoch for the `YYYY-MM-DD` prefix of an RFC 3339 timestamp (date granularity is
/// enough for half-life decay). Returns 0 on a malformed/short input (so decay degrades to none).
fn day_number(ts: &str) -> i64 {
    if ts.len() < 10 {
        return 0;
    }
    let p = |a: usize, b: usize| ts[a..b].parse::<i64>().ok();
    match (p(0, 4), p(5, 7), p(8, 10)) {
        (Some(y), Some(m), Some(d)) if (1..=12).contains(&m) && (1..=31).contains(&d) => days_from_civil(y, m, d),
        _ => 0,
    }
}

/// Days from 1970-01-01 to the given civil date (Howard Hinnant's algorithm; valid for any Gregorian
/// date, no leap-second concern at day granularity).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400; // [0, 399]
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146097 + doe - 719468
}

/// Add a directed residual edge `u → v` of capacity `c` (and ensure the reverse residual exists).
fn mf_add(cap: &mut HashMap<(usize, usize), i64>, adj: &mut HashMap<usize, BTreeSet<usize>>, u: usize, v: usize, c: i64) {
    *cap.entry((u, v)).or_insert(0) += c;
    cap.entry((v, u)).or_insert(0);
    adj.entry(u).or_default().insert(v);
    adj.entry(v).or_default().insert(u);
}

/// Edmonds-Karp max-flow from `s` to `t`. Every augmenting path here carries unit flow (the only finite
/// capacities are 1), so the returned flow is the number of internally vertex-disjoint paths.
fn mf_maxflow(cap: &mut HashMap<(usize, usize), i64>, adj: &HashMap<usize, BTreeSet<usize>>, s: usize, t: usize) -> usize {
    let mut flow = 0usize;
    loop {
        let mut prev: HashMap<usize, usize> = HashMap::from([(s, s)]);
        let mut q = VecDeque::from([s]);
        while let Some(u) = q.pop_front() {
            if u == t {
                break;
            }
            if let Some(neighbours) = adj.get(&u) {
                for &v in neighbours {
                    if !prev.contains_key(&v) && cap.get(&(u, v)).copied().unwrap_or(0) > 0 {
                        prev.insert(v, u);
                        q.push_back(v);
                    }
                }
            }
        }
        if !prev.contains_key(&t) {
            return flow; // no augmenting path remains
        }
        let mut v = t;
        while v != s {
            let u = prev[&v];
            *cap.get_mut(&(u, v)).unwrap() -= 1;
            *cap.entry((v, u)).or_insert(0) += 1;
            v = u;
        }
        flow += 1;
    }
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
    /// Minimum aggregate confidence (noisy-OR over weighted, decayed supporting paths) to trust the
    /// subject. `0.0` (default) disables the confidence gate.
    #[serde(default)]
    pub min_confidence: f64,
    /// Attestation confidence half-life in days for recency decay. `None` (default) disables decay.
    #[serde(default)]
    pub half_life_days: Option<f64>,
    /// Minimum number of internally **vertex-disjoint** root→subject paths (max-flow with unit vertex
    /// capacities) — the strong Sybil gate. `0` (default) disables it (falling back to the count gate).
    #[serde(default)]
    pub min_disjoint_paths: usize,
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
    /// Aggregate confidence in [0, 1] (noisy-OR over weighted, decayed supporting paths).
    pub confidence: f64,
    /// Internally vertex-disjoint root→subject paths (computed only when `min_disjoint_paths > 0`).
    pub disjoint_paths: usize,
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
    pub fn evaluate_trust(
        &self,
        graph: &AttestationGraph,
        subject: &str,
        domain: Option<&str>,
        at: Option<&str>,
    ) -> TrustVerdict {
        if self.trusted_roots.contains(subject) {
            return TrustVerdict {
                trusted: true,
                supporting: vec![],
                confidence: 1.0,
                disjoint_paths: 0,
                reason: format!("`{subject}` is a trusted root"),
            };
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

        // Distinct trusted vouchers attesting to the subject directly.
        let supporting: Vec<String> = positive
            .get(subject)
            .map(|a| a.iter().filter(|x| trusted.contains(*x)).cloned().collect())
            .unwrap_or_default();
        let distrusted = self.allow_distrust_override && graph.distrusters(subject).iter().any(|d| trusted.contains(d));

        // Confidence (noisy-OR over weighted, decayed supporting paths) and vertex-disjoint path count.
        let scores = self.confidence_scores(graph, domain, at, &trusted);
        let confidence = scores.get(subject).copied().unwrap_or(0.0);
        let disjoint_paths =
            if self.min_disjoint_paths > 0 { self.max_disjoint_paths(graph, domain, &trusted, subject) } else { 0 };

        // Combined gate: all configured requirements must hold (and no trusted distrust).
        let count_ok = supporting.len() >= self.min_distinct_paths;
        let confidence_ok = self.min_confidence <= 0.0 || confidence >= self.min_confidence;
        let disjoint_ok = self.min_disjoint_paths == 0 || disjoint_paths >= self.min_disjoint_paths;
        let trusted_answer = !distrusted && count_ok && confidence_ok && disjoint_ok;

        let reason = if trusted_answer {
            let mut r = format!("trusted via {} distinct attester(s): [{}]", supporting.len(), supporting.join(", "));
            if self.min_confidence > 0.0 {
                r.push_str(&format!("; confidence {confidence:.3} ≥ {:.3}", self.min_confidence));
            }
            if self.min_disjoint_paths > 0 {
                r.push_str(&format!("; {disjoint_paths} vertex-disjoint path(s) ≥ {}", self.min_disjoint_paths));
            }
            r
        } else if distrusted {
            format!("a trusted agent `distrusts` `{subject}`")
        } else if !count_ok && supporting.is_empty() {
            format!("no trusted voucher attests to `{subject}` within depth {}", self.max_depth)
        } else if !count_ok {
            format!("only {} trusted attester(s); policy requires {}", supporting.len(), self.min_distinct_paths)
        } else if !confidence_ok {
            format!("confidence {confidence:.3} below required {:.3}", self.min_confidence)
        } else {
            format!(
                "only {disjoint_paths} vertex-disjoint path(s); policy requires {}",
                self.min_disjoint_paths
            )
        };
        TrustVerdict { trusted: trusted_answer, supporting, confidence, disjoint_paths, reason }
    }

    /// Confidence score per agent: roots are 1.0; a non-root's score is the noisy-OR over its incoming
    /// edges from already-trusted attesters of `attester_score × edge_confidence × recency_decay`. The
    /// fixpoint is monotone (scores only rise) and bounded by 1, so it converges within `max_depth`.
    fn confidence_scores(
        &self,
        graph: &AttestationGraph,
        domain: Option<&str>,
        at: Option<&str>,
        trusted: &BTreeSet<String>,
    ) -> HashMap<String, f64> {
        let edges = graph.positive_edges(domain);
        let subjects: BTreeSet<&String> = edges.iter().map(|e| &e.subject).collect();
        let mut score: HashMap<String, f64> = HashMap::new();
        for r in &self.trusted_roots {
            score.insert(r.clone(), 1.0);
        }
        for _ in 0..self.max_depth {
            let mut changed = false;
            for s in &subjects {
                if self.trusted_roots.contains(*s) {
                    continue; // roots stay pinned at 1.0
                }
                let mut prod = 1.0_f64; // ∏ (1 − contribution) over independent supporters
                for e in edges.iter().filter(|e| &e.subject == *s && trusted.contains(&e.attester)) {
                    let attester_score = score.get(&e.attester).copied().unwrap_or(0.0);
                    if attester_score <= 0.0 {
                        continue;
                    }
                    let conf = e.confidence.unwrap_or(1.0).clamp(0.0, 1.0);
                    let contribution = attester_score * conf * self.decay(e.issued_at.as_deref(), at);
                    prod *= 1.0 - contribution;
                }
                let new = 1.0 - prod;
                if new > score.get(*s).copied().unwrap_or(0.0) + 1e-12 {
                    score.insert((*s).clone(), new);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        score
    }

    /// Recency decay `0.5 ^ (age_days / half_life)` from `issued_at` to `at`; `1.0` when decay is off or
    /// timestamps are missing. Future-dated or same-day attestations decay by 0 (factor 1).
    fn decay(&self, issued_at: Option<&str>, at: Option<&str>) -> f64 {
        match (self.half_life_days, issued_at, at) {
            (Some(hl), Some(iss), Some(now)) if hl > 0.0 => {
                let age = (day_number(now) - day_number(iss)).max(0) as f64;
                0.5_f64.powf(age / hl)
            }
            _ => 1.0,
        }
    }

    /// Max number of internally vertex-disjoint paths from any trusted root to `subject`, over the
    /// trusted positive subgraph. Computed by vertex-splitting (each node → in/out with unit capacity,
    /// so a node lies on at most one path) and running max-flow from a super-source over the roots to
    /// the subject. This is the Menger's-theorem diversity measure (corroboration funnelled through one
    /// intermediary counts once), strictly stronger than counting distinct final attesters.
    fn max_disjoint_paths(
        &self,
        graph: &AttestationGraph,
        domain: Option<&str>,
        trusted: &BTreeSet<String>,
        subject: &str,
    ) -> usize {
        const INF: i64 = 1 << 30;
        // The trusted positive edges that can be on a path to the subject.
        let edges: Vec<(&String, &String)> = graph
            .positive_edges(domain)
            .into_iter()
            .filter(|e| trusted.contains(&e.attester) && (e.subject == subject || trusted.contains(&e.subject)))
            .map(|e| (&e.attester, &e.subject))
            .collect();
        if edges.is_empty() {
            return 0;
        }
        // Node ids: 0 = super-source, 1 = subject (sink, not split). A trusted **root** is one uncapped
        // node (sources may be shared — the roots are the trust anchors). A non-root **intermediary** is
        // split into in/out with a unit edge (so it lies on at most one path — the Sybil constraint).
        // Edges *into the subject* carry unit capacity: a direct vouch is one path, and this bounds the
        // flow so all-INF paths can't augment forever.
        let mut entry: HashMap<String, usize> = HashMap::new(); // where an edge into the node arrives
        let mut exit: HashMap<String, usize> = HashMap::new(); // where an edge out of the node departs
        let mut cap: HashMap<(usize, usize), i64> = HashMap::new();
        let mut adj: HashMap<usize, BTreeSet<usize>> = HashMap::new();
        let mut next = 2usize;

        let mut names: BTreeSet<&String> = BTreeSet::new();
        for (u, v) in &edges {
            names.insert(u);
            if *v != subject {
                names.insert(v);
            }
        }
        for name in names {
            if self.trusted_roots.contains(name) {
                let id = next;
                next += 1;
                entry.insert(name.clone(), id);
                exit.insert(name.clone(), id);
                mf_add(&mut cap, &mut adj, 0, id, INF); // super-source → root (uncapped)
            } else {
                let (i, o) = (next, next + 1);
                next += 2;
                entry.insert(name.clone(), i);
                exit.insert(name.clone(), o);
                mf_add(&mut cap, &mut adj, i, o, 1); // unit vertex capacity on the intermediary
            }
        }
        for (u, v) in &edges {
            let from = exit[*u];
            if *v == subject {
                mf_add(&mut cap, &mut adj, from, 1, 1); // unit edge into the sink: one path per direct voucher
            } else {
                mf_add(&mut cap, &mut adj, from, entry[*v], INF);
            }
        }
        mf_maxflow(&mut cap, &adj, 0, 1)
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

    /// Whether a **function** (`subject`, a `fn_…` content-address) is certified *by a certifier this policy
    /// trusts*, given the attestation graph (which ingests signed certification records as `certifies`
    /// edges). A certification is only as good as its certifier: this intersects the function's certifiers
    /// with the agents the policy derives as trusted (via `evaluate_trust`), so a certificate signed by an
    /// unknown key counts for nothing. Lets the agent loop rely on a trusted third party's certification
    /// (trust-delegation) instead of re-running every check itself.
    pub fn certification_verdict(
        &self,
        graph: &AttestationGraph,
        subject: &str,
        domain: Option<&str>,
        at: Option<&str>,
    ) -> CertificationVerdict {
        let trusted_certifiers: Vec<String> = graph
            .certifiers(subject)
            .into_iter()
            .filter(|c| self.evaluate_trust(graph, c, domain, at).trusted)
            .collect();
        let certified = !trusted_certifiers.is_empty();
        let reason = if certified {
            format!("certified by {} trusted certifier(s)", trusted_certifiers.len())
        } else if graph.certifiers(subject).is_empty() {
            "no certification found for this function".to_string()
        } else {
            "certified, but no certifier is trusted under this policy".to_string()
        };
        CertificationVerdict { certified, trusted_certifiers, reason }
    }

    /// Whether a **weights record** (`subject`, a `wgt_…` content-address) has its measured capability
    /// attested *by a certifier this policy trusts*, given the attestation graph (which ingests signed
    /// eval-attestation records as `attests-eval` edges). The weights counterpart of
    /// [`Self::certification_verdict`]: for opaque bytes the attestation layer is the artifact's value,
    /// and an attestation signed by an unknown key counts for nothing.
    pub fn eval_attestation_verdict(
        &self,
        graph: &AttestationGraph,
        subject: &str,
        domain: Option<&str>,
        at: Option<&str>,
    ) -> CertificationVerdict {
        let trusted_certifiers: Vec<String> = graph
            .eval_attestors(subject)
            .into_iter()
            .filter(|c| self.evaluate_trust(graph, c, domain, at).trusted)
            .collect();
        let certified = !trusted_certifiers.is_empty();
        let reason = if certified {
            format!("eval-attested by {} trusted certifier(s)", trusted_certifiers.len())
        } else if graph.eval_attestors(subject).is_empty() {
            "no eval attestation found for these weights".to_string()
        } else {
            "eval-attested, but no attesting certifier is trusted under this policy".to_string()
        };
        CertificationVerdict { certified, trusted_certifiers, reason }
    }
}

/// The result of a certification query: is a function certified by a certifier the policy trusts?
#[derive(Debug, Clone)]
pub struct CertificationVerdict {
    pub certified: bool,
    /// The trusted certifiers whose signed certification backs this function.
    pub trusted_certifiers: Vec<String>,
    pub reason: String,
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
            min_confidence: 0.0,
            half_life_days: None,
            min_disjoint_paths: 0,
            satisfied_conditions: BTreeSet::new(),
        }
    }

    fn certification(certifier_seed: &str, subject: &str) -> J {
        use crate::{sign_artifact, ArtifactKind};
        let mut c = json!({
            "schema_version": "0.2.0", "kind": "certification", "subject": subject,
            "body_hash": "expr_0000000000000000000000000000000000000000000000000000000000000000",
            "checks": [{ "check": "typecheck", "verdict": "WELL-TYPED", "detail": "" }],
            "certified": true,
        });
        sign_artifact(&mut c, &signing_key_from_seed(certifier_seed), ArtifactKind::Certification).unwrap();
        c
    }

    #[test]
    fn certification_by_a_trusted_certifier_counts() {
        // root vouches for `carol`; carol certifies function F. Under the policy, F is certified because a
        // TRUSTED certifier signed its certification.
        let (root, carol) = (did("root"), did("carol"));
        let f = format!("fn_{}", "a".repeat(64));
        let g = AttestationGraph::from_messages(
            &[attest("root", &carol, "vouches-for", None), certification("carol", &f)],
            None,
        );
        let v = policy(&[&root], 1).certification_verdict(&g, &f, None, None);
        assert!(v.certified, "{}", v.reason);
        assert!(v.trusted_certifiers.contains(&carol));
    }

    #[test]
    fn certification_by_an_untrusted_certifier_does_not_count() {
        // `mallory` certifies F but no trusted root vouches for mallory → the certificate carries no weight.
        let root = did("root");
        let f = format!("fn_{}", "b".repeat(64));
        let g = AttestationGraph::from_messages(&[certification("mallory", &f)], None);
        let v = policy(&[&root], 1).certification_verdict(&g, &f, None, None);
        assert!(!v.certified, "an untrusted certifier's certification must not count");
    }

    fn eval_attestation(certifier_seed: &str, subject: &str) -> J {
        use crate::{sign_artifact, ArtifactKind};
        let mut a = json!({
            "schema_version": "0.1.0", "kind": "eval-attestation", "subject": subject,
            "eval": { "harness": "tooling/eval/eval_harness.py", "task_set": { "tasks": 360 } },
            "results": { "write": { "pass": 167, "total": 179 } },
        });
        sign_artifact(&mut a, &signing_key_from_seed(certifier_seed), ArtifactKind::EvalAttestation).unwrap();
        a
    }

    #[test]
    fn eval_attestation_by_a_trusted_certifier_counts() {
        // root vouches for `carol`; carol attests the measured eval of weights W. Under the policy, W is
        // attested because a TRUSTED certifier signed the attestation — the weights analogue of
        // `certification_by_a_trusted_certifier_counts`.
        let (root, carol) = (did("root"), did("carol"));
        let w = format!("wgt_{}", "c".repeat(64));
        let g = AttestationGraph::from_messages(
            &[attest("root", &carol, "vouches-for", None), eval_attestation("carol", &w)],
            None,
        );
        let v = policy(&[&root], 1).eval_attestation_verdict(&g, &w, None, None);
        assert!(v.certified, "{}", v.reason);
        assert!(v.trusted_certifiers.contains(&carol));
    }

    #[test]
    fn eval_attestation_by_an_untrusted_certifier_does_not_count() {
        let root = did("root");
        let w = format!("wgt_{}", "d".repeat(64));
        let g = AttestationGraph::from_messages(&[eval_attestation("mallory", &w)], None);
        let v = policy(&[&root], 1).eval_attestation_verdict(&g, &w, None, None);
        assert!(!v.certified, "an untrusted certifier's eval attestation must not count");
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
        assert!(p.evaluate_trust(&g, &alice, None, None).trusted);
        assert!(p.evaluate_trust(&g, &bob, None, None).trusted, "trust is transitive within depth");
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
        assert!(!p.evaluate_trust(&g, &bob, None, None).trusted, "one voucher is insufficient under min_distinct_paths=2");
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
        let v = p.evaluate_trust(&g, &bob, None, None);
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
        assert!(!p.evaluate_trust(&g, &bob, None, None).trusted, "a trusted distrust overrides the positive path");
    }

    #[test]
    fn domain_scoped_trust_only_applies_in_its_domain() {
        let (root, alice) = (did("root"), did("alice"));
        let g = AttestationGraph::from_messages(
            &[attest("root", &alice, "trusts-claims-about", Some("rust_ingestion"))],
            None,
        );
        let p = policy(&[&root], 1);
        assert!(p.evaluate_trust(&g, &alice, Some("rust_ingestion"), None).trusted);
        assert!(!p.evaluate_trust(&g, &alice, Some("crypto"), None).trusted, "domain-scoped trust does not transfer domains");
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
        assert_eq!(p.min_confidence, 0.0);
        assert_eq!(p.half_life_days, None);
        assert_eq!(p.min_disjoint_paths, 0);
        assert!(p.trusted_roots.contains("did:nova:abc"));
    }

    // ---- richer policy: confidence-weighting, recency decay, vertex-disjoint diversity ----

    /// A `vouches-for` attestation carrying an optional `confidence` and `issued_at`.
    fn attest_full(seed: &str, subject: &str, confidence: Option<f64>, issued_at: Option<&str>) -> J {
        let mut m = json!({
            "schema_version": "0.2.0", "kind": "assert", "to": null, "in_reply_to": null,
            "body": { "subject": subject, "claim": {
                "kind": "attestation", "subject": subject, "verb": "vouches-for", "domain": null,
                "confidence": confidence, "issued_at": issued_at, "expires_at": null } }
        });
        sign_message(&mut m, &signing_key_from_seed(seed)).unwrap();
        m
    }

    #[test]
    fn confidence_gate_blocks_low_confidence() {
        // root fully vouches alice; alice vouches bob with confidence 0.5.
        let (root, alice, bob) = (did("root"), did("alice"), did("bob"));
        let g = AttestationGraph::from_messages(
            &[attest("root", &alice, "vouches-for", None), attest_full("alice", &bob, Some(0.5), None)],
            None,
        );
        let mut p = policy(&[&root], 1);
        p.min_confidence = 0.8;
        let v = p.evaluate_trust(&g, &bob, None, None);
        assert!(!v.trusted, "{}", v.reason);
        assert!((v.confidence - 0.5).abs() < 1e-9, "confidence {} ", v.confidence);
        p.min_confidence = 0.4;
        assert!(p.evaluate_trust(&g, &bob, None, None).trusted, "0.5 clears a 0.4 bar");
    }

    #[test]
    fn confidence_combines_supporters_by_noisy_or() {
        // Two independent 0.5 supporters → 1 − (0.5)(0.5) = 0.75.
        let (root, alice, carol, bob) = (did("root"), did("alice"), did("carol"), did("bob"));
        let g = AttestationGraph::from_messages(
            &[
                attest("root", &alice, "vouches-for", None),
                attest("root", &carol, "vouches-for", None),
                attest_full("alice", &bob, Some(0.5), None),
                attest_full("carol", &bob, Some(0.5), None),
            ],
            None,
        );
        let mut p = policy(&[&root], 1);
        p.min_confidence = 0.7;
        let v = p.evaluate_trust(&g, &bob, None, None);
        assert!((v.confidence - 0.75).abs() < 1e-9, "noisy-OR confidence {}", v.confidence);
        assert!(v.trusted, "0.75 ≥ 0.7: {}", v.reason);
    }

    #[test]
    fn recency_decay_ages_out_a_stale_voucher() {
        // alice vouches bob (full confidence) on 2026-01-01; half-life 30 days, bar 0.5.
        let (root, alice, bob) = (did("root"), did("alice"), did("bob"));
        let g = AttestationGraph::from_messages(
            &[
                attest("root", &alice, "vouches-for", None),
                attest_full("alice", &bob, Some(1.0), Some("2026-01-01T00:00:00Z")),
            ],
            None,
        );
        let mut p = policy(&[&root], 1);
        p.min_confidence = 0.5;
        p.half_life_days = Some(30.0);
        // 1 day old → decay ≈ 0.98 → trusted.
        assert!(p.evaluate_trust(&g, &bob, None, Some("2026-01-02T00:00:00Z")).trusted);
        // ~181 days old → decay ≈ 0.015 → below the bar.
        let stale = p.evaluate_trust(&g, &bob, None, Some("2026-07-01T00:00:00Z"));
        assert!(!stale.trusted, "stale confidence {}", stale.confidence);
    }

    #[test]
    fn vertex_disjoint_paths_detect_a_funnel() {
        // root→a, root→b; a→m, b→m; m→x, m→y; x→bob, y→bob.
        // bob has 2 distinct final attesters (x, y) — but both route through `m`.
        let (root, a, b, m, x, y, bob) =
            (did("root"), did("a"), did("b"), did("m"), did("x"), did("y"), did("bob"));
        let g = AttestationGraph::from_messages(
            &[
                attest("root", &a, "vouches-for", None),
                attest("root", &b, "vouches-for", None),
                attest("a", &m, "vouches-for", None),
                attest("b", &m, "vouches-for", None),
                attest("m", &x, "vouches-for", None),
                attest("m", &y, "vouches-for", None),
                attest("x", &bob, "vouches-for", None),
                attest("y", &bob, "vouches-for", None),
            ],
            None,
        );
        let mut p = policy(&[&root], 2);
        // The distinct-attester count gate alone is satisfied (x and y both attest bob).
        assert!(p.evaluate_trust(&g, &bob, None, None).trusted, "count gate sees two attesters");
        // The vertex-disjoint gate sees that both paths funnel through `m` → only one disjoint path.
        p.min_disjoint_paths = 2;
        let v = p.evaluate_trust(&g, &bob, None, None);
        assert_eq!(v.disjoint_paths, 1, "the funnel through `m` admits one disjoint path");
        assert!(!v.trusted, "{}", v.reason);
    }

    #[test]
    fn vertex_disjoint_paths_pass_when_independent() {
        // root→a, root→b; a→x, b→y; x→bob, y→bob. Two independent chains, no shared intermediary.
        let (root, a, b, x, y, bob) =
            (did("root"), did("a"), did("b"), did("x"), did("y"), did("bob"));
        let g = AttestationGraph::from_messages(
            &[
                attest("root", &a, "vouches-for", None),
                attest("root", &b, "vouches-for", None),
                attest("a", &x, "vouches-for", None),
                attest("b", &y, "vouches-for", None),
                attest("x", &bob, "vouches-for", None),
                attest("y", &bob, "vouches-for", None),
            ],
            None,
        );
        let mut p = policy(&[&root], 2);
        p.min_disjoint_paths = 2;
        let v = p.evaluate_trust(&g, &bob, None, None);
        assert_eq!(v.disjoint_paths, 2, "two vertex-disjoint chains");
        assert!(v.trusted, "{}", v.reason);
    }
}
