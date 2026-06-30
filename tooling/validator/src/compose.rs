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
//! - **complexity** — **precise** when every stage carries the v0.3 `cost` metadata (a `time` class and
//!   an `output_size` relation), else a coarse upper bound. The precise path threads the value's size
//!   through the pipeline as a polynomial degree in the input `n` and substitutes each stage's cost at
//!   its actual input size (`precise_complexity`): a stage costing `O(m^t)` on a size-`Θ(n^d)` input
//!   costs `O(n^{t·d})`, and its `output_size` updates `d`. This is **sound under expansion**, which the
//!   coarse max is not — a stage that turns `n` elements into `n²` followed by `O(m²)`-on-its-input work
//!   is `O(n⁴)`, which `max(O(n²), O(n²))` misses. It also *tightens* collapse pipelines (after a
//!   constant-size output, downstream size-measured costs are `O(1)`). The size-collapse shortcut is kept
//!   sound by the `measure` field: a **value**-measured stage (cost tracks a number's magnitude, not a
//!   structural size — `length ; factorial`) can't substitute, so the whole composite falls back to the
//!   coarse max. Without `cost`, the coarse maximum stage complexity (a safe bound for non-expanding
//!   pipelines) is reported, with `complexity_basis` recording which path was taken.
//!
//! So an assembled pipeline becomes as described as a leaf — the precondition for principle 4
//! ("assemble, don't write") to yield artifacts that are themselves verifiable.
//!
//! **Multi-argument stages.** A pipeline threads one value, but a stage need not be unary: the threaded
//! value feeds each stage's **first** parameter, and the stage's **remaining** parameters become
//! additional inputs of the *composite*. So `f : a -> b ; g : (b, c) -> d` composes to `(a, c) -> d` — a
//! pipeline of multi-argument stages is itself a multi-argument function, with `input_type` the primary
//! (threaded) input and `extra_input_types` the auxiliaries gathered left to right. A unary pipeline is
//! the special case with no extras. Composability still requires each stage's result to fit the *next*
//! stage's first parameter; the auxiliaries are free composite inputs and constrain nothing. (Complexity
//! is measured in the size of the primary/threaded input, auxiliaries held constant — the single-variable
//! `cost` model.) A nullary (zero-parameter) stage has nothing to thread and is non-composable.

use serde_json::Value as J;
use std::collections::{BTreeSet, HashMap};

/// The derived metadata of a composed pipeline.
#[derive(Debug, Clone)]
pub struct CompositionMetadata {
    pub composable: bool,
    pub reason: String,
    /// The composite's primary (threaded) input type — `f1`'s first parameter — and output type (`fn`'s
    /// result), as type-exprs.
    pub input_type: Option<J>,
    pub output_type: Option<J>,
    /// Auxiliary input types: the non-first parameters of multi-argument stages, gathered left to right.
    /// Empty for a unary pipeline. The composite is `(input_type, extra_input_types…) -> output_type`.
    pub extra_input_types: Vec<J>,
    pub effects: Vec<String>,
    pub capabilities: Vec<String>,
    pub terminates: String,
    pub complexity: String,
    /// How `complexity` was derived: `"precise (output-size substitution)"` when every stage carries
    /// size-measured `cost` metadata, else `"coarse upper bound"` (the max-rank fallback).
    pub complexity_basis: String,
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

/// Rename the free type variables of `types` to canonical `a, b, c, …` in order of appearance (across the
/// whole list), so the fresh `_tN` instantiation names don't leak into the reported composite type.
fn canonicalize_free_vars(types: &[J]) -> Vec<J> {
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
    for t in types {
        collect(t, &mut order);
    }
    let ren: HashMap<String, String> = order
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let name = (b'a' + (i % 26) as u8) as char;
            (v.clone(), if i < 26 { name.to_string() } else { format!("{name}{}", i / 26) })
        })
        .collect();
    types.iter().map(|t| rename_tyvars(t, &ren)).collect()
}

