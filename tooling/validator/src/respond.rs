//! The Nova Locutio agent loop — a reference *responder*. This is the piece that finally makes
//! Nova Locutio *actionable*: messages were validated, signed, encrypted, stored, and discoverable,
//! but nothing yet consumed one to drive behavior. `respond_to_request` closes the loop:
//!
//! 1. consume a signed `request` whose body is `action: "apply"` over a commons `target` and
//!    value-expression `args` (spec/value-expression.schema.json),
//! 2. resolve the target's body from the commons (the same link map `run --records` builds, so
//!    `fn_ref` arguments compose — principle 4) and **run** it on the args (interp.rs),
//! 3. emit a signed `assert` (spec/message.v0.2.schema.json) whose `predicate` claim states the
//!    computed equation `eq( target(args…), result )`, threaded back to the request via
//!    `in_reply_to` and addressed `to` the original sender.
//!
//! The result is self-verifying by construction: the claim is an ordinary predicate-expression
//! (spec/predicate-expression.schema.json) that any receiver can re-run with the same evaluator to
//! confirm — verification is re-execution, no trusted responder required (principles 3, 6, 7).
//!
//! Scope: by default the responder fulfils only **pure** targets — it grants no effects, so an
//! effectful target is refused with a signed, policy-shaped `reject` (see [`effect_refusal`]). The
//! *operator* may grant specific effects ([`crate::set_effect_grants`] / the `--grant` flag on
//! `respond`/`orchestrate`); grants are measured against the target's verified effect declaration
//! and the runtime sandbox enforces at perform time regardless (spec/agent-loop.md §Scope). A
//! request whose args don't decode, or whose target has no resolvable body, is an honest error
//! rather than a silent empty assert. Note an assert produced under grants is an *observation* —
//! a verifier without the same grants (and the same world) cannot CONFIRM it by re-execution.

use anyhow::{anyhow, bail, Context, Result};
use ed25519_dalek::SigningKey;
use serde_json::{json, Value as J};
use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};

use crate::{clear_resolver, eval_body, set_resolver, sign_message, verify_delegation_chain};

thread_local! {
    /// The trace artifact behind the most recent `observed` assert (see [`assert_application`]).
    /// The claim carries only the trace's `trc_…` content-address; the artifact itself must travel
    /// alongside the assert (published to the node, written next to the message) or nobody can
    /// replay it — the caller drains this after each respond.
    static PENDING_TRACE: RefCell<Option<J>> = const { RefCell::new(None) };
}

/// Drain the trace artifact produced by the most recent assert, if that assert's claim was
/// `observed` (i.e. the run performed effects). `None` = the last assert was a pure `predicate`
/// claim. The artifact is `{kind: "trace", version, ops: […]}`, self-addressed as `trc_…`.
pub fn take_trace_artifact() -> Option<J> {
    PENDING_TRACE.with(|t| t.borrow_mut().take())
}

/// A responder's **local trust policy** for verifying presented capabilities (spec/trust-model.md).
/// When `roots` is non-empty the capability gate switches from possession-only ("did the request list
/// the string?") to chain-verified ("can the sender exhibit a signed `delegate` chain back to a root I
/// recognize?"), using the [`crate::verify_delegation_chain`] verifier. An empty policy (the default)
/// keeps the legacy possession-only behavior, so a responder with no configured roots is unchanged.
#[derive(Default, Clone)]
pub struct TrustPolicy {
    /// DIDs the receiver recognizes as roots per local policy (a root is self-authorizing).
    pub roots: BTreeSet<String>,
    /// The pool of signed `delegate` tokens the receiver has on hand to reconstruct chains from.
    pub delegations: Vec<J>,
    /// Verification instant (RFC 3339 UTC) for expiry checks; `None` ignores expiry.
    pub at: Option<String>,
}

/// Run the responder over a `request` message, returning the signed `assert` reply.
///
/// `link_map` is an address → body-AST map (see [`crate::build_link_map`]); it resolves both the
/// request's `target` body and any `fn_ref` arguments. `signing_key` is the responder's identity —
/// `sign_message` sets `from`/`hash`/`signature`. `timestamp` is an optional ISO-8601 instant for
/// the reply (advisory; `None` → `null`, which keeps the reply deterministic for a given seed).
pub fn respond_to_request(
    request: &J,
    link_map: HashMap<String, J>,
    signing_key: &SigningKey,
    timestamp: Option<&str>,
) -> Result<J> {
    // 1. Validate the request shape: a `request` speech act asking to `apply` a target.
    let kind = request.get("kind").and_then(|k| k.as_str()).unwrap_or_default();
    if kind != "request" {
        bail!("respond expects a `request` message, got kind `{kind}`");
    }
    let body = request.get("body").ok_or_else(|| anyhow!("request has no `body`"))?;
    let action = body.get("action").and_then(|a| a.as_str()).unwrap_or_default();
    if action != "apply" {
        bail!("respond only handles the `apply` request action, got `{action}`");
    }
    let target = body
        .get("target")
        .and_then(|t| t.as_str())
        .ok_or_else(|| anyhow!("an `apply` request must carry a `target` content-address"))?;
    let args = body.get("args").and_then(|a| a.as_array()).cloned().unwrap_or_default();

    // Identity + threading: reply goes back to the sender, in reply to this exact message.
    let requester = request
        .get("from")
        .and_then(|f| f.as_str())
        .ok_or_else(|| anyhow!("request has no `from` to reply to"))?;
    let req_hash = request
        .get("hash")
        .and_then(|h| h.as_str())
        .ok_or_else(|| anyhow!("request has no `hash` to thread `in_reply_to`"))?;

    // Resolve, run, and assert the result (shared with acting on an `apply` commitment).
    assert_application(requester, req_hash, target, &args, link_map, signing_key, timestamp)
}

/// Resolve `target`, run it on `args`, and return a signed `assert` whose predicate claim is
/// `eq(target(args…), result)`. Shared by `request`/`apply` and acting on an `apply` commitment.
fn assert_application(
    requester: &str,
    in_reply_to: &str,
    target: &str,
    args: &[J],
    link_map: HashMap<String, J>,
    signing_key: &SigningKey,
    timestamp: Option<&str>,
) -> Result<J> {
    let target_body = link_map
        .get(target)
        .cloned()
        .ok_or_else(|| anyhow!("cannot resolve target `{target}`: no body for it in the provided records"))?;
    set_resolver(link_map);
    let _ = crate::take_effect_trace(); // drop observations from any earlier run this thread did
    let computed = eval_body(&target_body, args);
    clear_resolver();
    let result = computed.with_context(|| format!("running target `{target}` on the arguments"))?;
    let ops = crate::take_effect_trace();

    // The predicate claim: eq( target(arg0, …), result ). Each arg is a value-expression `lit`; the
    // target is an `app` op by content-address.
    let app_args: Vec<J> = args.iter().map(|a| json!({ "kind": "lit", "value": a })).collect();
    let claim_expr = json!({
        "kind": "app", "op": "eq", "args": [
            { "kind": "app", "op": target, "args": app_args },
            { "kind": "lit", "value": result }
        ]
    });
    // A run that performed effects is an OBSERVATION, not a stably re-runnable equation. The claim
    // becomes `observed`, conditioned on the recorded trace by content-address: any receiver can
    // replay the computation against the trace — no grants, no secrets — and confirm the result
    // follows deterministically from the recorded observations. (The observations themselves remain
    // the signer's testimony; the trust model prices that, spec/agent-loop.md §Scope.)
    let claim = if ops.is_empty() {
        PENDING_TRACE.with(|t| *t.borrow_mut() = None);
        json!({ "kind": "predicate", "expr": claim_expr })
    } else {
        let trace_artifact = json!({ "kind": "trace", "version": "0.1.0", "ops": ops });
        let trace_addr = crate::hash_artifact_with_kind(&trace_artifact, crate::ArtifactKind::Trace)
            .context("hashing the recorded effect trace")?;
        PENDING_TRACE.with(|t| *t.borrow_mut() = Some(trace_artifact));
        json!({ "kind": "observed", "expr": claim_expr, "trace": trace_addr })
    };
    let mut assert = build_envelope("assert", requester, in_reply_to, timestamp,
        json!({ "subject": target, "claim": claim, "evidence": null }));
    sign_message(&mut assert, signing_key).context("signing the assert reply")?;
    Ok(assert)
}

