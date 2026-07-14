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
    /// When the attestation was issued (RFC 3339 UTC). Drives recency decay in the policy engine.
    pub issued_at: Option<String>,
    pub expires_at: Option<String>,
    /// The `assert` message's content-address — what a `retract` targets.
    pub hash: String,
    /// For a symmetric artifact relation (`equivalent-to`): the other endpoint. `None` for the
    /// agent-trust verbs, whose relation is attester → subject.
    pub peer: Option<String>,
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
            match m.get("kind").and_then(|k| k.as_str()) {
                // A signed attestation `assert`: `<attester> <verb> <subject>`. A signed
                // **equivalence** `assert` (claim kind `equivalent`, spec/claim-expression) also
                // lands here: it contributes a SYMMETRIC `equivalent-to` edge pair between the two
                // functions. Like `certifies` it is objective and re-checkable (verify-claim
                // re-proves it from the two bodies) and on a separate axis from agent trust — the
                // graph records who signed it, and a consumer prices that testimony (or re-proves
                // locally) before acting on it.
                Some("assert") => {
                    let claim_kind = m.pointer("/body/claim/kind").and_then(|k| k.as_str());
                    if claim_kind != Some("attestation") && claim_kind != Some("equivalent") {
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
                    if claim_kind == Some("equivalent") {
                        // A DOMAIN-QUALIFIED equivalence (`∀x. D(x) ⇒ a(x) = b(x)`) is not an
                        // unconditional edge: substitution is licensed only on the domain, and
                        // an `equivalent-to` edge would let every collapse view merge the pair
                        // for arbitrary applications. The claim stays queryable (the node serves
                        // it under /equivalences); it just never enters the graph.
                        if claim.get("domain").is_some() {
                            continue;
                        }
                        let (Some(attester), Some(a), Some(b)) = (
                            m.get("from").and_then(|f| f.as_str()),
                            claim.get("a").and_then(|s| s.as_str()),
                            claim.get("b").and_then(|s| s.as_str()),
                        ) else {
                            continue;
                        };
                        for (subject, peer) in [(a, b), (b, a)] {
                            edges.push(Attestation {
                                attester: attester.to_string(),
                                subject: subject.to_string(),
                                verb: "equivalent-to".to_string(),
                                domain: None,
                                confidence: None,
                                issued_at: m.get("timestamp").and_then(|t| t.as_str()).map(String::from),
                                expires_at: None,
                                hash: hash.clone(),
                                peer: Some(peer.to_string()),
                            });
                        }
                        continue;
                    }
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
                        issued_at: claim.get("issued_at").and_then(|t| t.as_str()).map(String::from),
                        expires_at,
                        hash,
                        peer: None,
                    });
                }
                // A signed **certification** record (`certify --sign`): `<certifier> certifies <function>`.
                // It is an *objective, re-checkable* attestation — the certifier ran every verified-by-default
                // check and signed the result — so a positive one contributes a `certifies` edge (a distinct
                // axis from vouches-for: it does not make the *certifier* trusted, only records that this
                // function passed verification under that certifier). A `certified: false` record is not an
                // endorsement, so it adds no edge.
                Some("certification") => {
                    if m.get("certified").and_then(|c| c.as_bool()) != Some(true) {
                        continue;
                    }
                    let hash = match m.get("hash").and_then(|h| h.as_str()) {
                        Some(h) if !retracted.contains(h) => h.to_string(),
                        _ => continue,
                    };
                    if crate::verify_signature(m).is_err() {
                        continue;
                    }
                    let (Some(attester), Some(subject)) = (
                        m.get("from").and_then(|f| f.as_str()),
                        m.get("subject").and_then(|s| s.as_str()),
                    ) else {
                        continue;
                    };
                    edges.push(Attestation {
                        attester: attester.to_string(),
                        subject: subject.to_string(),
                        verb: "certifies".to_string(),
                        domain: None,
                        confidence: None,
                        issued_at: m.get("timestamp").and_then(|t| t.as_str()).map(String::from),
                        expires_at: None,
                        hash,
                        peer: None,
                    });
                }
                // A signed **eval attestation** (`attest-weights --sign`): `<certifier> attests-eval
                // <weights>`. The weights analogue of a certification — an accountable, re-runnable
                // measured-capability statement about a `wgt_` record. Like `certifies`, it is a
                // separate axis from vouches-for: it records that this certifier measured the model,
                // not that anyone is trusted.
                Some("eval-attestation") => {
                    let hash = match m.get("hash").and_then(|h| h.as_str()) {
                        Some(h) if !retracted.contains(h) => h.to_string(),
                        _ => continue,
                    };
                    if crate::verify_signature(m).is_err() {
                        continue;
                    }
                    let (Some(attester), Some(subject)) = (
                        m.get("from").and_then(|f| f.as_str()),
                        m.get("subject").and_then(|s| s.as_str()),
                    ) else {
                        continue;
                    };
                    edges.push(Attestation {
                        attester: attester.to_string(),
                        subject: subject.to_string(),
                        verb: "attests-eval".to_string(),
                        domain: None,
                        confidence: None,
                        issued_at: m.get("timestamp").and_then(|t| t.as_str()).map(String::from),
                        expires_at: None,
                        hash,
                        peer: None,
                    });
                }
                _ => continue,
            }
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

    /// Distinct certifiers who have signed a positive `certifies` edge for `subject` (a function's
    /// content-address). These are the agents whose certification the policy can weigh (a certification is
    /// meaningful only when the certifier is itself trusted).
    pub fn certifiers(&self, subject: &str) -> BTreeSet<String> {
        self.edges
            .iter()
            .filter(|e| e.subject == subject && e.verb == "certifies")
            .map(|e| e.attester.clone())
            .collect()
    }

    /// Distinct certifiers who have signed an `attests-eval` edge for `subject` (a weights record's
    /// content-address). The weights counterpart of [`Self::certifiers`]: an eval attestation is
    /// meaningful only when the attesting certifier is itself trusted under the consumer's policy.
    pub fn eval_attestors(&self, subject: &str) -> BTreeSet<String> {
        self.edges
            .iter()
            .filter(|e| e.subject == subject && e.verb == "attests-eval")
            .map(|e| e.attester.clone())
            .collect()
    }

    /// The functions attested extensionally equivalent to `subject` — the TRANSITIVE closure over
    /// `equivalent-to` edges (equivalence classes compose: a≡b and b≡c puts c in a's class). Every
    /// edge here came from a signed, signature-verified `equivalent` claim; whether to *act* on one
    /// is still the consumer's call (re-prove locally, or price the signer's testimony — see
    /// [`Self::equivalence_edge`] for per-edge attribution).
    pub fn equivalents(&self, subject: &str) -> BTreeSet<String> {
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut frontier = vec![subject.to_string()];
        while let Some(cur) = frontier.pop() {
            for e in self.edges.iter().filter(|e| e.verb == "equivalent-to" && e.subject == cur) {
                if let Some(p) = &e.peer {
                    if p != subject && seen.insert(p.clone()) {
                        frontier.push(p.clone());
                    }
                }
            }
        }
        seen
    }

    /// The `equivalent-to` edge connecting `x` and `y` DIRECTLY (either orientation), if any —
    /// exposes who signed it, so a policy can decide whether that asserter's testimony counts.
    pub fn equivalence_edge(&self, x: &str, y: &str) -> Option<&Attestation> {
        self.edges.iter().find(|e| {
            e.verb == "equivalent-to" && e.subject == x && e.peer.as_deref() == Some(y)
        })
    }

    /// Attesters who `distrusts` `subject`.
    pub fn distrusters(&self, subject: &str) -> BTreeSet<String> {
        self.edges
            .iter()
            .filter(|e| e.subject == subject && e.is_distrust())
            .map(|e| e.attester.clone())
            .collect()
    }

    /// Positive edges applying to `domain`, with their metadata (confidence/issued_at) — the policy
    /// engine needs these for confidence-weighting, recency decay, and vertex-disjoint path counting.
    pub(crate) fn positive_edges(&self, domain: Option<&str>) -> Vec<&Attestation> {
        self.edges.iter().filter(|e| e.is_positive() && e.applies_to_domain(domain)).collect()
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

    // ---- equivalence claims as symmetric `equivalent-to` edges ----

    fn fn_addr(fill: char) -> String {
        format!("fn_{}", fill.to_string().repeat(64))
    }

    /// A signed equivalence `assert` (claim kind `equivalent`) between two functions.
    fn equiv_assert(asserter_seed: &str, a: &str, b: &str) -> J {
        crate::respond::build_equivalence_assert(
            a, b, "normal-form", None, &did(asserter_seed), Some("2026-07-13T00:00:00Z"),
            &signing_key_from_seed(asserter_seed),
        )
        .unwrap()
    }

    #[test]
    fn equivalence_claim_yields_symmetric_edges() {
        let (a, b) = (fn_addr('1'), fn_addr('2'));
        let g = AttestationGraph::from_messages(&[equiv_assert("alice", &a, &b)], None);
        assert_eq!(g.len(), 2, "one claim, two directed edges");
        assert!(g.equivalents(&a).contains(&b));
        assert!(g.equivalents(&b).contains(&a));
        let e = g.equivalence_edge(&a, &b).expect("direct edge either orientation");
        assert_eq!(e.attester, did("alice"));
        assert_eq!(e.verb, "equivalent-to");
    }

    #[test]
    fn domain_qualified_claim_never_becomes_an_edge() {
        // `∀x. D(x) ⇒ a(x) = b(x)` licenses substitution only ON the domain — an unconditional
        // `equivalent-to` edge would let every collapse view merge the pair for arbitrary
        // applications. The graph must skip it entirely.
        let (a, b) = (fn_addr('1'), fn_addr('2'));
        let domain = serde_json::json!({ "vars": ["n"], "expr": { "kind": "app", "op": "ge", "args": [
            { "kind": "var", "name": "n" }, { "kind": "lit", "value": { "kind": "int", "value": 0 } }] } });
        let m = crate::respond::build_equivalence_assert(
            &a, &b, "induction", Some(&domain), &did("alice"), Some("2026-07-14T00:00:00Z"),
            &signing_key_from_seed("alice"),
        )
        .unwrap();
        let g = AttestationGraph::from_messages(&[m], None);
        assert_eq!(g.len(), 0, "a domain-qualified claim contributes no edges");
        assert!(g.equivalents(&a).is_empty());
        assert!(g.equivalence_edge(&a, &b).is_none());
    }

    #[test]
    fn equivalents_closure_is_transitive() {
        let (a, b, c) = (fn_addr('1'), fn_addr('2'), fn_addr('3'));
        let g = AttestationGraph::from_messages(
            &[equiv_assert("alice", &a, &b), equiv_assert("carol", &b, &c)],
            None,
        );
        let class = g.equivalents(&a);
        assert!(class.contains(&b) && class.contains(&c), "a≡b, b≡c puts c in a's class");
        assert!(!class.contains(&a), "the closure reports the OTHERS in the class");
        // But the direct edge between a and c does not exist — attribution stays per-claim.
        assert!(g.equivalence_edge(&a, &c).is_none());
    }

    #[test]
    fn retracted_equivalence_claim_drops_both_edges() {
        let (a, b) = (fn_addr('1'), fn_addr('2'));
        let m = equiv_assert("alice", &a, &b);
        let h = m["hash"].as_str().unwrap().to_string();
        let g = AttestationGraph::from_messages(&[m, retract("alice", &h)], None);
        assert!(g.is_empty());
    }

    #[test]
    fn tampered_equivalence_claim_is_skipped() {
        let (a, b) = (fn_addr('1'), fn_addr('2'));
        let mut m = equiv_assert("alice", &a, &b);
        m["body"]["claim"]["b"] = json!(fn_addr('9')); // tamper after signing
        let g = AttestationGraph::from_messages(&[m], None);
        assert!(g.is_empty());
    }

    // ---- certifications as `certifies` edges ----

    /// A signed certification record (`certify --sign`): `certifier` certifies function `subject`.
    fn certification(certifier_seed: &str, subject: &str, certified: bool) -> J {
        use crate::{sign_artifact, ArtifactKind};
        let mut c = json!({
            "schema_version": "0.2.0", "kind": "certification", "subject": subject,
            "body_hash": "expr_0000000000000000000000000000000000000000000000000000000000000000",
            "checks": [{ "check": "typecheck", "verdict": "WELL-TYPED", "detail": "" }],
            "certified": certified,
        });
        sign_artifact(&mut c, &signing_key_from_seed(certifier_seed), ArtifactKind::Certification).unwrap();
        c
    }

    #[test]
    fn certification_builds_a_certifies_edge() {
        let f = format!("fn_{}", "1".repeat(64));
        let g = AttestationGraph::from_messages(&[certification("carol", &f, true)], None);
        assert!(g.certifiers(&f).contains(&did("carol")), "a positive certification names its certifier");
        // A certifies edge is a SEPARATE axis — it does not make the function a `vouches-for` subject.
        assert!(g.positive_attesters(&f, None).is_empty(), "certifies is not vouches-for");
    }

    #[test]
    fn uncertified_record_adds_no_edge() {
        let f = format!("fn_{}", "2".repeat(64));
        let g = AttestationGraph::from_messages(&[certification("carol", &f, false)], None);
        assert!(g.certifiers(&f).is_empty(), "a `certified: false` record is not an endorsement");
    }

    #[test]
    fn tampered_certification_is_skipped() {
        let f = format!("fn_{}", "3".repeat(64));
        let mut c = certification("carol", &f, true);
        c["subject"] = json!(format!("fn_{}", "9".repeat(64))); // repoint after signing
        let g = AttestationGraph::from_messages(&[c], None);
        assert!(g.is_empty(), "a tampered certification fails signature verification and is skipped");
    }

    #[test]
    fn retracted_certification_drops_the_edge() {
        let f = format!("fn_{}", "4".repeat(64));
        let c = certification("carol", &f, true);
        let h = c["hash"].as_str().unwrap().to_string();
        let g = AttestationGraph::from_messages(&[c, retract("carol", &h)], None);
        assert!(g.certifiers(&f).is_empty(), "a retracted certification is dropped");
    }

    // ---- eval attestations as `attests-eval` edges ----

    /// A signed eval attestation (`attest-weights --sign`): `certifier` attests measured capability
    /// of weights `subject`.
    fn eval_attestation(certifier_seed: &str, subject: &str) -> J {
        use crate::{sign_artifact, ArtifactKind};
        let mut a = json!({
            "schema_version": "0.1.0", "kind": "eval-attestation", "subject": subject,
            "eval": { "harness": "tooling/eval/eval_harness.py",
                      "task_set": { "tasks": 360 } },
            "results": { "write": { "pass": 167, "total": 179 } },
        });
        sign_artifact(&mut a, &signing_key_from_seed(certifier_seed), ArtifactKind::EvalAttestation).unwrap();
        a
    }

    #[test]
    fn eval_attestation_builds_an_attests_eval_edge() {
        let w = format!("wgt_{}", "5".repeat(64));
        let g = AttestationGraph::from_messages(&[eval_attestation("carol", &w)], None);
        assert!(g.eval_attestors(&w).contains(&did("carol")), "an eval attestation names its certifier");
        // A separate axis from both vouches-for and certifies.
        assert!(g.positive_attesters(&w, None).is_empty(), "attests-eval is not vouches-for");
        assert!(g.certifiers(&w).is_empty(), "attests-eval is not certifies");
    }

    #[test]
    fn tampered_eval_attestation_is_skipped() {
        let w = format!("wgt_{}", "6".repeat(64));
        let mut a = eval_attestation("carol", &w);
        a["results"] = json!({ "write": { "pass": 179, "total": 179 } }); // inflate after signing
        let g = AttestationGraph::from_messages(&[a], None);
        assert!(g.is_empty(), "a tampered eval attestation fails signature verification and is skipped");
    }

    #[test]
    fn retracted_eval_attestation_drops_the_edge() {
        let w = format!("wgt_{}", "7".repeat(64));
        let a = eval_attestation("carol", &w);
        let h = a["hash"].as_str().unwrap().to_string();
        let g = AttestationGraph::from_messages(&[a, retract("carol", &h)], None);
        assert!(g.eval_attestors(&w).is_empty(), "a retracted eval attestation is dropped");
    }
}