/// Precise composite types for a pipeline: the primary (threaded) input, the auxiliary inputs (each
/// stage's non-first parameters, left to right), and the output — threading polymorphic variables across
/// the stages (each result unified with the next stage's *first* parameter). `None` if a stage isn't a
/// function, is nullary, or the precise unification clashes — the caller then keeps the coarse verbatim
/// types (composability itself is decided by the coarse structural check, so this never changes the verdict).
fn propagate_types(records: &[J]) -> Option<(J, Vec<J>, J)> {
    let mut counter = 0usize;
    // Per stage: (first/threaded parameter, auxiliary parameters, result).
    let mut stages: Vec<(J, Vec<J>, J)> = Vec::new();
    for r in records {
        let (vars, fnt) = fn_type_with_vars(r)?;
        let mut ren = HashMap::new();
        for v in &vars {
            ren.insert(v.clone(), format!("_t{counter}"));
            counter += 1;
        }
        let params = fnt.get("params").and_then(|p| p.as_array())?;
        let first = rename_tyvars(params.first()?, &ren);
        let extras: Vec<J> = params[1..].iter().map(|p| rename_tyvars(p, &ren)).collect();
        let result = rename_tyvars(fnt.get("result")?, &ren);
        stages.push((first, extras, result));
    }
    let mut subst: HashMap<String, J> = HashMap::new();
    for i in 0..stages.len() - 1 {
        if !unify(&stages[i].2, &stages[i + 1].0, &mut subst) {
            return None;
        }
    }
    let input = apply_subst(&stages[0].0, &subst);
    let extras: Vec<J> = stages.iter().flat_map(|(_, ex, _)| ex).map(|e| apply_subst(e, &subst)).collect();
    let output = apply_subst(&stages.last().unwrap().2, &subst);
    // Canonicalize free variables across input + auxiliaries + output, then split them back out.
    let mut all = Vec::with_capacity(extras.len() + 2);
    all.push(input);
    all.extend(extras);
    all.push(output);
    let canon = canonicalize_free_vars(&all);
    let output = canon.last().cloned()?;
    let input = canon.first().cloned()?;
    let extras = canon[1..canon.len() - 1].to_vec();
    Some((input, extras, output))
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

// --- precise complexity via output-size substitution ----------------------------------------------
//
// The coarse `complexity_of` above is the maximum stage class — a sound upper bound only when no stage
// *expands* its input. For an expanding pipeline it UNDER-reports: a stage that turns an `n`-element list
// into `n²` elements, followed by an `O(m²)`-on-its-input stage, is `O(n⁴)`, which `max(O(n²), O(n²))`
// misses entirely. The fix is to know each stage's **output-size relation** (the v0.3 `cost` field) and
// thread the size through the pipeline: track the current value's size as a polynomial degree in the
// pipeline input `n`; a stage costing `O(m^t · (log m)^l)` on an input of size `Θ(n^d)` costs
// `O(n^{t·d} · (log n)^l)`, and its `output_size` updates `d`. The composite is the max term. This is
// exact for the supported classes and, unlike the coarse max, sound under expansion. It also *tightens*
// collapse pipelines (after a stage whose output is constant-size, `d = 0`, so a size-measured downstream
// cost is `O(1)`). Only **size-measured** costs substitute soundly — a value-measured stage (its cost
// tracks a number's magnitude, not its size) makes the whole pipeline fall back to the coarse bound.

/// A complexity class as `O(n^deg · (log n)^logs)`. Covers the schema's `time` classes: O(1)=(0,0),
/// O(log n)=(0,1), O(n)=(1,0), O(n log n)=(1,1), O(n^2)=(2,0), O(n^2 log n)=(2,1), O(n^3)=(3,0).
#[derive(Clone, Copy, PartialEq, Eq)]
struct Cost {
    deg: u32,
    logs: u32,
}

fn parse_time_class(s: &str) -> Option<Cost> {
    Some(match s {
        "O(1)" => Cost { deg: 0, logs: 0 },
        "O(log n)" => Cost { deg: 0, logs: 1 },
        "O(n)" => Cost { deg: 1, logs: 0 },
        "O(n log n)" => Cost { deg: 1, logs: 1 },
        "O(n^2)" => Cost { deg: 2, logs: 0 },
        "O(n^2 log n)" => Cost { deg: 2, logs: 1 },
        "O(n^3)" => Cost { deg: 3, logs: 0 },
        _ => return None,
    })
}

/// Asymptotic order on `Cost`: by polynomial degree, then by log-factor count.
fn cost_max(a: Cost, b: Cost) -> Cost {
    if (a.deg, a.logs) >= (b.deg, b.logs) {
        a
    } else {
        b
    }
}

fn cost_to_string(c: Cost) -> String {
    let np = match c.deg {
        0 => String::new(),
        1 => "n".to_string(),
        d => format!("n^{d}"),
    };
    let lp = match c.logs {
        0 => String::new(),
        1 => "log n".to_string(),
        l => format!("(log n)^{l}"),
    };
    match (np.is_empty(), lp.is_empty()) {
        (true, true) => "O(1)".to_string(),
        (false, true) => format!("O({np})"),
        (true, false) => format!("O({lp})"),
        (false, false) => format!("O({np} {lp})"),
    }
}

/// Precise composite complexity by substituting each stage's input size through the pipeline. Returns
/// `None` when any stage lacks usable `cost` metadata (unknown `time`/`output_size`, or a value-measured
/// cost) — the caller then reports the coarse bound. Never returns a *smaller-than-true* class: a stage's
/// input-size degree only grows from real expansion, so the contributed cost is an exact substitution.
fn precise_complexity(records: &[J]) -> Option<String> {
    let mut cur_deg: u32 = 1; // the pipeline input has size Θ(n) = degree 1
    let mut worst = Cost { deg: 0, logs: 0 };
    for r in records {
        let cost = r.pointer("/signature/cost")?;
        // Value-measured costs track a number's magnitude, not a structural size, so size substitution
        // through the pipeline doesn't apply — bail to the coarse bound for the whole composite.
        if cost.get("measure").and_then(|m| m.as_str()).unwrap_or("size") != "size" {
            return None;
        }
        let time = parse_time_class(cost.get("time")?.as_str()?)?;
        // Cost on an input of size Θ(n^cur_deg): a constant-size input (cur_deg 0) makes any size-measured
        // cost O(1); otherwise the poly degree multiplies and the log-factor count carries through.
        let contributed = if cur_deg == 0 {
            Cost { deg: 0, logs: 0 }
        } else {
            Cost { deg: time.deg.saturating_mul(cur_deg), logs: time.logs }
        };
        worst = cost_max(worst, contributed);
        cur_deg = match cost.get("output_size")?.as_str()? {
            "constant" => 0,
            "preserving" | "bounded" => cur_deg, // bounded ≤ preserving; keep cur_deg as the size upper bound
            "quadratic" => cur_deg.saturating_mul(2),
            "cubic" => cur_deg.saturating_mul(3),
            _ => return None, // "unknown" (or anything unrecognized) — can't substitute soundly
        };
    }
    Some(cost_to_string(worst))
}

/// Derive the metadata of the sequential pipeline `records[0]; records[1]; …` (each stage applied to the
/// previous stage's single result). Never errors — composability and the reason are in the returned value.
pub fn compose(records: &[J]) -> CompositionMetadata {
    let unknown = |reason: String| CompositionMetadata {
        composable: false,
        reason,
        input_type: None,
        output_type: None,
        extra_input_types: vec![],
        effects: vec![],
        capabilities: vec![],
        terminates: "unknown".to_string(),
        complexity: "unknown".to_string(),
        complexity_basis: "coarse upper bound".to_string(),
    };
    if records.is_empty() {
        return unknown("empty pipeline".to_string());
    }
    // Every stage must be a function with at least one parameter — the first is the threaded value; any
    // remaining parameters become auxiliary inputs of the composite. A nullary stage has nothing to thread.
    let mut fts = Vec::new();
    for (i, r) in records.iter().enumerate() {
        match fn_type(r) {
            Some(ft) if ft.get("params").and_then(|p| p.as_array()).map_or(0, |a| a.len()) >= 1 => fts.push(ft),
            Some(_) => return unknown(format!("stage {i} is nullary (a pipeline needs a value to thread)")),
            None => return unknown(format!("stage {i} is not a function")),
        }
    }
    // Each stage's result type must fit the next stage's FIRST (threaded) parameter type.
    for i in 0..fts.len() - 1 {
        let result = fts[i].get("result").unwrap();
        let next_param = &fts[i + 1].get("params").unwrap().as_array().unwrap()[0];
        if !compatible(&coarse(result), &coarse(next_param)) {
            return unknown(format!(
                "stage {i}'s output type does not fit stage {}'s threaded input type",
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

    // Precise composite type (primary input, auxiliary inputs, output) via type-variable propagation; fall
    // back to the verbatim types if the precise unification can't run (never changes the verdict above).
    let (input_type, extra_input_types, output_type) = match propagate_types(records) {
        Some((i, ex, o)) => (Some(i), ex, Some(o)),
        None => {
            let extras: Vec<J> = fts
                .iter()
                .flat_map(|ft| ft.get("params").and_then(|p| p.as_array()).map(|a| a[1..].to_vec()).unwrap_or_default())
                .collect();
            (
                fts[0].get("params").unwrap().as_array().unwrap().first().cloned(),
                extras,
                fts.last().unwrap().get("result").cloned(),
            )
        }
    };

    // Precise complexity when every stage carries size-measured `cost` metadata; otherwise the coarse
    // max bound. The precise path is exact and sound under expansion, where the coarse max under-reports.
    let (complexity, complexity_basis) = match precise_complexity(records) {
        Some(c) => (c, "precise (output-size substitution)".to_string()),
        None => (complexity_of(&complexities), "coarse upper bound".to_string()),
    };

    CompositionMetadata {
        composable: true,
        reason: format!("a {}-stage pipeline composes end to end", records.len()),
        input_type,
        output_type,
        extra_input_types,
        effects: effects.into_iter().collect(),
        capabilities: capabilities.into_iter().collect(),
        terminates: if terms_all_always { "always" } else { "unknown" }.to_string(),
        complexity,
        complexity_basis,
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
    fn binary_stage_output_must_fit_next_threaded_input() {
        // add : (int,int) -> int is now an allowed (binary) stage, but its int output cannot feed
        // length's `List` parameter — so [add, length] is still non-composable, for a TYPE reason now,
        // not an arity one.
        let m = compose(&[load("add.json"), load("length.json")]);
        assert!(!m.composable, "int output must not fit a List parameter");
    }

    #[test]
    fn binary_stage_threads_first_param_and_adds_an_auxiliary_input() {
        // f : int -> int ; g : (int, bool) -> int. The threaded int feeds g's first parameter; g's `bool`
        // second parameter becomes an auxiliary input of the composite — so the composite is (int, bool) -> int.
        let intt = json!({ "kind": "builtin", "name": "int" });
        let boolt = json!({ "kind": "builtin", "name": "bool" });
        let f = json!({ "signature": { "type": { "kind": "fn", "params": [intt.clone()], "result": intt.clone() },
            "effects": [], "capabilities": [], "terminates": "always", "complexity": "O(1)" } });
        let g = json!({ "signature": { "type": { "kind": "fn", "params": [intt.clone(), boolt.clone()], "result": intt.clone() },
            "effects": [], "capabilities": [], "terminates": "always", "complexity": "O(n)" } });
        let m = compose(&[f, g]);
        assert!(m.composable, "{}", m.reason);
        assert_eq!(coarse(m.input_type.as_ref().unwrap()), CTy::Int);
        assert_eq!(coarse(m.output_type.as_ref().unwrap()), CTy::Int);
        assert_eq!(m.extra_input_types.len(), 1, "one auxiliary input from g's second parameter");
        assert_eq!(coarse(&m.extra_input_types[0]), CTy::Bool);
    }

    #[test]
    fn lone_binary_stage_composes_to_its_own_signature() {
        // A single binary stage is a one-stage pipeline: both parameters are composite inputs (the first
        // is the primary, the second auxiliary). add : (int,int) -> int composes to (int, int) -> int.
        let m = compose(&[load("add.json")]);
        assert!(m.composable, "{}", m.reason);
        assert_eq!(coarse(m.input_type.as_ref().unwrap()), CTy::Int);
        assert_eq!(m.extra_input_types.len(), 1);
        assert_eq!(coarse(m.output_type.as_ref().unwrap()), CTy::Int);
    }

    #[test]
    fn nullary_stage_is_not_composable() {
        // A zero-parameter stage has no value to thread.
        let nilfn = json!({ "signature": { "type": { "kind": "fn", "params": [], "result": { "kind": "builtin", "name": "int" } },
            "effects": [], "capabilities": [], "terminates": "always", "complexity": "O(1)" } });
        let m = compose(&[nilfn]);
        assert!(!m.composable, "a nullary stage has nothing to thread");
    }

    #[test]
    fn auxiliary_input_threads_type_variables() {
        // f : a -> List a ; g : (List a, b) -> b  → composite (a, b) -> b: the threaded `List a` ties to
        // g's first param, and g's polymorphic `b` second param surfaces as an auxiliary input equal to the
        // output. Fresh instantiation keeps the two records' `a`/`b` from aliasing.
        let f = json!({ "signature": { "type": { "kind": "forall", "vars": ["a"], "body": { "kind": "fn",
            "params": [{ "kind": "var", "name": "a" }],
            "result": { "kind": "apply", "ctor": { "kind": "builtin", "name": "List" }, "args": [{ "kind": "var", "name": "a" }] } } } } });
        let g = json!({ "signature": { "type": { "kind": "forall", "vars": ["a", "b"], "body": { "kind": "fn",
            "params": [{ "kind": "apply", "ctor": { "kind": "builtin", "name": "List" }, "args": [{ "kind": "var", "name": "a" }] },
                       { "kind": "var", "name": "b" }],
            "result": { "kind": "var", "name": "b" } } } } });
        let m = compose(&[f, g]);
        assert!(m.composable, "{}", m.reason);
        assert_eq!(m.extra_input_types.len(), 1);
        // The auxiliary input and the output are the same variable (g's `b`).
        assert_eq!(m.extra_input_types[0].get("kind").and_then(|k| k.as_str()), Some("var"));
        assert_eq!(m.extra_input_types[0].get("name"), m.output_type.as_ref().unwrap().get("name"));
    }

    // A synthetic costed stage `param -> result` carrying both the coarse `complexity` and the v0.3 `cost`.
    fn costed(param: J, result: J, complexity: &str, time: &str, output_size: &str, measure: &str) -> J {
        json!({ "signature": {
            "type": { "kind": "fn", "params": [param], "result": result },
            "effects": [], "capabilities": [], "terminates": "always",
            "complexity": complexity,
            "cost": { "time": time, "output_size": output_size, "measure": measure } } })
    }
    fn list_int() -> J {
        json!({ "kind": "apply", "ctor": { "kind": "builtin", "name": "List" }, "args": [{ "kind": "builtin", "name": "int" }] })
    }
    fn int_t() -> J {
        json!({ "kind": "builtin", "name": "int" })
    }

    #[test]
    fn precise_complexity_is_sound_under_expansion() {
        // An EXPANDING stage (n elements -> n² elements, O(n²) to build) feeding an O(m²)-on-its-input
        // stage is O(n⁴). The coarse max — max(O(n²), O(n²)) = O(n²) — under-reports; the precise path
        // substitutes the n²-size input and reports O(n^4).
        let expand = costed(list_int(), list_int(), "O(n^2)", "O(n^2)", "quadratic", "size");
        let square = costed(list_int(), list_int(), "O(n^2)", "O(n^2)", "preserving", "size");
        let m = compose(&[expand, square]);
        assert!(m.composable);
        assert_eq!(m.complexity, "O(n^4)", "expanding pipeline is O(n^4), not the coarse max O(n^2)");
        assert_eq!(m.complexity_basis, "precise (output-size substitution)");
    }

    #[test]
    fn precise_complexity_tightens_after_a_size_collapse() {
        // A size-collapsing stage (List -> scalar) makes a downstream size-measured cost O(1), so the
        // composite is O(n), tighter than the coarse max(O(n), O(n^2)) = O(n^2).
        let collapse = costed(list_int(), int_t(), "O(n)", "O(n)", "constant", "size");
        let downstream = costed(int_t(), int_t(), "O(n^2)", "O(n^2)", "preserving", "size");
        let m = compose(&[collapse, downstream]);
        assert!(m.composable);
        assert_eq!(m.complexity, "O(n)", "after a constant-size output, a size-measured cost is O(1)");
        assert_eq!(m.complexity_basis, "precise (output-size substitution)");
    }

    #[test]
    fn value_measured_stage_falls_back_to_coarse() {
        // The collapsed scalar's VALUE (not its size) drives the downstream cost — size substitution is
        // unsound, so the whole composite falls back to the coarse max bound (O(n^2)), not a false O(n).
        let collapse = costed(list_int(), int_t(), "O(n)", "O(n)", "constant", "size");
        let value_cost = costed(int_t(), int_t(), "O(n^2)", "O(n^2)", "preserving", "value");
        let m = compose(&[collapse, value_cost]);
        assert!(m.composable);
        assert_eq!(m.complexity, "O(n^2)", "a value-measured stage forces the coarse bound");
        assert_eq!(m.complexity_basis, "coarse upper bound");
    }

    #[test]
    fn missing_cost_metadata_uses_the_coarse_bound() {
        // reverse/length carry no `cost` field, so the composite is the coarse max bound (the basis says so).
        let m = compose(&[load("reverse.json"), load("length.json")]);
        assert!(m.composable);
        assert_eq!(m.complexity_basis, "coarse upper bound");
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
