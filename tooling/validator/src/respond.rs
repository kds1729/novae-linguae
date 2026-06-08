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
//! Scope (v0.1 evaluator): the target must be a pure body the evaluator handles; effects are not
//! modelled. A request whose args don't decode, or whose target has no resolvable body, is an
//! honest error rather than a silent empty assert.

use anyhow::{anyhow, bail, Context, Result};
use ed25519_dalek::SigningKey;
use serde_json::{json, Value as J};
use std::collections::{BTreeSet, HashMap};

use crate::{clear_resolver, eval_body, set_resolver, sign_message, verify_delegation_chain};

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
    let computed = eval_body(&target_body, args);
    clear_resolver();
    let result = computed.with_context(|| format!("running target `{target}` on the arguments"))?;

    // The predicate claim: eq( target(arg0, …), result ). Each arg is a value-expression `lit`; the
    // target is an `app` op by content-address.
    let app_args: Vec<J> = args.iter().map(|a| json!({ "kind": "lit", "value": a })).collect();
    let claim_expr = json!({
        "kind": "app", "op": "eq", "args": [
            { "kind": "app", "op": target, "args": app_args },
            { "kind": "lit", "value": result }
        ]
    });
    let mut assert = build_envelope("assert", requester, in_reply_to, timestamp,
        json!({ "subject": target, "claim": { "kind": "predicate", "expr": claim_expr }, "evidence": null }));
    sign_message(&mut assert, signing_key).context("signing the assert reply")?;
    Ok(assert)
}

/// Verify an `assert`'s `predicate` claim by RE-RUNNING it. Installs `link_map` so the claim's
/// content-addressed functions resolve, evaluates the predicate, and returns whether it holds.
///
/// This is the *verifier half* of the agent loop: the receiver confirms the asserted computation by
/// re-executing it rather than trusting the asserter (principle 3 — verification is re-execution;
/// principle 7 — no privileged party). `Ok(true)` = the claim re-ran true. Errors if the claim is
/// not a runnable `predicate` claim, or the predicate is undecidable / non-boolean.
pub fn verify_claim(assert: &J, link_map: HashMap<String, J>) -> Result<bool> {
    let expr = assert
        .pointer("/body/claim/expr")
        .ok_or_else(|| anyhow!("assert has no `body.claim.expr` to re-run (not a `predicate` claim?)"))?
        .clone();
    set_resolver(link_map);
    let verdict = crate::interp::eval_claim(&expr);
    clear_resolver();
    match verdict {
        Some(crate::interp::Val::Bool(b)) => Ok(b),
        Some(other) => bail!(
            "claim predicate did not evaluate to a boolean: {}",
            crate::interp::encode_value(&other)
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
    match kind {
        "request" => {
            let action = message.pointer("/body/action").and_then(|a| a.as_str()).unwrap_or_default();
            match action {
                "apply" => match capability_gate(message, &record_map, policy, signing_key, timestamp) {
                    Some(reject) => reject,
                    None => respond_to_request(message, link_map, signing_key, timestamp),
                },
                "validate" => validate_reply(message, link_map, record_map, signing_key, timestamp),
                "store" => store_reply(message, signing_key, timestamp),
                other => bail!("respond handles the `apply`/`validate`/`store` request actions, not `{other}`"),
            }
        }
        "query" => query_reply(message, record_map, signing_key, timestamp),
        "propose" => match capability_gate(message, &record_map, policy, signing_key, timestamp) {
            Some(reject) => reject,
            None => propose_reply(message, link_map, signing_key, timestamp),
        },
        "commit" => commit_reply(message, link_map, signing_key, timestamp),
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