/// Verify an `assert`'s claim by RE-RUNNING it. Installs `link_map` so the claim's
/// content-addressed functions resolve, evaluates the predicate, and returns whether it holds.
///
/// This is the *verifier half* of the agent loop: the receiver confirms the asserted computation by
/// re-executing it rather than trusting the asserter (principle 3 — verification is re-execution;
/// principle 7 — no privileged party). `Ok(true)` = the claim re-ran true. Errors if the claim is
/// not a runnable `predicate`/`observed` claim, or the predicate is undecidable / non-boolean.
///
/// An **`observed`** claim is verified by REPLAY: its `trace` address is resolved (from the same
/// `link_map`), the recorded trace is installed, and the computation re-runs with every effect
/// served from the record — no grants, no secrets. The trace must be consumed EXACTLY (a leftover
/// or mismatched entry means the trace does not correspond to this computation). `Ok(true)` here
/// means the result follows deterministically from the recorded observations; whether the world
/// really said that is the signer's testimony, priced by the trust model.
pub fn verify_claim(assert: &J, link_map: HashMap<String, J>) -> Result<bool> {
    let expr = assert
        .pointer("/body/claim/expr")
        .ok_or_else(|| anyhow!("assert has no `body.claim.expr` to re-run (not a `predicate`/`observed` claim?)"))?
        .clone();
    let kind = assert.pointer("/body/claim/kind").and_then(|k| k.as_str()).unwrap_or("predicate");
    let replaying = kind == "observed";
    if replaying {
        let trace_addr = assert
            .pointer("/body/claim/trace")
            .and_then(|t| t.as_str())
            .ok_or_else(|| anyhow!("`observed` claim has no `trace` content-address"))?;
        let trace = link_map.get(trace_addr).ok_or_else(|| {
            anyhow!("cannot resolve the claim's trace `{trace_addr}`: not in the provided records/node — without the recorded observations an observed claim cannot be replayed")
        })?;
        let ops = trace
            .get("ops")
            .and_then(|o| o.as_array())
            .ok_or_else(|| anyhow!("trace `{trace_addr}` has no `ops` array"))?
            .clone();
        crate::interp::set_effect_replay(ops);
    }
    set_resolver(link_map.clone());
    let verdict = crate::interp::eval_claim(&expr);
    let leftover = crate::interp::effect_replay_remaining().unwrap_or(0);
    crate::interp::clear_effect_replay();
    clear_resolver();
    match verdict {
        Some(_) if replaying && leftover > 0 => bail!(
            "observed claim's trace was not fully consumed ({leftover} recorded observation{} left over) — the trace does not correspond to this computation",
            if leftover == 1 { "" } else { "s" }
        ),
        Some(crate::interp::Val::Bool(b)) => Ok(b),
        Some(other) => bail!(
            "claim predicate did not evaluate to a boolean: {}",
            crate::interp::encode_value(&other)
        ),
        None if replaying => bail!(
            "observed claim is undecidable under replay (unresolved function, malformed, or the recorded trace mismatches the computation)"
        ),
        None => bail!("claim predicate is undecidable (unresolved function, malformed, or effectful)"),
    }
}

/// The common reply envelope; `sign_message` fills `from`/`hash`/`signature`.
fn build_envelope(kind: &str, to: &str, in_reply_to: &str, timestamp: Option<&str>, body: J) -> J {
    json!({
        "schema_version": "0.2.0",
        "kind": kind,
        "to": to,
        "in_reply_to": in_reply_to,
        "timestamp": timestamp,
        "constraints": null,
        "body": body
    })
}

fn sign_envelope(mut envelope: J, key: &SigningKey) -> Result<J> {
    sign_message(&mut envelope, key).context("signing the reply")?;
    Ok(envelope)
}

/// The fuller agent loop: dispatch a consumed message to the right handler and return a signed reply.
/// Handles `request`/`apply` (assert a computed result — see [`respond_to_request`]),
/// `request`/`validate` (typecheck + run the target, then assert it `verified` or `reject`), and
/// `query` (search the records, `ack` with the matches). `link_map` resolves bodies (apply/validate);
/// `record_map` (address → function record) backs validate and query.
pub fn respond_to_message(
    message: &J,
    link_map: HashMap<String, J>,
    record_map: HashMap<String, J>,
    signing_key: &SigningKey,
    timestamp: Option<&str>,
) -> Result<J> {
    respond_to_message_with_trust(message, link_map, record_map, signing_key, timestamp, &TrustPolicy::default())
}

/// As [`respond_to_message`], but the capability gate verifies presented capabilities against the
/// responder's `policy`: with recognized roots configured, `apply`/`propose` succeed only when the
/// sender can exhibit a valid signed `delegate` chain to a root (not merely list the capability
/// string). An empty policy is identical to [`respond_to_message`].
pub fn respond_to_message_with_trust(
    message: &J,
    link_map: HashMap<String, J>,
    record_map: HashMap<String, J>,
    signing_key: &SigningKey,
    timestamp: Option<&str>,
    policy: &TrustPolicy,
) -> Result<J> {
    let kind = message.get("kind").and_then(|k| k.as_str()).unwrap_or_default();

    // Effect gate for the target-executing paths: refuse (signed, policy-shaped) when the target
    // needs effects beyond the operator's grants — see [`effect_refusal`]. Threading mirrors each
    // arm's reply addressee: a `commit`'s reply goes to its `to` (the original proposer), falling
    // back to its sender; everything else replies to `from`.
    let effect_gate = |target_ptr: &str| -> Option<Result<J>> {
        let target = message.pointer(target_ptr).and_then(|t| t.as_str())?;
        let (code, reason) = effect_refusal(target, &link_map, &record_map)?;
        let hash = message.get("hash").and_then(|h| h.as_str())?;
        let reply_to = if kind == "commit" {
            message.get("to").and_then(|t| t.as_str()).or_else(|| message.get("from").and_then(|f| f.as_str()))?
        } else {
            message.get("from").and_then(|f| f.as_str())?
        };
        Some(sign_envelope(
            build_envelope("reject", reply_to, hash, timestamp,
                json!({ "rejects": hash, "code": code, "reason": reason })),
            signing_key,
        ))
    };
    match kind {
        "request" => {
            let action = message.pointer("/body/action").and_then(|a| a.as_str()).unwrap_or_default();
            match action {
                "apply" => match capability_gate(message, &record_map, policy, signing_key, timestamp)
                    .or_else(|| effect_gate("/body/target"))
                {
                    Some(reject) => reject,
                    None => respond_to_request(message, link_map, signing_key, timestamp),
                },
                "validate" => validate_reply(message, link_map, record_map, signing_key, timestamp),
                "store" => store_reply(message, signing_key, timestamp),
                other => bail!("respond handles the `apply`/`validate`/`store` request actions, not `{other}`"),
            }
        }
        "query" => query_reply(message, record_map, signing_key, timestamp),
        "propose" => match capability_gate(message, &record_map, policy, signing_key, timestamp)
            .or_else(|| effect_gate("/body/target"))
        {
            Some(reject) => reject,
            None => propose_reply(message, link_map, signing_key, timestamp),
        },
        "commit" => match effect_gate("/body/commitment/fn") {
            Some(reject) => reject,
            None => commit_reply(message, link_map, signing_key, timestamp),
        },
        "delegate" => delegate_reply(message, signing_key, timestamp),
        "retract" => retract_reply(message, signing_key, timestamp),
        other => bail!("respond handles request/query/propose/commit/delegate/retract, not `{other}`"),
    }
}

