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
    analyze_termination, build_link_map, build_record_map, certify_record, clear_resolver,
    did_nova_from_pubkey, eval_body, infer_effects, prove_by_induction_with_exploration, prove_property,
    respond_to_message, set_resolver, sign_message, verify_claim, AttestationGraph, InductionOutcome,
    Policy, ProofOutcome, TerminationOutcome, TrustVerdict, DEFAULT_LEMMA_DEPTH,
};
use crate::interp::{decode_value, val_eq};

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
    orchestrate_with_maps(link, records, stages, args, orchestrator, responder, timestamp)
}

/// [`orchestrate`] over already-materialized maps — the commons view may come from a local
/// directory or a remote node ([`crate::commons_client::maps_from_node`]); the loop is identical.
pub fn orchestrate_with_maps(
    link: std::collections::HashMap<String, J>,
    records: std::collections::HashMap<String, J>,
    stages: &[String],
    args: Vec<J>,
    orchestrator: &SigningKey,
    responder: &SigningKey,
    timestamp: Option<&str>,
) -> Result<Run> {
    let mut link = link;
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

        // An effectful fulfilment produced an OBSERVED claim + its trace artifact: index the trace
        // (so the re-verify below replays it) and carry it as a step (so --publish ships it — the
        // claim is unverifiable by anyone who can't fetch the trace).
        if let Some(trace) = crate::respond::take_trace_artifact() {
            let addr = crate::hash_artifact_with_kind(&trace, crate::ArtifactKind::Trace)?;
            link.insert(addr, trace.clone());
            steps.push(Step { label: format!("{pfx}trace"), message: trace });
        }

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

/// The transcript of a *verified* orchestration: discover → trust-gate → **certify** the function → prove
/// its own declared property → apply → re-verify the result.
pub struct VerifiedRun {
    pub steps: Vec<Step>,
    /// Whether the discovered function is trusted under the policy (`None` if no policy was supplied).
    pub trusted: Option<bool>,
    /// Whether the chosen function is **certified** — every "verified by default" check passed (`None` if
    /// the run aborted before the certify step, or the function's body was unavailable to certify).
    pub certified: Option<bool>,
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
    Str,
    Float,
    Map, // Map string a — key type is fixed, the value type stays coarse
    Sum, // any variant value / any sum-typed (incl. Json) parameter — sums are opaque, matching typecheck
    Lst(Box<Ty>),
    Fun,
    Any, // unknown — unifies with anything (kept permissive to avoid dropping valid candidates)
}

/// The coarse type of a value-expression argument.
fn value_ty(v: &J) -> Ty {
    match v.get("kind").and_then(|k| k.as_str()) {
        Some("int") | Some("nat") => Ty::Int,
        Some("bool") => Ty::Bool,
        Some("string") => Ty::Str,
        Some("float") => Ty::Float,
        Some("map") => Ty::Map,
        Some("variant") => Ty::Sum,
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
            Some("string") => *arg == Ty::Str,
            Some("float") => *arg == Ty::Float,
            Some("Json") => *arg == Ty::Sum, // a Json parameter takes a variant value (JNull/JObj/…)
            Some("List") => matches!(arg, Ty::Lst(_)),
            Some("Map") => *arg == Ty::Map,
            _ => true, // unknown builtin sort: permissive
        },
        Some("sum") => *arg == Ty::Sum, // a structurally-spelled sum parameter takes a variant value
        Some("apply") => {
            match ptype.pointer("/ctor/name").and_then(|n| n.as_str()) {
                Some("List") => match arg {
                    Ty::Lst(e) => ptype.get("args").and_then(|a| a.as_array()).and_then(|a| a.first()).map_or(true, |pe| unify_ty(pe, e, subst)),
                    _ => false,
                },
                Some("Map") => *arg == Ty::Map,
                _ => true, // other type constructors: permissive
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
            Some("string") => Ty::Str,
            Some("float") => Ty::Float,
            Some("Json") => Ty::Sum,
            Some("List") => Ty::Lst(Box::new(Ty::Any)),
            Some("Map") => Ty::Map,
            _ => Ty::Any,
        },
        Some("sum") => Ty::Sum,
        Some("apply") if t.pointer("/ctor/name").and_then(|n| n.as_str()) == Some("List") => {
            Ty::Lst(Box::new(t.get("args").and_then(|a| a.as_array()).and_then(|a| a.first()).map_or(Ty::Any, type_to_ty)))
        }
        Some("apply") if t.pointer("/ctor/name").and_then(|n| n.as_str()) == Some("Map") => Ty::Map,
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

/// Does the record's declared *result* type unify with the coarse sort of `expect` — the caller's
/// expected result value? Discovery matches by intent and the signature filter by ARGUMENT fit, so
/// nothing constrains what a candidate *returns*: a `(string, string) -> bool` predicate and a
/// `(string, string) -> Maybe string` builder both survive a two-string application (the GW16
/// unsplittable-fits residual). A caller-stated expectation pins the result sort too.
fn result_fits(record: &J, expect: &J) -> bool {
    let Some(mut t) = record.pointer("/signature/type") else { return false };
    if t.get("kind").and_then(|k| k.as_str()) == Some("forall") {
        match t.get("body") {
            Some(b) => t = b,
            None => return false,
        }
    }
    let Some(res) = t.get("result") else { return false };
    if !unify_ty(res, &value_ty(expect), &mut std::collections::HashMap::new()) {
        return false;
    }
    // A variant-valued expectation carries payload depth the coarse `Sum` sort erases (found at
    // GitHub scale: a `Just false` goal kept every `Maybe Json` fit, and fetch order applied a
    // wrong-goal projection). The declared result must OFFER the expected tag with a payload type
    // that unifies with the payload's sort: `Just false` can only come from a Just-arm carrying a
    // bool — a `Maybe Json` body answers `Just (JBool false)`, a different value. A caller who
    // wants the Json encoding states it (the payload is then itself a variant, which a Json arm
    // accepts). Unknown shapes stay permissive — under-rejection is caught by re-verification.
    if expect.get("kind").and_then(|k| k.as_str()) == Some("variant") {
        if let Some(tag) = expect.get("tag").and_then(|t| t.as_str()) {
            return variant_arm_fits(res, tag, expect.get("payload"));
        }
    }
    true
}

/// Does a declared result type offer variant `tag` with a payload whose declared type unifies
/// with `payload`'s coarse sort? See `result_fits` — the variant-expectation tightening.
fn variant_arm_fits(res: &J, tag: &str, payload: Option<&J>) -> bool {
    let pay_ty = payload.map(value_ty);
    let unifies = |arm_ty: Option<&J>| -> bool {
        match (arm_ty, &pay_ty) {
            (None, None) => true,
            (Some(_), None) | (None, Some(_)) => false,
            (Some(t), Some(p)) => unify_ty(t, p, &mut std::collections::HashMap::new()),
        }
    };
    match res.get("kind").and_then(|k| k.as_str()) {
        Some("sum") => res.get("variants").and_then(|v| v.as_array()).map_or(true, |arms| {
            match arms.iter().find(|a| a.get("tag").and_then(|t| t.as_str()) == Some(tag)) {
                Some(arm) => unifies(arm.get("type")),
                None => false,
            }
        }),
        Some("apply") => {
            let args = res.get("args").and_then(|a| a.as_array());
            match (res.pointer("/ctor/name").and_then(|n| n.as_str()), tag) {
                (Some("Maybe"), "Just") => unifies(args.and_then(|a| a.first())),
                (Some("Maybe"), "None") => pay_ty.is_none(),
                (Some("Maybe"), _) => false,
                (Some("Result"), "Ok") => unifies(args.and_then(|a| a.first())),
                (Some("Result"), "Err") => unifies(args.and_then(|a| a.get(1))),
                (Some("Result"), _) => false,
                _ => true, // unknown constructor: permissive
            }
        }
        Some("builtin") => match res.get("name").and_then(|n| n.as_str()) {
            // A nominal Json result answers exactly the six J-constructors, payload-sorted.
            Some("Json") => match tag {
                "JNull" => pay_ty.is_none(),
                "JBool" => matches!(pay_ty, Some(Ty::Bool)),
                "JNum" => matches!(pay_ty, Some(Ty::Int) | Some(Ty::Float)),
                "JStr" => matches!(pay_ty, Some(Ty::Str)),
                "JList" => matches!(pay_ty, Some(Ty::Lst(_))),
                "JObj" => matches!(pay_ty, Some(Ty::Map)),
                _ => false,
            },
            _ => true, // other builtins were already filtered by unify_ty
        },
        _ => true, // var / forall / unknown shape: permissive
    }
}

/// How a candidate's dry-run against the caller's expected result came out.
#[derive(Clone, Copy, PartialEq)]
enum DryRun {
    /// Evaluated on the actual arguments and produced exactly the expected value.
    Match,
    /// Evaluated and produced something else — the wrong function for this goal.
    Mismatch,
    /// Evaluation failed on these arguments.
    Error,
    /// No expectation given, or the body isn't statically pure + terminating, so it was not run.
    NotRun,
}

impl DryRun {
    fn score(self) -> i64 {
        match self {
            DryRun::Match => 2,
            DryRun::NotRun => 1,
            DryRun::Mismatch | DryRun::Error => 0,
        }
    }
    fn label(self) -> &'static str {
        match self {
            DryRun::Match => "match",
            DryRun::Mismatch => "mismatch",
            DryRun::Error => "error",
            DryRun::NotRun => "not-run",
        }
    }
}

/// A candidate's fit against the caller's GOAL, beyond argument fit: dry-run outcome against the
/// expected result, intent-tag specificity, and name-hint affinity with the queried intent.
struct GoalScore {
    dry: DryRun,
    tags: i64,
    name: i64,
}

impl GoalScore {
    fn key(&self) -> (i64, i64, i64) {
        (self.dry.score(), self.tags, self.name)
    }
}

fn intent_tokens(intent: &str) -> Vec<&str> {
    intent.split(['/', '-', '_']).filter(|t| !t.is_empty()).collect()
}

/// Score one candidate against the goal. Deterministic and local — every signal is either the
/// caller's own input (the intent, the expected result) or the candidate record's declared metadata.
///
/// The dry-run only executes a body this node can STATICALLY verify pure (no effects, nothing
/// opaque/unresolved) and terminating — running arbitrary discovered code before certification would
/// be an effect/divergence hole, so anything unverifiable simply isn't run (`NotRun`), the same
/// verify-before-run discipline as everywhere else. An effectful candidate is never executed for
/// ranking; it keeps its neutral score and is disambiguated by the declared-metadata signals.
fn goal_score(
    record: &J,
    body: Option<&J>,
    intent: &str,
    expect: Option<&J>,
    args: &[J],
    records: &std::collections::HashMap<String, J>,
) -> GoalScore {
    let dry = match (body, expect) {
        (Some(b), Some(e)) => {
            let inf = infer_effects(b, records);
            let safe = inf.effects.is_empty()
                && !inf.opaque
                && !inf.unresolved
                && matches!(analyze_termination(b), TerminationOutcome::Always);
            if !safe {
                DryRun::NotRun
            } else {
                match eval_body(b, args) {
                    Ok(got) => match (decode_value(&got), decode_value(e)) {
                        (Ok(g), Ok(w)) if val_eq(&g, &w) => DryRun::Match,
                        _ => DryRun::Mismatch,
                    },
                    Err(_) => DryRun::Error,
                }
            }
        }
        _ => DryRun::NotRun,
    };
    // Tag specificity: tags that EXTEND the queried intent (`intent/…`) mark a more specific fit.
    // The exact tag itself is shared by every discovered candidate, so it carries no signal.
    let prefix = format!("{intent}/");
    let tags = record.get("intent_tags").and_then(|t| t.as_array()).map_or(0, |ts| {
        ts.iter().filter_map(|t| t.as_str()).filter(|t| t.starts_with(&prefix)).count() as i64
    });
    // Name affinity: token overlap between the queried intent and the best of the record's name_hints.
    let want = intent_tokens(intent);
    let name = record.get("name_hints").and_then(|n| n.as_array()).map_or(0, |hints| {
        hints
            .iter()
            .filter_map(|h| h.as_str())
            .map(|h| h.split('_').filter(|t| want.contains(t)).count() as i64)
            .max()
            .unwrap_or(0)
    });
    GoalScore { dry, tags, name }
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
    require_certified: bool,
    expect: Option<J>,
) -> Result<VerifiedRun> {
    let link = build_link_map(records_dir)?;
    let records = build_record_map(records_dir)?;
    orchestrate_verified_with_maps(
        link, records, intent, args, orchestrator, responder, solver, policy, attestations, timestamp, require_certified,
        expect,
    )
}

/// [`orchestrate_verified`] over already-materialized maps (local directory or a remote node —
/// [`crate::commons_client::maps_from_node`]); the verified loop is identical either way.
#[allow(clippy::too_many_arguments)]
pub fn orchestrate_verified_with_maps(
    link: std::collections::HashMap<String, J>,
    records: std::collections::HashMap<String, J>,
    intent: &str,
    args: Vec<J>,
    orchestrator: &SigningKey,
    responder: &SigningKey,
    solver: &str,
    policy: Option<&Policy>,
    attestations: &[J],
    timestamp: Option<&str>,
    require_certified: bool,
    expect: Option<J>,
) -> Result<VerifiedRun> {
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
    // however trusted it is). A caller-stated expected result additionally pins the RESULT sort — the
    // argument-only filter cannot split two same-argument candidates that return different things.
    let compatible: Vec<String> = matches
        .into_iter()
        .filter(|m| {
            records.get(m).is_some_and(|r| {
                signature_fits(r, &args, &records) && expect.as_ref().map_or(true, |e| result_fits(r, e))
            })
        })
        .collect();
    if compatible.is_empty() {
        return Ok(VerifiedRun { steps, trusted: None, certified: None, property: None, confirmed: false });
    }

    // The attestation graph over the supplied attestations — including any signed CERTIFICATION records,
    // which contribute `certifies` edges. Shared by the trust gate and the certify step below.
    let graph = AttestationGraph::from_messages(attestations, timestamp);

    // TRUST GATE + DISAMBIGUATION: among the signature-compatible candidates, rank by the local policy
    // (no central authority — principle 7); untrusted candidates are excluded outright. Without a policy
    // there is no trust signal, so keep discovery order. The result is an ORDERED candidate list, not a
    // single choice: the coarse signature filter matches by argument fit alone, so the first fit can be
    // the wrong function (GW14's io/network/http run proposed one and aborted on its signed reject) —
    // the loop below advances to the next candidate instead, keeping every reject in the transcript.
    let (ordered, trusted): (Vec<String>, Option<bool>) = match policy {
        Some(p) => {
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
                    let mut idx: Vec<usize> = (0..compatible.len()).filter(|&j| verdicts[j].trusted).collect();
                    idx.sort_by(|&a, &b| {
                        rank_key(&verdicts[b]).partial_cmp(&rank_key(&verdicts[a])).unwrap_or(std::cmp::Ordering::Equal)
                    });
                    (idx.into_iter().map(|j| compatible[j].clone()).collect(), Some(true))
                }
                None => {
                    steps.push(Step {
                        label: "trust".into(),
                        message: json!({ "chosen": null, "trusted": false,
                            "reason": "no discovered function is trusted under the policy", "candidates": candidates }),
                    });
                    return Ok(VerifiedRun { steps, trusted: Some(false), certified: None, property: None, confirmed: false });
                }
            }
        }
        None => (compatible.clone(), None),
    };

    // GOAL RANK: order the surviving candidates by fit against the caller's GOAL — dry-run outcome
    // against the expected result (statically pure + terminating bodies only), then intent-tag
    // specificity, then name-hint affinity. The sort is stable, so the trust order (or, without a
    // policy, the discovery order) remains the tie-break: trust decides WHETHER a candidate may be
    // used, the goal decides WHICH to try first. The scores go into the transcript — the choice is
    // auditable, not oracular.
    let mut ordered = ordered;
    if ordered.len() > 1 {
        set_resolver(link.clone());
        let scores: std::collections::HashMap<String, GoalScore> = ordered
            .iter()
            .map(|m| (m.clone(), goal_score(&records[m], link.get(m), intent, expect.as_ref(), &args, &records)))
            .collect();
        clear_resolver();
        ordered.sort_by(|a, b| scores[b].key().cmp(&scores[a].key()));
        steps.push(Step {
            label: "rank".into(),
            message: json!({
                "goal": { "intent": intent, "expected_result": expect.is_some() },
                "ordered": ordered,
                "scores": ordered.iter().map(|m| {
                    let s = &scores[m];
                    json!({ "function": m, "dry_run": s.dry.label(),
                            "tag_specificity": s.tags, "name_affinity": s.name })
                }).collect::<Vec<J>>(),
            }),
        });
    }

    let mut link = link;
    let total = ordered.len();
    for (attempt, target) in ordered.iter().enumerate() {
        let target = target.clone();
        let last = attempt + 1 == total;

        // CERTIFY the candidate before using it — run every "verified by default" check (type, effects,
        // refinements, termination, complexity/cost) against its body. "Assemble, don't write" only yields
        // verifiable artifacts if the pieces are themselves verified (principle 3); with `require_certified`,
        // an uncertified candidate is SKIPPED (the next may certify), and only the last aborts the run.
        // A signed certification from a certifier this policy trusts rides along (trust-delegation).
        let commons_cert = policy.map(|p| p.certification_verdict(&graph, &target, None, timestamp));
        let certified = match (records.get(&target), link.get(&target)) {
            (Some(rec), Some(body)) => {
                let cert = certify_record(rec, body, &records, solver);
                let failed: Vec<&str> = cert.checks.iter().filter(|c| c.hard_fail).map(|c| c.check.as_str()).collect();
                steps.push(Step {
                    label: "certify".into(),
                    message: json!({ "function": target, "certified": cert.certified,
                        "checks": cert.checks.iter().map(|c| json!({ "check": c.check, "verdict": c.verdict })).collect::<Vec<_>>(),
                        "failed": failed,
                        "commons_certified": commons_cert.as_ref().map(|c| c.certified),
                        "trusted_certifiers": commons_cert.as_ref().map(|c| c.trusted_certifiers.clone()).unwrap_or_default() }),
                });
                Some(cert.certified)
            }
            // No resolvable body to certify — leave `certified` unknown rather than claim a verdict.
            _ => None,
        };
        if require_certified && certified != Some(true) {
            if last {
                return Ok(VerifiedRun { steps, trusted, certified, property: None, confirmed: false });
            }
            steps.push(Step {
                label: "retry".into(),
                message: json!({ "after": target, "reason": "not certified", "remaining": total - attempt - 1 }),
            });
            continue;
        }

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

        // APPLY: propose → commit → assert. A signed `reject` (a policy answer — e.g. "effect not
        // granted") advances to the next candidate rather than ending the run.
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
            if last {
                return Ok(VerifiedRun { steps, trusted, certified, property, confirmed: false });
            }
            steps.push(Step {
                label: "retry".into(),
                message: json!({ "after": target, "reason": "rejected", "remaining": total - attempt - 1 }),
            });
            continue;
        }
        let assert = respond_to_message(&commit, link.clone(), records.clone(), responder, timestamp)?;

        // An effectful fulfilment produced an OBSERVED claim + its trace artifact: index the trace so
        // the re-verify below replays it, and carry it as a step so --publish ships it with the assert.
        if let Some(trace) = crate::respond::take_trace_artifact() {
            let addr = crate::hash_artifact_with_kind(&trace, crate::ArtifactKind::Trace)?;
            link.insert(addr, trace.clone());
            steps.push(Step { label: "trace".into(), message: trace });
        }

        // RE-VERIFY the result by re-running the claim (trust nothing — principle 3).
        let confirmed = verify_claim(&assert, link.clone()).unwrap_or(false);
        steps.push(Step { label: "assert".into(), message: assert });
        return Ok(VerifiedRun { steps, trusted, certified, property, confirmed });
    }
    // `compatible` was non-empty, so the loop always returns from its last iteration.
    unreachable!("candidate loop returns on its last attempt")
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
    fn signature_filter_discriminates_string_map_and_json_sorts() {
        // The live-workflow gap this closes: json_get : string -> Json -> Maybe Json and
        // json_path : List string -> Json -> Maybe Json share the intent tag "json" and arity 2 —
        // with a (List string, Json) application only json_path may survive the filter, else the
        // orchestrator proposes json_get and the responder rejects at apply time.
        let records = crate::build_record_map(&examples()).unwrap();
        let load = |n: &str| crate::read_json(&examples().join(n)).unwrap();
        let (json_get, json_path) = (load("json-get.v0.2.json"), load("json-path.v0.2.json"));
        let path = json!({ "kind": "list", "elems": [{ "kind": "string", "value": "owner" }] });
        let key = json!({ "kind": "string", "value": "owner" });
        let jval = json!({ "kind": "variant", "tag": "JNum", "payload": { "kind": "int", "value": 7 } });

        assert!(signature_fits(&json_path, &[path.clone(), jval.clone()], &records));
        assert!(!signature_fits(&json_get, &[path.clone(), jval.clone()], &records),
            "a List string arg must not pass a string parameter");
        assert!(signature_fits(&json_get, &[key.clone(), jval.clone()], &records));
        assert!(!signature_fits(&json_path, &[key.clone(), jval.clone()], &records),
            "a string arg must not pass a List string parameter");
        // The Json-typed parameter takes only variant values.
        assert!(!signature_fits(&json_get, &[key.clone(), key.clone()], &records),
            "a string arg must not pass a Json parameter");
        // A Map-typed parameter (config_port : Map string int -> int) rejects non-map values.
        let config_port = load("config-port.v0.2.json");
        let map = json!({ "kind": "map", "entries": [{ "key": "port", "value": { "kind": "int", "value": 1 } }] });
        assert!(signature_fits(&config_port, &[map], &records));
        assert!(!signature_fits(&config_port, &[key], &records), "a string arg must not pass a Map parameter");
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
            &signing_key_from_seed("orch"), &signing_key_from_seed("resp"), s, Some(&pol), &atts, None, true, None,
        ).unwrap();
        assert_eq!(run.trusted, Some(true));
        assert_eq!(run.certified, Some(true), "the chosen function certified (require_certified passed)");
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
            &orch, &resp, s, Some(&pol), &atts, None, true, None,
        ).unwrap();

        assert_eq!(run.trusted, Some(true), "the vouched function is trusted");
        assert_eq!(run.certified, Some(true), "the vouched function is also certified");
        assert_eq!(run.property, Some(("doubles".to_string(), true)), "its declared property is proved");
        assert!(run.confirmed, "the applied result re-verifies");
        let labels: Vec<&str> = run.steps.iter().map(|x| x.label.as_str()).collect();
        assert_eq!(labels, ["query", "ack", "trust", "certify", "prove", "propose", "commit", "assert"]);
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
    fn wrong_first_fit_advances_to_the_next_candidate() {
        // Two candidates share the intent AND the signature fit (one string argument) — the coarse
        // filter cannot split them. The lexicographically-first one is EFFECTFUL, so the grantless
        // responder answers a signed `reject` ("effect not granted") — the run must ADVANCE to the
        // pure second candidate and confirm, keeping the reject in the transcript (the GW14
        // io/network/http abort, fixed).
        let (fn_eff, fn_pure) = (format!("fn_{}", "a".repeat(64)), format!("fn_{}", "b".repeat(64)));
        let (expr_eff, expr_pure) = (format!("expr_{}", "a".repeat(64)), format!("expr_{}", "b".repeat(64)));
        let body_eff = json!({ "kind": "lambda", "params": [{ "name": "u" }],
            "body": { "kind": "app", "fn": { "kind": "var", "name": "http_get" },
                      "args": [{ "kind": "var", "name": "u" }] } });
        let body_pure = json!({ "kind": "lambda", "params": [{ "name": "u" }],
            "body": { "kind": "app", "fn": { "kind": "var", "name": "str_length" },
                      "args": [{ "kind": "var", "name": "u" }] } });
        let rec = |hash: &str, name: &str, result: &str, effects: Vec<&str>, body_hash: &str| {
            json!({
                "schema_version": "0.2.0", "hash": hash, "name_hints": [name],
                "signature": { "type": { "kind": "fn", "params": [{ "kind": "builtin", "name": "string" }],
                                         "result": { "kind": "builtin", "name": result } },
                               "refinements": [], "effects": effects, "capabilities": [],
                               "terminates": "always" },
                "examples": [{ "args": [{ "kind": "string", "value": "x" }],
                               "result": { "kind": "int", "value": 1 } }],
                "intent_tags": ["retrytest"], "derived_from": null, "supersedes": null,
                "body_hash": body_hash })
        };
        let records: std::collections::HashMap<String, J> = [
            (fn_eff.clone(), rec(&fn_eff, "eff_fetch", "string", vec!["net.read"], &expr_eff)),
            (fn_pure.clone(), rec(&fn_pure, "pure_len", "nat", vec![], &expr_pure)),
        ].into();
        let link: std::collections::HashMap<String, J> = [
            (expr_eff, body_eff.clone()), (fn_eff.clone(), body_eff),
            (expr_pure, body_pure.clone()), (fn_pure.clone(), body_pure),
        ].into();

        let run = orchestrate_verified_with_maps(
            link, records, "retrytest", vec![json!({ "kind": "string", "value": "hello" })],
            &signing_key_from_seed("orch"), &signing_key_from_seed("resp"),
            "z3", None, &[], None, false, None,
        ).unwrap();

        assert!(run.confirmed, "the pure second candidate must confirm after the effectful first is rejected");
        let labels: Vec<&str> = run.steps.iter().map(|x| x.label.as_str()).collect();
        assert_eq!(labels, ["query", "ack", "rank", "certify", "propose", "reject", "retry",
                            "certify", "propose", "commit", "assert"]);
        let retry = run.steps.iter().find(|s| s.label == "retry").unwrap();
        assert_eq!(retry.message.get("after").unwrap().as_str().unwrap(), fn_eff);
        assert_eq!(retry.message.get("reason").unwrap(), "rejected");
        // The winning claim is about the PURE candidate: str_length("hello") = 5.
        let result = run.steps.last().unwrap().message.pointer("/body/claim/expr/args/1/value").unwrap();
        assert_eq!(result, &json!({ "kind": "int", "value": 5 }));
    }

    /// A minimal pure record/body pair for ranking tests: `hash`, `name_hints`, string→? signature.
    fn rank_rec(hash: &str, name: &str, tags: &[&str], params: &[&str], result: &str, body_hash: &str) -> J {
        json!({
            "schema_version": "0.2.0", "hash": hash, "name_hints": [name],
            "signature": { "type": { "kind": "fn",
                                     "params": params.iter().map(|p| json!({ "kind": "builtin", "name": p })).collect::<Vec<J>>(),
                                     "result": { "kind": "builtin", "name": result } },
                           "refinements": [], "effects": [], "capabilities": [],
                           "terminates": "always" },
            "examples": [{ "args": [{ "kind": "int", "value": 1 }],
                           "result": { "kind": "int", "value": 1 } }],
            "intent_tags": tags, "derived_from": null, "supersedes": null,
            "body_hash": body_hash })
    }

    #[test]
    fn expected_result_sort_drops_wrong_return_type() {
        // Both candidates take one string; one RETURNS bool (a predicate), the other nat. The
        // argument-only filter cannot split them (the GW16 unsplittable-fits residual) — an expected
        // result of int sort must drop the predicate before ranking, so only the length function runs.
        let (fn_pred, fn_len) = (format!("fn_{}", "a".repeat(64)), format!("fn_{}", "b".repeat(64)));
        let (expr_pred, expr_len) = (format!("expr_{}", "a".repeat(64)), format!("expr_{}", "b".repeat(64)));
        let body_pred = json!({ "kind": "lambda", "params": [{ "name": "u" }],
            "body": { "kind": "app", "fn": { "kind": "var", "name": "gt" },
                      "args": [{ "kind": "app", "fn": { "kind": "var", "name": "str_length" },
                                 "args": [{ "kind": "var", "name": "u" }] },
                               { "kind": "lit", "value": { "kind": "int", "value": 5 } }] } });
        let body_len = json!({ "kind": "lambda", "params": [{ "name": "u" }],
            "body": { "kind": "app", "fn": { "kind": "var", "name": "str_length" },
                      "args": [{ "kind": "var", "name": "u" }] } });
        let records: std::collections::HashMap<String, J> = [
            (fn_pred.clone(), rank_rec(&fn_pred, "is_long", &["sorttest"], &["string"], "bool", &expr_pred)),
            (fn_len.clone(), rank_rec(&fn_len, "len_of", &["sorttest"], &["string"], "nat", &expr_len)),
        ].into();
        let link: std::collections::HashMap<String, J> = [
            (expr_pred, body_pred.clone()), (fn_pred.clone(), body_pred),
            (expr_len, body_len.clone()), (fn_len.clone(), body_len),
        ].into();

        let run = orchestrate_verified_with_maps(
            link, records, "sorttest", vec![json!({ "kind": "string", "value": "hello" })],
            &signing_key_from_seed("orch"), &signing_key_from_seed("resp"),
            "z3", None, &[], None, false, Some(json!({ "kind": "int", "value": 5 })),
        ).unwrap();

        assert!(run.confirmed);
        // The predicate never survives the filter: ONE candidate remains, so no rank step, no retry.
        let labels: Vec<&str> = run.steps.iter().map(|x| x.label.as_str()).collect();
        assert_eq!(labels, ["query", "ack", "certify", "propose", "commit", "assert"]);
        let target = run.steps.iter().find(|s| s.label == "propose").unwrap().message.pointer("/body/target").unwrap();
        assert_eq!(target.as_str().unwrap(), fn_len, "only the int-sorted candidate survives an int expectation");
    }

    #[test]
    fn variant_expectation_splits_maybe_payload_sorts() {
        // The GitHub-scale finding: a `Just false` goal must NOT keep `Maybe Json` fits — their
        // Just-arm answers `Just (JBool …)`, a different value. A caller who wants the Json
        // encoding states it, and then the Json fit (and only it) survives.
        let maybe_bool = json!({
            "signature": { "type": { "kind": "fn", "params": [{ "kind": "builtin", "name": "string" }],
                "result": { "kind": "sum", "variants": [
                    { "tag": "Just", "type": { "kind": "builtin", "name": "bool" } },
                    { "tag": "None" } ] } } } });
        let maybe_json = json!({
            "signature": { "type": { "kind": "fn", "params": [{ "kind": "builtin", "name": "string" }],
                "result": { "kind": "sum", "variants": [
                    { "tag": "Just", "type": { "kind": "builtin", "name": "Json" } },
                    { "tag": "None" } ] } } } });
        let maybe_bool_nominal = json!({
            "signature": { "type": { "kind": "fn", "params": [{ "kind": "builtin", "name": "string" }],
                "result": { "kind": "apply", "ctor": { "kind": "builtin", "name": "Maybe" },
                            "args": [{ "kind": "builtin", "name": "bool" }] } } } });

        let just_false = json!({ "kind": "variant", "tag": "Just",
                                 "payload": { "kind": "bool", "value": false } });
        assert!(result_fits(&maybe_bool, &just_false), "a bool Just-arm answers Just false");
        assert!(result_fits(&maybe_bool_nominal, &just_false), "nominal Maybe bool too");
        assert!(!result_fits(&maybe_json, &just_false),
                "a Maybe Json body can never literally produce Just false");

        let just_jbool = json!({ "kind": "variant", "tag": "Just",
                                 "payload": { "kind": "variant", "tag": "JBool",
                                              "payload": { "kind": "bool", "value": false } } });
        assert!(result_fits(&maybe_json, &just_jbool),
                "the Json-encoded expectation keeps the Json fit");
        assert!(!result_fits(&maybe_bool, &just_jbool),
                "…and drops the bare-bool fit (its Just-arm never holds a variant)");

        // A nominal Json result answers exactly the J-constructors, payload-sorted.
        let json_res = json!({
            "signature": { "type": { "kind": "fn", "params": [{ "kind": "builtin", "name": "string" }],
                "result": { "kind": "builtin", "name": "Json" } } } });
        let jstr = json!({ "kind": "variant", "tag": "JStr",
                           "payload": { "kind": "string", "value": "x" } });
        assert!(result_fits(&json_res, &jstr));
        assert!(!result_fits(&json_res, &just_false), "Just is not a Json constructor");
        // The None case: a Maybe answers it, payloadless.
        let none = json!({ "kind": "variant", "tag": "None" });
        assert!(result_fits(&maybe_bool, &none));
        assert!(result_fits(&maybe_json, &none));
    }

    #[test]
    fn dry_run_against_expected_result_orders_candidates() {
        // Two pure (int)->int candidates share intent, signature, AND result sort — only executing
        // them can split them. With expect square(3)=9, the lexicographically-SECOND candidate must
        // rank first (blind order would propose double). Both bodies are statically pure+terminating,
        // so the dry-run is allowed to run them before certification.
        let (fn_double, fn_square) = (format!("fn_{}", "a".repeat(64)), format!("fn_{}", "b".repeat(64)));
        let (expr_double, expr_square) = (format!("expr_{}", "a".repeat(64)), format!("expr_{}", "b".repeat(64)));
        let body_double = json!({ "kind": "lambda", "params": [{ "name": "n" }],
            "body": { "kind": "app", "fn": { "kind": "var", "name": "mul" },
                      "args": [{ "kind": "var", "name": "n" }, { "kind": "lit", "value": { "kind": "int", "value": 2 } }] } });
        let body_square = json!({ "kind": "lambda", "params": [{ "name": "n" }],
            "body": { "kind": "app", "fn": { "kind": "var", "name": "mul" },
                      "args": [{ "kind": "var", "name": "n" }, { "kind": "var", "name": "n" }] } });
        let records: std::collections::HashMap<String, J> = [
            (fn_double.clone(), rank_rec(&fn_double, "double_it", &["dryruntest"], &["int"], "int", &expr_double)),
            (fn_square.clone(), rank_rec(&fn_square, "square_it", &["dryruntest"], &["int"], "int", &expr_square)),
        ].into();
        let link: std::collections::HashMap<String, J> = [
            (expr_double, body_double.clone()), (fn_double.clone(), body_double),
            (expr_square, body_square.clone()), (fn_square.clone(), body_square),
        ].into();

        let run = orchestrate_verified_with_maps(
            link, records, "dryruntest", vec![json!({ "kind": "int", "value": 3 })],
            &signing_key_from_seed("orch"), &signing_key_from_seed("resp"),
            "z3", None, &[], None, false, Some(json!({ "kind": "int", "value": 9 })),
        ).unwrap();

        assert!(run.confirmed);
        let rank = run.steps.iter().find(|s| s.label == "rank").expect("two candidates must produce a rank step");
        assert_eq!(rank.message.pointer("/ordered/0").unwrap().as_str().unwrap(), fn_square,
            "the dry-run match outranks the lexicographically-first mismatch");
        assert_eq!(rank.message.pointer("/scores/0/dry_run").unwrap(), "match");
        assert_eq!(rank.message.pointer("/scores/1/dry_run").unwrap(), "mismatch");
        // The FIRST propose goes to square — no wasted reject/retry on the wrong fit.
        let target = run.steps.iter().find(|s| s.label == "propose").unwrap().message.pointer("/body/target").unwrap();
        assert_eq!(target.as_str().unwrap(), fn_square);
        let result = run.steps.last().unwrap().message.pointer("/body/claim/expr/args/1/value").unwrap();
        assert_eq!(result, &json!({ "kind": "int", "value": 9 }));
    }

    #[test]
    fn name_affinity_orders_ties_without_an_expectation() {
        // No expectation, identical bodies and tags: the record whose name_hint shares tokens with the
        // queried intent ranks first, even though blind (lexicographic) order would pick the other.
        let (fn_other, fn_affine) = (format!("fn_{}", "a".repeat(64)), format!("fn_{}", "b".repeat(64)));
        let (expr_a, expr_b) = (format!("expr_{}", "a".repeat(64)), format!("expr_{}", "b".repeat(64)));
        let body = json!({ "kind": "lambda", "params": [{ "name": "u" }],
            "body": { "kind": "app", "fn": { "kind": "var", "name": "str_length" },
                      "args": [{ "kind": "var", "name": "u" }] } });
        let records: std::collections::HashMap<String, J> = [
            (fn_other.clone(), rank_rec(&fn_other, "frobnicate", &["length-of-thing"], &["string"], "nat", &expr_a)),
            (fn_affine.clone(), rank_rec(&fn_affine, "thing_length", &["length-of-thing"], &["string"], "nat", &expr_b)),
        ].into();
        let link: std::collections::HashMap<String, J> = [
            (expr_a, body.clone()), (fn_other.clone(), body.clone()),
            (expr_b, body.clone()), (fn_affine.clone(), body),
        ].into();

        let run = orchestrate_verified_with_maps(
            link, records, "length-of-thing", vec![json!({ "kind": "string", "value": "hey" })],
            &signing_key_from_seed("orch"), &signing_key_from_seed("resp"),
            "z3", None, &[], None, false, None,
        ).unwrap();

        assert!(run.confirmed);
        let target = run.steps.iter().find(|s| s.label == "propose").unwrap().message.pointer("/body/target").unwrap();
        assert_eq!(target.as_str().unwrap(), fn_affine, "name-hint affinity with the intent breaks the tie");
    }

    #[test]
    fn goal_score_reads_tag_specificity_and_name_affinity() {
        let records = std::collections::HashMap::new();
        let specific = json!({ "name_hints": ["fetch_pages"], "intent_tags": ["query", "query/pages", "query/pages/all"] });
        let plain = json!({ "name_hints": ["unrelated"], "intent_tags": ["query"] });
        let s = goal_score(&specific, None, "query", None, &[], &records);
        let p = goal_score(&plain, None, "query", None, &[], &records);
        assert_eq!(s.tags, 2, "two tags extend `query/`");
        assert_eq!(p.tags, 0);
        assert_eq!(goal_score(&specific, None, "fetch-pages", None, &[], &records).name, 2,
            "intent tokens match both name_hint tokens");
        assert!(s.key() > p.key());
    }

    #[test]
    fn verified_run_aborts_on_an_untrusted_function() {
        // Same discovery, but the policy trusts a root that vouches for nothing → the discovered function
        // is untrusted, so the run stops before proving or applying it.
        let (orch, resp) = (signing_key_from_seed("orch"), signing_key_from_seed("resp"));
        let pol = policy(&[&did("lonely-root")]);
        let run = orchestrate_verified(
            &examples(), "arithmetic", vec![json!({ "kind": "nat", "value": 21 })],
            &orch, &resp, "z3", Some(&pol), &[], None, false, None,
        ).unwrap();

        assert_eq!(run.trusted, Some(false));
        assert!(run.property.is_none());
        assert!(!run.confirmed);
        let labels: Vec<&str> = run.steps.iter().map(|x| x.label.as_str()).collect();
        assert_eq!(labels, ["query", "ack", "trust"], "aborts at the trust gate");
    }
}
