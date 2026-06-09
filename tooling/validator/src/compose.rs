//! Composition metadata propagation — addressing the README's "composition opacity" open problem: even
//! when every leaf function is fully described, a *pipeline* of them has emergent metadata that nobody
//! computed. Given a sequential pipeline `f1; f2; …; fn` (each stage applied to the previous stage's
//! result), this derives the composite's metadata from the leaves' declared signatures:
//!
//! - **type composability** — stage `i`'s result type must fit stage `i+1`'s parameter type; the
//!   composite runs from `f1`'s input type to `fn`'s result type. (Checked structurally/coarsely, with
//!   polymorphic type variables treated as wildcards — enough to catch a `nat`-producing stage feeding a
//!   `List`-consuming one.)
//! - **effects** — the *union* of every stage's declared effects (a pipeline performs all of them).
//! - **capabilities** — the union, likewise.
//! - **termination** — `always` only if every stage is `always`, else `unknown` (conservative).
//! - **complexity** — a coarse upper bound: the maximum stage complexity (sequential composition's
//!   dominant term), or `unknown` if any stage's is unrecognized. Not an exact cost model.
//!
//! So an assembled pipeline becomes as described as a leaf — the precondition for principle 4
//! ("assemble, don't write") to yield artifacts that are themselves verifiable. Scope (v0.1): each stage
//! is a unary function (the pipeline threads one value); arity ≠ 1 makes the chain non-composable.

use serde_json::Value as J;
use std::collections::BTreeSet;

/// The derived metadata of a composed pipeline.
#[derive(Debug, Clone)]
pub struct CompositionMetadata {
    pub composable: bool,
    pub reason: String,
    /// The composite's input type (`f1`'s parameter) and output type (`fn`'s result), as type-exprs.
    pub input_type: Option<J>,
    pub output_type: Option<J>,
    pub effects: Vec<String>,
    pub capabilities: Vec<String>,
    pub terminates: String,
    pub complexity: String,
}

/// A coarse type for structural composability checks (polymorphic variables become `Any`).
#[derive(Debug, Clone, PartialEq)]
enum CTy {
    Int,
    Bool,
    Lst(Box<CTy>),
    Fun,
    Any,
}

fn coarse(t: &J) -> CTy {
    let t = if t.get("kind").and_then(|k| k.as_str()) == Some("forall") {
        t.get("body").unwrap_or(t)
    } else {
        t
    };
    match t.get("kind").and_then(|k| k.as_str()) {
        Some("builtin") => match t.get("name").and_then(|n| n.as_str()) {
            Some("int") | Some("nat") => CTy::Int,
            Some("bool") => CTy::Bool,
            Some("List") => CTy::Lst(Box::new(CTy::Any)),
            _ => CTy::Any,
        },
        Some("apply") if t.pointer("/ctor/name").and_then(|n| n.as_str()) == Some("List") => {
            CTy::Lst(Box::new(t.get("args").and_then(|a| a.as_array()).and_then(|a| a.first()).map_or(CTy::Any, coarse)))
        }
        Some("fn") => CTy::Fun,
        _ => CTy::Any,
    }
}

fn compatible(a: &CTy, b: &CTy) -> bool {
    match (a, b) {
        (CTy::Any, _) | (_, CTy::Any) => true,
        (CTy::Lst(x), CTy::Lst(y)) => compatible(x, y),
        _ => a == b,
    }
}

/// The `fn` type node of a record's signature (unwrapping `forall`), or `None` if not a function.
fn fn_type(record: &J) -> Option<&J> {
    let mut t = record.pointer("/signature/type")?;
    if t.get("kind").and_then(|k| k.as_str()) == Some("forall") {
        t = t.get("body")?;
    }
    (t.get("kind").and_then(|k| k.as_str()) == Some("fn")).then_some(t)
}

