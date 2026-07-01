//! Static effect inference — the verification counterpart to runtime effect enforcement (interp.rs).
//!
//! Runtime enforcement (`run` / `eval --grant`) catches an undeclared effect when an effectful builtin
//! actually executes. This infers a body's effects *without running it*, by walking the body-expression
//! AST: every effectful builtin it names (`print` → `io.console`, `rand` → `random`) contributes its
//! effect, and a function record is **sound** iff the inferred effects ⊆ its declared
//! `signature.effects`. So a record that *under-declares* — claims fewer effects than its body can
//! perform — is caught statically, before any execution (principles 3 + 5).
//!
//! Honest scope: a function's *own* effects are what it performs directly via builtins; the effects of
//! a higher-order argument belong to the caller (effect polymorphism — `map`'s declared `[]` is correct
//! even though `map(f, xs)` runs `f`). But a body that **directly applies** something opaque — a
//! parameter / let-binding / `fn_ref` used in function position — could perform effects we cannot see
//! statically, so the verdict is UNVERIFIABLE (not SOUND): the inferred set is then only a lower bound.

use anyhow::{bail, Result};
use serde_json::Value as J;
use std::collections::{BTreeSet, HashMap};

use crate::interp::{builtin_effect, is_builtin};

/// Result of walking a body for the effects it performs.
pub struct EffectInference {
    /// Effects the body performs — directly via effectful builtins, and via the *declared* effects of
    /// any `fn_ref` callee resolved from the record map.
    pub effects: BTreeSet<String>,
    /// True if the body directly applies an opaque callee — a non-builtin `var` (a higher-order
    /// parameter / external name) — so its real effect set may exceed `effects`.
    pub opaque: bool,
    /// True if the body references a `fn_ref` callee not present in the record map, so its declared
    /// effects couldn't be folded in.
    pub unresolved: bool,
}

/// Infer the effects a body-expression AST performs (see module docs for scope). `records` (address →
/// function record) resolves `fn_ref` callees to their declared `signature.effects`; pass an empty map
/// to skip resolution (every `fn_ref` then counts as `unresolved`).
pub fn infer_effects(body: &J, records: &HashMap<String, J>) -> EffectInference {
    let mut inf = EffectInference { effects: BTreeSet::new(), opaque: false, unresolved: false };
    walk(body, records, &[], &mut inf);
    inf
}

