use nl_validator::check_predicate_well_formed;
use serde_json::json;

// ---- leaves ----

#[test]
fn var_leaf_passes() {
    let v = json!({"kind": "var", "name": "x"});
    assert!(check_predicate_well_formed(&v).is_ok());
}

#[test]
fn lit_leaf_passes() {
    let v = json!({"kind": "lit", "value": 42});
    assert!(check_predicate_well_formed(&v).is_ok());
}

// ---- app arity checks ----

#[test]
fn app_not_arity_1_passes() {
    let v = json!({
        "kind": "app", "op": "not",
        "args": [{"kind": "var", "name": "p"}]
    });
    assert!(check_predicate_well_formed(&v).is_ok());
}

#[test]
fn app_not_wrong_arity_fails() {
    let v = json!({
        "kind": "app", "op": "not",
        "args": [{"kind": "var", "name": "p"}, {"kind": "var", "name": "q"}]
    });
    let e = check_predicate_well_formed(&v).unwrap_err();
    assert!(e.to_string().contains("not"), "{e}");
    assert!(e.to_string().contains("1"), "{e}");
}

#[test]
fn app_nil_arity_0_passes() {
    let v = json!({"kind": "app", "op": "nil", "args": []});
    assert!(check_predicate_well_formed(&v).is_ok());
}

#[test]
fn app_nil_wrong_arity_fails() {
    let v = json!({
        "kind": "app", "op": "nil",
        "args": [{"kind": "lit", "value": 1}]
    });
    let e = check_predicate_well_formed(&v).unwrap_err();
    assert!(e.to_string().contains("nil"), "{e}");
}

#[test]
fn app_and_arity_2_passes() {
    let v = json!({
        "kind": "app", "op": "and",
        "args": [{"kind": "var", "name": "p"}, {"kind": "var", "name": "q"}]
    });
    assert!(check_predicate_well_formed(&v).is_ok());
}

#[test]
fn app_foldl_arity_3_passes() {
    let v = json!({
        "kind": "app", "op": "foldl",
        "args": [
            {"kind": "var", "name": "f"},
            {"kind": "var", "name": "z"},
            {"kind": "var", "name": "xs"}
        ]
    });
    assert!(check_predicate_well_formed(&v).is_ok());
}

#[test]
fn app_foldl_wrong_arity_fails() {
    let v = json!({
        "kind": "app", "op": "foldl",
        "args": [{"kind": "var", "name": "f"}]
    });
    let e = check_predicate_well_formed(&v).unwrap_err();
    assert!(e.to_string().contains("foldl"), "{e}");
    assert!(e.to_string().contains("3"), "{e}");
}

#[test]
fn app_unknown_op_skips_arity_check() {
    // A content-address reference as op — no arity rule applies.
    let hash = format!("fn_{}", "a".repeat(64));
    let v = json!({
        "kind": "app", "op": hash,
        "args": [{"kind": "var", "name": "x"}, {"kind": "var", "name": "y"}, {"kind": "var", "name": "z"}]
    });
    assert!(check_predicate_well_formed(&v).is_ok());
}

// ---- nested app ----

#[test]
fn nested_app_passes() {
    let v = json!({
        "kind": "app", "op": "and",
        "args": [
            {
                "kind": "app", "op": "eq",
                "args": [{"kind": "var", "name": "x"}, {"kind": "lit", "value": 0}]
            },
            {
                "kind": "app", "op": "gt",
                "args": [{"kind": "var", "name": "y"}, {"kind": "lit", "value": 0}]
            }
        ]
    });
    assert!(check_predicate_well_formed(&v).is_ok());
}

#[test]
fn nested_app_bad_arity_propagates() {
    let v = json!({
        "kind": "app", "op": "and",
        "args": [
            {"kind": "app", "op": "not", "args": []},  // wrong arity
            {"kind": "var", "name": "q"}
        ]
    });
    assert!(check_predicate_well_formed(&v).is_err());
}

// ---- forall / exists ----

#[test]
fn forall_passes() {
    let v = json!({
        "kind": "forall",
        "vars": ["x"],
        "body": {
            "kind": "app", "op": "eq",
            "args": [{"kind": "var", "name": "x"}, {"kind": "var", "name": "x"}]
        }
    });
    assert!(check_predicate_well_formed(&v).is_ok());
}

#[test]
fn exists_passes() {
    let v = json!({
        "kind": "exists",
        "vars": ["n"],
        "body": {
            "kind": "app", "op": "gt",
            "args": [{"kind": "var", "name": "n"}, {"kind": "lit", "value": 0}]
        }
    });
    assert!(check_predicate_well_formed(&v).is_ok());
}

#[test]
fn forall_bad_body_arity_propagates() {
    let v = json!({
        "kind": "forall",
        "vars": ["x"],
        "body": {"kind": "app", "op": "not", "args": []}  // wrong arity
    });
    assert!(check_predicate_well_formed(&v).is_err());
}

// ---- unknown kind ----

#[test]
fn unknown_kind_fails() {
    let v = json!({"kind": "bogus", "x": 1});
    let e = check_predicate_well_formed(&v).unwrap_err();
    assert!(e.to_string().contains("bogus"), "{e}");
}

#[test]
fn missing_kind_fails() {
    let v = json!({"op": "eq", "args": []});
    assert!(check_predicate_well_formed(&v).is_err());
}

#[test]
fn not_an_object_fails() {
    let v = serde_json::json!("not an object");
    assert!(check_predicate_well_formed(&v).is_err());
}
