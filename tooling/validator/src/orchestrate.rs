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
    Policy, ProofOutcome, TrustVerdict, DEFAULT_LEMMA_DEPTH,
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

/// Trust-ranking key for a candidate: prefer higher aggregate confidence, then more vertex-disjoint
/// paths, then more distinct attesters. Disambiguates functions that match the same intent.
fn rank_key(v: &TrustVerdict) -> (f64, usize, usize) {
    (v.confidence, v.disjoint_paths, v.supporting.len())
}

/// Evaluate every candidate under `policy` and return all verdicts plus the index of the **most-trusted**
/// one (first-max on ties — earlier matches win equal trust; `None` if none is trusted). This is the
/// consumer's *local* trust ranking: discovery returns a set, and the receiver — not any central
/// authority (principle 7) — decides which to use, by its own policy over its own attestation graph.
fn best_trusted(
    candidates: &[String],
    policy: &Policy,
    graph: &AttestationGraph,
    at: Option<&str>,
) -> (Vec<TrustVerdict>, Option<usize>) {
    let verdicts: Vec<TrustVerdict> = candidates.iter().map(|m| policy.evaluate_trust(graph, m, None, at)).collect();
    let mut best: Option<usize> = None;
    for (i, v) in verdicts.iter().enumerate() {
        if v.trusted && best.map_or(true, |b| rank_key(v) > rank_key(&verdicts[b])) {
            best = Some(i);
        }
    }
    (verdicts, best)
}

/// A coarse value type, for checking that an argument fits a parameter's declared type.
#[derive(Clone, PartialEq)]
enum Ty {
    Int, // int and nat (nat ≡ int in the evaluator)
    Bool,
    Lst(Box<Ty>),
    Fun,
    Any, // unknown — unifies with anything (kept permissive to avoid dropping valid candidates)
}

/// The coarse type of a value-expression argument.
fn value_ty(v: &J) -> Ty {
    match v.get("kind").and_then(|k| k.as_str()) {
        Some("int") | Some("nat") => Ty::Int,
        Some("bool") => Ty::Bool,
        Some("fn_ref") => Ty::Fun,
        Some("list") => {
            let elem = v.get("elems").and_then(|e| e.as_array()).and_then(|a| a.first()).map(value_ty);
            Ty::Lst(Box::new(elem.unwrap_or(Ty::Any)))
        }
        _ => Ty::Any,
    }
}

fn tys_match(a: &Ty, b: &Ty) -> bool {
    match (a, b) {
        (Ty::Any, _) | (_, Ty::Any) => true,
        (Ty::Lst(x), Ty::Lst(y)) => tys_match(x, y),
        _ => a == b,
    }
}

/// Unify a parameter *type-expression* against an argument's coarse type, threading a type-variable
/// substitution (so `forall a. (a, a) -> a` rejects mismatched args). Permissive on `Any`/unknown to
/// avoid dropping valid candidates — under-rejection is caught later by re-verification, over-rejection
/// would silently hide usable functions.
fn unify_ty(ptype: &J, arg: &Ty, subst: &mut std::collections::HashMap<String, Ty>) -> bool {
    if *arg == Ty::Any {
        return true;
    }
    match ptype.get("kind").and_then(|k| k.as_str()) {
        Some("var") => {
            let name = ptype.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
            match subst.get(&name) {
                Some(bound) => tys_match(&bound.clone(), arg),
                None => {
                    subst.insert(name, arg.clone());
                    true
                }
            }
        }
        Some("builtin") => match ptype.get("name").and_then(|n| n.as_str()) {
            Some("int") | Some("nat") => *arg == Ty::Int,
            Some("bool") => *arg == Ty::Bool,
            Some("List") => matches!(arg, Ty::Lst(_)),
            _ => true, // unknown builtin sort: permissive
        },
        Some("apply") => {
            if ptype.pointer("/ctor/name").and_then(|n| n.as_str()) == Some("List") {
                match arg {
                    Ty::Lst(e) => ptype.get("args").and_then(|a| a.as_array()).and_then(|a| a.first()).map_or(true, |pe| unify_ty(pe, e, subst)),
                    _ => false,
                }
            } else {
                true // other type constructors: permissive
            }
        }
        Some("fn") => *arg == Ty::Fun,
        _ => true, // unknown type shape: permissive
    }
}