/// `bound` is the names in lexical scope (lambda params, `let` and `case`-`bind` names). Applying a
/// bound name is effect-polymorphic (its effects belong to the caller, like `map`); applying a
/// *free* non-builtin name is a genuinely external opaque callee.
fn walk(node: &J, records: &HashMap<String, J>, bound: &[String], inf: &mut EffectInference) {
    let Some(kind) = node.get("kind").and_then(|k| k.as_str()) else { return };
    match kind {
        "var" => {
            if let Some(name) = node.get("name").and_then(|n| n.as_str()) {
                if let Some(e) = builtin_effect(name) {
                    inf.effects.insert(e.to_string());
                }
            }
        }
        "lit" => {
            // A `fn_ref` value names a concrete commons function this body uses; its declared effects
            // accumulate here (effect polymorphism applies to *parameters*, not to a function the body
            // itself chose to reference). Resolve via the record map, else flag unresolved.
            if node.pointer("/value/kind").and_then(|k| k.as_str()) == Some("fn_ref") {
                if let Some(target) = node.pointer("/value/target").and_then(|t| t.as_str()) {
                    match records.get(target).and_then(|r| r.pointer("/signature/effects")).and_then(|e| e.as_array()) {
                        Some(effs) => {
                            for e in effs {
                                if let Some(s) = e.as_str() {
                                    inf.effects.insert(s.to_string());
                                }
                            }
                        }
                        None => inf.unresolved = true,
                    }
                }
            }
        }
        "app" => {
            if let Some(f) = node.get("fn") {
                // A var callee that is neither a builtin nor a name in scope is an external opaque
                // function; a bound name is effect-polymorphic (the caller supplies it). `self` is the
                // function itself — its effects are the record's own declared set (the recursion's
                // fixpoint), so a self-call can never introduce an effect the body doesn't already perform,
                // and must not read as an opaque callee (else every recursive record is UNVERIFIABLE).
                if f.get("kind").and_then(|k| k.as_str()) == Some("var") {
                    let n = f.get("name").and_then(|n| n.as_str()).unwrap_or_default();
                    if !is_builtin(n) && n != "self" && !bound.iter().any(|b| b == n) {
                        inf.opaque = true;
                    }
                }
                walk(f, records, bound, inf);
            }
            for a in node.get("args").and_then(|a| a.as_array()).into_iter().flatten() {
                walk(a, records, bound, inf);
            }
        }
        "let" => {
            walk(&node["value"], records, bound, inf);
            let mut b2 = bound.to_vec();
            if let Some(name) = node.get("name").and_then(|n| n.as_str()) {
                b2.push(name.to_string());
            }
            walk(&node["body"], records, &b2, inf);
        }
        "lambda" => {
            let mut b2 = bound.to_vec();
            for p in node.get("params").and_then(|p| p.as_array()).into_iter().flatten() {
                if let Some(name) = p.get("name").and_then(|n| n.as_str()) {
                    b2.push(name.to_string());
                }
            }
            walk(&node["body"], records, &b2, inf);
        }
        "case" => {
            walk(&node["scrutinee"], records, bound, inf);
            for arm in node.get("arms").and_then(|a| a.as_array()).into_iter().flatten() {
                let mut b2 = bound.to_vec();
                if arm.pointer("/pattern/kind").and_then(|k| k.as_str()) == Some("bind") {
                    if let Some(pn) = arm.pointer("/pattern/name").and_then(|n| n.as_str()) {
                        b2.push(pn.to_string());
                    }
                }
                walk(&arm["body"], records, &b2, inf);
            }
        }
        "field" => walk(&node["record"], records, bound, inf),
        _ => {}
    }
}

