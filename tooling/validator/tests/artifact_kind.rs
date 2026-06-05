//! Artifact-kind auto-detection and the field-stripping rules that hang off it.

mod common;

use common::example;
use nl_validator::{strip_for_hash, ArtifactKind};
use serde_json::json;

#[test]
fn detects_function_record() {
    assert_eq!(
        ArtifactKind::detect(&example("map.json")).unwrap(),
        ArtifactKind::FunctionRecord
    );
    assert_eq!(
        ArtifactKind::detect(&example("double.v0.2.json")).unwrap(),
        ArtifactKind::FunctionRecord
    );
}

#[test]
fn detects_messages() {
    assert_eq!(
        ArtifactKind::detect(&example("request.json")).unwrap(),
        ArtifactKind::Message
    );
    assert_eq!(
        ArtifactKind::detect(&example("assert.json")).unwrap(),
        ArtifactKind::Message
    );
}

#[test]
fn detects_body_expression() {
    assert_eq!(
        ArtifactKind::detect(&example("body-double.json")).unwrap(),
        ArtifactKind::BodyExpression
    );
}

#[test]
fn every_speech_act_detects_as_message() {
    for act in [
        "request", "assert", "query", "propose", "commit", "retract", "delegate", "ack", "reject",
    ] {
        let v = json!({ "kind": act });
        assert_eq!(
            ArtifactKind::detect(&v).unwrap(),
            ArtifactKind::Message,
            "speech act `{act}` should detect as a message"
        );
    }
}

#[test]
fn unknown_kind_is_rejected() {
    let v = json!({ "kind": "frobnicate" });
    assert!(ArtifactKind::detect(&v).is_err());
}

#[test]
fn non_object_is_rejected() {
    assert!(ArtifactKind::detect(&json!([1, 2, 3])).is_err());
    assert!(ArtifactKind::detect(&json!("scalar")).is_err());
}

#[test]
fn shape_without_kind_or_body_hash_is_rejected() {
    // Has `signature` (like a message) but no `kind` and no `body_hash`, so it
    // is neither a detectable message nor a function record.
    let v = json!({ "signature": "ed25519:..." });
    assert!(ArtifactKind::detect(&v).is_err());
}

#[test]
fn prefixes_are_correct() {
    assert_eq!(ArtifactKind::FunctionRecord.prefix(), "fn");
    assert_eq!(ArtifactKind::Message.prefix(), "msg");
    assert_eq!(ArtifactKind::BodyExpression.prefix(), "expr");
}

#[test]
fn function_record_strips_only_hash() {
    let stripped = strip_for_hash(&example("map.json"), ArtifactKind::FunctionRecord);
    assert!(stripped.get("hash").is_none());
    // A record's `signature` is its *type* signature, not a crypto signature —
    // it is part of the hashed content and must NOT be stripped.
    assert!(stripped.get("signature").is_some());
    assert!(stripped.get("body_hash").is_some()); // not stripped
}

#[test]
fn message_strips_hash_and_signature() {
    let stripped = strip_for_hash(&example("request.json"), ArtifactKind::Message);
    assert!(stripped.get("hash").is_none());
    assert!(stripped.get("signature").is_none());
    assert!(stripped.get("from").is_some()); // not stripped
}

#[test]
fn body_expression_strips_nothing() {
    let original = example("body-double.json");
    let stripped = strip_for_hash(&original, ArtifactKind::BodyExpression);
    assert_eq!(stripped, original);
}