/// Coarsen a *type-expression* (a parameter or result type) to a [`Ty`]. A type variable becomes `Any`
/// (the referenced function's own polymorphism — left permissive); used to check a higher-order
/// argument's declared signature against the expected function-parameter type.
fn type_to_ty(t: &J) -> Ty {
    let t = if t.get("kind").and_then(|k| k.as_str()) == Some("forall") {
        t.get("body").unwrap_or(t)
    } else {
        t
    };
    match t.get("kind").and_then(|k| k.as_str()) {
        Some("builtin") => match t.get("name").and_then(|n| n.as_str()) {
            Some("int") | Some("nat") => Ty::Int,
            Some("bool") => Ty::Bool,
            Some("List") => Ty::Lst(Box::new(Ty::Any)),
            _ => Ty::Any,
        },
        Some("apply") if t.pointer("/ctor/name").and_then(|n| n.as_str()) == Some("List") => {
            Ty::Lst(Box::new(t.get("args").and_then(|a| a.as_array()).and_then(|a| a.first()).map_or(Ty::Any, type_to_ty)))
        }
        Some("fn") => Ty::Fun,
        _ => Ty::Any, // var or unknown
    }
}

/// Does parameter type `ptype` accept argument value `arg`? For a **function parameter**, the argument
/// must be a `fn_ref` whose target (resolved from `records`) is a function of matching arity whose
/// parameter/result types unify with what's expected — closing the hole where any `fn_ref` was accepted
/// for a higher-order slot. A `fn_ref` to a function this node can't resolve is rejected (it can't be
/// type-checked, so it doesn't qualify). Non-function parameters use the coarse value type as before.
fn arg_fits(ptype: &J, arg: &J, records: &std::collections::HashMap<String, J>, subst: &mut std::collections::HashMap<String, Ty>) -> bool {
    if ptype.get("kind").and_then(|k| k.as_str()) != Some("fn") {
        return unify_ty(ptype, &value_ty(arg), subst);
    }
    // Function parameter: require a resolvable fn_ref of matching shape.
    if arg.get("kind").and_then(|k| k.as_str()) != Some("fn_ref") {
        return false;
    }
    let Some(target) = arg.get("target").and_then(|t| t.as_str()) else { return false };
    let Some(rec) = records.get(target) else { return false };
    let Some(mut tt) = rec.pointer("/signature/type") else { return false };
    if tt.get("kind").and_then(|k| k.as_str()) == Some("forall") {
        match tt.get("body") {
            Some(b) => tt = b,
            None => return false,
        }
    }
    let (Some(tparams), Some(tresult)) = (tt.get("params").and_then(|p| p.as_array()), tt.get("result")) else {
        return false; // target isn't a function
    };
    let eparams = ptype.get("params").and_then(|p| p.as_array());
    let eresult = ptype.get("result");
    let (Some(eparams), Some(eresult)) = (eparams, eresult) else { return false };
    if eparams.len() != tparams.len() {
        return false; // arity of the supplied function doesn't match the expected function type
    }
    eparams.iter().zip(tparams).all(|(e, a)| unify_ty(e, &type_to_ty(a), subst)) && unify_ty(eresult, &type_to_ty(tresult), subst)
}

