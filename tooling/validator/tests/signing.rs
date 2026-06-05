//! Ed25519 signing/verification, deterministic key derivation, DID and
//! signature (de)serialization round-trips, and tamper detection.

mod common;

use common::example;
use nl_validator::{
    did_nova_from_pubkey, format_signature, parse_signature, pubkey_from_did_nova, sign_message,
    signing_key_from_seed, verify_artifact_hash, verify_signature,
};
use serde_json::json;

// Deterministic seeds documented in spec/README.md for the example messages.
const REQUEST_SEED: &str = "novae-linguae-example-claude";
const ASSERT_SEED: &str = "novae-linguae-example-verifier";

#[test]
fn signing_is_deterministic_and_reproduces_examples() {
    // Re-signing the on-disk message with its documented seed must reproduce
    // the committed `from`, `hash`, and `signature` byte-for-byte.
    for (name, seed) in [("request.json", REQUEST_SEED), ("assert.json", ASSERT_SEED)] {
        let original = example(name);
        let mut resigned = original.clone();
        let key = signing_key_from_seed(seed);
        sign_message(&mut resigned, &key).unwrap();
        assert_eq!(resigned, original, "re-signing {name} did not reproduce it");
    }
}

#[test]
fn seed_derives_the_messages_from_did() {
    let key = signing_key_from_seed(REQUEST_SEED);
    let did = did_nova_from_pubkey(&key.verifying_key());
    assert_eq!(did, example("request.json")["from"].as_str().unwrap());
}

#[test]
fn key_derivation_is_stable_across_calls() {
    let a = signing_key_from_seed("some-seed");
    let b = signing_key_from_seed("some-seed");
    assert_eq!(a.verifying_key().as_bytes(), b.verifying_key().as_bytes());
}

#[test]
fn example_signatures_verify() {
    assert!(verify_signature(&example("request.json")).is_ok());
    assert!(verify_signature(&example("assert.json")).is_ok());
}

#[test]
fn did_round_trips() {
    let did = example("request.json")["from"].as_str().unwrap().to_string();
    let pk = pubkey_from_did_nova(&did).unwrap();
    assert_eq!(did_nova_from_pubkey(&pk), did);
}

#[test]
fn signature_round_trips() {
    let sig_str = example("request.json")["signature"]
        .as_str()
        .unwrap()
        .to_string();
    let sig = parse_signature(&sig_str).unwrap();
    assert_eq!(format_signature(&sig), sig_str);
}

#[test]
fn pubkey_from_did_rejects_malformed_input() {
    assert!(pubkey_from_did_nova("did:key:abcd").is_err()); // wrong method
    assert!(pubkey_from_did_nova("did:nova:dead").is_err()); // too short
    assert!(pubkey_from_did_nova(&format!("did:nova:{}", "zz".repeat(32))).is_err()); // non-hex
}

#[test]
fn parse_signature_rejects_malformed_input() {
    assert!(parse_signature("rsa:abcd").is_err()); // wrong algorithm prefix
    assert!(parse_signature("ed25519:!!notbase64!!").is_err()); // bad base64
    assert!(parse_signature("ed25519:YWJj").is_err()); // valid base64, wrong length
}

#[test]
fn tampering_with_signed_body_fails_verification() {
    let mut m = example("request.json");
    m["body"]["action"] = json!("delete");
    assert!(
        verify_signature(&m).is_err(),
        "a mutated message body must fail signature verification"
    );
}

#[test]
fn tampering_with_a_record_fails_hash_check() {
    let mut v = example("map.json");
    v["name_hints"] = json!(["tampered"]);
    assert!(!verify_artifact_hash(&v).unwrap().matches);
}