fn str_array(v: Option<&J>) -> Vec<String> {
    v.and_then(|a| a.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default()
}

/// Known complexity classes, ascending. The composite reports the highest present (a coarse bound).
const COMPLEXITY_RANK: &[&str] = &["O(1)", "O(log n)", "O(n)", "O(n log n)", "O(n^2)", "O(n^3)"];

fn complexity_of(stages: &[String]) -> String {
    let mut best: Option<usize> = Some(0);
    for s in stages {
        match COMPLEXITY_RANK.iter().position(|c| c == s) {
            Some(r) => best = best.map(|b| b.max(r)),
            None => return "unknown".to_string(), // an unrecognized class makes the bound unknown
        }
    }
    best.map(|r| COMPLEXITY_RANK[r].to_string()).unwrap_or_else(|| "O(1)".to_string())
}

/// Derive the metadata of the sequential pipeline `records[0]; records[1]; …` (each stage applied to the
/// previous stage's single result). Never errors — composability and the reason are in the returned value.
pub fn compose(records: &[J]) -> CompositionMetadata {
    let unknown = |reason: String| CompositionMetadata {
        composable: false,
        reason,
        input_type: None,
        output_type: None,
        effects: vec![],
        capabilities: vec![],
        terminates: "unknown".to_string(),
        complexity: "unknown".to_string(),
    };
    if records.is_empty() {
        return unknown("empty pipeline".to_string());
    }
    // Every stage must be a unary function so the pipeline threads a single value.
    let mut fts = Vec::new();
    for (i, r) in records.iter().enumerate() {
        match fn_type(r) {
            Some(ft) if ft.get("params").and_then(|p| p.as_array()).map_or(0, |a| a.len()) == 1 => fts.push(ft),
            Some(_) => return unknown(format!("stage {i} is not unary (a pipeline threads one value)")),
            None => return unknown(format!("stage {i} is not a function")),
        }
    }
    // Each stage's result type must fit the next stage's parameter type.
    for i in 0..fts.len() - 1 {
        let result = fts[i].get("result").unwrap();
        let next_param = &fts[i + 1].get("params").unwrap().as_array().unwrap()[0];
        if !compatible(&coarse(result), &coarse(next_param)) {
            return unknown(format!(
                "stage {i}'s output type does not fit stage {}'s input type",
                i + 1
            ));
        }
    }

    let mut effects = BTreeSet::new();
    let mut capabilities = BTreeSet::new();
    let mut terms_all_always = true;
    let mut complexities = Vec::new();
    for r in records {
        effects.extend(str_array(r.pointer("/signature/effects")));
        capabilities.extend(str_array(r.pointer("/signature/capabilities")));
        if r.pointer("/signature/terminates").and_then(|t| t.as_str()) != Some("always") {
            terms_all_always = false;
        }
        if let Some(c) = r.pointer("/signature/complexity").and_then(|c| c.as_str()) {
            complexities.push(c.to_string());
        }
    }

    CompositionMetadata {
        composable: true,
        reason: format!("a {}-stage pipeline composes end to end", records.len()),
        input_type: fts[0].get("params").unwrap().as_array().unwrap().first().cloned(),
        output_type: fts.last().unwrap().get("result").cloned(),
        effects: effects.into_iter().collect(),
        capabilities: capabilities.into_iter().collect(),
        terminates: if terms_all_always { "always" } else { "unknown" }.to_string(),
        complexity: complexity_of(&complexities),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn examples() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spec/examples")
    }
    fn load(n: &str) -> J {
        crate::read_json(&examples().join(n)).unwrap()
    }

    #[test]
    fn reverse_then_length_composes_list_to_nat() {
        // reverse : List a -> List a ; length : List a -> nat → composite List a -> nat.
        let m = compose(&[load("reverse.json"), load("length.json")]);
        assert!(m.composable, "{}", m.reason);
        assert_eq!(coarse(m.input_type.as_ref().unwrap()), CTy::Lst(Box::new(CTy::Any)));
        assert_eq!(coarse(m.output_type.as_ref().unwrap()), CTy::Int);
        assert_eq!(m.terminates, "always");
        assert!(m.effects.is_empty());
    }

    #[test]
    fn length_then_reverse_does_not_compose() {
        // length : List a -> nat ; reverse : List a -> List a — a nat cannot feed a List parameter.
        let m = compose(&[load("length.json"), load("reverse.json")]);
        assert!(!m.composable, "nat output must not fit a List input");
    }

    #[test]
    fn reverse_then_reverse_composes() {
        let m = compose(&[load("reverse.json"), load("reverse.json")]);
        assert!(m.composable);
        assert_eq!(coarse(m.input_type.as_ref().unwrap()), CTy::Lst(Box::new(CTy::Any)));
        assert_eq!(coarse(m.output_type.as_ref().unwrap()), CTy::Lst(Box::new(CTy::Any)));
    }

    #[test]
    fn effects_union_and_termination_conjunction() {
        // Synthetic stages: one effectful + non-terminating, one pure → union of effects, terminates unknown.
        let eff = json!({ "signature": { "type": { "kind": "fn", "params": [{ "kind": "builtin", "name": "int" }], "result": { "kind": "builtin", "name": "int" } },
            "effects": ["io.console"], "capabilities": ["cap:x"], "terminates": "unknown", "complexity": "O(n)" } });
        let pure = json!({ "signature": { "type": { "kind": "fn", "params": [{ "kind": "builtin", "name": "int" }], "result": { "kind": "builtin", "name": "int" } },
            "effects": ["random"], "capabilities": [], "terminates": "always", "complexity": "O(n^2)" } });
        let m = compose(&[eff, pure]);
        assert!(m.composable);
        assert_eq!(m.effects, vec!["io.console".to_string(), "random".to_string()]);
        assert_eq!(m.capabilities, vec!["cap:x".to_string()]);
        assert_eq!(m.terminates, "unknown");
        assert_eq!(m.complexity, "O(n^2)", "coarse max of O(n) and O(n^2)");
    }

    #[test]
    fn non_unary_stage_is_not_composable() {
        let m = compose(&[load("add.json"), load("length.json")]);
        assert!(!m.composable, "add is binary — not a pipeline stage");
    }
}