/// Capability gate for `apply`/`propose`. The target's record declares the capabilities it requires
/// (`signature.capabilities`); the gate decides whether the sender is authorized for all of them,
/// returning `Some(signed reject `not_authorized`)` if not, else `None` (proceed). Two modes,
/// selected by the responder's [`TrustPolicy`] (principle 6: capability delegation is first-class):
///
/// - **Possession-only** (no recognized roots): the request must list each required capability in
///   `constraints.capabilities`. This is the legacy behavior — the string *is* the claim.
/// - **Chain-verified** (recognized roots configured): each required capability must be backed by a
///   valid signed `delegate` chain from the sender (`from`) back to a recognized root, verified by
///   [`crate::verify_delegation_chain`] against the policy's token pool. Listing the string no longer
///   suffices — the sender must actually hold the delegation.
fn capability_gate(
    message: &J,
    record_map: &HashMap<String, J>,
    policy: &TrustPolicy,
    signing_key: &SigningKey,
    timestamp: Option<&str>,
) -> Option<Result<J>> {
    let target = message.pointer("/body/target").and_then(|t| t.as_str())?;
    let required: Vec<&str> = record_map
        .get(target)
        .and_then(|r| r.pointer("/signature/capabilities"))
        .and_then(|c| c.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str()).collect())
        .unwrap_or_default();
    if required.is_empty() {
        return None;
    }
    let from = message.get("from").and_then(|f| f.as_str())?;
    let hash = message.get("hash").and_then(|h| h.as_str())?;
    let reject = |reason: String| {
        Some(sign_envelope(
            build_envelope("reject", from, hash, timestamp, json!({
                "rejects": hash, "code": "not_authorized", "reason": reason
            })),
            signing_key,
        ))
    };

    if policy.roots.is_empty() {
        // Possession-only: the request must list each required capability.
        let presented: Vec<&str> = message
            .pointer("/constraints/capabilities")
            .and_then(|c| c.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str()).collect())
            .unwrap_or_default();
        let missing: Vec<&str> = required.iter().copied().filter(|c| !presented.contains(c)).collect();
        if missing.is_empty() {
            return None;
        }
        return reject(format!(
            "target requires capabilit{} [{}] not presented in constraints.capabilities",
            if missing.len() == 1 { "y" } else { "ies" }, missing.join(", ")
        ));
    }

    // Chain-verified: each required capability must trace through a signed delegation chain to a root.
    let at = policy.at.as_deref();
    let unbacked: Vec<&str> = required
        .iter()
        .copied()
        .filter(|cap| !verify_delegation_chain(cap, from, &policy.delegations, &policy.roots, at).authorized)
        .collect();
    if unbacked.is_empty() {
        return None;
    }
    reject(format!(
        "no valid signed delegation chain authorizes `{from}` for capabilit{} [{}] back to a recognized root",
        if unbacked.len() == 1 { "y" } else { "ies" }, unbacked.join(", ")
    ))
}

/// Static effect gate for a target about to be *executed* on behalf of a remote sender
/// (spec/agent-loop.md §Scope). The operator declares the effects this responder will perform for
/// strangers by installing grants ([`crate::set_effect_grants`], the `--grant` flag); the default is
/// none — pure-only. This gate runs before execution so an effectful target gets a *distinct,
/// policy-shaped* refusal instead of a generic eval error (the runtime sandbox still enforces at
/// perform time regardless — this is reporting, the sandbox is the boundary). Two checks:
///
/// 1. **Honesty**: the body's statically inferred effects must not exceed the record's *declared*
///    `signature.effects` (a free local `check-effects` — grants are measured against verified
///    declarations, never the record's word). Violation → `constraint_violated`.
/// 2. **Policy**: every inferred effect must be operator-granted. Shortfall → `refused`, with an
///    `effect not granted:` reason naming the missing effects.
///
/// Returns `Some((code, reason))` to refuse, `None` to proceed. A pure target always proceeds.
fn effect_refusal(
    target: &str,
    link_map: &HashMap<String, J>,
    record_map: &HashMap<String, J>,
) -> Option<(&'static str, String)> {
    let body = link_map.get(target)?;
    let inf = crate::effects::infer_effects(body, record_map);
    if inf.effects.is_empty() {
        return None; // pure (any opaque callee is the runtime sandbox's to stop)
    }
    if let Some(declared) = record_map.get(target).map(|r| {
        r.pointer("/signature/effects")
            .and_then(|e| e.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect::<std::collections::BTreeSet<_>>())
            .unwrap_or_default()
    }) {
        let undeclared: Vec<&String> = inf.effects.iter().filter(|e| !declared.contains(*e)).collect();
        if !undeclared.is_empty() {
            return Some((
                "constraint_violated",
                format!(
                    "under-declared effects: body performs [{}] beyond the record's declared effects",
                    undeclared.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
                ),
            ));
        }
    }
    let granted = crate::interp::current_effect_grants();
    // A scoped grant (`net.write@api.example.com`, `net.write@host/path`, `fs.read@/dir`)
    // satisfies the static gate for its BASE
    // effect — which host a call actually targets is only known at the effect boundary, where
    // the sandbox enforces the scope (interp::effect_op_at).
    let granted_bases: std::collections::BTreeSet<String> =
        granted.iter().map(|g| g.split('@').next().unwrap_or(g).to_string()).collect();
    let ungranted: Vec<&String> = inf.effects.iter().filter(|e| !granted_bases.contains(*e)).collect();
    if ungranted.is_empty() {
        return None;
    }
    Some((
        "refused",
        format!(
            "effect not granted: target requires [{}] beyond the responder's granted effects [{}]",
            ungranted.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", "),
            granted.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
        ),
    ))
}

/// `(from, hash)` of a message, for replies threaded back to the sender.
fn envelope_ids(message: &J) -> Result<(&str, &str)> {
    let from = message.get("from").and_then(|f| f.as_str()).ok_or_else(|| anyhow!("message has no `from`"))?;
    let hash = message.get("hash").and_then(|h| h.as_str()).ok_or_else(|| anyhow!("message has no `hash`"))?;
    Ok((from, hash))
}

/// `request`/`store`: verify the inline payload artifact is self-consistent (its content-address
/// recomputes), then `ack` that it was admitted, else `reject`.
fn store_reply(message: &J, signing_key: &SigningKey, timestamp: Option<&str>) -> Result<J> {
    let (requester, req_hash) = envelope_ids(message)?;
    let reject = |code: &str, reason: String| {
        sign_envelope(build_envelope("reject", requester, req_hash, timestamp,
            json!({ "rejects": req_hash, "code": code, "reason": reason })), signing_key)
    };
    let Some(payload) = message.pointer("/body/payload") else {
        return reject("malformed", "a `store` request must carry a `payload`".into());
    };
    match crate::verify_artifact_hash(payload) {
        Ok(v) if v.matches => {
            let stored = payload.get("hash").and_then(|h| h.as_str()).unwrap_or_default();
            sign_envelope(build_envelope("ack", requester, req_hash, timestamp,
                json!({ "acks": req_hash, "result": { "stored": stored } })), signing_key)
        }
        _ => reject("constraint_violated", "payload failed content-address verification".into()),
    }
}

