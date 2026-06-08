//! JSON Schema (draft 2020-12) structural validation: every committed example
//! validates against its schema, and representative malformations are rejected.

mod common;

use common::{example, schema, spec_dir};
use serde_json::{json, Value};

/// Validate `instance` against the named schema, resolving any cross-file
/// `$ref`s against `spec/`. (message.schema.json now references sibling schemas
/// for conditional payload validation, so the retriever-backed path is the
/// uniform one to use.)
fn check(sch: &str, instance: &Value) -> anyhow::Result<()> {
    nl_validator::validate_with_refs(&schema(sch), instance, &spec_dir())
}

/// (schema file, example file) pairs that MUST validate.
const VALID_PAIRS: &[(&str, &str)] = &[
    ("function-record.schema.json", "map.json"),
    ("function-record.v0.2.schema.json", "map.v0.2.json"),
    ("function-record.v0.2.schema.json", "double.v0.2.json"),
    ("function-record.v0.2.schema.json", "greet.v0.2.json"),
    ("message.schema.json", "request.json"),
    ("message.schema.json", "assert.json"),
    ("message.schema.json", "store-request.json"),
    ("message.v0.2.schema.json", "assert.v0.2.json"),
    ("message.v0.2.schema.json", "commit.v0.2.json"),
    ("message.v0.2.schema.json", "request.v0.2.json"),
    ("message.v0.2.schema.json", "assert-result.v0.2.json"),
    ("message.v0.2.schema.json", "request-validate.v0.2.json"),
    ("message.v0.2.schema.json", "assert-verified.v0.2.json"),
    ("message.v0.2.schema.json", "query.v0.2.json"),
    ("message.v0.2.schema.json", "ack-query.v0.2.json"),
    ("message.v0.2.schema.json", "propose.v0.2.json"),
    ("message.v0.2.schema.json", "commit-apply.v0.2.json"),
    ("message.v0.2.schema.json", "delegation/delegate-root-to-alice.json"),
    ("message.v0.2.schema.json", "delegation/delegate-alice-to-bob.json"),
    ("type-expression.schema.json", "type-map.json"),
    ("predicate-expression.schema.json", "predicate-identity.json"),
    ("value-expression.schema.json", "value-list-int.json"),
    ("body-expression.schema.json", "body-double.json"),
    ("body-expression.schema.json", "body-is-zero.json"),
    ("body-expression.schema.json", "body-map.json"),
    ("body-expression.schema.json", "body-greet.json"),
    ("encrypted-envelope.schema.json", "encrypted-envelope.json"),
    ("encrypted-envelope.schema.json", "encrypted-envelope-mlkem768.json"),
    ("did-document.schema.json", "did-document.json"),
];

#[test]
fn all_examples_validate_against_their_schemas() {
    for (sch, ex) in VALID_PAIRS {
        let result = check(sch, &example(ex));
        assert!(result.is_ok(), "{ex} should validate against {sch}: {result:?}");
    }
}

#[test]
fn unknown_field_is_rejected() {
    // `additionalProperties: false` everywhere — an unexpected key must fail.
    let mut v = example("map.json");
    v.as_object_mut()
        .unwrap()
        .insert("bogus_field".into(), json!(true));
    assert!(check("function-record.schema.json", &v).is_err());
}

#[test]
fn missing_required_field_is_rejected() {
    let mut v = example("map.json");
    v.as_object_mut().unwrap().remove("signature");
    assert!(check("function-record.schema.json", &v).is_err());
}

#[test]
fn out_of_vocabulary_speech_act_is_rejected() {
    let mut v = example("request.json");
    v.as_object_mut()
        .unwrap()
        .insert("kind".into(), json!("frobnicate"));
    assert!(check("message.schema.json", &v).is_err());
}

#[test]
fn lambda_param_type_is_optional() {
    // Reconciled with surface-syntax.md §4: a lambda param needs only `name`;
    // the type is optional (inferred when omitted). Both forms must validate.
    let untyped = json!({
        "kind": "lambda",
        "params": [{"name": "x"}],
        "body": {"kind": "var", "name": "x"}
    });
    assert!(check("body-expression.schema.json", &untyped).is_ok());

    let typed = json!({
        "kind": "lambda",
        "params": [{"name": "x", "type": {"kind": "builtin", "name": "int"}}],
        "body": {"kind": "var", "name": "x"}
    });
    assert!(check("body-expression.schema.json", &typed).is_ok());

    // `name` is still required — a param without it must fail.
    let nameless = json!({
        "kind": "lambda",
        "params": [{"type": {"kind": "builtin", "name": "int"}}],
        "body": {"kind": "var", "name": "x"}
    });
    assert!(check("body-expression.schema.json", &nameless).is_err());
}

#[test]
fn wrong_typed_field_is_rejected() {
    // `name_hints` is an array of strings; a scalar must fail structural checks.
    let mut v = example("map.json");
    v.as_object_mut()
        .unwrap()
        .insert("name_hints".into(), json!("not-an-array"));
    assert!(check("function-record.schema.json", &v).is_err());
}
