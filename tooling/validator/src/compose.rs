//! Composition metadata propagation — addressing the README's "composition opacity" open problem: even
//! when every leaf function is fully described, a *pipeline* of them has emergent metadata that nobody
//! computed. Given a sequential pipeline `f1; f2; …; fn` (each stage applied to the previous stage's
//! result), this derives the composite's metadata from the leaves' declared signatures:
//!
//! - **type composability** — stage `i`'s result type must fit stage `i+1`'s parameter type; the
//!   composite runs from `f1`'s input type to `fn`'s result type. Composability is checked
//!   structurally/coarsely (polymorphic type variables as wildcards — enough to catch a `nat`-producing
//!   stage feeding a `List`-consuming one), but the composite's reported input/output types are computed
//!   **precisely** by threading type variables through the pipeline (fresh-instantiate each stage, unify
//!   each result with the next parameter), so `wrap : a -> List a ; head : List b -> b` composes to the
//!   exact `a -> a`, not the imprecise `a -> b`.
//! - **effects** — the *union* of every stage's declared effects (a pipeline performs all of them).
//! - **capabilities** — the union, likewise.
//! - **termination** — `always` only if every stage is `always`, else `unknown` (conservative).
//! - **complexity** — a coarse upper bound: the maximum stage complexity (sequential composition's
//!   dominant term), or `unknown` if any stage's is unrecognized. This stays coarse on purpose: an
//!   *exact* composition needs each stage's **output-size relation** (how its result size depends on its
//!   input size) so a cost in terms of one stage's input can be re-expressed in terms of the pipeline's
//!   input — metadata the record schema doesn't carry yet (a v0.3 item). The tempting shortcut (a stage
//!   that returns a scalar makes everything downstream `O(1)`) is *unsound*: a downstream cost can depend
//!   on the scalar's value, not just its size (`length ; factorial`), so it would under-report. The
//!   max-bound is a safe over-approximation for non-expanding pipelines; we keep it rather than tighten
//!   it unsoundly.
//!
//! So an assembled pipeline becomes as described as a leaf — the precondition for principle 4
//! ("assemble, don't write") to yield artifacts that are themselves verifiable. Scope (v0.1): each stage
//! is a unary function (the pipeline threads one value); arity ≠ 1 makes the chain non-composable.

use serde_json::Value as J;
use std::collections::{BTreeSet, HashMap};

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

// --- precise type-variable propagation ------------------------------------------------------------
//
// The coarse `CTy` check above decides *composability*; this computes the composite's precise input and
// output **types** by threading polymorphic type variables across the stages. Each stage is instantiated
// with fresh variables (so two stages that both happen to name a variable `a` don't alias), then each
// stage's result is unified with the next stage's parameter; the composite input is the first stage's
// parameter and the composite output the last stage's result, read under the resulting substitution. So
// `wrap : a -> List a ; head : List b -> b` composes to `a -> a` (not the imprecise `a -> b`).

/// The forall-bound variables (if any) and the `fn` type node of a record's signature.
fn fn_type_with_vars(record: &J) -> Option<(Vec<String>, &J)> {
    let t = record.pointer("/signature/type")?;
    let (vars, body) = if t.get("kind").and_then(|k| k.as_str()) == Some("forall") {
        let vs = t
            .get("vars")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
            .unwrap_or_default();
        (vs, t.get("body")?)
    } else {
        (Vec::new(), t)
    };
    (body.get("kind").and_then(|k| k.as_str()) == Some("fn")).then_some((vars, body))
}

