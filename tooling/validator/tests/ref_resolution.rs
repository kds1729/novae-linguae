//! Cross-file `$ref` resolution: the `LocalSchemaRetriever` maps logical
//! Novae Linguae schema identifiers to sibling files in `spec/`, so a schema
//! can reference another schema's full shape. The message schema uses this for
//! conditional `store` payload validation.
//!
//! The negative cases are the important ones: they prove the referenced schema
//! is actually *applied*, not silently skipped.

mod common;

use common::{example, schema, spec_dir};
use nl_validator::validate_with_refs;
use serde_json::{json, Value};

fn validate_msg(instance: &Value) -> anyhow::Result<()> {
    validate_with_refs(&schema("message.schema.json"), instance, &spec_dir())
}

#[test]
fn store_request_validates_via_cross_file_payload_ref() {
    // payload_kind = "function-record"; payload must satisfy the (sibling)
    // function-record schema, which it does.
    assert!(validate_msg(&example("store-request.json")).is_ok());
}

#[test]
fn invalid_payload_is_rejected_via_cross_file_ref() {
    // Drop a field the function-record schema requires. If the cross-file ref
    // were not applied, this would wrongly pass.
    let mut v = example("store-request.json");
    v["body"]["payload"]
        .as_object_mut()
        .unwrap()
        .remove("body_hash");
    assert!(
        validate_msg(&v).is_err(),
        "a payload missing a required function-record field must be rejected"
    );
}

#[test]
fn payload_not_matching_declared_kind_is_rejected() {
    // payload_kind still says function-record, but the payload is a body
    // expression — the referenced schema must reject it.
    let mut v = example("store-request.json");
    v["body"]["payload"] = json!({
        "kind": "lambda",
        "params": [{ "name": "n", "type": { "kind": "builtin", "name": "nat" } }],
        "body": { "kind": "var", "name": "n" }
    });
    assert!(validate_msg(&v).is_err());
}

#[test]
fn payload_without_kind_only_requires_an_object() {
    // Back-compat: with no payload_kind discriminator, payload is just checked
    // to be an object (the pre-existing v0.1 behavior).
    let mut v = example("store-request.json");
    v["body"].as_object_mut().unwrap().remove("payload_kind");
    v["body"]["payload"] = json!({ "arbitrary": 123 });
    assert!(validate_msg(&v).is_ok());
}

// ---- direct retriever behavior, independent of the message schema ----

#[test]
fn absolute_logical_ref_resolves_to_file() {
    let composed = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$ref": "https://novae-linguae.org/spec/v0.1/type-expression.schema.json"
    });
    // type-map.json is a valid type expression; a malformed one is not.
    assert!(validate_with_refs(&composed, &example("type-map.json"), &spec_dir()).is_ok());
    assert!(validate_with_refs(&composed, &json!({ "kind": "var" }), &spec_dir()).is_err());
}

#[test]
fn relative_ref_resolves_against_schema_id() {
    // A schema that identifies itself in the namespace and references a sibling
    // by a *relative* path; resolution must produce the logical URI -> file.
    let composed = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": "https://novae-linguae.org/spec/v0.1/_composed-test.schema.json",
        "$ref": "value-expression.schema.json"
    });
    assert!(validate_with_refs(&composed, &example("value-list-int.json"), &spec_dir()).is_ok());
}

#[test]
fn unresolvable_namespace_ref_errors() {
    let composed = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$ref": "https://novae-linguae.org/spec/v0.1/does-not-exist.schema.json"
    });
    assert!(validate_with_refs(&composed, &json!({}), &spec_dir()).is_err());
}

#[test]
fn ref_outside_the_namespace_is_refused_without_network() {
    // A non-Novae URI must be refused by our retriever (never fetched). This
    // also confirms our retriever fully replaces any default HTTP resolver.
    let composed = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$ref": "https://example.com/some-schema.json"
    });
    assert!(validate_with_refs(&composed, &json!({}), &spec_dir()).is_err());
}
