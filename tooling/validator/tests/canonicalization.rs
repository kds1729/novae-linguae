//! JCS (RFC 8785) canonicalization behavior. These pin the bytes that feed the
//! hash, so they are part of the cross-implementation contract.

mod common;

use common::example;
use nl_validator::canonicalize;
use serde_json::json;

fn canon_str(v: &serde_json::Value) -> String {
    String::from_utf8(canonicalize(v).unwrap()).unwrap()
}

#[test]
fn object_keys_are_sorted_lexicographically() {
    let v = json!({ "b": 1, "a": 2, "c": 3 });
    assert_eq!(canon_str(&v), r#"{"a":2,"b":1,"c":3}"#);
}

#[test]
fn nested_object_keys_are_sorted() {
    let v = json!({ "z": { "y": 1, "x": 2 }, "a": 3 });
    assert_eq!(canon_str(&v), r#"{"a":3,"z":{"x":2,"y":1}}"#);
}

#[test]
fn array_order_is_preserved() {
    let v = json!([3, 1, 2]);
    assert_eq!(canon_str(&v), "[3,1,2]");
}

#[test]
fn no_insignificant_whitespace() {
    let v = json!({ "a": [1, 2], "b": { "c": 3 } });
    let s = canon_str(&v);
    assert!(!s.contains(' '), "canonical form contains a space: {s}");
    assert!(!s.contains('\n'), "canonical form contains a newline: {s}");
}

#[test]
fn no_trailing_newline() {
    let bytes = canonicalize(&json!({ "a": 1 })).unwrap();
    assert_ne!(bytes.last(), Some(&b'\n'));
}

#[test]
fn canonicalization_is_idempotent() {
    // Canonicalizing the canonical-bytes-reparsed value yields the same bytes.
    let v = example("map.json");
    let once = canonicalize(&v).unwrap();
    let reparsed: serde_json::Value = serde_json::from_slice(&once).unwrap();
    let twice = canonicalize(&reparsed).unwrap();
    assert_eq!(once, twice);
}

#[test]
fn key_order_in_source_does_not_matter() {
    // Two source objects differing only in key order canonicalize identically.
    let a = json!({ "from": "x", "kind": "request", "body": { "p": 1, "q": 2 } });
    let b = json!({ "body": { "q": 2, "p": 1 }, "kind": "request", "from": "x" });
    assert_eq!(canonicalize(&a).unwrap(), canonicalize(&b).unwrap());
}
