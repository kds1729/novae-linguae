//! Inductive proof backend — proving laws over **unbounded recursive structures**, the rung above the
//! first-order SMT backend (prove.rs). That backend handles the `Int`/`Bool` fragment and honestly
//! reports list/recursion laws UNSUPPORTED, because a plain SMT query over recursively-defined functions
//! and a universal quantifier is undecidable: the solver will not invent the induction.
//!
//! So we supply the induction principle and let the solver discharge each case. For a goal
//! `forall xs. P(xs)` with `xs` a list, structural induction over `Lst = nil | cons(Int, Lst)` is:
//!
//! - **base** — prove `P(nil)`;
//! - **step** — for fresh `h : Int`, `t : Lst`, *assume* `P(t)` (the induction hypothesis) and prove
//!   `P(cons(h, t))`.
//!
//! Each case becomes an SMT-LIB obligation: the list operations the goal uses (`length`, `append`,
//! `reverse`, `map`, `filter`, …) are emitted as z3 `define-fun-rec` definitions over the `Lst`
//! datatype, the case's substitution is applied, and we assert the *negation* of the goal (plus the IH,
//! for the step). `unsat` on both ⇒ **PROVED-BY-INDUCTION**; the solver closes each case by unfolding
//! the recursive definitions one level and using the IH.
//!
//! **Lemma discovery (Layer A).** Where one unfold + IH is not enough — a law that needs an auxiliary
//! lemma, classically `reverse(reverse(xs)) = xs`, whose step needs `reverse(append(as, bs)) =
//! append(reverse(bs), reverse(as))` — [`prove_by_induction_with_lemmas`] selects relevant lemmas from
//! a curated catalog ([`crate::lemmas`]), **proves each one by induction first** (recursively: the
//! lemmas may depend on one another — `reverse_append` rests on `append_assoc` + `append_nil`), and
//! re-runs the stalled obligation with the proved lemmas added as universally-quantified axioms. A
//! lemma is assumed only after it is itself discharged, so a false goal can never be closed: this is
//! exactly as honest as the bare engine, just able to reach further. When no catalog lemma helps, the
//! verdict is still **UNKNOWN** — never a false proof. (Relevance is gated by the goal's *prelude
//! closure* so an unrelated lemma's recursive definition can't derail the solver into a timeout.)
//!
//! The emitted scripts together **are the proof certificate**: the goal's base + step (assuming the
//! lemmas as axioms) plus each lemma's own base + step. Any SMT solver re-checks the whole tree —
//! every obligation `unsat` on its own — so the induction is re-checkable, not trusted (principles 3,
//! 5). The generalizable follow-on (Layer B) is *theory exploration*: conjecturing lemmas by
//! enumerating terms over the goal's operations and testing them before proof. The catalog is the seam.
//!
//! Honest scope (v0.1). Element sort is `Int`; the list is `Lst`. Supported operations: `length`,
//! `append`, `reverse`, `map`, `filter`, `cons`, `head`, `tail`, `null`, plus the `Int`/`Bool`
//! element algebra. `map`/`filter` take at most one function/predicate, modelled as `id` or a single
//! uninterpreted symbol (so `forall f xs. length(map(f, xs)) = length(xs)` is provable with `f`
//! uninterpreted). `foldl`/`foldr`, `self`, lists-of-lists, and multiple distinct function arguments
//! are out of scope and reported UNSUPPORTED.

use anyhow::{anyhow, bail, Result};
use serde_json::Value as J;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InductionOutcome {
    /// Both the base and step obligations were discharged (`unsat`): the law holds by induction.
    Proved,
    /// Discharged, but only after assuming one or more auxiliary lemmas (each itself proved by
    /// induction first). Carries the names of the lemmas used, in dependency order.
    ProvedWithLemmas(Vec<String>),
    /// A case was satisfiable — the law does not hold (or the chosen induction does not close it).
    Failed(String),
    /// The solver could not decide a case (typically the step needs an auxiliary lemma we lack).
    Unknown,
    /// The goal is outside the supported recursive fragment — not attempted.
    Unsupported(String),
    /// No solver binary was found.
    NoSolver,
}

/// The base and step SMT-LIB obligations — together, the re-checkable induction certificate. When the
/// proof needed auxiliary lemmas, `base`/`step` are the *augmented* obligations (the lemmas asserted as
/// quantified axioms) and `lemmas` holds each lemma's own discharge, in dependency order — so the whole
/// proof tree re-checks: each lemma's base/step is unsat on its own, then the goal's base/step is unsat
/// given the lemmas as axioms.
pub struct InductionCertificate {
    pub var: String,
    pub base: String,
    pub step: String,
    pub lemmas: Vec<LemmaCertificate>,
}

