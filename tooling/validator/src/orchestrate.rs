//! Autonomous multi-step orchestration — the agent loop driven end to end.
//!
//! Given a goal (apply a commons function *found by intent* to some arguments), the orchestrator runs
//! a full Nova Locutio conversation against a responder: **QUERY** the commons to discover a function
//! → **PROPOSE** applying it → receive the responder's **COMMIT** → the committer fulfils the
//! commitment and **ASSERT**s the result → the orchestrator **VERIFIES** the claim by re-running it.
//! Every message is signed and threaded; the outcome is self-verifying (principles 1, 3, 4, 6, 7).
//! This is "assemble, don't write" (principle 4) made autonomous — the agent never names the
//! function, it discovers one.

use anyhow::Result;
use ed25519_dalek::SigningKey;
use serde_json::{json, Value as J};
use std::path::Path;

use crate::{
    build_link_map, build_record_map, did_nova_from_pubkey, prove_by_induction_with_exploration,
    prove_property, respond_to_message, sign_message, verify_claim, AttestationGraph, InductionOutcome,
    Policy, ProofOutcome, DEFAULT_LEMMA_DEPTH,
};

/// One message in the orchestrated conversation.
pub struct Step {
    pub label: String,
    pub message: J,
}

/// The transcript of an orchestrated run plus whether the final assert's claim re-ran true.
pub struct Run {
    pub steps: Vec<Step>,
    pub confirmed: bool,
}

/// Drive a multi-stage `query → propose → commit → assert → verify` pipeline. `stages` is one intent
/// tag per stage: each stage discovers a function by that intent and applies it to the previous
/// stage's result (the first stage to `args`), composing the discovered functions. Every stage's
/// claim is verified; `Run.confirmed` is true iff all did. `orchestrator` signs the outbound
/// query/propose; `responder` signs the replies. `timestamp` is advisory (None → deterministic).
pub fn orchestrate(
    records_dir: &Path,
    stages: &[String],
    args: Vec<J>,
    orchestrator: &SigningKey,
    responder: &SigningKey,
    timestamp: Option<&str>,
) -> Result<Run> {
    let link = build_link_map(records_dir)?;
    let records = build_record_map(records_dir)?;
    let responder_did = did_nova_from_pubkey(&responder.verifying_key());
    let mut steps = Vec::new();
    let mut stage_args = args; // stage 0 gets the initial args; each later stage gets [prev result]
    let mut confirmed = !stages.is_empty();
    let multi = stages.len() > 1;

    for (i, intent) in stages.iter().enumerate() {
        let pfx = if multi { format!("{i}:") } else { String::new() };

        // QUERY the commons for a function matching this stage's intent.
        let mut query = json!({
            "schema_version": "0.2.0", "kind": "query", "to": responder_did,
            "in_reply_to": null, "timestamp": timestamp, "constraints": null,
            "body": { "pattern": { "intent_tags": [intent] } }
        });
        sign_message(&mut query, orchestrator)?;
        steps.push(Step { label: format!("{pfx}query"), message: query.clone() });

        let ack = respond_to_message(&query, link.clone(), records.clone(), responder, timestamp)?;
        let target = match ack.pointer("/body/result/matches/0").and_then(|m| m.as_str()) {
            Some(t) => t.to_string(),
            None => {
                steps.push(Step { label: format!("{pfx}ack"), message: ack });
                return Ok(Run { steps, confirmed: false }); // nothing discovered for this intent
            }
        };
        steps.push(Step { label: format!("{pfx}ack"), message: ack });

        // PROPOSE applying the discovered function to this stage's args.
        let mut propose = json!({
            "schema_version": "0.2.0", "kind": "propose", "to": responder_did,
            "in_reply_to": null, "timestamp": timestamp, "constraints": null,
            "body": { "action": "apply", "target": target, "args": stage_args }
        });
        sign_message(&mut propose, orchestrator)?;
        steps.push(Step { label: format!("{pfx}propose"), message: propose.clone() });

        // The responder COMMITs (or rejects), then fulfils → ASSERT.
        let commit = respond_to_message(&propose, link.clone(), records.clone(), responder, timestamp)?;
        let kind = commit.get("kind").and_then(|k| k.as_str()).unwrap_or("?").to_string();
        steps.push(Step { label: format!("{pfx}{kind}"), message: commit.clone() });
        if kind != "commit" {
            return Ok(Run { steps, confirmed: false });
        }
        let assert = respond_to_message(&commit, link.clone(), records.clone(), responder, timestamp)?;

        // Verify this stage's claim, and thread its result into the next stage.
        confirmed = confirmed && verify_claim(&assert, link.clone()).unwrap_or(false);
        match assert.pointer("/body/claim/expr/args/1/value").cloned() {
            Some(result) => stage_args = vec![result],
            None => confirmed = false,
        }
        steps.push(Step { label: format!("{pfx}assert"), message: assert });
    }

    Ok(Run { steps, confirmed })
}

