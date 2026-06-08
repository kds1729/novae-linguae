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
use std::collections::HashMap;

use crate::{clear_resolver, eval_body, set_resolver, sign_message};

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

    // 2. Resolve the target's body from the commons.
    let target_body = link_map
        .get(target)
        .cloned()
        .ok_or_else(|| anyhow!("cannot resolve target `{target}`: no body for it in the provided records"))?;

    // 3. Execute: install the link map so the target body and its `fn_ref` args compose, run, clear.
    set_resolver(link_map);
    let computed = eval_body(&target_body, &args);
    clear_resolver();
    let result = computed.with_context(|| format!("running target `{target}` on the request arguments"))?;

    // 4. Build the predicate claim: eq( target(arg0, arg1, …), result ). Each request arg is a
    //    value-expression carried as a predicate `lit`; the target is an `app` op by content-address.
    let app_args: Vec<J> = args.iter().map(|a| json!({ "kind": "lit", "value": a })).collect();
    let claim_expr = json!({
        "kind": "app",
        "op": "eq",
        "args": [
            { "kind": "app", "op": target, "args": app_args },
            { "kind": "lit", "value": result }
        ]
    });

    // 5. Assemble and sign the `assert`. `sign_message` fills `from`/`hash`/`signature`.
    let mut assert = json!({
        "schema_version": "0.2.0",
        "kind": "assert",
        "to": requester,
        "in_reply_to": req_hash,
        "timestamp": timestamp,
        "constraints": null,
        "body": {
            "subject": target,
            "claim": { "kind": "predicate", "expr": claim_expr },
            "evidence": null
        }
    });
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
    let kind = message.get("kind").and_then(|k| k.as_str()).unwrap_or_default();
    match kind {
        "request" => {
            let action = message.pointer("/body/action").and_then(|a| a.as_str()).unwrap_or_default();
            match action {
                "apply" => respond_to_request(message, link_map, signing_key, timestamp),
                "validate" => validate_reply(message, link_map, record_map, signing_key, timestamp),
                other => bail!("respond handles the `apply` and `validate` request actions, not `{other}`"),
            }
        }
        "query" => query_reply(message, record_map, signing_key, timestamp),
        other => bail!("respond handles `request` and `query` speech acts, not `{other}`"),
    }
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
}