/// Acting on a received `commit`: an `apply` commitment is fulfilled (resolve + run the function →
/// `assert` the result); a `provide`/`refrain` commitment is acknowledged.
fn commit_reply(
    message: &J,
    link_map: HashMap<String, J>,
    signing_key: &SigningKey,
    timestamp: Option<&str>,
) -> Result<J> {
    // A commit is sent BY the committer TO the proposer; fulfilling it reports back to that proposer
    // (the commit's `to`), falling back to its sender if unaddressed.
    let commit_hash = message.get("hash").and_then(|h| h.as_str()).ok_or_else(|| anyhow!("message has no `hash`"))?;
    let requester = message.get("to").and_then(|t| t.as_str())
        .or_else(|| message.get("from").and_then(|f| f.as_str()))
        .ok_or_else(|| anyhow!("commit has no `to`/`from` to reply to"))?;
    let reject = |code: &str, reason: String| {
        sign_envelope(build_envelope("reject", requester, commit_hash, timestamp,
            json!({ "rejects": commit_hash, "code": code, "reason": reason })), signing_key)
    };
    let kind = message.pointer("/body/commitment/kind").and_then(|k| k.as_str()).unwrap_or_default();
    if kind != "apply" {
        // provide / refrain: acknowledge the commitment.
        return sign_envelope(build_envelope("ack", requester, commit_hash, timestamp,
            json!({ "acks": commit_hash, "result": { "acknowledged": kind } })), signing_key);
    }
    let Some(target) = message.pointer("/body/commitment/fn").and_then(|t| t.as_str()) else {
        return reject("malformed", "an `apply` commitment must carry `fn`".into());
    };
    let args = message.pointer("/body/commitment/args").and_then(|a| a.as_array()).cloned().unwrap_or_default();
    if !link_map.contains_key(target) {
        return reject("unknown_target", format!("cannot resolve commitment target `{target}`"));
    }
    match assert_application(requester, commit_hash, target, &args, link_map, signing_key, timestamp) {
        Ok(assert) => Ok(assert),
        Err(e) => reject("constraint_violated", format!("cannot fulfil commitment: {e:#}")),
    }
}

/// `delegate`: acknowledge the capability grant.
fn delegate_reply(message: &J, signing_key: &SigningKey, timestamp: Option<&str>) -> Result<J> {
    let (requester, h) = envelope_ids(message)?;
    let cap = message.pointer("/body/capability").and_then(|c| c.as_str()).unwrap_or_default();
    sign_envelope(build_envelope("ack", requester, h, timestamp,
        json!({ "acks": h, "result": { "delegated": cap } })), signing_key)
}

/// `retract`: acknowledge the retraction.
fn retract_reply(message: &J, signing_key: &SigningKey, timestamp: Option<&str>) -> Result<J> {
    let (requester, h) = envelope_ids(message)?;
    let retracts = message.pointer("/body/retracts").and_then(|r| r.as_str()).unwrap_or_default();
    sign_envelope(build_envelope("ack", requester, h, timestamp,
        json!({ "acks": h, "result": { "retracted": retracts } })), signing_key)
}

/// `propose`/`apply`: a proposal invites action with refusal allowed. The responder verifies it can
/// fulfil the application (resolve + test-run the target), then replies `commit` (an `apply`
/// commitment to run it) or `reject` with a reason.
fn propose_reply(
    message: &J,
    link_map: HashMap<String, J>,
    signing_key: &SigningKey,
    timestamp: Option<&str>,
) -> Result<J> {
    let requester = message.get("from").and_then(|f| f.as_str()).ok_or_else(|| anyhow!("message has no `from`"))?;
    let prop_hash = message.get("hash").and_then(|h| h.as_str()).ok_or_else(|| anyhow!("message has no `hash`"))?;
    let reject = |code: &str, reason: String| {
        sign_envelope(
            build_envelope("reject", requester, prop_hash, timestamp,
                json!({ "rejects": prop_hash, "code": code, "reason": reason })),
            signing_key,
        )
    };

    let action = message.pointer("/body/action").and_then(|a| a.as_str()).unwrap_or_default();
    if action != "apply" {
        return reject("refused", format!("propose action `{action}` is not supported (only `apply`)"));
    }
    let Some(target) = message.pointer("/body/target").and_then(|t| t.as_str()) else {
        return reject("malformed", "a propose/apply must carry a `target`".into());
    };
    let args = message.pointer("/body/args").and_then(|a| a.as_array()).cloned().unwrap_or_default();
    let Some(body_ast) = link_map.get(target).cloned() else {
        return reject("unknown_target", format!("cannot resolve target `{target}`"));
    };

    // Only commit to what we can actually fulfil: resolve + test-run the target on the proposed args.
    set_resolver(link_map);
    let runnable = eval_body(&body_ast, &args).is_ok();
    clear_resolver();
    if !runnable {
        return reject("constraint_violated", format!("cannot fulfil: running `{target}` on the proposed args errored"));
    }

    sign_envelope(
        build_envelope("commit", requester, prop_hash, timestamp,
            json!({ "commitment": { "kind": "apply", "fn": target, "args": args }, "conditions": [], "expires_at": null })),
        signing_key,
    )
}

/// `request`/`validate`: typecheck the target's body and run its examples; assert it `verified` (by
/// the responder's identity) if both pass, otherwise `reject` with a reason.
fn validate_reply(
    message: &J,
    link_map: HashMap<String, J>,
    record_map: HashMap<String, J>,
    signing_key: &SigningKey,
    timestamp: Option<&str>,
) -> Result<J> {
    let requester = message.get("from").and_then(|f| f.as_str()).ok_or_else(|| anyhow!("message has no `from`"))?;
    let req_hash = message.get("hash").and_then(|h| h.as_str()).ok_or_else(|| anyhow!("message has no `hash`"))?;
    let target = message
        .pointer("/body/target")
        .and_then(|t| t.as_str())
        .ok_or_else(|| anyhow!("a `validate` request must carry a `target` content-address"))?;

    let reject = |code: &str, reason: String| {
        sign_envelope(
            build_envelope("reject", requester, req_hash, timestamp,
                json!({ "rejects": req_hash, "code": code, "reason": reason })),
            signing_key,
        )
    };

    let (record, body) = match (record_map.get(target), link_map.get(target)) {
        (Some(r), Some(b)) => (r, b),
        _ => return reject("unknown_target", format!("no resolvable record + body for `{target}`")),
    };

    // Verify the target the same way `nl-validator typecheck`/`run` do — re-execution, not trust.
    set_resolver(link_map.clone());
    let typed = crate::typecheck_record(record, body);
    let runs = crate::run_examples(record, body);
    clear_resolver();

    if let Err(e) = &typed {
        return reject("constraint_violated", format!("ill-typed: {e:#}"));
    }
    match runs {
        Ok(rs) if rs.iter().all(|r| r.passed) => {}
        Ok(rs) => {
            let failed = rs.iter().filter(|r| !r.passed).count();
            return reject("constraint_violated", format!("{failed}/{} examples failed", rs.len()));
        }
        Err(e) => return reject("constraint_violated", format!("evaluation error: {e:#}")),
    }

    let responder = crate::did_nova_from_pubkey(&signing_key.verifying_key());
    sign_envelope(
        build_envelope("assert", requester, req_hash, timestamp,
            json!({ "subject": target, "claim": { "kind": "verified", "subject": target, "by": responder }, "evidence": null })),
        signing_key,
    )
}