/// A proved auxiliary lemma's obligations, recorded so the certificate stays self-contained.
#[derive(Clone)]
pub struct LemmaCertificate {
    pub name: String,
    pub var: String,
    pub base: String,
    pub step: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sort3 {
    Int,
    Bool,
    Lst,
    /// A unary function `Int -> Int` (the argument of `map`).
    Func,
    /// A unary predicate `Int -> Bool` (the argument of `filter`).
    Pred,
}

/// How the single `map` function (resp. `filter` predicate) is modelled.
#[derive(Debug, Clone, PartialEq, Eq)]
enum FnModel {
    None,
    Identity,
    /// An uninterpreted symbol named after the quantified variable it stands for.
    Uninterpreted(String),
}

// --- AST helpers (shared shapes with prove.rs, kept local to avoid cross-module coupling) ----------

fn head_op(node: &J) -> Option<String> {
    if let Some(op) = node.get("op").and_then(|o| o.as_str()) {
        return Some(op.to_string());
    }
    if node.pointer("/fn/kind").and_then(|k| k.as_str()) == Some("var") {
        return node.pointer("/fn/name").and_then(|n| n.as_str()).map(String::from);
    }
    None
}

fn args_of(node: &J) -> Vec<&J> {
    node.get("args").and_then(|a| a.as_array()).map(|a| a.iter().collect()).unwrap_or_default()
}

fn var_name(node: &J) -> Option<&str> {
    if node.get("kind").and_then(|k| k.as_str()) == Some("var") {
        node.get("name").and_then(|n| n.as_str())
    } else {
        None
    }
}

fn references_self(node: &J) -> bool {
    if var_name(node) == Some("self") || node.get("op").and_then(|o| o.as_str()) == Some("self") {
        return true;
    }
    for key in ["body", "value", "scrutinee", "fn"] {
        if let Some(c) = node.get(key) {
            if references_self(c) {
                return true;
            }
        }
    }
    if let Some(arr) = node.get("args").and_then(|a| a.as_array()) {
        if arr.iter().any(references_self) {
            return true;
        }
    }
    false
}

const LIST_OPS: &[&str] = &["length", "append", "reverse", "map", "filter", "cons", "head", "tail", "null"];

// --- sort inference -------------------------------------------------------------------------------

/// Infer each quantified variable's sort from its usage in the predicate.
fn infer_sorts(pred: &J, vars: &[String]) -> Result<BTreeMap<String, Sort3>> {
    let mut sorts: BTreeMap<String, Option<Sort3>> = vars.iter().map(|v| (v.clone(), None)).collect();
    let set: Vec<String> = vars.to_vec();
    walk_sorts(pred, &set, &mut sorts)?;
    // Default any still-unknown var to Int (the element sort).
    Ok(sorts.into_iter().map(|(k, v)| (k, v.unwrap_or(Sort3::Int))).collect())
}

fn assign(name: &str, s: Sort3, vars: &[String], sorts: &mut BTreeMap<String, Option<Sort3>>) -> Result<()> {
    if vars.iter().any(|v| v == name) {
        match sorts.get(name).copied().flatten() {
            Some(existing) if existing != s => bail!("variable `{name}` used at conflicting sorts"),
            _ => {
                sorts.insert(name.to_string(), Some(s));
            }
        }
    }
    Ok(())
}

fn walk_sorts(node: &J, vars: &[String], sorts: &mut BTreeMap<String, Option<Sort3>>) -> Result<()> {
    if node.get("kind").and_then(|k| k.as_str()) == Some("app") {
        let op = head_op(node).unwrap_or_default();
        let args = args_of(node);
        match op.as_str() {
            "length" | "reverse" => {
                if let Some(a) = args.first() {
                    if let Some(n) = var_name(a) {
                        assign(n, Sort3::Lst, vars, sorts)?;
                    }
                }
            }
            "append" => {
                for a in &args {
                    if let Some(n) = var_name(a) {
                        assign(n, Sort3::Lst, vars, sorts)?;
                    }
                }
            }
            "map" | "filter" => {
                // arg0 is a function/predicate, arg1 the list.
                if let Some(f) = args.first() {
                    if let Some(n) = var_name(f) {
                        assign(n, if op == "map" { Sort3::Func } else { Sort3::Pred }, vars, sorts)?;
                    }
                }
                if let Some(l) = args.get(1) {
                    if let Some(n) = var_name(l) {
                        assign(n, Sort3::Lst, vars, sorts)?;
                    }
                }
            }
            "cons" => {
                if let Some(h) = args.first() {
                    if let Some(n) = var_name(h) {
                        assign(n, Sort3::Int, vars, sorts)?;
                    }
                }
                if let Some(t) = args.get(1) {
                    if let Some(n) = var_name(t) {
                        assign(n, Sort3::Lst, vars, sorts)?;
                    }
                }
            }
            "add" | "sub" | "mul" | "neg" | "abs" | "min" | "max" | "mod" | "div" | "lt" | "le" | "gt" | "ge" => {
                for a in &args {
                    if let Some(n) = var_name(a) {
                        assign(n, Sort3::Int, vars, sorts)?;
                    }
                }
            }
            "and" | "or" | "xor" | "not" => {
                for a in &args {
                    if let Some(n) = var_name(a) {
                        assign(n, Sort3::Bool, vars, sorts)?;
                    }
                }
            }
            "foldl" | "foldr" => bail!("predicate uses `{op}` (fold is out of the inductive fragment)"),
            _ => {}
        }
        for a in &args {
            walk_sorts(a, vars, sorts)?;
        }
    }
    for key in ["body", "value", "scrutinee"] {
        if let Some(c) = node.get(key) {
            walk_sorts(c, vars, sorts)?;
        }
    }
    Ok(())
}

// --- function/predicate model + used-op collection ------------------------------------------------

/// Determine how `map`'s function argument is modelled across the predicate (must be consistent: all
/// `id`, or all the same quantified variable). `which` is "map" or "filter".
fn model_of(pred: &J, which: &str, vars: &[String]) -> Result<FnModel> {
    let mut forms: BTreeSet<String> = BTreeSet::new();
    collect_fn_forms(pred, which, &mut forms);
    if forms.is_empty() {
        return Ok(FnModel::None);
    }
    if forms.len() > 1 {
        bail!("`{which}` is applied to more than one distinct function (out of fragment)");
    }
    let form = forms.into_iter().next().unwrap();
    if form == "id" {
        Ok(FnModel::Identity)
    } else if vars.iter().any(|v| *v == form) {
        Ok(FnModel::Uninterpreted(form))
    } else {
        bail!("`{which}`'s function `{form}` is not `id` or a quantified variable (out of fragment)")
    }
}

fn collect_fn_forms(node: &J, which: &str, out: &mut BTreeSet<String>) {
    if node.get("kind").and_then(|k| k.as_str()) == Some("app") {
        let op = head_op(node).unwrap_or_default();
        let args = args_of(node);
        if op == which {
            if let Some(f) = args.first() {
                if let Some(n) = var_name(f) {
                    out.insert(n.to_string());
                } else if head_op(f).as_deref() == Some("id") {
                    out.insert("id".to_string());
                } else {
                    out.insert("<unsupported>".to_string());
                }
            }
        }
        for a in &args {
            collect_fn_forms(a, which, out);
        }
    }
}

fn collect_ops(node: &J, used: &mut BTreeSet<String>) {
    if node.get("kind").and_then(|k| k.as_str()) == Some("app") {
        if let Some(op) = head_op(node) {
            if LIST_OPS.contains(&op.as_str()) {
                used.insert(op);
            }
        }
        for a in args_of(node) {
            collect_ops(a, used);
        }
    }
    for key in ["body", "value", "scrutinee"] {
        if let Some(c) = node.get(key) {
            collect_ops(c, used);
        }
    }
}

// --- lowering -------------------------------------------------------------------------------------

/// Lower a predicate/term to an SMT-LIB term. `env` maps a variable name to a substituted SMT term
/// (used for the induction variable's `nil` / `(cons h t)` / `t` forms); unmapped vars lower to their
/// own name. `map_fn`/`filter_pred` name the modelled symbols.
fn lower(node: &J, env: &BTreeMap<String, String>) -> Result<String> {
    let kind = node.get("kind").and_then(|k| k.as_str()).unwrap_or_default();
    match kind {
        "lit" => lower_value(node.get("value").ok_or_else(|| anyhow!("lit has no value"))?),
        "var" => {
            let name = node.get("name").and_then(|n| n.as_str()).unwrap_or_default();
            if name == "nil" {
                return Ok("nil".to_string());
            }
            Ok(env.get(name).cloned().unwrap_or_else(|| name.to_string()))
        }
        "app" => lower_app(node, env),
        other => bail!("unsupported expression kind `{other}` (out of fragment)"),
    }
}

/// Lower a value-expression AST (literals, including list literals → cons spine).
fn lower_value(value: &J) -> Result<String> {
    match value.get("kind").and_then(|k| k.as_str()) {
        Some("bool") => Ok(value.get("value").and_then(|v| v.as_bool()).unwrap_or(false).to_string()),
        Some("int") | Some("nat") => {
            let v = value.get("value").ok_or_else(|| anyhow!("literal has no value"))?;
            let n: i128 = if let Some(i) = v.as_i64() {
                i as i128
            } else if let Some(s) = v.as_str() {
                s.parse().map_err(|_| anyhow!("bad integer literal {s:?}"))?
            } else {
                bail!("unsupported integer literal {v}")
            };
            Ok(if n < 0 { format!("(- {})", -n) } else { n.to_string() })
        }
        Some("list") => {
            // Fold elements right-to-left into a cons spine ending in nil.
            let elems = value.get("elems").and_then(|e| e.as_array()).cloned().unwrap_or_default();
            let mut acc = "nil".to_string();
            for e in elems.iter().rev() {
                acc = format!("(cons {} {acc})", lower_value(e)?);
            }
            Ok(acc)
        }
        other => bail!("unsupported literal kind: {other:?}"),
    }
}

fn lower_app(node: &J, env: &BTreeMap<String, String>) -> Result<String> {
    let op = head_op(node).ok_or_else(|| anyhow!("application with no resolvable head"))?;
    let args = args_of(node);
    let l = |i: usize| -> Result<String> { lower(args[i], env) };
    Ok(match op.as_str() {
        // List operations → the recursively-defined SMT functions / datatype constructors.
        "length" => format!("(length {})", l(0)?),
        "reverse" => format!("(reverse {})", l(0)?),
        "append" => format!("(append {} {})", l(0)?, l(1)?),
        "map" => format!("(mapf {})", l(1)?), // arg0 (the function) is modelled globally as `mapfn`
        "filter" => format!("(filterp {})", l(1)?),
        "cons" => format!("(cons {} {})", l(0)?, l(1)?),
        "head" => format!("(hd {})", l(0)?),
        "tail" => format!("(tl {})", l(0)?),
        "null" => format!("((_ is nil) {})", l(0)?),
        // Element algebra (Int / Bool), as in prove.rs.
        "id" => l(0)?,
        "add" => format!("(+ {} {})", l(0)?, l(1)?),
        "sub" => format!("(- {} {})", l(0)?, l(1)?),
        "mul" => format!("(* {} {})", l(0)?, l(1)?),
        "neg" => format!("(- {})", l(0)?),
        "abs" => format!("(abs {})", l(0)?),
        "mod" => format!("(mod {} {})", l(0)?, l(1)?),
        "div" => format!("(div {} {})", l(0)?, l(1)?),
        "and" => format!("(and {} {})", l(0)?, l(1)?),
        "or" => format!("(or {} {})", l(0)?, l(1)?),
        "xor" => format!("(xor {} {})", l(0)?, l(1)?),
        "not" => format!("(not {})", l(0)?),
        "eq" => format!("(= {} {})", l(0)?, l(1)?),
        "neq" => format!("(not (= {} {}))", l(0)?, l(1)?),
        "lt" => format!("(< {} {})", l(0)?, l(1)?),
        "le" => format!("(<= {} {})", l(0)?, l(1)?),
        "gt" => format!("(> {} {})", l(0)?, l(1)?),
        "ge" => format!("(>= {} {})", l(0)?, l(1)?),
        "min" => {
            let (a, b) = (l(0)?, l(1)?);
            format!("(ite (<= {a} {b}) {a} {b})")
        }
        "max" => {
            let (a, b) = (l(0)?, l(1)?);
            format!("(ite (>= {a} {b}) {a} {b})")
        }
        // Applying the modelled map-function directly (rare, e.g. element-wise laws).
        "apply" => {
            if let Some(f) = args.first().and_then(|a| var_name(a)) {
                let _ = f; // the symbol is global `mapfn`
                format!("(mapfn {})", l(1)?)
            } else {
                bail!("unsupported application form (out of fragment)")
            }
        }
        other => bail!("unsupported operator `{other}` (out of fragment)"),
    })
}

// --- prelude (datatype + recursive definitions) ---------------------------------------------------

/// The set of list operations the prelude will *define* given a goal that directly uses `used`. Mirrors
/// the dependency in [`build_prelude`]: `reverse` is defined via `append`, so a goal using `reverse`
/// has `append` available too. A candidate lemma is admissible iff its operations are a subset of this.
fn prelude_closure(used: &BTreeSet<String>) -> BTreeSet<String> {
    let mut c = used.clone();
    if c.contains("reverse") {
        c.insert("append".to_string());
    }
    c
}

fn build_prelude(used: &BTreeSet<String>, map_fn: &FnModel, filter_pred: &FnModel) -> String {
    let mut s = String::new();
    s.push_str("(set-logic ALL)\n");
    s.push_str("(declare-datatypes ((Lst 0)) (((nil) (cons (hd Int) (tl Lst)))))\n");
    // `reverse` is defined using `append`, so pull `append` in whenever `reverse` is used.
    let need_append = used.contains("append") || used.contains("reverse");
    if used.contains("length") {
        s.push_str("(define-fun-rec length ((xs Lst)) Int (ite ((_ is nil) xs) 0 (+ 1 (length (tl xs)))))\n");
    }
    if need_append {
        s.push_str("(define-fun-rec append ((xs Lst) (ys Lst)) Lst (ite ((_ is nil) xs) ys (cons (hd xs) (append (tl xs) ys))))\n");
    }
    if used.contains("reverse") {
        s.push_str("(define-fun-rec reverse ((xs Lst)) Lst (ite ((_ is nil) xs) nil (append (reverse (tl xs)) (cons (hd xs) nil))))\n");
    }
    if used.contains("map") {
        match map_fn {
            FnModel::Identity => s.push_str("(define-fun mapfn ((x Int)) Int x)\n"),
            _ => s.push_str("(declare-fun mapfn (Int) Int)\n"), // uninterpreted (covers `forall f`)
        }
        s.push_str("(define-fun-rec mapf ((xs Lst)) Lst (ite ((_ is nil) xs) nil (cons (mapfn (hd xs)) (mapf (tl xs)))))\n");
    }
    if used.contains("filter") {
        match filter_pred {
            FnModel::Identity => s.push_str("(define-fun filterpred ((x Int)) Bool true)\n"),
            _ => s.push_str("(declare-fun filterpred (Int) Bool)\n"),
        }
        s.push_str("(define-fun-rec filterp ((xs Lst)) Lst (ite ((_ is nil) xs) nil (ite (filterpred (hd xs)) (cons (hd xs) (filterp (tl xs))) (filterp (tl xs)))))\n");
    }
    s
}

/// Declarations for the free (non-induction) quantified variables — a free constant is universally
/// quantified under an unsat check.
fn declare_free(var_sorts: &BTreeMap<String, Sort3>, induction_var: &str) -> String {
    let mut s = String::new();
    for (v, sort) in var_sorts {
        if v == induction_var {
            continue;
        }
        match sort {
            Sort3::Int => s.push_str(&format!("(declare-const {v} Int)\n")),
            Sort3::Bool => s.push_str(&format!("(declare-const {v} Bool)\n")),
            Sort3::Lst => s.push_str(&format!("(declare-const {v} Lst)\n")),
            // Function/predicate vars are modelled by the global mapfn/filterpred symbols — no decl.
            Sort3::Func | Sort3::Pred => {}
        }
    }
    s
}

// --- public API -----------------------------------------------------------------------------------

/// Build the base + step induction obligations for a `forall <list> …` law. Err if the goal is outside
/// the supported recursive fragment (the caller maps that to UNSUPPORTED).
pub fn build_induction(prop_expr: &J, body: Option<&J>) -> Result<InductionCertificate> {
    build_obligations(prop_expr, body, &[])
}

/// Like [`build_induction`], but each entry of `lemmas` (a proved `forall` law) is emitted as a
/// universally-quantified SMT axiom in both the base and step obligations. The recursive defs cover the
/// union of operations used by the goal and the lemmas, so a lemma may mention an operation the goal
/// does not. `lemmas` must already be proved — see [`prove_by_induction_with_lemmas`].
fn build_obligations(prop_expr: &J, body: Option<&J>, lemmas: &[&J]) -> Result<InductionCertificate> {
    let _ = body; // self-defined recursive functions are future work (see module docs).
    if prop_expr.get("kind").and_then(|k| k.as_str()) != Some("forall") {
        bail!("not a `forall` — no induction to perform");
    }
    if references_self(prop_expr) {
        bail!("law references `self` — inductive proof of user-defined recursion is out of scope");
    }
    let vars: Vec<String> = prop_expr
        .get("vars")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default();
    if vars.is_empty() {
        bail!("`forall` has no variables");
    }
    let pred = prop_expr.get("body").ok_or_else(|| anyhow!("`forall` has no body"))?;

    let var_sorts = infer_sorts(pred, &vars)?;
    // The induction variable is the first list-sorted quantified variable, in declared order.
    let induction_var = vars
        .iter()
        .find(|v| var_sorts.get(*v) == Some(&Sort3::Lst))
        .cloned()
        .ok_or_else(|| anyhow!("no list-typed quantified variable to induct on (use the first-order prover)"))?;

    // The map/filter function model is taken from the *goal*. A lemma proved with an uninterpreted
    // function entails its specialisation to the goal's concrete function, so lowering a lemma's `map`
    // against the goal's global `mapfn` is sound regardless of which the goal uses.
    let map_fn = model_of(pred, "map", &vars)?;
    let filter_pred = model_of(pred, "filter", &vars)?;
    // Prelude covers the union of operations used by the goal and every assumed lemma.
    let mut used = BTreeSet::new();
    collect_ops(pred, &mut used);
    for lem in lemmas {
        if let Some(lb) = lem.get("body") {
            collect_ops(lb, &mut used);
        }
    }

    let prelude = build_prelude(&used, &map_fn, &filter_pred);
    let free = declare_free(&var_sorts, &induction_var);
    // The proved lemmas, each as a quantified axiom asserted before the goal's negation.
    let axioms = lemmas.iter().map(|l| lemma_axiom(l)).collect::<Result<Vec<_>>>()?.join("");

    // Base: xs := nil.
    let base = {
        let mut env = BTreeMap::new();
        env.insert(induction_var.clone(), "nil".to_string());
        let goal = lower(pred, &env)?;
        format!("{prelude}{free}{axioms}; base case: {induction_var} = nil\n(assert (not {goal}))\n(check-sat)\n")
    };

    // Step: xs := (cons h t); assume the IH for t.
    let step = {
        let (hv, tv) = (format!("{induction_var}_h"), format!("{induction_var}_t"));
        let mut ih_env = BTreeMap::new();
        ih_env.insert(induction_var.clone(), tv.clone());
        let ih = lower(pred, &ih_env)?;
        let mut env = BTreeMap::new();
        env.insert(induction_var.clone(), format!("(cons {hv} {tv})"));
        let goal = lower(pred, &env)?;
        format!(
            "{prelude}{free}{axioms}(declare-const {hv} Int)\n(declare-const {tv} Lst)\n; step case: assume IH for {tv}, prove for (cons {hv} {tv})\n(assert {ih})\n(assert (not {goal}))\n(check-sat)\n"
        )
    };

    Ok(InductionCertificate { var: induction_var, base, step, lemmas: Vec::new() })
}

/// Lower a proved `forall` lemma to a universally-quantified SMT `(assert (forall …))`. Function- and
/// predicate-sorted binders are dropped (they are modelled by the global `mapfn`/`filterpred` symbols),
/// matching [`declare_free`]; a lemma with no remaining binders becomes a plain `(assert …)`.
fn lemma_axiom(lemma: &J) -> Result<String> {
    let vars: Vec<String> = lemma
        .get("vars")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let pred = lemma.get("body").ok_or_else(|| anyhow!("lemma has no body"))?;
    let sorts = infer_sorts(pred, &vars)?;
    let body = lower(pred, &BTreeMap::new())?;
    let binders: Vec<String> = vars
        .iter()
        .filter_map(|v| match sorts.get(v) {
            Some(Sort3::Int) => Some(format!("({v} Int)")),
            Some(Sort3::Bool) => Some(format!("({v} Bool)")),
            Some(Sort3::Lst) => Some(format!("({v} Lst)")),
            _ => None, // Func / Pred: modelled globally, not bound here.
        })
        .collect();
    Ok(if binders.is_empty() {
        format!("(assert {body})\n")
    } else {
        format!("(assert (forall ({}) {body}))\n", binders.join(" "))
    })
}

/// Run a single induction certificate's base then step through the solver. `Unsat`+`Unsat` ⇒ `Proved`.
fn discharge(cert: &InductionCertificate, solver: &str) -> InductionOutcome {
    use crate::prove::{run_smt, SatAnswer};
    match run_smt(&cert.base, solver) {
        Err(e) => return InductionOutcome::Unsupported(format!("solver error (base): {e:#}")),
        Ok(SatAnswer::NoSolver) => return InductionOutcome::NoSolver,
        Ok(SatAnswer::Sat(_)) => return InductionOutcome::Failed("base case is satisfiable (law fails at nil)".into()),
        Ok(SatAnswer::Unknown) => return InductionOutcome::Unknown,
        Ok(SatAnswer::Unsat) => {}
    }
    match run_smt(&cert.step, solver) {
        Err(e) => InductionOutcome::Unsupported(format!("solver error (step): {e:#}")),
        Ok(SatAnswer::Unsat) => InductionOutcome::Proved,
        Ok(SatAnswer::Sat(_)) => InductionOutcome::Failed("step case is satisfiable (induction does not close)".into()),
        Ok(SatAnswer::Unknown) => InductionOutcome::Unknown,
        Ok(SatAnswer::NoSolver) => InductionOutcome::NoSolver,
    }
}

/// Attempt to prove a law by structural induction with a *single* unfold + IH (no lemma discovery).
/// Out-of-fragment goals yield `Unsupported`. A step that needs an auxiliary lemma reports `Unknown`.
pub fn prove_by_induction(prop_expr: &J, body: Option<&J>, solver: &str) -> (InductionOutcome, Option<InductionCertificate>) {
    let cert = match build_induction(prop_expr, body) {
        Err(e) => return (InductionOutcome::Unsupported(format!("{e:#}")), None),
        Ok(c) => c,
    };
    (discharge(&cert, solver), Some(cert))
}

/// The default recursion depth for lemma discovery: deep enough for the standard list laws
/// (`reverse∘reverse` → `reverse_append` → {`append_assoc`, `append_nil`} is two levels) with margin.
pub const DEFAULT_LEMMA_DEPTH: usize = 3;

/// Attempt to prove a law by structural induction, discovering and discharging auxiliary lemmas from
/// the catalog ([`crate::lemmas`]) when a single unfold + IH stalls. Soundness is preserved: a lemma is
/// assumed only after it is itself proved (to `max_depth` of nested discovery), so this never returns a
/// false `Proved`/`ProvedWithLemmas`. Falls back to the bare engine's verdict when no lemma helps.
pub fn prove_by_induction_with_lemmas(
    prop_expr: &J,
    body: Option<&J>,
    solver: &str,
    max_depth: usize,
) -> (InductionOutcome, Option<InductionCertificate>) {
    let mut in_progress = Vec::new();
    prove_rec(prop_expr, body, solver, max_depth, &mut in_progress)
}

/// Recursive core of lemma discovery. `in_progress` carries the goals currently being proved up the
/// stack, so a candidate identical to one already in flight is skipped (cycle guard).
fn prove_rec(
    prop_expr: &J,
    body: Option<&J>,
    solver: &str,
    depth: usize,
    in_progress: &mut Vec<J>,
) -> (InductionOutcome, Option<InductionCertificate>) {
    let bare = match build_induction(prop_expr, body) {
        Err(e) => return (InductionOutcome::Unsupported(format!("{e:#}")), None),
        Ok(c) => c,
    };
    match discharge(&bare, solver) {
        // A clean proof, a genuine failure, a missing solver, or an error: nothing a lemma can fix.
        InductionOutcome::Proved => return (InductionOutcome::Proved, Some(bare)),
        out @ (InductionOutcome::Failed(_)
        | InductionOutcome::NoSolver
        | InductionOutcome::Unsupported(_)) => return (out, Some(bare)),
        // Undecided: try to discover lemmas (if we have depth budget left).
        InductionOutcome::Unknown | InductionOutcome::ProvedWithLemmas(_) => {}
    }
    if depth == 0 {
        return (InductionOutcome::Unknown, Some(bare));
    }

    // Select relevant catalog lemmas: those whose operations all fit within the goal's *prelude
    // closure* (the recursive functions the goal already defines — and a `reverse` goal pulls in
    // `append`, since reverse is defined via append), excluding the goal itself and any goal already
    // being proved up the stack. The closure test is what keeps the search clean: a lemma over an
    // operation the goal never touches (e.g. `length_append` for a pure `reverse` law) would only add
    // an unused recursive definition and its quantifier, which derails z3 into a timeout.
    let mut goal_ops = BTreeSet::new();
    if let Some(b) = prop_expr.get("body") {
        collect_ops(b, &mut goal_ops);
    }
    let closure = prelude_closure(&goal_ops);
    in_progress.push(prop_expr.clone());
    let mut assumed: Vec<J> = Vec::new(); // directly-assumed lemma statements
    let mut proved_certs: Vec<LemmaCertificate> = Vec::new(); // full proof tree, dependency order
    for lemma in crate::lemmas::catalog() {
        let lops = {
            let mut s = BTreeSet::new();
            if let Some(b) = lemma.stmt.get("body") {
                collect_ops(b, &mut s);
            }
            s
        };
        if !lops.is_subset(&closure) || in_progress.iter().any(|g| g == &lemma.stmt) {
            continue;
        }
        let (out, cert) = prove_rec(&lemma.stmt, None, solver, depth - 1, in_progress);
        let proved = matches!(out, InductionOutcome::Proved | InductionOutcome::ProvedWithLemmas(_));
        if proved {
            if let Some(c) = cert {
                // Record this lemma's sub-lemmas first (dependency order), then the lemma itself.
                for sub in &c.lemmas {
                    if !proved_certs.iter().any(|p| p.name == sub.name) {
                        proved_certs.push(sub.clone());
                    }
                }
                if !proved_certs.iter().any(|p| p.name == lemma.name) {
                    proved_certs.push(LemmaCertificate {
                        name: lemma.name.to_string(),
                        var: c.var,
                        base: c.base,
                        step: c.step,
                    });
                }
            }
            assumed.push(lemma.stmt.clone());
        }
    }
    in_progress.pop();

    if assumed.is_empty() {
        return (InductionOutcome::Unknown, Some(bare));
    }

    // Re-build the obligations with the proved lemmas as axioms, and retry.
    let refs: Vec<&J> = assumed.iter().collect();
    let aug = match build_obligations(prop_expr, body, &refs) {
        Err(e) => return (InductionOutcome::Unsupported(format!("{e:#}")), Some(bare)),
        Ok(mut c) => {
            c.lemmas = proved_certs;
            c
        }
    };
    // `discharge` only ever returns Proved / Failed / Unknown / Unsupported / NoSolver.
    match discharge(&aug, solver) {
        InductionOutcome::Proved => {
            let names = aug.lemmas.iter().map(|l| l.name.clone()).collect();
            (InductionOutcome::ProvedWithLemmas(names), Some(aug))
        }
        // The lemmas did not close it (or the solver still stalled): honest UNKNOWN.
        InductionOutcome::Failed(_) | InductionOutcome::Unknown => (InductionOutcome::Unknown, Some(aug)),
        out @ (InductionOutcome::NoSolver | InductionOutcome::Unsupported(_)) => (out, Some(aug)),
        InductionOutcome::ProvedWithLemmas(_) => unreachable!("discharge never returns ProvedWithLemmas"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn forall(vars: &[&str], body: J) -> J {
        json!({ "kind": "forall", "vars": vars, "body": body })
    }
    fn app(op: &str, args: Vec<J>) -> J {
        json!({ "kind": "app", "op": op, "args": args })
    }
    fn var(n: &str) -> J {
        json!({ "kind": "var", "name": n })
    }

    fn solver() -> Option<&'static str> {
        for s in ["z3", "cvc5"] {
            if std::process::Command::new(s).arg("--version").output().map(|o| o.status.success()).unwrap_or(false) {
                return Some(s);
            }
        }
        None
    }

    #[test]
    fn certificate_emits_datatype_and_recursive_defs() {
        // forall xs. eq(length(map(id, xs)), length(xs))
        let prop = forall(&["xs"], app("eq", vec![
            app("length", vec![app("map", vec![var("id"), var("xs")])]),
            app("length", vec![var("xs")]),
        ]));
        let cert = build_induction(&prop, None).unwrap();
        assert_eq!(cert.var, "xs");
        assert!(cert.base.contains("(declare-datatypes ((Lst 0))"));
        assert!(cert.base.contains("define-fun-rec length"));
        assert!(cert.base.contains("define-fun-rec mapf"));
        // base substitutes xs = nil; step assumes the IH for xs_t.
        assert!(cert.base.contains("(length (mapf nil))") || cert.base.contains("mapf nil"));
        assert!(cert.step.contains("(declare-const xs_t Lst)"));
        assert!(cert.step.contains("(declare-const xs_h Int)"));
    }

    #[test]
    fn non_list_law_is_unsupported() {
        // forall n. eq(add(n, n), mul(2, n)) — no list var, leave it to the first-order prover.
        let prop = forall(&["n"], app("eq", vec![
            app("add", vec![var("n"), var("n")]),
            app("mul", vec![json!({ "kind": "lit", "value": { "kind": "int", "value": 2 } }), var("n")]),
        ]));
        assert!(build_induction(&prop, None).is_err());
    }

    #[test]
    fn fold_is_unsupported() {
        let prop = forall(&["xs"], app("eq", vec![var("xs"), app("foldr", vec![var("f"), var("xs"), var("xs")])]));
        assert!(build_induction(&prop, None).is_err());
    }

    // ---- solver-backed proofs (skip when no solver is on PATH) ----

    #[test]
    fn proves_map_identity_by_induction() {
        let Some(solver) = solver() else {
            eprintln!("no SMT solver on PATH — skipping inductive proof test");
            return;
        };
        // forall xs. eq(map(id, xs), xs)
        let prop = forall(&["xs"], app("eq", vec![app("map", vec![var("id"), var("xs")]), var("xs")]));
        let (o, _) = prove_by_induction(&prop, None, solver);
        assert_eq!(o, InductionOutcome::Proved, "map identity should be proved by induction");
    }

    #[test]
    fn proves_length_map_with_uninterpreted_f() {
        let Some(solver) = solver() else {
            return;
        };
        // forall f xs. eq(length(map(f, xs)), length(xs))  — f modelled as an uninterpreted symbol.
        let prop = forall(&["f", "xs"], app("eq", vec![
            app("length", vec![app("map", vec![var("f"), var("xs")])]),
            app("length", vec![var("xs")]),
        ]));
        let (o, _) = prove_by_induction(&prop, None, solver);
        assert_eq!(o, InductionOutcome::Proved, "length is preserved under map for any f");
    }

    #[test]
    fn proves_length_append_distributes() {
        let Some(solver) = solver() else {
            return;
        };
        // forall xs ys. eq(length(append(xs, ys)), add(length(xs), length(ys)))
        let prop = forall(&["xs", "ys"], app("eq", vec![
            app("length", vec![app("append", vec![var("xs"), var("ys")])]),
            app("add", vec![app("length", vec![var("xs")]), app("length", vec![var("ys")])]),
        ]));
        let (o, _) = prove_by_induction(&prop, None, solver);
        assert_eq!(o, InductionOutcome::Proved, "length distributes over append");
    }

    #[test]
    fn reverse_involution_is_unknown_without_a_lemma() {
        let Some(solver) = solver() else {
            return;
        };
        // forall xs. eq(reverse(reverse(xs)), xs) — needs an auxiliary lemma; one unfold + IH cannot
        // close the step, so an honest engine returns UNKNOWN rather than a false PROVED.
        let prop = forall(&["xs"], app("eq", vec![app("reverse", vec![app("reverse", vec![var("xs")])]), var("xs")]));
        let (o, _) = prove_by_induction(&prop, None, solver);
        assert!(matches!(o, InductionOutcome::Unknown | InductionOutcome::Failed(_)),
            "reverse involution needs a lemma — expected UNKNOWN/uncloseable, got {o:?}");
    }

    // ---- lemma discovery (Layer A) ----

    #[test]
    fn proves_reverse_involution_with_lemmas() {
        let Some(solver) = solver() else {
            return;
        };
        // forall xs. eq(reverse(reverse(xs)), xs) — closed by discovering `reverse_append` (which
        // itself rests on `append_nil` + `append_assoc`). The headline target for lemma discovery.
        let prop = forall(&["xs"], app("eq", vec![app("reverse", vec![app("reverse", vec![var("xs")])]), var("xs")]));
        let (o, cert) = prove_by_induction_with_lemmas(&prop, None, solver, DEFAULT_LEMMA_DEPTH);
        match o {
            InductionOutcome::ProvedWithLemmas(lemmas) => {
                assert!(lemmas.contains(&"reverse_append".to_string()), "expected reverse_append, got {lemmas:?}");
                // The full dependency tree is recorded for re-checking.
                assert!(lemmas.contains(&"append_nil".to_string()));
                assert!(lemmas.contains(&"append_assoc".to_string()));
            }
            other => panic!("expected ProvedWithLemmas, got {other:?}"),
        }
        // The certificate carries every sub-lemma's own base + step obligations.
        let cert = cert.expect("certificate present");
        assert_eq!(cert.lemmas.len(), 3, "append_nil, append_assoc, reverse_append");
        assert!(cert.step.contains("(forall"), "the goal's step assumes the lemmas as quantified axioms");
    }

    #[test]
    fn proves_reverse_append_with_lemmas() {
        let Some(solver) = solver() else {
            return;
        };
        // forall xs ys. eq(reverse(append(xs, ys)), append(reverse(ys), reverse(xs)))
        let prop = forall(&["xs", "ys"], app("eq", vec![
            app("reverse", vec![app("append", vec![var("xs"), var("ys")])]),
            app("append", vec![app("reverse", vec![var("ys")]), app("reverse", vec![var("xs")])]),
        ]));
        let (o, _) = prove_by_induction_with_lemmas(&prop, None, solver, DEFAULT_LEMMA_DEPTH);
        assert!(matches!(o, InductionOutcome::ProvedWithLemmas(_)), "expected ProvedWithLemmas, got {o:?}");
    }

    #[test]
    fn lemma_discovery_keeps_a_plain_proof_plain() {
        let Some(solver) = solver() else {
            return;
        };
        // A law the bare engine already closes must NOT get spuriously decorated with lemmas.
        let prop = forall(&["xs", "ys"], app("eq", vec![
            app("length", vec![app("append", vec![var("xs"), var("ys")])]),
            app("add", vec![app("length", vec![var("xs")]), app("length", vec![var("ys")])]),
        ]));
        let (o, _) = prove_by_induction_with_lemmas(&prop, None, solver, DEFAULT_LEMMA_DEPTH);
        assert_eq!(o, InductionOutcome::Proved, "length-append closes bare; no lemmas should be used");
    }

    #[test]
    fn lemma_discovery_never_proves_a_false_law() {
        let Some(solver) = solver() else {
            return;
        };
        // forall xs. eq(reverse(xs), xs) — false for lists of length ≥ 2. Assuming only *proved*
        // lemmas can never make a false goal provable; expect a non-PROVED verdict.
        let prop = forall(&["xs"], app("eq", vec![app("reverse", vec![var("xs")]), var("xs")]));
        let (o, _) = prove_by_induction_with_lemmas(&prop, None, solver, DEFAULT_LEMMA_DEPTH);
        assert!(
            matches!(o, InductionOutcome::Failed(_) | InductionOutcome::Unknown),
            "a false law must never be PROVED, got {o:?}"
        );
    }

    #[test]
    fn unreachable_law_stays_unknown() {
        let Some(solver) = solver() else {
            return;
        };
        // forall f xs. eq(map(f, reverse(xs)), reverse(map(f, xs))) — true, but needs a map/reverse
        // distribution lemma the catalog lacks. Honest UNKNOWN, not a false PROVED.
        let prop = forall(&["f", "xs"], app("eq", vec![
            app("map", vec![var("f"), app("reverse", vec![var("xs")])]),
            app("reverse", vec![app("map", vec![var("f"), var("xs")])]),
        ]));
        let (o, _) = prove_by_induction_with_lemmas(&prop, None, solver, DEFAULT_LEMMA_DEPTH);
        assert_eq!(o, InductionOutcome::Unknown, "no applicable lemma — expected UNKNOWN, got {o:?}");
    }
}