/// Check a function record's declared `signature.effects` against the effects inferred from its body.
/// `records` resolves `fn_ref` callees (pass `--records <dir>` on the CLI). Prints SOUND /
/// UNVERIFIABLE / UNDER-DECLARED; returns Err (exit 1) only when the body performs an effect the record
/// does not declare.
pub fn check_effects(record: &J, body: &J, records: &HashMap<String, J>) -> Result<()> {
    let inferred = infer_effects(body, records);
    let declared: BTreeSet<String> = record
        .pointer("/signature/effects")
        .and_then(|e| e.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default();

    let show = |s: &BTreeSet<String>| s.iter().cloned().collect::<Vec<_>>().join(", ");
    let under: Vec<String> = inferred.effects.difference(&declared).cloned().collect();
    if !under.is_empty() {
        println!(
            "UNDER-DECLARED  body performs [{}] not in declared [{}]",
            under.join(", "),
            show(&declared)
        );
        bail!("effect check failed: undeclared effect(s) [{}]", under.join(", "));
    }
    if inferred.opaque || inferred.unresolved {
        let why = if inferred.unresolved {
            "an unresolved fn_ref callee (pass --records to fold in its declared effects)"
        } else {
            "an opaque call (a higher-order / parameter application)"
        };
        println!(
            "UNVERIFIABLE    inferred [{}] ⊆ declared [{}], but {why} may perform more",
            show(&inferred.effects),
            show(&declared)
        );
        return Ok(());
    }
    let over: Vec<String> = declared.difference(&inferred.effects).cloned().collect();
    let note = if over.is_empty() { String::new() } else { format!("  (over-declared: [{}])", over.join(", ")) };
    println!("SOUND           effects [{}] ⊆ declared [{}]{}", show(&inferred.effects), show(&declared), note);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::Path;

    fn load(name: &str) -> J {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spec/examples").join(name);
        serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
    }

    fn no_records() -> HashMap<String, J> {
        HashMap::new()
    }

    #[test]
    fn greet_is_sound_and_io_console_is_inferred() {
        let inf = infer_effects(&load("body-greet.json"), &no_records());
        assert!(inf.effects.contains("io.console"));
        assert!(!inf.opaque && !inf.unresolved);
        assert!(check_effects(&load("greet.v0.2.json"), &load("body-greet.json"), &no_records()).is_ok());
    }

    #[test]
    fn double_is_pure_and_sound() {
        let inf = infer_effects(&load("body-double.json"), &no_records());
        assert!(inf.effects.is_empty() && !inf.opaque && !inf.unresolved);
        assert!(check_effects(&load("double.v0.2.json"), &load("body-double.json"), &no_records()).is_ok());
    }

    #[test]
    fn under_declaration_is_caught() {
        // The io.console body checked against a record that declares no effects → UNDER-DECLARED.
        assert!(check_effects(&load("double.v0.2.json"), &load("body-greet.json"), &no_records()).is_err());
    }

    #[test]
    fn applying_a_parameter_is_effect_polymorphic_not_opaque() {
        // \f x -> f(x): `f` is a bound parameter, so its effects belong to the caller (like `map`).
        let body = json!({ "kind": "lambda", "params": [{ "name": "f" }, { "name": "x" }],
            "body": { "kind": "app", "fn": { "kind": "var", "name": "f" }, "args": [{ "kind": "var", "name": "x" }] } });
        assert!(!infer_effects(&body, &no_records()).opaque);
    }

    #[test]
    fn self_call_is_not_opaque() {
        // \xs -> self(tail xs): a recursive self-call is the function itself (its effects are the record's
        // own declared set — the fixpoint), so it must not read as an opaque external callee.
        let body = json!({ "kind": "lambda", "params": [{ "name": "xs" }],
            "body": { "kind": "app", "fn": { "kind": "var", "name": "self" },
                      "args": [{ "kind": "app", "fn": { "kind": "var", "name": "tail" }, "args": [{ "kind": "var", "name": "xs" }] }] } });
        let inf = infer_effects(&body, &no_records());
        assert!(!inf.opaque && inf.effects.is_empty(), "a pure recursive body is SOUND, not UNVERIFIABLE");
    }

    #[test]
    fn applying_a_free_name_is_opaque() {
        // \x -> g(x): `g` is free (not a param / builtin / fn_ref) — a genuinely external callee.
        let body = json!({ "kind": "lambda", "params": [{ "name": "x" }],
            "body": { "kind": "app", "fn": { "kind": "var", "name": "g" }, "args": [{ "kind": "var", "name": "x" }] } });
        assert!(infer_effects(&body, &no_records()).opaque);
    }

    #[test]
    fn map_body_is_not_opaque() {
        // \f xs -> map(f, xs): the head is the builtin `map`; `f` is an argument (effect-polymorphic).
        assert!(!infer_effects(&load("body-map.json"), &no_records()).opaque);
    }

    #[test]
    fn fn_ref_callee_resolves_to_its_declared_effects() {
        // A body that applies greet by fn_ref: its io.console is UNVERIFIABLE without a record map,
        // and folded in (SOUND-able) once greet is resolvable.
        let greet = load("greet.v0.2.json");
        let greet_hash = greet["hash"].as_str().unwrap().to_string();
        let body = json!({ "kind": "lambda", "params": [{ "name": "m" }],
            "body": { "kind": "app", "fn": { "kind": "lit", "value": { "kind": "fn_ref", "target": greet_hash } },
                      "args": [{ "kind": "var", "name": "m" }] } });

        let bare = infer_effects(&body, &no_records());
        assert!(bare.unresolved && bare.effects.is_empty(), "unresolved without the record map");

        let mut records = HashMap::new();
        records.insert(greet_hash, greet);
        let resolved = infer_effects(&body, &records);
        assert!(!resolved.unresolved && resolved.effects.contains("io.console"), "greet's io.console folds in");
    }
}