/// `query`: `ack` with the records matching the query pattern (effects / intent_tags as containment,
/// terminates as equality; signature_type matching is deferred). Matches are sorted for determinism.
fn query_reply(
    message: &J,
    record_map: HashMap<String, J>,
    signing_key: &SigningKey,
    timestamp: Option<&str>,
) -> Result<J> {
    let requester = message.get("from").and_then(|f| f.as_str()).ok_or_else(|| anyhow!("message has no `from`"))?;
    let q_hash = message.get("hash").and_then(|h| h.as_str()).ok_or_else(|| anyhow!("message has no `hash`"))?;
    let pattern = message.pointer("/body/pattern").cloned().unwrap_or_else(|| json!({}));
    let limit = message.pointer("/body/limit").and_then(|l| l.as_u64()).unwrap_or(50) as usize;

    let mut matches: Vec<String> = record_map
        .iter()
        .filter(|(_, rec)| record_matches(rec, &pattern))
        .map(|(addr, _)| addr.clone())
        .collect();
    matches.sort();
    matches.truncate(limit);

    sign_envelope(
        build_envelope("ack", requester, q_hash, timestamp,
            json!({ "acks": q_hash, "result": { "matches": matches, "count": matches.len() } })),
        signing_key,
    )
}

/// True if `record` satisfies every constraint present in the query `pattern`.
fn record_matches(record: &J, pattern: &J) -> bool {
    let str_set = |v: Option<&J>| -> Vec<String> {
        v.and_then(|x| x.as_array())
            .map(|a| a.iter().filter_map(|e| e.as_str().map(String::from)).collect())
            .unwrap_or_default()
    };
    let want_effects = str_set(pattern.get("effects"));
    if !want_effects.is_empty() {
        let have = str_set(record.pointer("/signature/effects"));
        if !want_effects.iter().all(|e| have.contains(e)) {
            return false;
        }
    }
    let want_tags = str_set(pattern.get("intent_tags"));
    if !want_tags.is_empty() {
        let have = str_set(record.get("intent_tags"));
        if !want_tags.iter().all(|t| have.contains(t)) {
            return false;
        }
    }
    if let Some(want) = pattern.get("terminates").and_then(|t| t.as_str()) {
        if record.pointer("/signature/terminates").and_then(|t| t.as_str()) != Some(want) {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{build_link_map, build_record_map, signing_key_from_seed, verify_signature};
    use std::path::{Path, PathBuf};

    fn examples_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spec/examples")
    }

    fn load(name: &str) -> J {
        serde_json::from_str(&std::fs::read_to_string(examples_dir().join(name)).unwrap()).unwrap()
    }

    /// double's content-address, read from its record so the test never hardcodes a stale hash.
    fn double_addr() -> String {
        load("double.v0.2.json")["hash"].as_str().unwrap().to_string()
    }

    fn map_addr() -> String {
        load("map.v0.2.json")["hash"].as_str().unwrap().to_string()
    }

    /// A minimal signed-ish request (the responder only reads `from`/`hash`/`body`, not the sig).
    fn apply_request(from: &str, hash: &str, target: &str, args: J) -> J {
        json!({
            "schema_version": "0.2.0", "kind": "request",
            "from": from, "hash": hash, "to": null, "in_reply_to": null,
            "body": { "action": "apply", "target": target, "args": args }
        })
    }

    #[test]
    fn responds_to_apply_double() {
        // apply double to [21]  ->  assert claims eq(double(21), 42).
        let target = double_addr();
        let req = apply_request(
            "did:nova:11112222333344445555666677778888999900001111222233334444555566aa",
            "msg_0000000000000000000000000000000000000000000000000000000000000000",
            &target,
            json!([{ "kind": "nat", "value": 21 }]),
        );
        let map = build_link_map(&examples_dir()).unwrap();
        let key = signing_key_from_seed("test-responder");
        let assert = respond_to_request(&req, map, &key, None).unwrap();

        // It's a well-formed, signed assert threaded back to the requester.
        assert_eq!(assert["kind"], "assert");
        assert_eq!(assert["in_reply_to"], req["hash"]);
        assert_eq!(assert["to"], req["from"]);
        assert_eq!(assert["body"]["subject"], target.as_str());
        verify_signature(&assert).expect("assert signature must verify");

        // The claim is eq( double(21), <result> ) and the embedded result is 42.
        let claim = &assert["body"]["claim"];
        assert_eq!(claim["kind"], "predicate");
        let eq = &claim["expr"];
        assert_eq!(eq["op"], "eq");
        assert_eq!(eq["args"][0]["op"], target.as_str());
        assert_eq!(eq["args"][1]["value"], json!({ "kind": "int", "value": 42 }));
    }

    #[test]
    fn responds_to_apply_map_with_fn_ref_arg() {
        // apply map to (double, [1,2,3]) -> assert claims the result is [2,4,6]; the fn_ref arg
        // composes through the link map (principle 4).
        let target = map_addr();
        let args = json!([
            { "kind": "fn_ref", "target": double_addr() },
            { "kind": "list", "elems": [
                { "kind": "nat", "value": 1 }, { "kind": "nat", "value": 2 }, { "kind": "nat", "value": 3 }] }
        ]);
        let req = apply_request(
            "did:nova:11112222333344445555666677778888999900001111222233334444555566aa",
            "msg_1111111111111111111111111111111111111111111111111111111111111111",
            &target,
            args,
        );
        let map = build_link_map(&examples_dir()).unwrap();
        let key = signing_key_from_seed("test-responder");
        let assert = respond_to_request(&req, map, &key, None).unwrap();

        verify_signature(&assert).expect("assert signature must verify");
        let result = &assert["body"]["claim"]["expr"]["args"][1]["value"];
        assert_eq!(
            result,
            &json!({ "kind": "list", "elems": [
                { "kind": "int", "value": 2 }, { "kind": "int", "value": 4 }, { "kind": "int", "value": 6 }] })
        );
    }

    #[test]
    fn round_trip_respond_then_verify() {
        // The full loop on the committed example: respond to request.v0.2.json, then INDEPENDENTLY
        // re-run the resulting claim and confirm it holds (request -> compute -> assert -> verify).
        let req = load("request.v0.2.json");
        let key = signing_key_from_seed("novae-linguae-example-responder");
        let assert = respond_to_request(&req, build_link_map(&examples_dir()).unwrap(), &key, None).unwrap();

        assert!(
            verify_claim(&assert, build_link_map(&examples_dir()).unwrap()).unwrap(),
            "the asserted claim must re-run true"
        );

        // Tamper with the asserted result: the receiver must REFUTE it (no trust in the asserter).
        let mut bad = assert.clone();
        bad["body"]["claim"]["expr"]["args"][1]["value"]["elems"][0]["value"] = json!(999);
        assert!(
            !verify_claim(&bad, build_link_map(&examples_dir()).unwrap()).unwrap(),
            "a tampered result must be refuted"
        );
    }

    #[test]
    fn rejects_non_apply_and_unresolvable_target() {
        let key = signing_key_from_seed("test-responder");
        // Wrong speech act.
        let assertion = json!({ "kind": "assert", "from": "did:nova:x", "hash": "msg_x", "body": {} });
        assert!(respond_to_request(&assertion, HashMap::new(), &key, None).is_err());

        // apply to a target with no body in the (empty) link map.
        let req = apply_request("did:nova:x", "msg_y", "fn_deadbeef", json!([]));
        assert!(respond_to_request(&req, HashMap::new(), &key, None).is_err());
    }

    const REQUESTER: &str = "did:nova:11112222333344445555666677778888999900001111222233334444555566aa";
    const A_MSG: &str = "msg_2222222222222222222222222222222222222222222222222222222222222222";

    fn maps() -> (std::collections::HashMap<String, J>, std::collections::HashMap<String, J>) {
        (build_link_map(&examples_dir()).unwrap(), build_record_map(&examples_dir()).unwrap())
    }

    #[test]
    fn validate_request_asserts_verified() {
        // `validate` double: it typechecks and its examples run, so the reply asserts it `verified`.
        let req = json!({ "schema_version": "0.2.0", "kind": "request", "from": REQUESTER, "hash": A_MSG,
            "to": null, "in_reply_to": null, "body": { "action": "validate", "target": double_addr() } });
        let (link, recs) = maps();
        let key = signing_key_from_seed("novae-linguae-example-responder");
        let reply = respond_to_message(&req, link, recs, &key, None).unwrap();
        assert_eq!(reply["kind"], "assert");
        assert_eq!(reply["body"]["claim"]["kind"], "verified");
        assert_eq!(reply["body"]["claim"]["subject"], double_addr().as_str());
        assert_eq!(reply["in_reply_to"], A_MSG);
        verify_signature(&reply).expect("verified-assert signature must check");
    }

    #[test]
    fn validate_unknown_target_rejects() {
        let bad = "fn_0000000000000000000000000000000000000000000000000000000000000000";
        let req = json!({ "schema_version": "0.2.0", "kind": "request", "from": REQUESTER, "hash": A_MSG,
            "to": null, "in_reply_to": null, "body": { "action": "validate", "target": bad } });
        let (link, recs) = maps();
        let key = signing_key_from_seed("novae-linguae-example-responder");
        let reply = respond_to_message(&req, link, recs, &key, None).unwrap();
        assert_eq!(reply["kind"], "reject");
        assert_eq!(reply["body"]["code"], "unknown_target");
        verify_signature(&reply).unwrap();
    }

    // ---- operator effect grants (spec/agent-loop.md §Scope) ----

    fn greet_addr() -> String {
        load("greet.v0.2.json")["hash"].as_str().unwrap().to_string()
    }

    #[test]
    fn effectful_apply_refused_without_grants() {
        // Default = pure-only: applying greet (io.console) with no grants installed gets a signed,
        // policy-shaped refusal — a distinct `refused`/"effect not granted", not a generic eval error.
        crate::interp::set_effect_grants(Vec::<String>::new());
        let req = json!({ "schema_version": "0.2.0", "kind": "request", "from": REQUESTER, "hash": A_MSG,
            "to": null, "in_reply_to": null,
            "body": { "action": "apply", "target": greet_addr(), "args": [{ "kind": "string", "value": "hi" }] } });
        let (link, recs) = maps();
        let key = signing_key_from_seed("novae-linguae-example-responder");
        let reply = respond_to_message(&req, link, recs, &key, None).unwrap();
        assert_eq!(reply["kind"], "reject");
        assert_eq!(reply["body"]["code"], "refused");
        let reason = reply["body"]["reason"].as_str().unwrap();
        assert!(reason.contains("effect not granted") && reason.contains("io.console"), "{reason}");
        verify_signature(&reply).unwrap();
    }

    #[test]
    fn effectful_apply_runs_with_matching_grant() {
        // The operator granted io.console — greet fulfils and the reply is an ordinary assert.
        crate::interp::set_effect_grants(vec!["io.console".to_string()]);
        let req = json!({ "schema_version": "0.2.0", "kind": "request", "from": REQUESTER, "hash": A_MSG,
            "to": null, "in_reply_to": null,
            "body": { "action": "apply", "target": greet_addr(), "args": [{ "kind": "string", "value": "hi" }] } });
        let (link, recs) = maps();
        let key = signing_key_from_seed("novae-linguae-example-responder");
        let reply = respond_to_message(&req, link, recs, &key, None).unwrap();
        crate::interp::clear_effects();
        assert_eq!(reply["kind"], "assert", "{reply}");
        assert_eq!(reply["body"]["subject"], greet_addr().as_str());
        verify_signature(&reply).unwrap();
    }

    #[test]
    fn pure_apply_unaffected_by_grants() {
        // Grants present, pure target: nothing changes — double still asserts eq(double(21), 42).
        crate::interp::set_effect_grants(vec!["net.read".to_string()]);
        let req = json!({ "schema_version": "0.2.0", "kind": "request", "from": REQUESTER, "hash": A_MSG,
            "to": null, "in_reply_to": null,
            "body": { "action": "apply", "target": double_addr(), "args": [{ "kind": "nat", "value": 21 }] } });
        let (link, recs) = maps();
        let key = signing_key_from_seed("novae-linguae-example-responder");
        let reply = respond_to_message(&req, link, recs, &key, None).unwrap();
        crate::interp::clear_effects();
        assert_eq!(reply["kind"], "assert");
        assert_eq!(reply["body"]["claim"]["expr"]["args"][1]["value"], json!({ "kind": "int", "value": 42 }));
    }

    #[test]
    fn under_declared_effect_refused_even_when_granted() {
        // A body that performs io.console while its record declares NO effects: refused as
        // under-declared even though the effect is granted — grants are measured against the
        // *verified* declaration, never the record's word.
        crate::interp::set_effect_grants(vec!["io.console".to_string()]);
        let target = "fn_1111111111111111111111111111111111111111111111111111111111111111";
        let body = json!({ "kind": "lambda", "params": [{ "name": "msg" }],
            "body": { "kind": "app", "fn": { "kind": "var", "name": "print" },
                      "args": [{ "kind": "var", "name": "msg" }] } });
        let record = json!({ "hash": target, "signature": { "effects": [] } });
        let link: HashMap<String, J> = [(target.to_string(), body)].into();
        let recs: HashMap<String, J> = [(target.to_string(), record)].into();
        let req = json!({ "schema_version": "0.2.0", "kind": "request", "from": REQUESTER, "hash": A_MSG,
            "to": null, "in_reply_to": null,
            "body": { "action": "apply", "target": target, "args": [{ "kind": "string", "value": "hi" }] } });
        let key = signing_key_from_seed("novae-linguae-example-responder");
        let reply = respond_to_message(&req, link, recs, &key, None).unwrap();
        crate::interp::clear_effects();
        assert_eq!(reply["kind"], "reject");
        assert_eq!(reply["body"]["code"], "constraint_violated");
        assert!(reply["body"]["reason"].as_str().unwrap().contains("under-declared"), "{reply}");
    }

    #[test]
    fn effectful_apply_emits_observed_claim_with_trace() {
        // GW11: a fulfilment that performed effects is an OBSERVATION — the claim comes out
        // `observed`, conditioned on the recorded trace by trc_… content-address, and the trace
        // artifact itself is retrievable by the caller (it must travel with the assert).
        crate::interp::set_effect_grants(vec!["io.console".to_string()]);
        let req = json!({ "schema_version": "0.2.0", "kind": "request", "from": REQUESTER, "hash": A_MSG,
            "to": null, "in_reply_to": null,
            "body": { "action": "apply", "target": greet_addr(), "args": [{ "kind": "string", "value": "hi" }] } });
        let (link, recs) = maps();
        let key = signing_key_from_seed("novae-linguae-example-responder");
        let reply = respond_to_message(&req, link, recs, &key, None).unwrap();
        crate::interp::clear_effects();
        assert_eq!(reply["kind"], "assert", "{reply}");
        assert_eq!(reply["body"]["claim"]["kind"], "observed", "{reply}");
        let trace_addr = reply["body"]["claim"]["trace"].as_str().unwrap().to_string();
        assert!(trace_addr.starts_with("trc_"), "{trace_addr}");
        let trace = take_trace_artifact().expect("the observed assert left its trace artifact");
        assert_eq!(trace["kind"], "trace");
        assert_eq!(trace["ops"].as_array().unwrap().len(), 1, "{trace}");
        assert_eq!(trace["ops"][0]["effect"], "io.console");
        let recomputed =
            crate::hash_artifact_with_kind(&trace, crate::ArtifactKind::Trace).unwrap();
        assert_eq!(recomputed, trace_addr, "the claim references the trace's self-address");
        assert!(take_trace_artifact().is_none(), "the stash drains on take");
        verify_signature(&reply).unwrap();
    }

    #[test]
    fn observed_claim_verifies_by_replay_without_grants() {
        // The receiver's half: with the trace resolvable, verify_claim replays the computation —
        // NO grants, NO secrets — and confirms the result follows from the recorded observations.
        crate::interp::set_effect_grants(vec!["io.console".to_string()]);
        let req = json!({ "schema_version": "0.2.0", "kind": "request", "from": REQUESTER, "hash": A_MSG,
            "to": null, "in_reply_to": null,
            "body": { "action": "apply", "target": greet_addr(), "args": [{ "kind": "string", "value": "hi" }] } });
        let (link, recs) = maps();
        let key = signing_key_from_seed("novae-linguae-example-responder");
        let reply = respond_to_message(&req, link.clone(), recs, &key, None).unwrap();
        let trace = take_trace_artifact().unwrap();
        crate::interp::clear_effects(); // the verifier grants NOTHING

        let trace_addr = reply["body"]["claim"]["trace"].as_str().unwrap().to_string();
        let mut verifier_link = link.clone();
        verifier_link.insert(trace_addr.clone(), trace.clone());
        assert!(verify_claim(&reply, verifier_link).unwrap(), "replay-confirms grantlessly");

        // Tampered result → the replay refutes it (deterministically, still grantless).
        let mut bad = reply.clone();
        bad["body"]["claim"]["expr"]["args"][1]["value"] = json!({ "kind": "string", "value": "not what happened" });
        let mut verifier_link = link.clone();
        verifier_link.insert(trace_addr.clone(), trace);
        assert!(!verify_claim(&bad, verifier_link).unwrap(), "tampered observed claim refutes");

        // Without the trace, the observed claim cannot be replayed — an honest error naming it.
        let err = verify_claim(&reply, link).unwrap_err().to_string();
        assert!(err.contains(&trace_addr) && err.contains("cannot resolve"), "{err}");
    }

    #[test]
    fn observed_claim_must_consume_its_trace_exactly() {
        // A trace with observations the computation never used does NOT correspond to it — the
        // strict-consumption rule fails such a claim instead of quietly confirming a prefix.
        let trace = json!({ "kind": "trace", "version": "0.1.0",
            "ops": [{ "effect": "io.console", "detail": { "line": "stray" }, "result": { "kind": "unit" } }] });
        let trace_addr = crate::hash_artifact_with_kind(&trace, crate::ArtifactKind::Trace).unwrap();
        // A pure equation wrapped as `observed` over that unrelated trace.
        let assert = json!({ "kind": "assert", "body": { "subject": null, "claim": {
            "kind": "observed", "trace": trace_addr.clone(),
            "expr": { "kind": "app", "op": "eq", "args": [
                { "kind": "lit", "value": { "kind": "int", "value": 1 } },
                { "kind": "lit", "value": { "kind": "int", "value": 1 } } ] } } } });
        let link: HashMap<String, J> = [(trace_addr, trace)].into();
        let err = verify_claim(&assert, link).unwrap_err().to_string();
        assert!(err.contains("not fully consumed"), "{err}");
    }

    #[test]
    fn pure_apply_stays_a_predicate_claim_even_under_grants() {
        // Purity is observable: no effects performed → ordinary re-runnable `predicate` claim,
        // no trace artifact left behind.
        crate::interp::set_effect_grants(vec!["net.read".to_string()]);
        let req = json!({ "schema_version": "0.2.0", "kind": "request", "from": REQUESTER, "hash": A_MSG,
            "to": null, "in_reply_to": null,
            "body": { "action": "apply", "target": double_addr(), "args": [{ "kind": "nat", "value": 21 }] } });
        let (link, recs) = maps();
        let key = signing_key_from_seed("novae-linguae-example-responder");
        let reply = respond_to_message(&req, link, recs, &key, None).unwrap();
        crate::interp::clear_effects();
        assert_eq!(reply["body"]["claim"]["kind"], "predicate", "{reply}");
        assert!(reply["body"]["claim"].get("trace").is_none());
        assert!(take_trace_artifact().is_none());
    }

    #[test]
    fn effectful_commit_gated_like_apply() {
        // Acting on an apply commitment is an execution path too — the same gate covers it.
        crate::interp::set_effect_grants(Vec::<String>::new());
        let committer = "did:nova:aaaa2222333344445555666677778888999900001111222233334444555566bb";
        let commit = json!({ "schema_version": "0.2.0", "kind": "commit", "from": committer, "hash": A_MSG,
            "to": REQUESTER, "in_reply_to": null,
            "body": { "commitment": { "kind": "apply", "fn": greet_addr(),
                                       "args": [{ "kind": "string", "value": "hi" }] } } });
        let (link, recs) = maps();
        let key = signing_key_from_seed("novae-linguae-example-responder");
        let reply = respond_to_message(&commit, link, recs, &key, None).unwrap();
        assert_eq!(reply["kind"], "reject");
        assert_eq!(reply["body"]["code"], "refused");
        assert_eq!(reply["to"], REQUESTER, "commit refusal threads to the commit's `to`");
    }

    #[test]
    fn query_acks_matching_records() {
        // Query for io.console effects: greet is the only such record.
        let q = json!({ "schema_version": "0.2.0", "kind": "query", "from": REQUESTER, "hash": A_MSG,
            "to": null, "in_reply_to": null, "body": { "pattern": { "effects": ["io.console"] } } });
        let (link, recs) = maps();
        let key = signing_key_from_seed("novae-linguae-example-responder");
        let reply = respond_to_message(&q, link, recs, &key, None).unwrap();
        assert_eq!(reply["kind"], "ack");
        assert_eq!(reply["body"]["acks"], A_MSG);
        let greet = load("greet.v0.2.json")["hash"].as_str().unwrap().to_string();
        let matches = reply["body"]["result"]["matches"].as_array().unwrap();
        assert!(matches.iter().any(|m| m == &json!(greet)), "greet should match the io.console query");
        verify_signature(&reply).unwrap();
    }

    #[test]
    fn propose_apply_commits() {
        // Propose applying double to [21]: the responder test-runs it, then commits.
        let prop = json!({ "schema_version": "0.2.0", "kind": "propose", "from": REQUESTER, "hash": A_MSG,
            "to": null, "in_reply_to": null,
            "body": { "action": "apply", "target": double_addr(), "args": [{ "kind": "nat", "value": 21 }] } });
        let (link, recs) = maps();
        let key = signing_key_from_seed("novae-linguae-example-responder");
        let reply = respond_to_message(&prop, link, recs, &key, None).unwrap();
        assert_eq!(reply["kind"], "commit");
        assert_eq!(reply["body"]["commitment"]["kind"], "apply");
        assert_eq!(reply["body"]["commitment"]["fn"], double_addr().as_str());
        assert_eq!(reply["in_reply_to"], A_MSG);
        verify_signature(&reply).expect("commit signature must verify");
    }

    #[test]
    fn propose_unknown_target_rejects() {
        let bad = "fn_0000000000000000000000000000000000000000000000000000000000000000";
        let prop = json!({ "schema_version": "0.2.0", "kind": "propose", "from": REQUESTER, "hash": A_MSG,
            "to": null, "in_reply_to": null, "body": { "action": "apply", "target": bad, "args": [] } });
        let (link, recs) = maps();
        let key = signing_key_from_seed("novae-linguae-example-responder");
        let reply = respond_to_message(&prop, link, recs, &key, None).unwrap();
        assert_eq!(reply["kind"], "reject");
        assert_eq!(reply["body"]["code"], "unknown_target");
    }

    #[test]
    fn acting_on_an_apply_commit_asserts_the_result() {
        // A commit to apply double(21): the responder fulfils it and asserts eq(double(21), 42).
        let commit = json!({ "schema_version": "0.2.0", "kind": "commit", "from": REQUESTER, "hash": A_MSG,
            "to": null, "in_reply_to": null, "body": {
                "commitment": { "kind": "apply", "fn": double_addr(), "args": [{ "kind": "nat", "value": 21 }] },
                "conditions": [], "expires_at": null } });
        let (link, recs) = maps();
        let key = signing_key_from_seed("novae-linguae-example-responder");
        let reply = respond_to_message(&commit, link, recs, &key, None).unwrap();
        assert_eq!(reply["kind"], "assert");
        assert_eq!(reply["body"]["claim"]["expr"]["args"][1]["value"], json!({ "kind": "int", "value": 42 }));
        verify_signature(&reply).unwrap();
    }

    #[test]
    fn store_verifies_payload_and_acks() {
        // Storing a self-consistent record acks with its address.
        let req = json!({ "schema_version": "0.2.0", "kind": "request", "from": REQUESTER, "hash": A_MSG,
            "to": null, "in_reply_to": null,
            "body": { "action": "store", "payload_kind": "function-record-v0.2", "payload": load("double.v0.2.json") } });
        let (link, recs) = maps();
        let key = signing_key_from_seed("novae-linguae-example-responder");
        let reply = respond_to_message(&req, link, recs, &key, None).unwrap();
        assert_eq!(reply["kind"], "ack");
        assert_eq!(reply["body"]["result"]["stored"], double_addr().as_str());
        verify_signature(&reply).unwrap();
    }

    #[test]
    fn delegate_and_retract_are_acked() {
        let key = signing_key_from_seed("novae-linguae-example-responder");
        let del = json!({ "schema_version": "0.2.0", "kind": "delegate", "from": REQUESTER, "hash": A_MSG,
            "to": null, "in_reply_to": null, "body": { "capability": "cap:apply/double" } });
        let (l1, r1) = maps();
        assert_eq!(respond_to_message(&del, l1, r1, &key, None).unwrap()["kind"], "ack");
        let ret = json!({ "schema_version": "0.2.0", "kind": "retract", "from": REQUESTER, "hash": A_MSG,
            "to": null, "in_reply_to": null, "body": { "retracts": A_MSG, "reason": "superseded" } });
        let (l2, r2) = maps();
        assert_eq!(respond_to_message(&ret, l2, r2, &key, None).unwrap()["kind"], "ack");
    }

    #[test]
    fn apply_gated_on_required_capability() {
        // A target whose record requires cap:io/write is rejected unless the request presents it.
        let key = signing_key_from_seed("novae-linguae-example-responder");
        let gated_record = || {
            let mut m = std::collections::HashMap::new();
            m.insert(double_addr(), json!({ "hash": double_addr(), "signature": { "capabilities": ["cap:io/write"] } }));
            m
        };
        let apply_req = |caps: serde_json::Value| json!({
            "schema_version": "0.2.0", "kind": "request", "from": REQUESTER, "hash": A_MSG, "to": null,
            "in_reply_to": null, "constraints": { "capabilities": caps },
            "body": { "action": "apply", "target": double_addr(), "args": [{ "kind": "nat", "value": 21 }] } });

        // Missing the capability → not_authorized.
        let r = respond_to_message(&apply_req(json!([])), build_link_map(&examples_dir()).unwrap(), gated_record(), &key, None).unwrap();
        assert_eq!(r["kind"], "reject");
        assert_eq!(r["body"]["code"], "not_authorized");
        verify_signature(&r).unwrap();

        // Presenting it → fulfilled.
        let r2 = respond_to_message(&apply_req(json!(["cap:io/write"])), build_link_map(&examples_dir()).unwrap(), gated_record(), &key, None).unwrap();
        assert_eq!(r2["kind"], "assert");
    }

    #[test]
    fn apply_gate_requires_signed_delegation_chain_under_a_trust_policy() {
        // With a trust policy configured, merely *listing* the capability no longer suffices — the
        // sender (REQUESTER) must hold a signed `delegate` chain back to a recognized root.
        use crate::{did_nova_from_pubkey, TrustPolicy};
        let key = signing_key_from_seed("novae-linguae-example-responder");
        let gated_record = || {
            let mut m = std::collections::HashMap::new();
            m.insert(double_addr(), json!({ "hash": double_addr(), "signature": { "capabilities": ["cap:io/write"] } }));
            m
        };
        let apply_req = json!({
            "schema_version": "0.2.0", "kind": "request", "from": REQUESTER, "hash": A_MSG, "to": null,
            "in_reply_to": null, "constraints": { "capabilities": ["cap:io/write"] },
            "body": { "action": "apply", "target": double_addr(), "args": [{ "kind": "nat", "value": 21 }] } });

        // A recognized root that grants cap:io/write directly to REQUESTER.
        let root_key = signing_key_from_seed("gate-trust-root");
        let root_did = did_nova_from_pubkey(&root_key.verifying_key());
        let mut grant = json!({ "schema_version": "0.2.0", "kind": "delegate", "to": REQUESTER,
            "in_reply_to": null, "body": { "capability": "cap:io/write", "expires_at": null, "conditions": [] } });
        sign_message(&mut grant, &root_key).unwrap();

        // Policy recognizing the root, holding the grant → the listed capability is now backed → fulfilled.
        let backed = TrustPolicy {
            roots: std::collections::BTreeSet::from([root_did]),
            delegations: vec![grant],
            at: None,
        };
        let ok = respond_to_message_with_trust(
            &apply_req, build_link_map(&examples_dir()).unwrap(), gated_record(), &key, None, &backed).unwrap();
        assert_eq!(ok["kind"], "assert", "a backed capability under a trust policy is authorized");

        // Same request, same listed capability, but a policy with a root and NO matching delegation →
        // listing the string is not enough → rejected.
        let empty_root = TrustPolicy {
            roots: std::collections::BTreeSet::from([did_nova_from_pubkey(&signing_key_from_seed("other-root").verifying_key())]),
            delegations: vec![],
            at: None,
        };
        let denied = respond_to_message_with_trust(
            &apply_req, build_link_map(&examples_dir()).unwrap(), gated_record(), &key, None, &empty_root).unwrap();
        assert_eq!(denied["kind"], "reject");
        assert_eq!(denied["body"]["code"], "not_authorized");
        verify_signature(&denied).unwrap();
    }
}