/// Rename type variables in a type-expr per `ren`.
fn rename_tyvars(t: &J, ren: &HashMap<String, String>) -> J {
    match t {
        J::Object(m) => {
            if m.get("kind").and_then(|k| k.as_str()) == Some("var") {
                if let Some(f) = m.get("name").and_then(|n| n.as_str()).and_then(|n| ren.get(n)) {
                    let mut o = m.clone();
                    o.insert("name".into(), J::String(f.clone()));
                    return J::Object(o);
                }
            }
            J::Object(m.iter().map(|(k, v)| (k.clone(), rename_tyvars(v, ren))).collect())
        }
        J::Array(a) => J::Array(a.iter().map(|v| rename_tyvars(v, ren)).collect()),
        o => o.clone(),
    }
}

/// Follow a variable's binding chain to a non-variable (or an unbound variable).
fn resolve(t: &J, subst: &HashMap<String, J>) -> J {
    let mut cur = t.clone();
    while cur.get("kind").and_then(|k| k.as_str()) == Some("var") {
        match cur.get("name").and_then(|n| n.as_str()).and_then(|n| subst.get(n)) {
            Some(next) => cur = next.clone(),
            None => break,
        }
    }
    cur
}

/// Unify two type-exprs, extending `subst` (variable name → type-expr). Returns false on a clash.
fn unify(a: &J, b: &J, subst: &mut HashMap<String, J>) -> bool {
    let a = resolve(a, subst);
    let b = resolve(b, subst);
    let (ak, bk) = (a.get("kind").and_then(|k| k.as_str()), b.get("kind").and_then(|k| k.as_str()));
    match (ak, bk) {
        (Some("var"), Some("var")) if a.get("name") == b.get("name") => true,
        (Some("var"), _) => {
            subst.insert(a.get("name").and_then(|n| n.as_str()).unwrap_or_default().to_string(), b);
            true
        }
        (_, Some("var")) => {
            subst.insert(b.get("name").and_then(|n| n.as_str()).unwrap_or_default().to_string(), a);
            true
        }
        (Some("builtin"), Some("builtin")) => a.get("name") == b.get("name"),
        (Some("apply"), Some("apply")) => {
            a.pointer("/ctor/name") == b.pointer("/ctor/name")
                && match (a.get("args").and_then(|x| x.as_array()), b.get("args").and_then(|x| x.as_array())) {
                    (Some(aa), Some(ba)) if aa.len() == ba.len() => aa.iter().zip(ba).all(|(x, y)| unify(x, y, subst)),
                    _ => false,
                }
        }
        (Some("fn"), Some("fn")) => {
            let params_ok = match (a.get("params").and_then(|x| x.as_array()), b.get("params").and_then(|x| x.as_array())) {
                (Some(ap), Some(bp)) if ap.len() == bp.len() => ap.iter().zip(bp).all(|(x, y)| unify(x, y, subst)),
                _ => false,
            };
            params_ok
                && match (a.get("result"), b.get("result")) {
                    (Some(ar), Some(br)) => unify(ar, br, subst),
                    _ => false,
                }
        }
        _ => false,
    }
}

/// Apply a substitution fully to a type-expr.
fn apply_subst(t: &J, subst: &HashMap<String, J>) -> J {
    let r = resolve(t, subst);
    match &r {
        J::Object(m) => J::Object(m.iter().map(|(k, v)| (k.clone(), apply_subst(v, subst))).collect()),
        J::Array(a) => J::Array(a.iter().map(|v| apply_subst(v, subst)).collect()),
        o => o.clone(),
    }
}

/// Rename the free type variables of `(input, output)` to canonical `a, b, c, …` in order of appearance,
/// so the fresh `_tN` instantiation names don't leak into the reported composite type.
fn canonicalize_free_vars(input: &J, output: &J) -> (J, J) {
    fn collect(t: &J, order: &mut Vec<String>) {
        match t {
            J::Object(m) => {
                if m.get("kind").and_then(|k| k.as_str()) == Some("var") {
                    if let Some(n) = m.get("name").and_then(|n| n.as_str()) {
                        if !order.iter().any(|x| x == n) {
                            order.push(n.to_string());
                        }
                    }
                }
                for v in m.values() {
                    collect(v, order);
                }
            }
            J::Array(a) => a.iter().for_each(|v| collect(v, order)),
            _ => {}
        }
    }
    let mut order = Vec::new();
    collect(input, &mut order);
    collect(output, &mut order);
    let ren: HashMap<String, String> = order
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let name = (b'a' + (i % 26) as u8) as char;
            (v.clone(), if i < 26 { name.to_string() } else { format!("{name}{}", i / 26) })
        })
        .collect();
    (rename_tyvars(input, &ren), rename_tyvars(output, &ren))
}