/// The transcript of a *verified* orchestration: discover → trust-gate → prove the function's own
/// declared property → apply → re-verify the result.
pub struct VerifiedRun {
    pub steps: Vec<Step>,
    /// Whether the discovered function is trusted under the policy (`None` if no policy was supplied).
    pub trusted: Option<bool>,
    /// `(property name, proved?)` for the discovered function's first `forall` property (`None` if it
    /// has none, or if the run aborted before the proof step).
    pub property: Option<(String, bool)>,
    /// Whether the applied result's claim re-verified by re-running it.
    pub confirmed: bool,
}

/// Try to prove a `forall` law: first-order SMT, then induction with lemma discovery (mirrors `prove`).
fn prove_law(expr: &J, body: Option<&J>, solver: &str) -> bool {
    match prove_property(expr, body, solver).0 {
        ProofOutcome::Proved => true,
        ProofOutcome::Unsupported(_) => matches!(
            prove_by_induction_with_exploration(expr, body, solver, DEFAULT_LEMMA_DEPTH).0,
            InductionOutcome::Proved | InductionOutcome::ProvedWithLemmas(_)
        ),
        _ => false,
    }
}

/// The agent loop with verification folded in — the project's thesis in one autonomous run: **discover**
/// a function by intent, decide whether to **trust** it (the receiver's local policy over an attestation
/// graph — principle 7), **prove** its own declared property over the unbounded domain (don't trust the
/// record's claim — re-prove it), then **apply** it and **re-verify** the result by re-running. Ties the
/// commons, the trust model, the prover, and the message loop together. A `policy` of `None` skips the
/// trust gate; an untrusted function aborts the run before it is used.
#[allow(clippy::too_many_arguments)]
pub fn orchestrate_verified(
    records_dir: &Path,
    intent: &str,
    args: Vec<J>,
    orchestrator: &SigningKey,
    responder: &SigningKey,
    solver: &str,
    policy: Option<&Policy>,
    attestations: &[J],
    timestamp: Option<&str>,
) -> Result<VerifiedRun> {
    let link = build_link_map(records_dir)?;
    let records = build_record_map(records_dir)?;
    let responder_did = did_nova_from_pubkey(&responder.verifying_key());
    let mut steps = Vec::new();

    // DISCOVER: query the commons by intent.
    let mut query = json!({
        "schema_version": "0.2.0", "kind": "query", "to": responder_did,
        "in_reply_to": null, "timestamp": timestamp, "constraints": null,
        "body": { "pattern": { "intent_tags": [intent] } }
    });
    sign_message(&mut query, orchestrator)?;
    steps.push(Step { label: "query".into(), message: query.clone() });
    let ack = respond_to_message(&query, link.clone(), records.clone(), responder, timestamp)?;
    let target = match ack.pointer("/body/result/matches/0").and_then(|m| m.as_str()) {
        Some(t) => t.to_string(),
        None => {
            steps.push(Step { label: "ack".into(), message: ack });
            return Ok(VerifiedRun { steps, trusted: None, property: None, confirmed: false });
        }
    };
    steps.push(Step { label: "ack".into(), message: ack });

    // TRUST GATE: only use a function the local policy trusts (no central authority — principle 7).
    let trusted = match policy {
        Some(p) => {
            let graph = AttestationGraph::from_messages(attestations, timestamp);
            let v = p.evaluate_trust(&graph, &target, None, timestamp);
            steps.push(Step {
                label: "trust".into(),
                message: json!({ "subject": target, "trusted": v.trusted, "reason": v.reason }),
            });
            if !v.trusted {
                return Ok(VerifiedRun { steps, trusted: Some(false), property: None, confirmed: false });
            }
            Some(true)
        }
        None => None,
    };

    // PROVE the discovered function's own declared property — verify the *piece*, not just one result.
    let property = records.get(&target).and_then(|r| r.get("properties")).and_then(|p| p.as_array()).and_then(|ps| {
        ps.iter().find(|p| p.pointer("/expr/kind").and_then(|k| k.as_str()) == Some("forall")).map(|p| {
            let name = p.get("name").and_then(|n| n.as_str()).unwrap_or("<unnamed>").to_string();
            let expr = p.get("expr").unwrap();
            let proved = prove_law(expr, link.get(&target), solver);
            steps.push(Step {
                label: "prove".into(),
                message: json!({ "function": target, "property": name, "proved": proved }),
            });
            (name, proved)
        })
    });

    // APPLY: propose → commit → assert.
    let mut propose = json!({
        "schema_version": "0.2.0", "kind": "propose", "to": responder_did,
        "in_reply_to": null, "timestamp": timestamp, "constraints": null,
        "body": { "action": "apply", "target": target, "args": args }
    });
    sign_message(&mut propose, orchestrator)?;
    steps.push(Step { label: "propose".into(), message: propose.clone() });
    let commit = respond_to_message(&propose, link.clone(), records.clone(), responder, timestamp)?;
    let kind = commit.get("kind").and_then(|k| k.as_str()).unwrap_or("?").to_string();
    steps.push(Step { label: kind.clone(), message: commit.clone() });
    if kind != "commit" {
        return Ok(VerifiedRun { steps, trusted, property, confirmed: false });
    }
    let assert = respond_to_message(&commit, link.clone(), records.clone(), responder, timestamp)?;

    // RE-VERIFY the result by re-running the claim (trust nothing — principle 3).
    let confirmed = verify_claim(&assert, link.clone()).unwrap_or(false);
    steps.push(Step { label: "assert".into(), message: assert });
    Ok(VerifiedRun { steps, trusted, property, confirmed })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signing_key_from_seed;

    #[test]
    fn orchestrate_discovers_and_runs_a_function() {
        // Goal: apply *some arithmetic function* to 21. The orchestrator discovers `double` by intent,
        // proposes, the responder commits + fulfils, and the orchestrator confirms double(21) = 42.
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spec/examples");
        let orch = signing_key_from_seed("test-orchestrator");
        let resp = signing_key_from_seed("test-responder");
        let run = orchestrate(&dir, &["arithmetic".to_string()], vec![json!({ "kind": "nat", "value": 21 })], &orch, &resp, None).unwrap();

        assert!(run.confirmed, "the discovered-and-applied claim must verify");
        let labels: Vec<&str> = run.steps.iter().map(|s| s.label.as_str()).collect();
        assert_eq!(labels, ["query", "ack", "propose", "commit", "assert"]);
        let result = run.steps.last().unwrap().message.pointer("/body/claim/expr/args/1/value").unwrap();
        assert_eq!(result, &json!({ "kind": "int", "value": 42 }));
    }

    #[test]
    fn orchestrate_pipelines_multiple_discovered_functions() {
        // Two `arithmetic` stages each discover `double` and compose: double(double(21)) = 84.
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spec/examples");
        let orch = signing_key_from_seed("test-orchestrator");
        let resp = signing_key_from_seed("test-responder");
        let stages = ["arithmetic".to_string(), "arithmetic".to_string()];
        let run = orchestrate(&dir, &stages, vec![json!({ "kind": "nat", "value": 21 })], &orch, &resp, None).unwrap();

        assert!(run.confirmed, "every stage's claim must verify");
        assert_eq!(run.steps.len(), 10); // 2 stages × 5 messages
        let final_result = run.steps.last().unwrap().message.pointer("/body/claim/expr/args/1/value").unwrap();
        assert_eq!(final_result, &json!({ "kind": "int", "value": 84 }));
    }

    // ---- verified orchestration: discover → trust → prove → apply → re-verify ----

    fn solver() -> Option<&'static str> {
        for s in ["z3", "cvc5"] {
            if std::process::Command::new(s).arg("--version").output().map(|o| o.status.success()).unwrap_or(false) {
                return Some(s);
            }
        }
        None
    }

    fn did(seed: &str) -> String {
        did_nova_from_pubkey(&signing_key_from_seed(seed).verifying_key())
    }

    /// A signed `vouches-for` attestation about `subject` (an agent DID or an artifact content-address).
    fn vouch(seed: &str, subject: &str) -> J {
        let mut m = json!({
            "schema_version": "0.2.0", "kind": "assert", "to": null, "in_reply_to": null,
            "body": { "subject": subject, "claim": {
                "kind": "attestation", "subject": subject, "verb": "vouches-for", "domain": null, "expires_at": null } }
        });
        sign_message(&mut m, &signing_key_from_seed(seed)).unwrap();
        m
    }

    fn policy(roots: &[&str]) -> Policy {
        Policy {
            trusted_roots: roots.iter().map(|s| s.to_string()).collect(),
            max_depth: 5,
            min_distinct_paths: 1,
            allow_distrust_override: true,
            min_confidence: 0.0,
            half_life_days: None,
            min_disjoint_paths: 0,
            satisfied_conditions: Default::default(),
        }
    }

    fn examples() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spec/examples")
    }
    fn double_hash() -> String {
        crate::read_json(&examples().join("double.v0.2.json")).unwrap()["hash"].as_str().unwrap().to_string()
    }

    #[test]
    fn verified_run_discovers_trusts_proves_applies() {
        let Some(s) = solver() else { return };
        // A trusted root vouches for the `double` function record; the orchestrator then discovers it by
        // intent, confirms it's trusted, PROVES its declared property, applies it, and re-verifies 21→42.
        let (orch, resp, root) = (signing_key_from_seed("orch"), signing_key_from_seed("resp"), "root");
        let atts = [vouch(root, &double_hash())];
        let pol = policy(&[&did(root)]);
        let run = orchestrate_verified(
            &examples(), "arithmetic", vec![json!({ "kind": "nat", "value": 21 })],
            &orch, &resp, s, Some(&pol), &atts, None,
        ).unwrap();

        assert_eq!(run.trusted, Some(true), "the vouched function is trusted");
        assert_eq!(run.property, Some(("doubles".to_string(), true)), "its declared property is proved");
        assert!(run.confirmed, "the applied result re-verifies");
        let labels: Vec<&str> = run.steps.iter().map(|x| x.label.as_str()).collect();
        assert_eq!(labels, ["query", "ack", "trust", "prove", "propose", "commit", "assert"]);
    }

    #[test]
    fn verified_run_aborts_on_an_untrusted_function() {
        // Same discovery, but the policy trusts a root that vouches for nothing → the discovered function
        // is untrusted, so the run stops before proving or applying it.
        let (orch, resp) = (signing_key_from_seed("orch"), signing_key_from_seed("resp"));
        let pol = policy(&[&did("lonely-root")]);
        let run = orchestrate_verified(
            &examples(), "arithmetic", vec![json!({ "kind": "nat", "value": 21 })],
            &orch, &resp, "z3", Some(&pol), &[], None,
        ).unwrap();

        assert_eq!(run.trusted, Some(false));
        assert!(run.property.is_none());
        assert!(!run.confirmed);
        let labels: Vec<&str> = run.steps.iter().map(|x| x.label.as_str()).collect();
        assert_eq!(labels, ["query", "ack", "trust"], "aborts at the trust gate");
    }
}
