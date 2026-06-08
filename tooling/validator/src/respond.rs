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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{build_link_map, signing_key_from_seed, verify_signature};
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
}
