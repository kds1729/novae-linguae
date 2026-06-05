use nl_validator::check_body_well_formed;
use serde_json::json;

// ---- var ----

#[test]
fn var_passes() {
    assert!(check_body_well_formed(&json!({"kind": "var", "name": "x"})).is_ok());
}

// ---- lit ----

#[test]
fn lit_with_valid_value_passes() {
    let v = json!({
        "kind": "lit",
        "value": {"kind": "nat", "value": 42}
    });
    assert!(check_body_well_formed(&v).is_ok());
}

#[test]
fn lit_with_invalid_value_fails() {
    let v = json!({
        "kind": "lit",
        "value": {"kind": "bogus"}
    });
    assert!(check_body_well_formed(&v).is_err());
}

// ---- app ----

#[test]
fn app_passes() {
    let v = json!({
        "kind": "app",
        "fn":   {"kind": "var", "name": "double"},
        "args": [{"kind": "var", "name": "x"}]
    });
    assert!(check_body_well_formed(&v).is_ok());
}

#[test]
fn app_with_lambda_fn_passes() {
    let v = json!({
        "kind": "app",
        "fn": {
            "kind": "lambda",
            "params": [{"name": "x", "type": {"kind": "builtin", "name": "Int"}}],
            "body":   {"kind": "var", "name": "x"}
        },
        "args": [{"kind": "lit", "value": {"kind": "int", "value": 1}}]
    });
    assert!(check_body_well_formed(&v).is_ok());
}

#[test]
fn app_with_bad_fn_fails() {
    let v = json!({
        "kind": "app",
        "fn":   {"kind": "bogus"},
        "args": []
    });
    assert!(check_body_well_formed(&v).is_err());
}

#[test]
fn app_with_bad_arg_fails() {
    let v = json!({
        "kind": "app",
        "fn":   {"kind": "var", "name": "f"},
        "args": [{"kind": "bogus"}]
    });
    assert!(check_body_well_formed(&v).is_err());
}

// ---- let ----

#[test]
fn let_passes() {
    let v = json!({
        "kind":  "let",
        "name":  "y",
        "value": {"kind": "lit", "value": {"kind": "nat", "value": 0}},
        "body":  {"kind": "var", "name": "y"}
    });
    assert!(check_body_well_formed(&v).is_ok());
}

#[test]
fn let_bad_value_fails() {
    let v = json!({
        "kind":  "let",
        "name":  "y",
        "value": {"kind": "bogus"},
        "body":  {"kind": "var", "name": "y"}
    });
    assert!(check_body_well_formed(&v).is_err());
}

// ---- lambda ----

#[test]
fn lambda_unique_params_passes() {
    let v = json!({
        "kind": "lambda",
        "params": [
            {"name": "a", "type": {"kind": "builtin", "name": "Int"}},
            {"name": "b", "type": {"kind": "builtin", "name": "Int"}}
        ],
        "body": {"kind": "var", "name": "a"}
    });
    assert!(check_body_well_formed(&v).is_ok());
}

#[test]
fn lambda_duplicate_params_fails() {
    let v = json!({
        "kind": "lambda",
        "params": [
            {"name": "x", "type": {"kind": "builtin", "name": "Int"}},
            {"name": "x", "type": {"kind": "builtin", "name": "Int"}}
        ],
        "body": {"kind": "var", "name": "x"}
    });
    let e = check_body_well_formed(&v).unwrap_err();
    assert!(e.to_string().contains('x'), "{e}");
    assert!(e.to_string().contains("more than once"), "{e}");
}

#[test]
fn lambda_zero_params_passes() {
    let v = json!({
        "kind":   "lambda",
        "params": [],
        "body":   {"kind": "lit", "value": {"kind": "unit"}}
    });
    assert!(check_body_well_formed(&v).is_ok());
}

#[test]
fn lambda_bad_body_fails() {
    let v = json!({
        "kind":   "lambda",
        "params": [{"name": "x", "type": {"kind": "builtin", "name": "Int"}}],
        "body":   {"kind": "bogus"}
    });
    assert!(check_body_well_formed(&v).is_err());
}

// ---- case ----

#[test]
fn case_with_wildcard_arm_passes() {
    let v = json!({
        "kind": "case",
        "scrutinee": {"kind": "var", "name": "opt"},
        "arms": [
            {
                "pattern": {"kind": "variant", "tag": "Some", "payload": {"kind": "bind", "name": "x"}},
                "body":    {"kind": "var", "name": "x"}
            },
            {
                "pattern": {"kind": "wildcard"},
                "body":    {"kind": "lit", "value": {"kind": "nat", "value": 0}}
            }
        ]
    });
    assert!(check_body_well_formed(&v).is_ok());
}

#[test]
fn case_bad_scrutinee_fails() {
    let v = json!({
        "kind": "case",
        "scrutinee": {"kind": "bogus"},
        "arms": [{"pattern": {"kind": "wildcard"}, "body": {"kind": "var", "name": "x"}}]
    });
    assert!(check_body_well_formed(&v).is_err());
}

#[test]
fn case_bad_arm_body_fails() {
    let v = json!({
        "kind": "case",
        "scrutinee": {"kind": "var", "name": "x"},
        "arms": [{"pattern": {"kind": "wildcard"}, "body": {"kind": "bogus"}}]
    });
    assert!(check_body_well_formed(&v).is_err());
}

#[test]
fn case_lit_pattern_valid_value_passes() {
    let v = json!({
        "kind": "case",
        "scrutinee": {"kind": "var", "name": "n"},
        "arms": [
            {
                "pattern": {"kind": "lit", "value": {"kind": "nat", "value": 0}},
                "body":    {"kind": "lit", "value": {"kind": "bool", "value": true}}
            },
            {
                "pattern": {"kind": "wildcard"},
                "body":    {"kind": "lit", "value": {"kind": "bool", "value": false}}
            }
        ]
    });
    assert!(check_body_well_formed(&v).is_ok());
}

#[test]
fn case_lit_pattern_invalid_value_fails() {
    let v = json!({
        "kind": "case",
        "scrutinee": {"kind": "var", "name": "n"},
        "arms": [
            {
                "pattern": {"kind": "lit", "value": {"kind": "bogus"}},
                "body":    {"kind": "var", "name": "n"}
            }
        ]
    });
    assert!(check_body_well_formed(&v).is_err());
}

// ---- field ----

#[test]
fn field_passes() {
    let v = json!({
        "kind":   "field",
        "record": {"kind": "var", "name": "rec"},
        "name":   "x"
    });
    assert!(check_body_well_formed(&v).is_ok());
}

#[test]
fn field_bad_record_fails() {
    let v = json!({
        "kind":   "field",
        "record": {"kind": "bogus"},
        "name":   "x"
    });
    assert!(check_body_well_formed(&v).is_err());
}

// ---- unknown kind ----

#[test]
fn unknown_kind_fails() {
    let v = json!({"kind": "bogus"});
    let e = check_body_well_formed(&v).unwrap_err();
    assert!(e.to_string().contains("bogus"), "{e}");
}

#[test]
fn not_an_object_fails() {
    assert!(check_body_well_formed(&json!("nope")).is_err());
}