/// Precise composite `(input, output)` types for a unary pipeline. `None` if a stage isn't a unary
/// function or the precise unification clashes — the caller then keeps the coarse verbatim types
/// (composability itself is decided by the coarse structural check, so this never changes the verdict).
fn propagate_types(records: &[J]) -> Option<(J, J)> {
    let mut counter = 0usize;
    let mut stages: Vec<(J, J)> = Vec::new();
    for r in records {
        let (vars, fnt) = fn_type_with_vars(r)?;
        let mut ren = HashMap::new();
        for v in &vars {
            ren.insert(v.clone(), format!("_t{counter}"));
            counter += 1;
        }
        let param = fnt.get("params").and_then(|p| p.as_array()).and_then(|p| p.first())?;
        let result = fnt.get("result")?;
        stages.push((rename_tyvars(param, &ren), rename_tyvars(result, &ren)));
    }
    let mut subst: HashMap<String, J> = HashMap::new();
    for i in 0..stages.len() - 1 {
        if !unify(&stages[i].1, &stages[i + 1].0, &mut subst) {
            return None;
        }
    }
    let input = apply_subst(&stages[0].0, &subst);
    let output = apply_subst(&stages.last().unwrap().1, &subst);
    Some(canonicalize_free_vars(&input, &output))
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

    // Precise composite type via type-variable propagation; fall back to the verbatim endpoint types if
    // the precise unification can't run (it never changes the composability verdict above).
    let (input_type, output_type) = match propagate_types(records) {
        Some((i, o)) => (Some(i), Some(o)),
        None => (
            fts[0].get("params").unwrap().as_array().unwrap().first().cloned(),
            fts.last().unwrap().get("result").cloned(),
        ),
    };

    CompositionMetadata {
        composable: true,
        reason: format!("a {}-stage pipeline composes end to end", records.len()),
        input_type,
        output_type,
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

    #[test]
    fn type_variables_thread_through_pipeline() {
        // wrap : a -> List a ; head : List b -> b  → composite a -> a (the variable threads end to end).
        // Both records name the variable `a`; fresh instantiation keeps them from aliasing, and
        // unification then ties the composite output back to its input.
        let wrap = json!({ "signature": { "type": {
            "kind": "forall", "vars": ["a"], "body": { "kind": "fn",
                "params": [{ "kind": "var", "name": "a" }],
                "result": { "kind": "apply", "ctor": { "kind": "builtin", "name": "List" }, "args": [{ "kind": "var", "name": "a" }] } } } } });
        let head = json!({ "signature": { "type": {
            "kind": "forall", "vars": ["a"], "body": { "kind": "fn",
                "params": [{ "kind": "apply", "ctor": { "kind": "builtin", "name": "List" }, "args": [{ "kind": "var", "name": "a" }] }],
                "result": { "kind": "var", "name": "a" } } } } });
        let m = compose(&[wrap, head]);
        assert!(m.composable, "{}", m.reason);
        let (iv, ov) = (m.input_type.as_ref().unwrap(), m.output_type.as_ref().unwrap());
        assert_eq!(iv.get("kind").and_then(|k| k.as_str()), Some("var"));
        assert_eq!(ov.get("kind").and_then(|k| k.as_str()), Some("var"));
        assert_eq!(iv.get("name"), ov.get("name"), "input and output are the same variable (a -> a, not a -> b)");
    }
}