/// Does the function record's signature accept these arguments — matching arity *and* parameter types,
/// including the signatures of higher-order (`fn_ref`) arguments? `records` resolves those targets.
fn signature_fits(record: &J, args: &[J], records: &std::collections::HashMap<String, J>) -> bool {
    let Some(mut t) = record.pointer("/signature/type") else { return false };
    if t.get("kind").and_then(|k| k.as_str()) == Some("forall") {
        match t.get("body") {
            Some(b) => t = b,
            None => return false,
        }
    }
    let params: &[J] = match t.get("params").and_then(|p| p.as_array()) {
        Some(p) => p,
        None => &[], // a non-function type accepts only a nullary application
    };
    if params.len() != args.len() {
        return false;
    }
    let mut subst = std::collections::HashMap::new();
    params.iter().zip(args).all(|(p, a)| arg_fits(p, a, records, &mut subst))
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
    let matches: Vec<String> = ack
        .pointer("/body/result/matches")
        .and_then(|m| m.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default();
    steps.push(Step { label: "ack".into(), message: ack });

    // SIGNATURE FILTER: discovery matches by intent only, so drop candidates whose signature doesn't fit
    // this application *before* trust-ranking the rest — arity AND parameter types must accept the
    // arguments (a binary function, or one expecting a list where an int is passed, is no candidate,
    // however trusted it is).
    let compatible: Vec<String> =
        matches.into_iter().filter(|m| records.get(m).is_some_and(|r| signature_fits(r, &args, &records))).collect();
    if compatible.is_empty() {
        return Ok(VerifiedRun { steps, trusted: None, property: None, confirmed: false });
    }

    // TRUST GATE + DISAMBIGUATION: among the signature-compatible candidates, rank by the local policy
    // and use the most-trusted one (no central authority — principle 7). Without a policy there is no
    // trust signal, so fall back to the first compatible match.
    let (target, trusted) = match policy {
        Some(p) => {
            let graph = AttestationGraph::from_messages(attestations, timestamp);
            let (verdicts, best) = best_trusted(&compatible, p, &graph, timestamp);
            let candidates: Vec<J> = compatible
                .iter()
                .zip(&verdicts)
                .map(|(m, v)| json!({ "function": m, "trusted": v.trusted, "confidence": v.confidence }))
                .collect();
            match best {
                Some(i) => {
                    steps.push(Step {
                        label: "trust".into(),
                        message: json!({ "chosen": compatible[i], "reason": verdicts[i].reason, "candidates": candidates }),
                    });
                    (compatible[i].clone(), Some(true))
                }
                None => {
                    steps.push(Step {
                        label: "trust".into(),
                        message: json!({ "chosen": null, "trusted": false,
                            "reason": "no discovered function is trusted under the policy", "candidates": candidates }),
                    });
                    return Ok(VerifiedRun { steps, trusted: Some(false), property: None, confirmed: false });
                }
            }
        }
        None => (compatible[0].clone(), None),
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
    fn add_hash() -> String {
        crate::read_json(&examples().join("add.json")).unwrap()["hash"].as_str().unwrap().to_string()
    }

    #[test]
    fn signature_fits_checks_arity_and_types() {
        let records = crate::build_record_map(&examples()).unwrap();
        let load = |n: &str| crate::read_json(&examples().join(n)).unwrap();
        let (double, add, reverse) = (load("double.v0.2.json"), load("add.json"), load("reverse.json"));
        let int = json!({ "kind": "int", "value": 5 });
        let list = json!({ "kind": "list", "elems": [{ "kind": "int", "value": 1 }] });

        // double : nat -> nat
        assert!(signature_fits(&double, &[int.clone()], &records), "int fits a nat parameter");
        assert!(!signature_fits(&double, &[list.clone()], &records), "a list does not fit a nat parameter");
        assert!(!signature_fits(&double, &[int.clone(), int.clone()], &records), "arity 2 ≠ 1");
        // add : (int, int) -> int
        assert!(signature_fits(&add, &[int.clone(), int.clone()], &records));
        assert!(!signature_fits(&add, &[int.clone(), list.clone()], &records), "second arg must be int, not a list");
        // reverse : forall a. List a -> List a
        assert!(signature_fits(&reverse, &[list.clone()], &records), "a list fits List a");
        assert!(!signature_fits(&reverse, &[int.clone()], &records), "an int does not fit List a");
    }

    #[test]
    fn higher_order_argument_signature_is_checked() {
        // foldr : forall a b. ((a,b)->b, b, List a) -> b. Its first parameter is a *function*; a fn_ref
        // there is type-checked against (a,b)->b, not waved through as opaque.
        let records = crate::build_record_map(&examples()).unwrap();
        let foldr = crate::read_json(&examples().join("foldr.json")).unwrap();
        let zero = json!({ "kind": "int", "value": 0 });
        let list = json!({ "kind": "list", "elems": [{ "kind": "int", "value": 1 }] });
        let add_ref = json!({ "kind": "fn_ref", "target": add_hash() });
        let double_ref = json!({ "kind": "fn_ref", "target": double_hash() });

        // add is binary → fits the (a,b)->b fold function; the whole application type-checks.
        assert!(signature_fits(&foldr, &[add_ref, zero.clone(), list.clone()], &records));
        // double is UNARY → cannot stand in for the binary fold function (the closed hole).
        assert!(!signature_fits(&foldr, &[double_ref, zero.clone(), list.clone()], &records),
            "a unary fn_ref must not pass as the binary fold function");
        // a non-function (an int) for the function parameter is rejected too.
        assert!(!signature_fits(&foldr, &[zero.clone(), zero.clone(), list], &records),
            "an int does not satisfy a function parameter");
    }

    #[test]
    fn arity_filter_selects_the_compatible_function() {
        let Some(s) = solver() else { return };
        // A *two-argument* apply: `double` (arity 1) is dropped by the signature filter before ranking;
        // `add` (arity 2, vouched) is selected, its property proved, and add(2,3) = 5 re-verified.
        let atts = [vouch("root", &add_hash())];
        let pol = policy(&[&did("root")]);
        let run = orchestrate_verified(
            &examples(), "arithmetic",
            vec![json!({ "kind": "int", "value": 2 }), json!({ "kind": "int", "value": 3 })],
            &signing_key_from_seed("orch"), &signing_key_from_seed("resp"), s, Some(&pol), &atts, None,
        ).unwrap();
        assert_eq!(run.trusted, Some(true));
        assert!(run.property.map(|(_, p)| p).unwrap_or(false), "the chosen function's property proved");
        assert!(run.confirmed, "add(2,3) = 5 re-verified");
        let chosen = run.steps.iter().find(|s| s.label == "trust").unwrap().message.get("chosen").unwrap().as_str().unwrap().to_string();
        assert_eq!(chosen, add_hash(), "the arity-2 function was selected, not the arity-1 double");
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
    fn trust_ranking_picks_the_trusted_candidate_not_the_first() {
        // Two matched candidates; only the SECOND is vouched. Ranking must select index 1 — proving
        // disambiguation is by trust, not by position (the old matches[0]).
        let (a, b) = (format!("fn_{}", "a".repeat(64)), format!("fn_{}", "b".repeat(64)));
        let graph = AttestationGraph::from_messages(&[vouch("root", &b)], None);
        let (verdicts, best) = best_trusted(&[a, b], &policy(&[&did("root")]), &graph, None);
        assert!(!verdicts[0].trusted && verdicts[1].trusted);
        assert_eq!(best, Some(1), "the trusted second candidate is chosen over the untrusted first");
    }

    #[test]
    fn trust_ranking_prefers_more_corroboration() {
        // Both trusted, but `b` is vouched by two distinct roots vs `a`'s one → b ranks higher.
        let (a, b) = (format!("fn_{}", "a".repeat(64)), format!("fn_{}", "b".repeat(64)));
        let graph = AttestationGraph::from_messages(
            &[vouch("r1", &a), vouch("r1", &b), vouch("r2", &b)],
            None,
        );
        let (verdicts, best) = best_trusted(&[a, b], &policy(&[&did("r1"), &did("r2")]), &graph, None);
        assert!(verdicts[0].trusted && verdicts[1].trusted);
        assert!(verdicts[1].supporting.len() > verdicts[0].supporting.len());
        assert_eq!(best, Some(1), "more distinct trusted attesters wins the tie");
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
