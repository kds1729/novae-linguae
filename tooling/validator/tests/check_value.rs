use nl_validator::check_value_well_formed;
use serde_json::json;

// ---- scalar leaves ----

#[test]
fn bool_passes() {
    assert!(check_value_well_formed(&json!({"kind": "bool", "value": true})).is_ok());
}

#[test]
fn int_passes() {
    assert!(check_value_well_formed(&json!({"kind": "int", "value": -42})).is_ok());
}

#[test]
fn nat_passes() {
    assert!(check_value_well_formed(&json!({"kind": "nat", "value": 7})).is_ok());
}

#[test]
fn float_passes() {
    assert!(check_value_well_formed(&json!({"kind": "float", "value": 3.14})).is_ok());
}

#[test]
fn string_passes() {
    assert!(check_value_well_formed(&json!({"kind": "string", "value": "hello"})).is_ok());
}

#[test]
fn bytes_passes() {
    assert!(check_value_well_formed(&json!({"kind": "bytes", "value": "aGVsbG8="})).is_ok());
}

#[test]
fn unit_passes() {
    assert!(check_value_well_formed(&json!({"kind": "unit"})).is_ok());
}

#[test]
fn fn_ref_passes() {
    let hash = format!("fn_{}", "a".repeat(64));
    assert!(check_value_well_formed(&json!({"kind": "fn_ref", "target": hash})).is_ok());
}

// ---- list ----

#[test]
fn empty_list_passes() {
    assert!(check_value_well_formed(&json!({"kind": "list", "elems": []})).is_ok());
}

#[test]
fn list_with_valid_elems_passes() {
    let v = json!({
        "kind": "list",
        "elems": [
            {"kind": "nat", "value": 1},
            {"kind": "nat", "value": 2}
        ]
    });
    assert!(check_value_well_formed(&v).is_ok());
}

#[test]
fn list_with_invalid_elem_fails() {
    let v = json!({
        "kind": "list",
        "elems": [{"kind": "bogus"}]
    });
    assert!(check_value_well_formed(&v).is_err());
}

// ---- tuple ----

#[test]
fn tuple_passes() {
    let v = json!({
        "kind": "tuple",
        "elems": [
            {"kind": "bool", "value": true},
            {"kind": "int", "value": 42}
        ]
    });
    assert!(check_value_well_formed(&v).is_ok());
}

// ---- record ----

#[test]
fn record_with_unique_fields_passes() {
    let v = json!({
        "kind": "record",
        "fields": [
            {"name": "x", "value": {"kind": "int", "value": 1}},
            {"name": "y", "value": {"kind": "int", "value": 2}}
        ]
    });
    assert!(check_value_well_formed(&v).is_ok());
}

#[test]
fn record_with_duplicate_field_fails() {
    let v = json!({
        "kind": "record",
        "fields": [
            {"name": "x", "value": {"kind": "int", "value": 1}},
            {"name": "x", "value": {"kind": "int", "value": 2}}
        ]
    });
    let e = check_value_well_formed(&v).unwrap_err();
    assert!(e.to_string().contains('x'), "{e}");
    assert!(e.to_string().contains("more than once"), "{e}");
}

#[test]
fn record_with_nested_invalid_value_fails() {
    let v = json!({
        "kind": "record",
        "fields": [
            {"name": "a", "value": {"kind": "bogus"}}
        ]
    });
    assert!(check_value_well_formed(&v).is_err());
}

// ---- variant ----

#[test]
fn variant_without_payload_passes() {
    let v = json!({"kind": "variant", "tag": "None"});
    assert!(check_value_well_formed(&v).is_ok());
}

#[test]
fn variant_with_valid_payload_passes() {
    let v = json!({
        "kind": "variant",
        "tag": "Some",
        "payload": {"kind": "nat", "value": 5}
    });
    assert!(check_value_well_formed(&v).is_ok());
}

#[test]
fn variant_with_invalid_payload_fails() {
    let v = json!({
        "kind": "variant",
        "tag": "Some",
        "payload": {"kind": "bogus"}
    });
    assert!(check_value_well_formed(&v).is_err());
}

// ---- nested structures ----

#[test]
fn deeply_nested_value_passes() {
    let v = json!({
        "kind": "list",
        "elems": [
            {
                "kind": "record",
                "fields": [
                    {"name": "ok", "value": {"kind": "bool", "value": true}},
                    {"name": "count", "value": {"kind": "nat", "value": 0}}
                ]
            }
        ]
    });
    assert!(check_value_well_formed(&v).is_ok());
}

// ---- errors ----

#[test]
fn unknown_kind_fails() {
    let v = json!({"kind": "bogus"});
    let e = check_value_well_formed(&v).unwrap_err();
    assert!(e.to_string().contains("bogus"), "{e}");
}

#[test]
fn not_an_object_fails() {
    assert!(check_value_well_formed(&json!([1, 2, 3])).is_err());
}

// ---- maps (spec/expressiveness.md phase 2) ----

#[test]
fn map_sorted_unique_keys_pass() {
    let v = json!({
        "kind": "map",
        "entries": [
            {"key": "a", "value": {"kind": "int", "value": 1}},
            {"key": "b", "value": {"kind": "int", "value": 2}}
        ]
    });
    assert!(check_value_well_formed(&v).is_ok());
    // Empty maps are fine, and arbitrary string keys (not record-name-shaped) are allowed.
    assert!(check_value_well_formed(&json!({"kind": "map", "entries": []})).is_ok());
    let odd_keys = json!({
        "kind": "map",
        "entries": [
            {"key": "", "value": {"kind": "unit"}},
            {"key": "two words", "value": {"kind": "unit"}}
        ]
    });
    assert!(check_value_well_formed(&odd_keys).is_ok());
}

#[test]
fn map_duplicate_key_fails() {
    let v = json!({
        "kind": "map",
        "entries": [
            {"key": "a", "value": {"kind": "int", "value": 1}},
            {"key": "a", "value": {"kind": "int", "value": 2}}
        ]
    });
    let e = check_value_well_formed(&v).unwrap_err();
    assert!(e.to_string().contains("more than once"), "{e}");
}

#[test]
fn map_out_of_order_keys_fail() {
    // Sortedness is part of the canonical form: equal maps must hash identically.
    let v = json!({
        "kind": "map",
        "entries": [
            {"key": "b", "value": {"kind": "int", "value": 2}},
            {"key": "a", "value": {"kind": "int", "value": 1}}
        ]
    });
    let e = check_value_well_formed(&v).unwrap_err();
    assert!(e.to_string().contains("canonical order"), "{e}");
}

#[test]
fn map_nested_value_is_checked() {
    // A malformed value INSIDE a map entry is caught.
    let v = json!({
        "kind": "map",
        "entries": [
            {"key": "a", "value": {"kind": "bogus"}}
        ]
    });
    assert!(check_value_well_formed(&v).is_err());
}
