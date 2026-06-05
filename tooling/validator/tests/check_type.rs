//! Type-expression well-formedness — the semantic checks beyond JSON Schema:
//! variable scoping, rank-1 polymorphism, uniqueness within records/sums, and
//! `apply.ctor` constructor-kind compatibility.

mod common;

use common::example;
use nl_validator::check_type_well_formed;
use serde_json::json;

#[test]
fn the_map_type_is_well_formed() {
    assert!(check_type_well_formed(&example("type-map.json")).is_ok());
}

#[test]
fn bound_variable_under_forall_is_ok() {
    let t = json!({ "kind": "forall", "vars": ["a"], "body": { "kind": "var", "name": "a" } });
    assert!(check_type_well_formed(&t).is_ok());
}

#[test]
fn unbound_variable_is_rejected() {
    let t = json!({ "kind": "var", "name": "a" });
    assert!(check_type_well_formed(&t).is_err());
}

#[test]
fn variable_outside_its_forall_scope_is_rejected() {
    // `b` is never bound; only `a` is.
    let t = json!({
        "kind": "forall",
        "vars": ["a"],
        "body": { "kind": "fn", "params": [{ "kind": "var", "name": "a" }], "result": { "kind": "var", "name": "b" } }
    });
    assert!(check_type_well_formed(&t).is_err());
}

#[test]
fn nested_forall_is_rejected_rank1() {
    let t = json!({
        "kind": "forall",
        "vars": ["a"],
        "body": { "kind": "forall", "vars": ["b"], "body": { "kind": "var", "name": "b" } }
    });
    assert!(check_type_well_formed(&t).is_err());
}

#[test]
fn duplicate_record_field_is_rejected() {
    let t = json!({
        "kind": "record",
        "fields": [
            { "name": "x", "type": { "kind": "builtin", "name": "int" } },
            { "name": "x", "type": { "kind": "builtin", "name": "bool" } }
        ]
    });
    assert!(check_type_well_formed(&t).is_err());
}

#[test]
fn unique_record_fields_are_ok() {
    let t = json!({
        "kind": "record",
        "fields": [
            { "name": "x", "type": { "kind": "builtin", "name": "int" } },
            { "name": "y", "type": { "kind": "builtin", "name": "bool" } }
        ]
    });
    assert!(check_type_well_formed(&t).is_ok());
}

#[test]
fn duplicate_sum_variant_tag_is_rejected() {
    let t = json!({
        "kind": "sum",
        "variants": [
            { "tag": "Some", "type": { "kind": "builtin", "name": "int" } },
            { "tag": "Some" }
        ]
    });
    assert!(check_type_well_formed(&t).is_err());
}

#[test]
fn apply_with_concrete_ctor_is_rejected() {
    // `fn` is not a type constructor and cannot sit in `apply.ctor`.
    let t = json!({
        "kind": "apply",
        "ctor": { "kind": "fn", "params": [{ "kind": "builtin", "name": "int" }], "result": { "kind": "builtin", "name": "int" } },
        "args": [{ "kind": "builtin", "name": "int" }]
    });
    assert!(check_type_well_formed(&t).is_err());
}

#[test]
fn apply_with_builtin_ctor_is_ok() {
    let t = json!({
        "kind": "apply",
        "ctor": { "kind": "builtin", "name": "List" },
        "args": [{ "kind": "builtin", "name": "int" }]
    });
    assert!(check_type_well_formed(&t).is_ok());
}

#[test]
fn unknown_kind_is_rejected() {
    let t = json!({ "kind": "intersection", "members": [] });
    assert!(check_type_well_formed(&t).is_err());
}
