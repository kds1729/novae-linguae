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
use std::collections::BTreeSet;

use crate::interp::{builtin_effect, is_builtin};

/// Result of walking a body for the effects it performs.
pub struct EffectInference {
    /// Effects the body performs directly via effectful builtins.
    pub effects: BTreeSet<String>,
    /// True if the body directly applies an opaque callee (a non-builtin var or a `fn_ref`), so its
    /// real effect set may exceed `effects`.
    pub opaque: bool,
}

/// Infer the effects a body-expression AST performs (see module docs for scope).
pub fn infer_effects(body: &J) -> EffectInference {
    let mut effects = BTreeSet::new();
    let mut opaque = false;
    walk(body, &mut effects, &mut opaque);
    EffectInference { effects, opaque }
}

fn walk(node: &J, effects: &mut BTreeSet<String>, opaque: &mut bool) {
    let Some(kind) = node.get("kind").and_then(|k| k.as_str()) else { return };
    match kind {
        "var" => {
            if let Some(name) = node.get("name").and_then(|n| n.as_str()) {
                if let Some(e) = builtin_effect(name) {
                    effects.insert(e.to_string());
                }
            }
        }
        "app" => {
            if let Some(f) = node.get("fn") {
                if applies_opaque(f) {
                    *opaque = true;
                }
                walk(f, effects, opaque);
            }
            if let Some(args) = node.get("args").and_then(|a| a.as_array()) {
                for a in args {
                    walk(a, effects, opaque);
                }
            }
        }
        "let" => {
            walk(&node["value"], effects, opaque);
            walk(&node["body"], effects, opaque);
        }
        "lambda" => walk(&node["body"], effects, opaque),
        "case" => {
            walk(&node["scrutinee"], effects, opaque);
            if let Some(arms) = node.get("arms").and_then(|a| a.as_array()) {
                for arm in arms {
                    walk(&arm["body"], effects, opaque);
                }
            }
        }
        "field" => walk(&node["record"], effects, opaque),
        _ => {} // `lit`: a value-expression payload, not a call site
    }
}

/// Is the callee of an `app` something whose effects we cannot see statically? A non-builtin `var`
/// (a parameter / let-binding / free name applied as a function) or a `lit` `fn_ref`. A `lambda`
/// head (IIFE) or a curried `app` head is analyzed by the normal walk, so not opaque here.
fn applies_opaque(f: &J) -> bool {
    match f.get("kind").and_then(|k| k.as_str()) {
        Some("var") => f.get("name").and_then(|n| n.as_str()).map(|n| !is_builtin(n)).unwrap_or(true),
        Some("lit") => f.pointer("/value/kind").and_then(|k| k.as_str()) == Some("fn_ref"),
        _ => false,
    }
}

/// Check a function record's declared `signature.effects` against the effects inferred from its body.
/// Prints SOUND / UNVERIFIABLE / UNDER-DECLARED; returns Err (exit 1) only when the body performs an
/// effect the record does not declare.
pub fn check_effects(record: &J, body: &J) -> Result<()> {
    let inferred = infer_effects(body);
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
    if inferred.opaque {
        println!(
            "UNVERIFIABLE    inferred [{}] ⊆ declared [{}], but an opaque call (a higher-order / fn_ref application) may perform more",
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

    #[test]
    fn greet_is_sound_and_io_console_is_inferred() {
        let inf = infer_effects(&load("body-greet.json"));
        assert!(inf.effects.contains("io.console"));
        assert!(!inf.opaque);
        assert!(check_effects(&load("greet.v0.2.json"), &load("body-greet.json")).is_ok());
    }

    #[test]
    fn double_is_pure_and_sound() {
        let inf = infer_effects(&load("body-double.json"));
        assert!(inf.effects.is_empty());
        assert!(!inf.opaque);
        assert!(check_effects(&load("double.v0.2.json"), &load("body-double.json")).is_ok());
    }

    #[test]
    fn under_declaration_is_caught() {
        // The io.console body checked against a record that declares no effects → UNDER-DECLARED.
        assert!(check_effects(&load("double.v0.2.json"), &load("body-greet.json")).is_err());
    }

    #[test]
    fn applying_a_parameter_is_opaque() {
        // \f x -> f(x): the head `f` is not a builtin, so effects are UNVERIFIABLE.
        let body = json!({ "kind": "lambda", "params": [{ "name": "f" }, { "name": "x" }],
            "body": { "kind": "app", "fn": { "kind": "var", "name": "f" }, "args": [{ "kind": "var", "name": "x" }] } });
        assert!(infer_effects(&body).opaque);
    }

    #[test]
    fn map_body_is_not_opaque() {
        // \f xs -> map(f, xs): the head is the builtin `map`; `f` is an argument (effect-polymorphic).
        assert!(!infer_effects(&load("body-map.json")).opaque);
    }
}
