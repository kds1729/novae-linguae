//! Autonomous multi-step orchestration — the agent loop driven end to end.
//!
//! Given a goal (apply a commons function *found by intent* to some arguments), the orchestrator runs
//! a full Nova Locutio conversation against a responder: **QUERY** the commons to discover a function
//! → **PROPOSE** applying it → receive the responder's **COMMIT** → the committer fulfils the
//! commitment and **ASSERT**s the result → the orchestrator **VERIFIES** the claim by re-running it.
//! Every message is signed and threaded; the outcome is self-verifying (principles 1, 3, 4, 6, 7).
//! This is "assemble, don't write" (principle 4) made autonomous — the agent never names the
//! function, it discovers one.

use anyhow::{anyhow, Result};
use ed25519_dalek::SigningKey;
use serde_json::{json, Value as J};
use std::path::Path;

use crate::{
    build_link_map, build_record_map, did_nova_from_pubkey, respond_to_message, sign_message,
    verify_claim,
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

/// Drive a `query → propose → commit → assert → verify` conversation. `intent_tags` selects the target
/// by `intent_tags` containment; `args` are the value-expression arguments to apply. `orchestrator`
/// signs the outbound query/propose; `responder` signs the replies. `timestamp` is advisory (None →
/// deterministic per seed).
pub fn orchestrate(
    records_dir: &Path,
    intent_tags: &[String],
    args: Vec<J>,
    orchestrator: &SigningKey,
    responder: &SigningKey,
    timestamp: Option<&str>,
) -> Result<Run> {
    let link = build_link_map(records_dir)?;
    let records = build_record_map(records_dir)?;
    let responder_did = did_nova_from_pubkey(&responder.verifying_key());
    let mut steps = Vec::new();

    // 1. QUERY the commons for a function matching the intent.
    let mut query = json!({
        "schema_version": "0.2.0", "kind": "query", "to": responder_did,
        "in_reply_to": null, "timestamp": timestamp, "constraints": null,
        "body": { "pattern": { "intent_tags": intent_tags } }
    });
    sign_message(&mut query, orchestrator)?;
    steps.push(Step { label: "query".into(), message: query.clone() });

    let ack = respond_to_message(&query, link.clone(), records.clone(), responder, timestamp)?;
    let target = ack
        .pointer("/body/result/matches/0")
        .and_then(|m| m.as_str())
        .ok_or_else(|| anyhow!("no commons function matched intent {intent_tags:?}"))?
        .to_string();
    steps.push(Step { label: "ack".into(), message: ack });

    // 2. PROPOSE applying the discovered function to the args.
    let mut propose = json!({
        "schema_version": "0.2.0", "kind": "propose", "to": responder_did,
        "in_reply_to": null, "timestamp": timestamp, "constraints": null,
        "body": { "action": "apply", "target": target, "args": args }
    });
    sign_message(&mut propose, orchestrator)?;
    steps.push(Step { label: "propose".into(), message: propose.clone() });

    // 3. The responder COMMITs (or rejects).
    let commit = respond_to_message(&propose, link.clone(), records.clone(), responder, timestamp)?;
    let committed = commit.get("kind").and_then(|k| k.as_str()) == Some("commit");
    let label = commit.get("kind").and_then(|k| k.as_str()).unwrap_or("?").to_string();
    steps.push(Step { label, message: commit.clone() });
    if !committed {
        return Ok(Run { steps, confirmed: false });
    }

    // 4. The committer fulfils the commitment → ASSERT the result.
    let assert = respond_to_message(&commit, link.clone(), records.clone(), responder, timestamp)?;
    steps.push(Step { label: "assert".into(), message: assert.clone() });

    // 5. The orchestrator VERIFIES the claim by re-running it against the commons.
    let confirmed = verify_claim(&assert, link).unwrap_or(false);
    Ok(Run { steps, confirmed })
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
}
