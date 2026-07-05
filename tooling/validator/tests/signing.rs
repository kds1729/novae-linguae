//! Ed25519 signing/verification, deterministic key derivation, DID and
//! signature (de)serialization round-trips, and tamper detection.

mod common;

use common::example;
use nl_validator::{
    did_nova_from_pubkey, format_signature, hash_artifact, parse_signature, pubkey_from_did_nova,
    sign_artifact, sign_message, signing_key_from_seed, verify_artifact_hash, verify_signature,
    ArtifactKind,
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

// ---- signed certification records (`certify --sign`) ----

fn unsigned_certification() -> serde_json::Value {
    json!({
        "schema_version": "0.2.0",
        "kind": "certification",
        "subject": "fn_deadbeef",
        "body_hash": "expr_cafebabe",
        "checks": [{ "check": "typecheck", "verdict": "WELL-TYPED", "detail": "int -> int" }],
        "certified": true,
    })
}

#[test]
fn signed_certification_hash_and_signature_verify() {
    let key = signing_key_from_seed("novae-linguae-example-certifier");
    let mut cert = unsigned_certification();
    sign_artifact(&mut cert, &key, ArtifactKind::Certification).unwrap();

    // `certify --sign` produces a certification-kind artifact (its hash carries the `cert_` prefix).
    assert_eq!(ArtifactKind::detect(&cert).unwrap(), ArtifactKind::Certification);
    assert!(cert["hash"].as_str().unwrap().starts_with("cert_"));
    // Both the content-hash and the Ed25519 signature verify.
    assert!(verify_artifact_hash(&cert).unwrap().matches);
    assert!(verify_signature(&cert).is_ok());
    // The `from` DID matches the signing identity.
    assert_eq!(cert["from"], json!(did_nova_from_pubkey(&key.verifying_key())));
    // Signing is deterministic (byte-reproducible with no timestamp).
    let mut again = unsigned_certification();
    sign_artifact(&mut again, &key, ArtifactKind::Certification).unwrap();
    assert_eq!(again, cert);
}

#[test]
fn tampering_with_a_certification_fails_verification() {
    let key = signing_key_from_seed("novae-linguae-example-certifier");
    let mut cert = unsigned_certification();
    sign_artifact(&mut cert, &key, ArtifactKind::Certification).unwrap();
    // Flip the verdict a certifier signed — signature must no longer verify.
    let mut tampered = cert.clone();
    tampered["certified"] = json!(false);
    assert!(verify_signature(&tampered).is_err(), "a mutated certification must fail signature verification");
    // And its recorded `hash` no longer matches the content.
    assert!(!verify_artifact_hash(&tampered).unwrap().matches);
}

#[test]
fn certification_hash_uses_the_cert_prefix() {
    // The auto-detected hash of an (unsigned) certification is `cert_`-prefixed, distinct from fn_/msg_.
    let h = hash_artifact(&unsigned_certification()).unwrap();
    assert!(h.starts_with("cert_"), "got {h}");
}

// ---- weights pointer records + signed eval attestations (spec/weights.md) ----

fn weights_record() -> serde_json::Value {
    json!({
        "schema_version": "0.1.0",
        "kind": "weights",
        "base": { "model": "Qwen/Qwen2.5-Coder-7B-Instruct", "license": "Apache-2.0" },
        "format": "lora-peft-safetensors",
        "files": [{ "name": "adapter_model.safetensors", "sha256": "f".repeat(64), "bytes": 161061273 }],
        "recipe": {
            "corpus": { "sha256": "a".repeat(64) },
            "train_split": { "sha256": "b".repeat(64), "examples": 5787 },
            "trainer": "tooling/eval/train_lora_cpu.py",
            "seed": 1, "epochs": 2
        },
    })
}

#[test]
fn weights_record_detects_and_hashes_with_the_wgt_prefix() {
    // A weights record is unsigned and hashed like a function record (strip `hash` only).
    let mut w = weights_record();
    assert_eq!(ArtifactKind::detect(&w).unwrap(), ArtifactKind::Weights);
    let h = hash_artifact(&w).unwrap();
    assert!(h.starts_with("wgt_"), "got {h}");
    w["hash"] = json!(h);
    assert!(verify_artifact_hash(&w).unwrap().matches);
    // Tampering with the blob manifest breaks the address.
    w["files"][0]["sha256"] = json!("0".repeat(64));
    assert!(!verify_artifact_hash(&w).unwrap().matches);
}

#[test]
fn signed_eval_attestation_hash_and_signature_verify() {
    let key = signing_key_from_seed("novae-linguae-example-certifier");
    let mut att = json!({
        "schema_version": "0.1.0",
        "kind": "eval-attestation",
        "subject": format!("wgt_{}", "e".repeat(64)),
        "eval": { "harness": "tooling/eval/eval_harness.py",
                  "settings": { "conventions": "off", "shots": 0 },
                  "task_set": { "tasks": 360 } },
        "results": { "write": { "pass": 167, "total": 179 } },
    });
    sign_artifact(&mut att, &key, ArtifactKind::EvalAttestation).unwrap();
    assert_eq!(ArtifactKind::detect(&att).unwrap(), ArtifactKind::EvalAttestation);
    assert!(att["hash"].as_str().unwrap().starts_with("evl_"));
    assert!(verify_artifact_hash(&att).unwrap().matches);
    assert!(verify_signature(&att).is_ok());
    // Inflating the signed score fails signature verification — the accountability property.
    let mut tampered = att.clone();
    tampered["results"]["write"]["pass"] = json!(179);
    assert!(verify_signature(&tampered).is_err(), "a mutated eval attestation must fail signature verification");
}
