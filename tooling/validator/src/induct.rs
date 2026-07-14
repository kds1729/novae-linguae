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
//! 5).
//!
//! **Theory exploration (Layer B).** When the curated catalog can't close the goal,
//! [`prove_by_induction_with_exploration`] falls back to [`crate::explore`]: it *conjectures* fresh
//! lemmas by enumerating and testing terms over the goal's operations, proves the survivors by
//! induction (same machinery), and retries. To stay sound and fast it adds discovered lemmas one at a
//! time, trying to close the goal with a **minimal** axiom set (catalog + a single discovered lemma) —
//! piling every conjecture into one query overwhelms the solver even when a small subset closes
//! instantly. Proofs are **memoized**, so a shared lemma (e.g. `reverse_append`) is discharged once and
//! reused across the whole search.
//!
//! **User-defined recursion (`self`).** A law over a user-defined recursive function — `self`, supplied
//! as a body — is handled by encoding that body as its own `define-fun-rec self` (the body branches with
//! a boolean `case` on `null(xs)` and recurses via `self`/`apply(self, …)`, since the language has no
//! native `cons`/`nil` patterns). The induction then discharges it exactly as it does the built-in
//! recursive list ops. So e.g. a user-defined recursive `length` is proved to distribute over `append`.
//! `self` may **return a list** (the SMT return sort is read off its base arm — a `nil`/cons arm ⇒ `Lst`),
//! so a cons-recursive map is proved length-preserving; and it may take a **second list parameter** carried
//! through the recursion (induction on the first), so a user-defined recursive `append` is proved
//! length-additive.
//!
//! **Folds.** `foldr(f, z, xs)` and `foldl(f, z, xs)` are emitted as their own `define-fun-rec`s over a
//! single global uninterpreted binary `foldfn` (so a law holds for *every* `f`). `foldr` discharges with
//! the ordinary hypothesis; `foldl` threads its accumulator, so for fold laws the step also asserts the
//! induction hypothesis **generalized over the non-induction variables** (`forall others. P(t, others)`)
//! — letting the solver instantiate it at the changed accumulator. Both `foldr`/`foldl` are proved to
//! distribute over `append`.
//!
//! Honest scope (v0.1). Element sort is `Int`; the list is `Lst`. Supported operations: `length`,
//! `append`, `reverse`, `map`, `filter`, `cons`, `head`, `tail`, `null`, `foldr`, `foldl`, plus the
//! `Int`/`Bool` element algebra. `map`/`filter`/`fold` take at most one function/predicate, modelled as
//! `id` or a single uninterpreted symbol (so `forall f xs. length(map(f, xs)) = length(xs)` is provable
//! with `f` uninterpreted). `self` recurses on its first list parameter; any number of additional
//! spectator parameters are threaded through and ∀-generalized in the induction hypothesis (so arity > 2
//! is supported). Lists-of-lists and multiple distinct function arguments are out of scope (UNSUPPORTED).

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value as J};
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// Memo of *proved* goals (keyed by canonical statement JSON), so an auxiliary lemma proved once — e.g.
/// `reverse_append` and its sub-lemmas — is reused across every later proof instead of re-discharged.
/// Only positive results are cached (sound to reuse at any depth); Unknown/Failed are recomputed.
type Memo = HashMap<String, (InductionOutcome, Option<InductionCertificate>)>;

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
#[derive(Clone)]
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

/// Unwind a curried application of `self`/`self__g` to its arguments in order, if `node` is one.
/// `apply(self, x)` → `("self", [x])`; `apply(apply(self, x), y)` → `("self", [x, y])`. Returns `None`
/// for any application not headed by a recursion marker (e.g. `apply(f, x)` for a modelled `f`).
fn unwind_self_apply(node: &J) -> Option<(&str, Vec<&J>)> {
    if head_op(node).as_deref() != Some("apply") {
        return None;
    }
    let args = args_of(node);
    let f = *args.first()?;
    let x = *args.get(1)?;
    if let Some(name) = var_name(f).filter(|n| *n == "self" || *n == "self__g") {
        return Some((name, vec![x]));
    }
    let (name, mut inner) = unwind_self_apply(f)?;
    inner.push(x);
    Some((name, inner))
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

const LIST_OPS: &[&str] =
    &["length", "append", "reverse", "map", "filter", "cons", "head", "tail", "null", "foldr", "foldl"];

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
            // `self` recurses on a list, so its argument is a list. Both call forms: `self(xs)` and the
            // curried `apply(self, xs)`.
            "self" => {
                if let Some(n) = args.first().and_then(|a| var_name(a)) {
                    assign(n, Sort3::Lst, vars, sorts)?;
                }
            }
            "apply" if args.first().and_then(|a| var_name(a)) == Some("self") => {
                if let Some(n) = args.get(1).and_then(|a| var_name(a)) {
                    assign(n, Sort3::Lst, vars, sorts)?;
                }
            }
            // fold(f, z, xs): f is the (binary) fold function — modelled globally, so treated like a
            // map function (skipped at declaration); z is the Int accumulator; xs is the list.
            "foldl" | "foldr" => {
                if let Some(n) = args.first().and_then(|a| var_name(a)) {
                    assign(n, Sort3::Func, vars, sorts)?;
                }
                if let Some(n) = args.get(1).and_then(|a| var_name(a)) {
                    assign(n, Sort3::Int, vars, sorts)?;
                }
                if let Some(n) = args.get(2).and_then(|a| var_name(a)) {
                    assign(n, Sort3::Lst, vars, sorts)?;
                }
            }
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
    // Descend into `case` arms (a recursive `self` body branches here).
    if let Some(arms) = node.get("arms").and_then(|a| a.as_array()) {
        for arm in arms {
            if let Some(b) = arm.get("body") {
                collect_ops(b, used);
            }
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
        "case" => lower_case(node, env),
        other => bail!("unsupported expression kind `{other}` (out of fragment)"),
    }
}

/// Lower a `case` to an SMT `ite`. Supported scope: a boolean scrutinee with `true`/`false` literal
/// arms (the `case null(xs) of true -> … | false -> …` idiom that recursion over lists is written
/// with, since the language has no native `cons`/`nil` patterns), plus an optional wildcard/bind
/// default. This is exactly what a recursive `self` body needs lowered for its `define-fun-rec`.
fn lower_case(node: &J, env: &BTreeMap<String, String>) -> Result<String> {
    let scrut = node.get("scrutinee").ok_or_else(|| anyhow!("case has no scrutinee"))?;
    let scrut_smt = lower(scrut, env)?;
    let arms = node.get("arms").and_then(|a| a.as_array()).ok_or_else(|| anyhow!("case has no arms"))?;
    let (mut t_arm, mut f_arm, mut default) = (None, None, None);
    for arm in arms {
        let abody = arm.get("body").ok_or_else(|| anyhow!("arm has no body"))?;
        match arm.get("pattern").and_then(|p| p.get("kind")).and_then(|k| k.as_str()) {
            Some("wildcard") | Some("bind") => default = Some(lower(abody, env)?),
            Some("lit") => match arm.pointer("/pattern/value/value").and_then(|v| v.as_bool()) {
                Some(true) => t_arm = Some(lower(abody, env)?),
                Some(false) => f_arm = Some(lower(abody, env)?),
                None => bail!("case over non-boolean literal patterns (out of fragment)"),
            },
            other => bail!("unsupported case pattern {other:?} (out of fragment)"),
        }
    }
    let t = t_arm.or_else(|| default.clone()).ok_or_else(|| anyhow!("case has no true/default arm"))?;
    let f = f_arm.or(default).ok_or_else(|| anyhow!("case has no false/default arm"))?;
    Ok(format!("(ite {scrut_smt} {t} {f})"))
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
        // fold(f, z, xs): arg0 (f) is the global binary `foldfn`; arg1 is the accumulator, arg2 the list.
        "foldr" => format!("(foldr_f {} {})", l(1)?, l(2)?),
        "foldl" => format!("(foldl_f {} {})", l(1)?, l(2)?),
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
        // The recursive function under test, as an SMT `define-fun-rec` named `self` (`self__g` is the
        // reserved name for a *second* recursive function in the equivalence path). Applied directly in
        // the `op`/`fn`-var form to one or two arguments (`self(xs)`, `self(tail xs, ys)`).
        "self" | "self__g" => {
            let lowered = (0..args.len()).map(l).collect::<Result<Vec<_>>>()?.join(" ");
            format!("({op} {lowered})")
        }
        // Curried application: an `apply` spine bottoming out in `self`/`self__g` is a recursive call
        // (one or two args — `apply(apply(self, xs), ys)` → `(self xs ys)`); `apply(f, x)` for any other
        // function variable → the modelled global `mapfn`.
        "apply" => {
            if let Some((name, sargs)) = unwind_self_apply(node) {
                let lowered = sargs.iter().map(|a| lower(a, env)).collect::<Result<Vec<_>>>()?.join(" ");
                format!("({name} {lowered})")
            } else {
                match args.first().and_then(|a| var_name(a)) {
                    Some(_) => format!("(mapfn {})", l(1)?),
                    None => bail!("unsupported application form (out of fragment)"),
                }
            }
        }
        other => bail!("unsupported operator `{other}` (out of fragment)"),
    })
}

// --- user-defined recursive function (`self`) ------------------------------------------------------

/// Best-effort sort of an expression's result, used to pick `self`'s SMT return sort. Defaults to `Int`.
fn infer_result_sort(node: &J) -> Sort3 {
    match node.get("kind").and_then(|k| k.as_str()) {
        Some("lit") => match node.pointer("/value/kind").and_then(|k| k.as_str()) {
            Some("bool") => Sort3::Bool,
            Some("list") => Sort3::Lst,
            _ => Sort3::Int,
        },
        // `nil` is the empty-list constant (written as a `var`), so a base arm of `nil` means the
        // recursive function returns a list — e.g. a cons-recursive `map`/`append` whose base case is nil.
        Some("var") if node.get("name").and_then(|n| n.as_str()) == Some("nil") => Sort3::Lst,
        Some("case") => {
            // Infer from the arms, preferring a concrete list/bool sort over the Int default: an arm that
            // conses (or is `nil`) makes the function list-returning even when another arm is a bare
            // variable that reads as Int (e.g. `append`'s base arm `ys`). A genuinely Int-returning
            // function has every arm Int, so this stays correct; a body with truly mixed-sort arms is
            // ill-typed and the resulting SMT def fails to sort-check (⇒ UNSUPPORTED, never a false PROVED).
            let arms = node.get("arms").and_then(|a| a.as_array());
            let sorts: Vec<Sort3> = arms
                .map(|arms| arms.iter().filter_map(|a| a.get("body")).map(infer_result_sort).collect())
                .unwrap_or_default();
            if sorts.contains(&Sort3::Lst) {
                Sort3::Lst
            } else if sorts.contains(&Sort3::Bool) {
                Sort3::Bool
            } else {
                Sort3::Int
            }
        }
        Some("app") => match head_op(node).as_deref().unwrap_or_default() {
            "append" | "reverse" | "cons" | "tail" | "map" | "filter" => Sort3::Lst,
            "and" | "or" | "xor" | "not" | "eq" | "neq" | "lt" | "le" | "gt" | "ge" | "null" => Sort3::Bool,
            _ => Sort3::Int, // length, head, add, sub, mul, …, and recursive `self`
        },
        _ => Sort3::Int,
    }
}

/// Lower a single-list-parameter recursive lambda body to an SMT `(define-fun-rec self …)`. The body
/// recurses on its parameter via `self`/`apply(self, …)`, branches with a boolean `case`, and uses the
/// list selectors/builtins — all of which [`lower`] now handles. Errors if the body is outside this
/// shape (the caller maps that to UNSUPPORTED).
fn lower_self_def(body: &J) -> Result<String> {
    lower_rec_def(body, "self")
}

/// Like [`lower_self_def`] but emits the `define-fun-rec` under `smt_name`. The body's recursive calls
/// must already reference `smt_name` (`self` for the single-self prover; a caller wanting a second
/// recursive function renames `self` → its reserved name with [`rename_self`] first). [`lower`] recognizes
/// `self` and the reserved `self__g` as recursive heads.
fn sort_kw(s: Sort3) -> Result<&'static str> {
    Ok(match s {
        Sort3::Int => "Int",
        Sort3::Bool => "Bool",
        Sort3::Lst => "Lst",
        Sort3::Func | Sort3::Pred => bail!("recursive function uses a function/predicate sort (out of fragment)"),
    })
}

fn lower_rec_def(body: &J, smt_name: &str) -> Result<String> {
    Ok(lower_rec_def_with_sorts(body, smt_name)?.0)
}

/// Like [`lower_rec_def`] but also returns the parameter sorts in positional order. The first is always
/// `Lst` (the induction parameter); any spectator's sort is inferred. The two-recursive equiv prover reads
/// these to declare and thread the spectator argument(s) through both functions.
fn lower_rec_def_with_sorts(body: &J, smt_name: &str) -> Result<(String, Vec<Sort3>)> {
    if body.get("kind").and_then(|k| k.as_str()) != Some("lambda") {
        bail!("recursive body is not a lambda");
    }
    let params = body.get("params").and_then(|p| p.as_array()).ok_or_else(|| anyhow!("lambda has no params"))?;
    // The recursion is on the FIRST parameter (a list); the remaining parameters are threaded through as
    // "spectators" — declared free in the goal and ∀-quantified in the induction hypothesis (see
    // `prove_equiv_by_induction`). ANY arity ≥ 1 is supported: the generalized IH closes carried,
    // descending, and concrete-unfold spectators uniformly, so there is no upper cap on parameter count.
    if params.is_empty() {
        bail!("self-induction needs at least one parameter");
    }
    let pnames: Vec<String> = params
        .iter()
        .map(|p| p.get("name").and_then(|n| n.as_str()).map(String::from).ok_or_else(|| anyhow!("param has no name")))
        .collect::<Result<_>>()?;
    let inner = body.get("body").ok_or_else(|| anyhow!("lambda has no body"))?;
    let ret_sort = infer_result_sort(inner);
    let ret = sort_kw(ret_sort)?;
    // Parameter sorts: the recursion parameter (first) is a list; any other is inferred from the body,
    // defaulting to the return sort when unconstrained (the list-combining shape, e.g. append's `ys`). A
    // wrong guess can't yield a false proof — the SMT def would fail to sort-check and report UNSUPPORTED.
    let mut psorts: BTreeMap<String, Option<Sort3>> = pnames.iter().map(|p| (p.clone(), None)).collect();
    walk_sorts(inner, &pnames, &mut psorts)?;
    let mut sorts = Vec::with_capacity(pnames.len());
    let mut decls = String::new();
    for (i, p) in pnames.iter().enumerate() {
        let s = if i == 0 { Sort3::Lst } else { psorts.get(p).copied().flatten().unwrap_or(ret_sort) };
        sorts.push(s);
        decls.push_str(&format!("({p} {})", sort_kw(s)?));
    }
    let lowered = lower(inner, &BTreeMap::new())?; // params lower to themselves, scoped by the define-fun-rec
    Ok((format!("(define-fun-rec {smt_name} ({decls}) {ret} {lowered})\n"), sorts))
}

/// Rename every `self` recursion marker (the `self` *var* and the `self` *op*) to `new_name` throughout
/// the AST — so a second recursive function's body, which also writes its recursion as `self`, can be
/// lowered to its own `define-fun-rec`.
fn rename_self(node: &J, new_name: &str) -> J {
    match node {
        J::Object(m) => {
            let mut out: serde_json::Map<String, J> =
                m.iter().map(|(k, v)| (k.clone(), rename_self(v, new_name))).collect();
            if out.get("kind").and_then(|k| k.as_str()) == Some("var")
                && out.get("name").and_then(|n| n.as_str()) == Some("self")
            {
                out.insert("name".into(), J::String(new_name.to_string()));
            }
            if out.get("op").and_then(|o| o.as_str()) == Some("self") {
                out.insert("op".into(), J::String(new_name.to_string()));
            }
            J::Object(out)
        }
        J::Array(items) => J::Array(items.iter().map(|v| rename_self(v, new_name)).collect()),
        other => other.clone(),
    }
}

/// Bound on the **blind stride search** used when a body's recursion stride can't be read off the AST:
/// the prover then tries every stride `1..=MAX_STRIDE`, so this directly bounds the number of solver
/// calls that search costs. Kept modest for that reason. When BOTH strides *are* readable the prover does
/// not search — it targets the exact realigning stride and this cap does not apply (see
/// `MAX_TARGETED_STRIDE`).
const MAX_STRIDE: usize = 12;

/// Bound on a **targeted** induction stride — the case where both recursion strides are read off the AST,
/// so the minimal realigning stride is known to be exactly `lcm(stride_f, stride_g)` and the prover makes a
/// *single* attempt at it (not a search). Because it is one attempt, this cap can be much larger than the
/// blind-search `MAX_STRIDE` at no cost to common pairs: raising it only affects a genuinely-submitted
/// large-lcm pair, which then checks `0..k` base cases and one step. Measured 2026-07-13 (twice): the old
/// caps were purely conservative — there is NO cliff, the single attempt's cost grows roughly LINEARLY
/// with the lcm (the base-case sweep dominates): on z3, lcm 30 ≈ 1.3 s, 56 ≈ 2.4 s, 63 ≈ 2.6 s, 90 ≈ 3.8 s,
/// 132 ≈ 5.5 s, 240 ≈ 10 s, 380 ≈ 15.7 s. The cap is therefore a TIME-BUDGET choice, not a capability
/// boundary; 240 keeps the one attempt well inside the solver-timeout backstop on modest hardware. Pairs
/// whose alignment period exceeds it (or that recurse at a non-constant stride) report UNKNOWN — never a
/// false verdict.
const MAX_TARGETED_STRIDE: usize = 240;

/// Greatest common divisor (Euclid).
fn gcd(a: usize, b: usize) -> usize {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

/// Least common multiple; `0` if either side is `0`.
fn lcm(a: usize, b: usize) -> usize {
    if a == 0 || b == 0 {
        0
    } else {
        a / gcd(a, b) * b
    }
}

/// Recursion **stride** of a single-list-parameter body: how many list elements each `self`-step
/// consumes — the number of nested `tail` applications wrapping the recursion variable in the self-call's
/// argument (`self(tail xs)` → 1, `self(tail(tail xs))` → 2). `Some(d)` when every self-call descends by
/// the same positive `d`; `None` when there is no self-call or the descents disagree (then the k-step
/// search falls back to trying each stride). Knowing the stride lets the prover target the single
/// realigning stride `lcm(stride_f, stride_g)` directly instead of searching.
fn recursion_stride(body: &J, self_name: &str) -> Option<usize> {
    let mut depths = BTreeSet::new();
    collect_self_descents(body, self_name, &mut depths);
    match depths.len() {
        1 => depths.into_iter().next().filter(|&d| d >= 1),
        _ => None,
    }
}

fn collect_self_descents(node: &J, self_name: &str, out: &mut BTreeSet<usize>) {
    match node {
        J::Object(m) => {
            if let Some(arg) = self_call_arg(m, self_name) {
                out.insert(tail_depth(arg));
            }
            for v in m.values() {
                collect_self_descents(v, self_name, out);
            }
        }
        J::Array(items) => items.iter().for_each(|v| collect_self_descents(v, self_name, out)),
        _ => {}
    }
}

/// If `m` is a self-call, return its recursion argument. Handles both the curried `apply(self, arg)`
/// op-spine (how a property/equiv body writes recursion) and a direct `fn:{var:self}` application.
fn self_call_arg<'a>(m: &'a serde_json::Map<String, J>, self_name: &str) -> Option<&'a J> {
    let args = m.get("args")?.as_array()?;
    if m.get("op").and_then(|o| o.as_str()) == Some("apply") {
        if args.first().and_then(|f| f.get("name")).and_then(|n| n.as_str()) == Some(self_name) {
            return args.get(1);
        }
    }
    if m.get("fn").and_then(|f| f.get("name")).and_then(|n| n.as_str()) == Some(self_name) {
        return args.first();
    }
    // Direct op-form self-call (`{op: "self", args: [arg0, …]}`, how a recursive body often writes
    // recursion) — the recursion descends on the first argument. Stride-detection only, so this can change
    // which `k` is tried, never a verdict.
    if m.get("op").and_then(|o| o.as_str()) == Some(self_name) {
        return args.first();
    }
    None
}

/// Number of nested `tail`/`tl` applications wrapping `node` (`tail(tail x)` → 2, a bare var → 0).
fn tail_depth(node: &J) -> usize {
    if let J::Object(m) = node {
        let head = m
            .get("op")
            .and_then(|o| o.as_str())
            .or_else(|| m.get("fn").and_then(|f| f.get("name")).and_then(|n| n.as_str()));
        if matches!(head, Some("tail") | Some("tl")) {
            if let Some(a0) = m.get("args").and_then(|a| a.as_array()).and_then(|a| a.first()) {
                return 1 + tail_depth(a0);
            }
        }
    }
    0
}

/// Decide `∀p0 ps…. f(p0, ps…) = g(p0, ps…)` for **two recursive** functions by structural induction
/// over the leading list parameter `p0`. Both bodies are emitted as `define-fun-rec`s (`self` and the
/// reserved `self__g`). Any remaining **spectator** parameters (`ps…`, ANY count) are
/// threaded through both functions: declared as free constants in the goal and **universally quantified in
/// the induction hypothesis**, which is the proper generalized IH. That single choice makes both a
/// carried spectator (append's second list, unchanged across the recursion) and a descending one
/// (zipWith's second list, tailed each step) close — the IH instantiates at whatever spectator the
/// recursive call uses — and scales to arity > 2 (e.g. a 3-list `interleave3` written with nested `cons`
/// vs with `append` of a concrete prefix, which unfolds). A spectator IH can only *fail* to fire, never prove a false equality, so it is
/// sound.
///
/// The induction **stride** `k` has base cases for every list length `0..k-1` and a step
/// `P(t) ⟹ P(cons^k(t))` — a valid induction principle for any `k` (every list of length `qk + r` reduces
/// by the step to its length-`r` base). When both recursion strides are read off the AST the minimal
/// realigning stride is exactly `lcm(stride_f, stride_g)` and is targeted directly in a single attempt
/// (`lcm ≤ MAX_TARGETED_STRIDE`); when a stride is unreadable the prover *searches* `k = 1..=MAX_STRIDE`.
/// `k = 1` is ordinary structural induction and decides recursions that align step-for-step but differ in
/// their element arithmetic (e.g. two list-sums written differently); a larger `k` aligns *misaligned*
/// recursions (length-by-1 vs length-by-2 at `k = 2`, 3-vs-5 at `k = 15`). The first stride whose base
/// **and** step all discharge ⇒ PROVED.
///
/// When the bare step does not discharge, the prover draws on **cross-function lemmas**: the curated
/// list-algebra catalog ([`close_equiv_step_with_lemmas`]), each lemma proved by its own induction before
/// being assumed, exactly the single-self soundness discipline. So a both-recursive pair whose step needs
/// e.g. `append_nil` or `append_assoc` now closes, as PROVED-with-lemmas.
///
/// Refutation falls out of the base cases: if any base case is **satisfiable**, that is a concrete short
/// list (with concrete spectators) on which the two functions differ — a genuine counterexample, so the
/// verdict is a clean DISTINCT (carried as `Failed(model)`), not UNKNOWN. (A *step* that stays satisfiable
/// only means this stride's induction doesn't close — not a refutation.) When no stride closes (even with
/// lemmas) and no base case refutes, the verdict is UNKNOWN — never a false verdict.
pub fn prove_equiv_by_induction(body_f: &J, body_g: &J, solver: &str) -> InductionOutcome {
    use crate::prove::{run_smt, SatAnswer};

    // Lower both functions and read the (shared, positional) parameter sorts off `f`. Both functions are
    // applied to the same argument tuple, so a sort disagreement makes the SMT sort-check fail and report
    // UNSUPPORTED — never a false proof.
    let (def_f, sorts_f) = match lower_rec_def_with_sorts(body_f, "self") {
        Ok(x) => x,
        Err(e) => return InductionOutcome::Unsupported(format!("{e:#}")),
    };
    let def_g = match lower_rec_def(&rename_self(body_g, "self__g"), "self__g") {
        Ok(s) => s,
        Err(e) => return InductionOutcome::Unsupported(format!("{e:#}")),
    };
    // Induction is on the leading parameter, which must be a list. Spectator parameters (positions 1..)
    // are threaded through — declared free in the goal, ∀-quantified in the IH.
    if sorts_f.first() != Some(&Sort3::Lst) {
        return InductionOutcome::Unsupported("two-recursive equiv inducts on a leading list parameter".into());
    }
    let spectators: Vec<Sort3> = sorts_f[1..].to_vec();
    // A higher-order spectator (function/predicate parameter) has no SMT constant sort — out of fragment.
    let spec_kw: Vec<&'static str> = match spectators.iter().map(|s| sort_kw(*s)).collect::<Result<Vec<_>>>() {
        Ok(v) => v,
        Err(_) => {
            return InductionOutcome::Unsupported("two-recursive equiv with a higher-order parameter (out of fragment)".into())
        }
    };

    // Prelude defines the list operations either body uses (element ops / `self` are not list ops, so
    // `collect_ops` ignores them); map/filter, if present, are modelled by the shared global `mapfn`.
    let mut used = BTreeSet::new();
    collect_ops(body_f, &mut used);
    collect_ops(body_g, &mut used);
    let preamble = format!("{}{def_f}{def_g}", build_prelude(&used, &FnModel::None, &FnModel::None));

    // Spectator goal constants (`s0`, `s1`, …) and the call-argument suffixes for the goal (free consts)
    // and the IH (separate bound vars `y0`, `y1`, …). Distinct names avoid any shadowing in the IH forall.
    let spec_decls: String = spec_kw.iter().enumerate().map(|(i, kw)| format!("(declare-const s{i} {kw})\n")).collect();
    let goal_args: String = (0..spectators.len()).map(|i| format!(" s{i}")).collect();
    let ih_binders: String =
        spec_kw.iter().enumerate().map(|(i, kw)| format!("(y{i} {kw})")).collect::<Vec<_>>().join(" ");
    let ih_args: String = (0..spectators.len()).map(|i| format!(" y{i}")).collect();

    // The list `cons(p0, cons(p1, …, cons(p_{n-1}, tail)))` with the given declarations.
    let spine = |prefix: &str, n: usize, tail: &str| -> (String, String) {
        let decls: String = (0..n).map(|i| format!("(declare-const {prefix}{i} Int)\n")).collect();
        let mut lst = tail.to_string();
        for i in (0..n).rev() {
            lst = format!("(cons {prefix}{i} {lst})");
        }
        (decls, lst)
    };

    // Determine the induction **stride(s)** first, so Phase 1 checks only the base cases those strides
    // actually need (`0..max_stride`) rather than the whole range — a common lockstep pair (stride 1) then
    // pays for a single base case, not twelve. When both recursion strides are readable off the AST, the
    // minimal realigning stride is exactly `lcm(stride_f, stride_g)` (1 for lockstep, 2 for 1-vs-2, 6 for
    // 2-vs-3, 12 for 3-vs-4, 15 for 3-vs-5 …) — target it directly with a SINGLE attempt, so its cap is the
    // larger `MAX_TARGETED_STRIDE` (a bigger lcm costs one submitted pair a longer base sweep, nothing to
    // common pairs); if that lcm exceeds even that, no stride we can afford will close it, so report UNKNOWN
    // without burning solver time. Bodies whose stride can't be read fall back to *searching* every stride
    // `1..=MAX_STRIDE` (the smaller cap, since a search pays per stride tried).
    let strides: Vec<usize> = match (recursion_stride(body_f, "self"), recursion_stride(body_g, "self")) {
        (Some(a), Some(b)) => {
            let k = lcm(a, b);
            if (1..=MAX_TARGETED_STRIDE).contains(&k) {
                vec![k]
            } else {
                return InductionOutcome::Unknown;
            }
        }
        _ => (1..=MAX_STRIDE).collect(),
    };
    let max_stride = strides.iter().copied().max().unwrap_or(0);
    // The base-case sweep serves two roles: (1) the induction **obligation** — a stride-`k` induction needs
    // every length `0..k` established as a base case, so we must reach `max_stride`; and (2) **refutation**
    // — a concrete short list where `f ≠ g` is a clean DISTINCT, and a distinct pair can agree at length 0
    // yet differ further along, so we sweep to at least depth 6 regardless of stride (the historical
    // refutation depth). `max(max_stride, 6)` covers both without over-checking a common lockstep pair.
    let base_depth = max_stride.max(6);

    // Phase 1 — refutation + induction base obligations. A satisfiable base case is a concrete short list
    // (with concrete spectators) on which `f ≠ g` — a genuine counterexample, so a clean DISTINCT.
    for j in 0..base_depth {
        let (decls, lst) = spine("a", j, "nil");
        let script = format!(
            "{preamble}{decls}{spec_decls}; base case: list of length {j}\n(assert (not (= (self {lst}{goal_args}) (self__g {lst}{goal_args}))))\n(check-sat)\n(get-model)\n"
        );
        match run_smt(&script, solver) {
            Ok(SatAnswer::Unsat) => {}
            Ok(SatAnswer::Sat(model)) => return InductionOutcome::Failed(model),
            Ok(SatAnswer::Unknown) => return InductionOutcome::Unknown,
            Ok(SatAnswer::NoSolver) => return InductionOutcome::NoSolver,
            Err(e) => return InductionOutcome::Unsupported(format!("solver error (base len {j}): {e:#}")),
        }
    }

    // The induction hypothesis: `∀ spectators. self(t, s…) = self__g(t, s…)` (the bare equality when there
    // are no spectators). The generalized (∀-quantified) form is the correct IH for the spectator-quantified
    // goal and is sound for both carried and descending spectators.
    let ih = if spectators.is_empty() {
        "(assert (= (self t) (self__g t)))\n".to_string()
    } else {
        format!("(assert (forall ({ih_binders}) (= (self t{ih_args}) (self__g t{ih_args}))))\n")
    };

    // Build the step script at stride `k`, with `axioms` (proved lemmas, possibly empty) asserted first.
    let mk_step = |k: usize, axioms: &str| -> String {
        let (decls, lst) = spine("h", k, "t");
        format!(
            "{preamble}{axioms}{decls}(declare-const t Lst)\n{spec_decls}; step (stride {k}): assume f(t)=g(t), prove for cons^{k}(t)\n{ih}(assert (not (= (self {lst}{goal_args}) (self__g {lst}{goal_args}))))\n(check-sat)\n"
        )
    };

    // Phase 2 — proof. All base cases up to `max_stride` are unsat, so a stride `k` proves the law as soon
    // as its step `P(t) ⟹ P(cons^k(t))` discharges. The first stride whose step discharges wins.
    for &k in &strides {
        match run_smt(&mk_step(k, ""), solver) {
            Ok(SatAnswer::Unsat) => return InductionOutcome::Proved,
            Ok(SatAnswer::Sat(_)) | Ok(SatAnswer::Unknown) => {} // this stride doesn't close — try a larger one
            Ok(SatAnswer::NoSolver) => return InductionOutcome::NoSolver,
            Err(e) => return InductionOutcome::Unsupported(format!("solver error (step k={k}): {e:#}")),
        }
    }

    // Phase 3 — lemma discovery. Only when the stride is determinate (the common case); the doubly-exotic
    // "unreadable stride AND needs a lemma" combination is left UNKNOWN to bound solver time. Every lemma
    // is proved by its own induction before being assumed, so this is as sound as the single-self path: a
    // bug can only fail to close (UNKNOWN), never assert an unproved law and mint a false EQUIVALENT.
    if strides.len() == 1 {
        if let Some(names) = close_equiv_step_with_lemmas(&mk_step, strides[0], &used, solver) {
            return InductionOutcome::ProvedWithLemmas(names);
        }
        // Accumulator transfer-invariance. When a function threads elements into ≥ 2 Int accumulators,
        // moving an amount between two of them can leave the result unchanged. That lemma bridges two
        // recursions that thread the head into DIFFERENT accumulators — e.g. `\xs a b -> …(a+head)…b` vs
        // `\xs a b -> …a…(b+head)`, both computing `a + b + sum(xs)` — which the bare spectator IH can't
        // close (the step needs `g(t, a+h, b) = g(t, a, b+h)`). Prove each such lemma by its own induction,
        // assert the proved ones (each LHS-triggered), and retry the step. Prove-before-assume ⇒ sound: a
        // lemma that doesn't hold is never asserted, so this can only close a real equivalence, never mint
        // a false one.
        let kws: Vec<&str> = std::iter::once("Lst").chain(spec_kw.iter().copied()).collect();
        let int_pos: Vec<usize> = (1..kws.len()).filter(|&m| kws[m] == "Int").collect();
        if int_pos.len() >= 2 {
            // Canonicalize each function's Int accumulators by collapsing every one into the LAST Int
            // position (`… p_i … p_last …  →  … 0 … p_i + p_last …`). Prove each collapse by its own
            // induction, assert the proved ones, retry the step. Both sides then share the canonical
            // (0, …, 0, Σ) accumulator shape, which the two-recursive IH bridges.
            let to = *int_pos.last().unwrap();
            let mut axioms = String::new();
            let mut names = Vec::new();
            for func in ["self", "self__g"] {
                for &from in &int_pos {
                    if from == to {
                        continue;
                    }
                    if let Some(ax) = prove_accumulator_collapse(&preamble, func, &kws, from, to, solver) {
                        axioms.push_str(&ax);
                        names.push(format!("{func}#collapse[{from}->{to}]"));
                    }
                }
            }
            if !axioms.is_empty() {
                if let Ok(SatAnswer::Unsat) = run_smt(&mk_step(strides[0], &axioms), solver) {
                    return InductionOutcome::ProvedWithLemmas(names);
                }
            }
        }
    }
    InductionOutcome::Unknown
}

/// Try to close the two-recursive induction **step** at stride `k` using auxiliary list-algebra lemmas.
/// Mirrors the single-self machinery's catalog phase ([`prove_rec_inner`] Phase A): every admissible
/// catalog lemma is proved by its own induction (via [`prove_one`]) before being assumed, then the step is
/// retried with the full set and, if that stalls, with minimal subsets — piling every lemma into one query
/// can overwhelm z3's quantifier instantiation. Soundness matches the single-self path: a lemma is asserted
/// only after it is discharged, so this can only fail to close (UNKNOWN), never assert an unproved law.
/// `mk_step(k, axioms)` builds the step script with `axioms` (lemma assertions) prepended. Returns the
/// names of the lemmas in the first closing set, or `None`.
fn close_equiv_step_with_lemmas<F: Fn(usize, &str) -> String>(
    mk_step: &F,
    k: usize,
    used: &BTreeSet<String>,
    solver: &str,
) -> Option<Vec<String>> {
    use crate::prove::{run_smt_secs, SatAnswer};
    let closure = prelude_closure(used);
    let mut in_progress = Vec::new();
    let mut memo = Memo::new();

    // Prove every admissible catalog lemma up front — each by its own induction, the soundness gate.
    let mut proved: Vec<(String, ProvedLemma)> = Vec::new();
    for lemma in crate::lemmas::catalog() {
        if let Some(p) =
            prove_one(lemma.name, &lemma.stmt, solver, DEFAULT_LEMMA_DEPTH, &closure, &mut in_progress, &mut memo)
        {
            proved.push((lemma.name.to_string(), p));
        }
    }
    if proved.is_empty() {
        return None;
    }

    // Does the step discharge with this subset of proved lemmas asserted as ∀-quantified axioms?
    let try_close = |sub: &[&(String, ProvedLemma)]| -> Option<Vec<String>> {
        let axioms: String = sub.iter().map(|(_, p)| lemma_axiom(&p.stmt)).collect::<Result<Vec<_>>>().ok()?.join("");
        match run_smt_secs(&mk_step(k, &axioms), solver, SEARCH_SECS) {
            Ok(SatAnswer::Unsat) => Some(sub.iter().map(|(n, _)| n.clone()).collect()),
            _ => None,
        }
    };

    // Full set first; then minimal proper subsets, smallest first (bounded), in case the full set overwhelms
    // the solver (a goal needing just `append_nil` can be derailed by the extra associativity axioms).
    let all: Vec<&(String, ProvedLemma)> = proved.iter().collect();
    if let Some(names) = try_close(&all) {
        return Some(names);
    }
    let n = proved.len();
    if (2..=16).contains(&n) {
        const MAX_SUBSET_ATTEMPTS: usize = 16;
        let mut masks: Vec<u32> = (1u32..(1 << n)).filter(|m| m.count_ones() < n as u32).collect();
        masks.sort_by_key(|m| m.count_ones());
        for mask in masks.into_iter().take(MAX_SUBSET_ATTEMPTS) {
            let sub: Vec<&(String, ProvedLemma)> = (0..n).filter(|i| mask & (1 << i) != 0).map(|i| &proved[i]).collect();
            if let Some(names) = try_close(&sub) {
                return Some(names);
            }
        }
    }
    None
}

/// Try to prove the **accumulator-collapse** lemma for one recursive function `func` (already defined in
/// `preamble` as a `define-fun-rec`): zeroing Int parameter position `from` and folding its value into
/// position `to` leaves the result unchanged —
///   `∀ xs p1…pn. func(xs, …, p_from, …, p_to, …) = func(xs, …, 0, …, p_from + p_to, …)`.
/// This canonicalizes a pair of interchangeable accumulators; asserting it for both sides of a
/// two-recursive equivalence bridges recursions that thread the head into *different* accumulators (e.g.
/// `\xs a b -> …(a+head)…b` vs `…a…(b+head)`, both `= a + b + sum(xs)`).
///
/// Proved by structural induction on the leading list parameter `xs`, the other parameters
/// **∀-generalized in the induction hypothesis** (the standard accumulator generalization). Crucially the
/// e-matching **trigger is the plain application `func(x, p1, …, pn)`** — NOT a term containing `+`: z3
/// silently drops triggers built over interpreted arithmetic, so a `+`-in-the-trigger phrasing never
/// instantiates and the step stalls at UNKNOWN. The rewrite is idempotent (`0 + X` simplifies to `X`), so
/// the liberal trigger cannot loop. Returns the lemma as an `(assert (forall … :pattern …))` axiom on
/// success — else `None`.
///
/// **Sound:** returned only when BOTH the base (`xs = nil`) and the step (`xs = cons(hh, t)`) are `unsat`.
/// A function whose accumulators are *not* interchangeable fails the base check and yields `None`, so a
/// caller may safely assume whatever this hands back.
fn prove_accumulator_collapse(preamble: &str, func: &str, kws: &[&str], from: usize, to: usize, solver: &str) -> Option<String> {
    use crate::prove::{run_smt_secs, SatAnswer};
    let n = kws.len();
    // Argument spine for `func`: leading list `xs`, then the non-list params `{prefix}m`. `collapse=true`
    // zeroes position `from` and replaces position `to` with `(+ p_from p_to)`.
    let spine = |xs: &str, prefix: &str, collapse: bool| -> String {
        let mut s = format!(" {xs}");
        for m in 1..n {
            if collapse && m == from {
                s.push_str(" 0");
            } else if collapse && m == to {
                s.push_str(&format!(" (+ {prefix}{from} {prefix}{to})"));
            } else {
                s.push_str(&format!(" {prefix}{m}"));
            }
        }
        s
    };
    let free_decls: String = (1..n).map(|m| format!("(declare-const c{m} {})\n", kws[m])).collect();

    // Base: xs = nil.
    let base = format!(
        "{preamble}{free_decls}(assert (not (= ({func}{}) ({func}{}))))\n(check-sat)\n",
        spine("nil", "c", false),
        spine("nil", "c", true),
    );
    if !matches!(run_smt_secs(&base, solver, SEARCH_SECS), Ok(SatAnswer::Unsat)) {
        return None;
    }

    // Step: xs = cons(hh, t); IH ∀-generalized over the non-list params, triggered on the PLAIN application.
    let ih_binders: String = (1..n).map(|m| format!("(b{m} {})", kws[m])).collect();
    let ih_lhs = format!("({func}{})", spine("t", "b", false));
    let ih = format!(
        "(assert (forall ({ih_binders}) (! (= {ih_lhs} ({func}{})) :pattern ({ih_lhs}))))\n",
        spine("t", "b", true),
    );
    let step = format!(
        "{preamble}(declare-const hh Int)\n(declare-const t Lst)\n{free_decls}{ih}(assert (not (= ({func}{}) ({func}{}))))\n(check-sat)\n",
        spine("(cons hh t)", "c", false),
        spine("(cons hh t)", "c", true),
    );
    if !matches!(run_smt_secs(&step, solver, DISCHARGE_SECS), Ok(SatAnswer::Unsat)) {
        return None;
    }

    // Proved. Emit the axiom over `∀ x p…`, triggered on the plain LHS `func(x, p1, …, pn)`.
    let axiom_binders: String =
        "(x Lst)".to_string() + &(1..n).map(|m| format!("(b{m} {})", kws[m])).collect::<String>();
    let lhs = format!("({func}{})", spine("x", "b", false));
    let rhs = format!("({func}{})", spine("x", "b", true));
    Some(format!("(assert (forall ({axiom_binders}) (! (= {lhs} {rhs}) :pattern ({lhs}))))\n"))
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
    // fold: one global uninterpreted binary `foldfn` (covers `forall f`), and the recursive fold(s).
    // foldr(f, z, xs) = f(x0, f(x1, … f(xn, z))); foldl(f, z, xs) threads the accumulator left-to-right.
    if used.contains("foldr") || used.contains("foldl") {
        s.push_str("(declare-fun foldfn (Int Int) Int)\n");
    }
    if used.contains("foldr") {
        s.push_str("(define-fun-rec foldr_f ((z Int) (xs Lst)) Int (ite ((_ is nil) xs) z (foldfn (hd xs) (foldr_f z (tl xs)))))\n");
    }
    if used.contains("foldl") {
        s.push_str("(define-fun-rec foldl_f ((z Int) (xs Lst)) Int (ite ((_ is nil) xs) z (foldl_f (foldfn z (hd xs)) (tl xs))))\n");
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
    if prop_expr.get("kind").and_then(|k| k.as_str()) != Some("forall") {
        bail!("not a `forall` — no induction to perform");
    }
    // A law over a user-defined recursive function: encode the body as a `define-fun-rec self`, and the
    // induction discharges it just like the built-in recursive list ops. Without a body we can't.
    let self_def = if references_self(prop_expr) {
        let b = body.ok_or_else(|| anyhow!("law references `self` but no body was supplied"))?;
        Some(lower_self_def(b)?)
    } else {
        None
    };
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
    // Prelude covers the union of operations used by the goal, every assumed lemma, and `self`'s body.
    let mut used = BTreeSet::new();
    collect_ops(pred, &mut used);
    for lem in lemmas {
        if let Some(lb) = lem.get("body") {
            collect_ops(lb, &mut used);
        }
    }
    if self_def.is_some() {
        if let Some(b) = body {
            collect_ops(b, &mut used);
        }
    }

    let uses_fold = used.contains("foldl") || used.contains("foldr");
    // `self`'s definition comes after the list-op defs it may call (SMT-LIB needs definitions first).
    let prelude = format!("{}{}", build_prelude(&used, &map_fn, &filter_pred), self_def.unwrap_or_default());
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
        // Accumulator-threading recursion (`foldl`) needs the IH *generalized over the non-induction
        // variables*, so it can be instantiated at the changed accumulator — the ground instance below
        // is only the hypothesis at the fixed free constants. Added only for fold laws; it never weakens
        // the easy cases (the ground IH still drives them), and it is sound (it is exactly the structural
        // induction hypothesis `forall others. P(t, others)`).
        let qih = if uses_fold {
            let binders: Vec<String> = var_sorts
                .iter()
                .filter(|(v, _)| **v != induction_var)
                .filter_map(|(v, s)| match s {
                    Sort3::Int => Some(format!("({v} Int)")),
                    Sort3::Bool => Some(format!("({v} Bool)")),
                    Sort3::Lst => Some(format!("({v} Lst)")),
                    Sort3::Func | Sort3::Pred => None,
                })
                .collect();
            if binders.is_empty() {
                String::new()
            } else {
                format!("(assert (forall ({}) {ih}))\n", binders.join(" "))
            }
        } else {
            String::new()
        };
        let mut env = BTreeMap::new();
        env.insert(induction_var.clone(), format!("(cons {hv} {tv})"));
        let goal = lower(pred, &env)?;
        format!(
            "{prelude}{free}{axioms}(declare-const {hv} Int)\n(declare-const {tv} Lst)\n; step case: assume IH for {tv}, prove for (cons {hv} {tv})\n{qih}(assert {ih})\n(assert (not {goal}))\n(check-sat)\n"
        )
    };

    Ok(InductionCertificate { var: induction_var, base, step, lemmas: Vec::new() })
}

/// Lower a proved `forall` lemma to a universally-quantified SMT `(assert (forall …))`. Function- and
/// predicate-sorted binders are dropped (they are modelled by the global `mapfn`/`filterpred` symbols),
/// matching [`declare_free`]; a lemma with no remaining binders becomes a plain `(assert …)`.
/// Whether `var` occurs as a whole SMT token in `smt` (not as a substring of a longer identifier).
fn token_present(smt: &str, var: &str) -> bool {
    smt.split(|c: char| !(c.is_alphanumeric() || c == '_')).any(|tok| tok == var)
}

fn lemma_axiom(lemma: &J) -> Result<String> {
    let vars: Vec<String> = lemma
        .get("vars")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let pred = lemma.get("body").ok_or_else(|| anyhow!("lemma has no body"))?;
    let sorts = infer_sorts(pred, &vars)?;
    let body = lower(pred, &BTreeMap::new())?;
    let mut binder_names: Vec<String> = Vec::new();
    let binders: Vec<String> = vars
        .iter()
        .filter_map(|v| {
            let kw = match sorts.get(v) {
                Some(Sort3::Int) => "Int",
                Some(Sort3::Bool) => "Bool",
                Some(Sort3::Lst) => "Lst",
                _ => return None, // Func / Pred: modelled globally, not bound here.
            };
            binder_names.push(v.clone());
            Some(format!("({v} {kw})"))
        })
        .collect();
    if binders.is_empty() {
        return Ok(format!("(assert {body})\n"));
    }
    // Pin the quantifier with an explicit e-matching trigger: the lemma's left-hand side (its
    // "rewrite-from" term) when it mentions every bound variable. Without this z3 picks a trigger
    // heuristically and instantiation becomes sensitive to *assertion order* — the same minimal lemma set
    // closes a goal in one order and returns UNKNOWN in another. An LHS trigger makes it order-independent.
    // Falls back to z3's auto-trigger when the LHS is unsuitable (not an equation, or misses a variable).
    let trigger = if head_op(pred).as_deref() == Some("eq") {
        match args_of(pred).first() {
            Some(lhs) => {
                let t = lower(lhs, &BTreeMap::new())?;
                if binder_names.iter().all(|v| token_present(&t, v)) {
                    Some(t)
                } else {
                    None
                }
            }
            None => None,
        }
    } else {
        None
    };
    Ok(match trigger {
        Some(t) => format!("(assert (forall ({}) (! {body} :pattern ({t}))))\n", binders.join(" ")),
        None => format!("(assert (forall ({}) {body}))\n", binders.join(" ")),
    })
}

/// The default per-check solver timeout (seconds) for discharging an obligation.
const DISCHARGE_SECS: u64 = 5;
/// A short timeout for exploratory lemma-subset search: a real list-law proof closes in well under a
/// second, so a non-closing subset needn't burn the full default budget (it would otherwise make the
/// subset search dominate wall-clock).
const SEARCH_SECS: u64 = 2;

/// Run a single induction certificate's base then step through the solver. `Unsat`+`Unsat` ⇒ `Proved`.
fn discharge(cert: &InductionCertificate, solver: &str, secs: u64) -> InductionOutcome {
    use crate::prove::{run_smt_secs, SatAnswer};
    match run_smt_secs(&cert.base, solver, secs) {
        Err(e) => return InductionOutcome::Unsupported(format!("solver error (base): {e:#}")),
        Ok(SatAnswer::NoSolver) => return InductionOutcome::NoSolver,
        Ok(SatAnswer::Sat(_)) => return InductionOutcome::Failed("base case is satisfiable (law fails at nil)".into()),
        Ok(SatAnswer::Unknown) => return InductionOutcome::Unknown,
        Ok(SatAnswer::Unsat) => {}
    }
    match run_smt_secs(&cert.step, solver, secs) {
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
    (discharge(&cert, solver, DISCHARGE_SECS), Some(cert))
}

/// The default recursion depth for lemma discovery: deep enough for the standard list laws
/// (`reverse∘reverse` → `reverse_append` → {`append_assoc`, `append_nil`} is two levels) with margin.
pub const DEFAULT_LEMMA_DEPTH: usize = 3;

/// Attempt to prove a law by structural induction, discovering and discharging auxiliary lemmas from
/// the **curated catalog** ([`crate::lemmas`], Layer A) when a single unfold + IH stalls. Soundness is
/// preserved: a lemma is assumed only after it is itself proved (to `max_depth` of nested discovery), so
/// this never returns a false `Proved`/`ProvedWithLemmas`. Falls back to the bare verdict when no
/// catalog lemma helps. (For catalog **plus theory exploration**, see
/// [`prove_by_induction_with_exploration`].)
pub fn prove_by_induction_with_lemmas(
    prop_expr: &J,
    body: Option<&J>,
    solver: &str,
    max_depth: usize,
) -> (InductionOutcome, Option<InductionCertificate>) {
    let mut in_progress = Vec::new();
    let mut memo = Memo::new();
    prove_rec(prop_expr, body, solver, max_depth, false, &mut in_progress, &mut memo)
}

/// Like [`prove_by_induction_with_lemmas`], but when the curated catalog can't close the goal it falls
/// back to **theory exploration** ([`crate::explore`], Layer B): conjecturing fresh lemmas by
/// enumerating and testing terms over the goal's operations, then proving the survivors by induction.
/// Strictly more powerful and equally sound (every conjecture is proved before use). Exploration runs
/// only for the top-level goal; nested lemma proofs use the catalog alone, which bounds the search.
pub fn prove_by_induction_with_exploration(
    prop_expr: &J,
    body: Option<&J>,
    solver: &str,
    max_depth: usize,
) -> (InductionOutcome, Option<InductionCertificate>) {
    let mut in_progress = Vec::new();
    let mut memo = Memo::new();
    prove_rec(prop_expr, body, solver, max_depth, true, &mut in_progress, &mut memo)
}

/// A proved candidate lemma: its statement (to assume as an axiom) plus its full sub-proof tree
/// (dependency order, the lemma itself last) for the certificate.
struct ProvedLemma {
    stmt: J,
    certs: Vec<LemmaCertificate>,
}

/// Append `b`'s certificates to a copy of `a`, deduplicating by lemma name.
fn merge_certs(a: &[LemmaCertificate], b: &[LemmaCertificate]) -> Vec<LemmaCertificate> {
    let mut out = a.to_vec();
    for c in b {
        if !out.iter().any(|p| p.name == c.name) {
            out.push(c.clone());
        }
    }
    out
}

/// Try to prove one candidate lemma by induction (recursively, **catalog-only** — nested proofs never
/// explore, which bounds the search). Returns its proof bundle if it holds and is admissible (operations
/// within `closure`) and not already in flight (cycle guard).
fn prove_one(
    name: &str,
    stmt: &J,
    solver: &str,
    depth: usize,
    closure: &BTreeSet<String>,
    in_progress: &mut Vec<J>,
    memo: &mut Memo,
) -> Option<ProvedLemma> {
    let mut lops = BTreeSet::new();
    if let Some(b) = stmt.get("body") {
        collect_ops(b, &mut lops);
    }
    if !lops.is_subset(closure) || in_progress.iter().any(|g| g == stmt) {
        return None;
    }
    let (out, cert) = prove_rec(stmt, None, solver, depth - 1, false, in_progress, memo);
    if !matches!(out, InductionOutcome::Proved | InductionOutcome::ProvedWithLemmas(_)) {
        return None;
    }
    let mut certs = Vec::new();
    if let Some(c) = cert {
        certs.extend(c.lemmas.iter().cloned()); // sub-lemmas first (dependency order)
        certs.push(LemmaCertificate { name: name.to_string(), var: c.var, base: c.base, step: c.step });
    }
    Some(ProvedLemma { stmt: stmt.clone(), certs })
}

/// Re-build the goal's obligations with `assumed` as axioms (attaching the proof tree `certs`) and
/// discharge them. Returns ProvedWithLemmas on success; Unknown if the lemmas didn't close it; and
/// surfaces NoSolver / Unsupported.
fn close_with(
    prop_expr: &J,
    body: Option<&J>,
    solver: &str,
    assumed: &[J],
    certs: Vec<LemmaCertificate>,
    secs: u64,
) -> (InductionOutcome, Option<InductionCertificate>) {
    let refs: Vec<&J> = assumed.iter().collect();
    let aug = match build_obligations(prop_expr, body, &refs) {
        Err(e) => return (InductionOutcome::Unsupported(format!("{e:#}")), None),
        Ok(mut c) => {
            c.lemmas = certs;
            c
        }
    };
    match discharge(&aug, solver, secs) {
        InductionOutcome::Proved => {
            let names = aug.lemmas.iter().map(|l| l.name.clone()).collect();
            (InductionOutcome::ProvedWithLemmas(names), Some(aug))
        }
        // The lemmas didn't close it (or the solver stalled / found the goal false): not proved here.
        InductionOutcome::Failed(_) | InductionOutcome::Unknown => (InductionOutcome::Unknown, Some(aug)),
        out @ (InductionOutcome::NoSolver | InductionOutcome::Unsupported(_)) => (out, Some(aug)),
        InductionOutcome::ProvedWithLemmas(_) => unreachable!("discharge never returns ProvedWithLemmas"),
    }
}

/// Try to close the goal with *subsets* of the already-proved catalog lemmas, smallest first, returning
/// the first subset that discharges it. The full set is assumed already tried by the caller, so this
/// searches proper subsets only. Capped at [`MAX_SUBSET_ATTEMPTS`] close attempts to bound the search on
/// genuinely-unknown goals. Soundness is unchanged: every lemma in any subset was proved before use.
fn close_with_minimal_subset(
    prop_expr: &J,
    body: Option<&J>,
    solver: &str,
    proved: &[ProvedLemma],
) -> Option<(InductionOutcome, Option<InductionCertificate>)> {
    const MAX_SUBSET_ATTEMPTS: usize = 16;
    let n = proved.len();
    // No proper non-empty subset to try when there are fewer than two lemmas (the lone lemma == full set).
    if n < 2 || n > 16 {
        return None;
    }
    // Proper non-empty subsets as bitmasks, ordered by ascending size (minimal sets first).
    let mut masks: Vec<u32> = (1u32..(1 << n)).filter(|m| m.count_ones() < n as u32).collect();
    masks.sort_by_key(|m| m.count_ones());
    for mask in masks.into_iter().take(MAX_SUBSET_ATTEMPTS) {
        let subset: Vec<&ProvedLemma> = (0..n).filter(|i| mask & (1 << i) != 0).map(|i| &proved[i]).collect();
        let assumed: Vec<J> = subset.iter().map(|p| p.stmt.clone()).collect();
        let certs = subset.iter().fold(Vec::new(), |acc, p| merge_certs(&acc, &p.certs));
        if let (out @ InductionOutcome::ProvedWithLemmas(_), cert) =
            close_with(prop_expr, body, solver, &assumed, certs, SEARCH_SECS)
        {
            return Some((out, cert));
        }
    }
    None
}

/// Memoizing wrapper over [`prove_rec_inner`]: a goal proved once is cached (by canonical statement) and
/// its result reused, so a shared auxiliary lemma is discharged a single time across the whole search.
fn prove_rec(
    prop_expr: &J,
    body: Option<&J>,
    solver: &str,
    depth: usize,
    explore: bool,
    in_progress: &mut Vec<J>,
    memo: &mut Memo,
) -> (InductionOutcome, Option<InductionCertificate>) {
    let key = prop_expr.to_string();
    if let Some(hit) = memo.get(&key) {
        if matches!(hit.0, InductionOutcome::Proved | InductionOutcome::ProvedWithLemmas(_)) {
            return hit.clone();
        }
    }
    let result = prove_rec_inner(prop_expr, body, solver, depth, explore, in_progress, memo);
    if matches!(result.0, InductionOutcome::Proved | InductionOutcome::ProvedWithLemmas(_)) {
        memo.insert(key, result.clone());
    }
    result
}

/// Recursive core of lemma discovery. `in_progress` carries the goals currently being proved up the
/// stack, so a candidate identical to one already in flight is skipped (cycle guard). When `explore` is
/// set and the catalog can't close the goal, theory exploration supplies extra candidate lemmas.
fn prove_rec_inner(
    prop_expr: &J,
    body: Option<&J>,
    solver: &str,
    depth: usize,
    explore: bool,
    in_progress: &mut Vec<J>,
    memo: &mut Memo,
) -> (InductionOutcome, Option<InductionCertificate>) {
    let bare = match build_induction(prop_expr, body) {
        Err(e) => return (InductionOutcome::Unsupported(format!("{e:#}")), None),
        Ok(c) => c,
    };
    match discharge(&bare, solver, DISCHARGE_SECS) {
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

    // Candidate lemmas must fit the goal's *prelude closure* (the recursive functions the goal already
    // defines — a `reverse` goal pulls in `append`). The closure test keeps the search clean: a lemma
    // over an operation the goal never touches would only add an unused recursive definition and its
    // quantifier, which derails z3 into a timeout.
    let mut goal_ops = BTreeSet::new();
    if let Some(b) = prop_expr.get("body") {
        collect_ops(b, &mut goal_ops);
    }
    let closure = prelude_closure(&goal_ops);

    in_progress.push(prop_expr.clone());

    // Phase A — curated catalog (fast path). Prove every admissible catalog lemma, then try to close the
    // goal with the lot. If the full set stalls, retry with minimal subsets: piling *every* admissible
    // lemma into one query can overwhelm z3's quantifier instantiation (associativity + reverse/append
    // distribution are classic trigger-loop culprits), so a goal that needs only a small subset closes
    // with that subset even when the full set yields UNKNOWN (e.g. filter/reverse commutation needs just
    // `filter_append` + `append_nil`, and the extra `reverse_append`/`append_assoc` axioms break it).
    let mut proved: Vec<ProvedLemma> = Vec::new();
    for lemma in crate::lemmas::catalog() {
        if let Some(p) = prove_one(lemma.name, &lemma.stmt, solver, depth, &closure, in_progress, memo) {
            proved.push(p);
        }
    }
    let base_assumed: Vec<J> = proved.iter().map(|p| p.stmt.clone()).collect();
    let base_certs: Vec<LemmaCertificate> =
        proved.iter().fold(Vec::new(), |acc, p| merge_certs(&acc, &p.certs));
    if !base_assumed.is_empty() {
        match close_with(prop_expr, body, solver, &base_assumed, base_certs.clone(), DISCHARGE_SECS) {
            (InductionOutcome::Unknown, _) => {
                // Full set didn't close it — try minimal subsets before falling through to Phase B.
                if let Some(res) = close_with_minimal_subset(prop_expr, body, solver, &proved) {
                    in_progress.pop();
                    return res;
                }
            }
            (out, cert) => {
                in_progress.pop();
                return (out, cert);
            }
        }
    }

    // Phase B — SELF-homomorphism discovery (the former "non-catalog cross-function lemma"
    // residual, probed 2026-07-13 by `sum(reverse(xs)) = sum(xs)`: its step needs
    // `self(append(a,b)) = add(self(a), self(b))`, which no catalog entry can state — catalog
    // lemmas range over the fixed prelude ops, never the function under proof — and exploration
    // cannot conjecture — `self` is not in its enumeration alphabet). When the goal itself applies
    // `self` and `append` is in its prelude closure, conjecture the two homomorphism shapes over
    // append (Int-valued: `add`; List-valued: `append`), prove each by ITS OWN induction with the
    // same supplied body (the accumulator-collapse precedent: conjecture + prove-before-assume, so
    // a non-homomorphic function fails the sub-proof and nothing is asserted), and retry the goal
    // with the proved one. Two bounded sub-inductions — deliberately BEFORE the wide exploration
    // sweep, which costs minutes where this costs seconds.
    if let Some(b) = body {
        if closure.contains("append") && crate::equiv::references_self(prop_expr) {
            for (name, stmt) in self_homomorphism_conjectures() {
                if in_progress.iter().any(|g| g == &stmt) {
                    continue;
                }
                let (out, cert) = prove_rec(&stmt, Some(b), solver, depth - 1, false, in_progress, memo);
                if !matches!(out, InductionOutcome::Proved | InductionOutcome::ProvedWithLemmas(_)) {
                    continue;
                }
                let mut certs = base_certs.clone();
                if let Some(c) = cert {
                    certs = merge_certs(&certs, &c.lemmas);
                    certs.push(LemmaCertificate { name: name.to_string(), var: c.var, base: c.base, step: c.step });
                }
                let mut assumed = base_assumed.clone();
                assumed.push(stmt.clone());
                if let (out @ InductionOutcome::ProvedWithLemmas(_), cert) =
                    close_with(prop_expr, body, solver, &assumed, certs, DISCHARGE_SECS)
                {
                    in_progress.pop();
                    return (out, cert);
                }
            }
        }
    }

    // Phase C — theory exploration (only if enabled and the catalog left it open). Prove conjectures
    // one at a time and, after each, try closing the goal with **just the catalog set plus that single
    // discovered lemma** — a minimal axiom set. This both stops early (no need to prove the rest once one
    // works) and avoids axiom bloat: piling every discovered lemma into one query overwhelms z3's
    // quantifier instantiation and times out, even when a two-lemma subset closes instantly.
    if explore {
        // Pass the goal's equated terms so exploration can relevance-promote lemmas from beyond its size
        // cap that share an operator shape with the goal (the smallest-cap base is unchanged regardless).
        let conjectures = crate::explore::explore_lemmas(&closure, prop_expr.get("body"));
        let mut extras: Vec<ProvedLemma> = Vec::new();
        for (name, stmt) in &conjectures {
            if base_assumed.iter().any(|a| a == stmt) {
                continue;
            }
            let Some(p) = prove_one(name, stmt, solver, depth, &closure, in_progress, memo) else {
                continue;
            };
            let mut assumed = base_assumed.clone();
            assumed.push(p.stmt.clone());
            let certs = merge_certs(&base_certs, &p.certs);
            if let (out @ InductionOutcome::ProvedWithLemmas(_), cert) =
                close_with(prop_expr, body, solver, &assumed, certs, DISCHARGE_SECS)
            {
                in_progress.pop();
                return (out, cert);
            }
            extras.push(p);
        }
        // Last resort: catalog + every discovered lemma together (only helps if a goal needs two or more
        // discovered lemmas at once; may bloat the query, but it's the final attempt before UNKNOWN).
        if !extras.is_empty() {
            let mut assumed = base_assumed.clone();
            let mut certs = base_certs.clone();
            for p in &extras {
                assumed.push(p.stmt.clone());
                certs = merge_certs(&certs, &p.certs);
            }
            if let (out @ InductionOutcome::ProvedWithLemmas(_), cert) =
                close_with(prop_expr, body, solver, &assumed, certs, DISCHARGE_SECS)
            {
                in_progress.pop();
                return (out, cert);
            }
        }
    }

    in_progress.pop();
    (InductionOutcome::Unknown, Some(bare))
}

/// The self-homomorphism conjecture shapes over `append` (see prove_rec_inner Phase C):
/// `∀ xs ys. self(append(xs, ys)) = OP(self(xs), self(ys))` for the Int-valued (`add`) and
/// List-valued (`append`) results. Stated over fresh variables, so they never capture the goal's.
fn self_homomorphism_conjectures() -> Vec<(&'static str, J)> {
    let self_app = |arg: J| json!({ "kind": "app", "op": "apply",
                                    "args": [{ "kind": "var", "name": "self" }, arg] });
    let xs = json!({ "kind": "var", "name": "xs" });
    let ys = json!({ "kind": "var", "name": "ys" });
    let append = json!({ "kind": "app", "op": "append", "args": [xs.clone(), ys.clone()] });
    ["add", "append"]
        .into_iter()
        .map(|op| {
            let stmt = json!({ "kind": "forall", "vars": ["xs", "ys"], "body": {
                "kind": "app", "op": "eq", "args": [
                    self_app(append.clone()),
                    { "kind": "app", "op": op, "args": [self_app(xs.clone()), self_app(ys.clone())] }] } });
            (if op == "add" { "self_append_add" } else { "self_append_append" }, stmt)
        })
        .collect()
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

    /// `sum` — head + self(tail), the canonical Int-valued list recursion.
    fn sum_body() -> J {
        json!({ "kind": "lambda", "params": [{ "name": "xs" }], "body": {
            "kind": "case",
            "scrutinee": { "kind": "app", "op": "null", "args": [{ "kind": "var", "name": "xs" }] },
            "arms": [
                { "pattern": { "kind": "lit", "value": { "kind": "bool", "value": true } },
                  "body": { "kind": "lit", "value": { "kind": "int", "value": 0 } } },
                { "pattern": { "kind": "lit", "value": { "kind": "bool", "value": false } },
                  "body": { "kind": "app", "op": "add", "args": [
                      { "kind": "app", "op": "head", "args": [{ "kind": "var", "name": "xs" }] },
                      { "kind": "app", "op": "apply", "args": [
                          { "kind": "var", "name": "self" },
                          { "kind": "app", "op": "tail", "args": [{ "kind": "var", "name": "xs" }] }] }] } }] } })
    }

    #[test]
    fn self_homomorphism_closes_sum_reverse_invariance() {
        let Some(s) = solver() else { return };
        // The former "non-catalog cross-function lemma" residual, pinned: sum(reverse(xs)) = sum(xs)
        // needs self(append(a,b)) = add(self(a), self(b)) — unstatable in the catalog (its lemmas
        // never mention `self`) and unexplorable (`self` is not in the enumeration alphabet).
        // Phase C conjectures the homomorphism, proves it by its own induction with the supplied
        // body, and the goal closes with it alongside the catalog laws.
        let prop = forall(&["xs"], app("eq", vec![
            app("apply", vec![var("self"), app("reverse", vec![var("xs")])]),
            app("apply", vec![var("self"), var("xs")]),
        ]));
        let (out, cert) = prove_by_induction_with_exploration(&prop, Some(&sum_body()), s, DEFAULT_LEMMA_DEPTH);
        match out {
            InductionOutcome::ProvedWithLemmas(names) => {
                assert!(names.iter().any(|n| n == "self_append_add"), "{names:?}");
            }
            other => panic!("expected ProvedWithLemmas, got {other:?}"),
        }
        assert!(cert.is_some(), "the proof carries a re-checkable certificate");
    }

    #[test]
    fn self_homomorphism_never_asserts_for_a_false_goal() {
        let Some(s) = solver() else { return };
        // Prove-before-assume: the homomorphism machinery must not help a FALSE self-goal —
        // sum(reverse(xs)) = sum(xs) + 1 stays unproved (refuted or unknown, never proved).
        let prop = forall(&["xs"], app("eq", vec![
            app("apply", vec![var("self"), app("reverse", vec![var("xs")])]),
            app("add", vec![
                app("apply", vec![var("self"), var("xs")]),
                { let one: J = json!({ "kind": "lit", "value": { "kind": "int", "value": 1 } }); one },
            ]),
        ]));
        let (out, _) = prove_by_induction_with_exploration(&prop, Some(&sum_body()), s, DEFAULT_LEMMA_DEPTH);
        assert!(!matches!(out, InductionOutcome::Proved | InductionOutcome::ProvedWithLemmas(_)), "{out:?}");
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
    fn proves_filter_reverse_commutation_with_lemmas() {
        let Some(solver) = solver() else {
            return;
        };
        // forall p xs. eq(filter(p, reverse(xs)), reverse(filter(p, xs))) — filter commutes with reverse.
        // The step needs the auxiliary `filter_append`; `p` is modelled by the global `filterpred`.
        let prop = forall(&["p", "xs"], app("eq", vec![
            app("filter", vec![var("p"), app("reverse", vec![var("xs")])]),
            app("reverse", vec![app("filter", vec![var("p"), var("xs")])]),
        ]));
        let (o, _) = prove_by_induction_with_lemmas(&prop, None, solver, DEFAULT_LEMMA_DEPTH);
        assert!(matches!(o, InductionOutcome::ProvedWithLemmas(_)), "expected ProvedWithLemmas, got {o:?}");
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
    fn map_reverse_commutation_proves_via_catalog() {
        let Some(solver) = solver() else {
            return;
        };
        // forall f xs. eq(map(f, reverse(xs)), reverse(map(f, xs))) — closes with the catalog's
        // `map_append` lemma. (The trigger + minimal-subset machinery isolates `map_append` from the rest
        // of the catalog, which together would otherwise stall z3's instantiation.) The honest-UNKNOWN
        // guard for a law needing a *non-catalog* lemma lives in `exploration_proves_a_law_the_catalog_cannot`.
        let prop = forall(&["f", "xs"], app("eq", vec![
            app("map", vec![var("f"), app("reverse", vec![var("xs")])]),
            app("reverse", vec![app("map", vec![var("f"), var("xs")])]),
        ]));
        let (o, _) = prove_by_induction_with_lemmas(&prop, None, solver, DEFAULT_LEMMA_DEPTH);
        assert!(matches!(o, InductionOutcome::ProvedWithLemmas(_)), "expected ProvedWithLemmas, got {o:?}");
    }

    // ---- theory exploration (Layer B) ----

    #[test]
    fn exploration_proves_a_law_the_catalog_cannot() {
        let Some(solver) = solver() else {
            return;
        };
        // forall xs ys. reverse(append(reverse(xs), ys)) = append(reverse(ys), xs).
        // Needs `reverse_append` (catalog) AND reverse-involution (NOT in the catalog). Only theory
        // exploration discovers the latter, so the catalog alone leaves it UNKNOWN while exploration
        // closes it — the load-bearing demonstration that Layer B reaches beyond Layer A.
        let goal = forall(&["xs", "ys"], app("eq", vec![
            app("reverse", vec![app("append", vec![app("reverse", vec![var("xs")]), var("ys")])]),
            app("append", vec![app("reverse", vec![var("ys")]), var("xs")]),
        ]));
        // Catalog-only: cannot close it.
        let (cat, _) = prove_by_induction_with_lemmas(&goal, None, solver, DEFAULT_LEMMA_DEPTH);
        assert_eq!(cat, InductionOutcome::Unknown, "catalog alone should leave it open, got {cat:?}");
        // With theory exploration: PROVED, using a discovered (non-catalog) lemma.
        let (exp, cert) = prove_by_induction_with_exploration(&goal, None, solver, DEFAULT_LEMMA_DEPTH);
        assert!(matches!(exp, InductionOutcome::ProvedWithLemmas(_)), "exploration should close it, got {exp:?}");
        let cert = cert.expect("certificate present");
        assert!(
            cert.lemmas.iter().any(|l| l.name.starts_with("discovered_")),
            "a discovered (non-catalog) lemma must appear in the proof tree, got {:?}",
            cert.lemmas.iter().map(|l| &l.name).collect::<Vec<_>>()
        );
    }

    // ---- induction over user-defined recursive bodies (`self`) ----

    fn lit_int(n: i64) -> J {
        json!({ "kind": "lit", "value": { "kind": "int", "value": n } })
    }
    fn lit_bool(b: bool) -> J {
        json!({ "kind": "lit", "value": { "kind": "bool", "value": b } })
    }
    fn call_self(x: J) -> J {
        app("apply", vec![var("self"), x])
    }
    /// `self = \xs -> case null(xs) of true -> 0 | false -> add(1, self(tail(xs)))` — a recursive length.
    fn recursive_length_body() -> J {
        json!({
            "kind": "lambda",
            "params": [{ "name": "xs" }],
            "body": {
                "kind": "case",
                "scrutinee": app("null", vec![var("xs")]),
                "arms": [
                    { "pattern": lit_bool(true),  "body": lit_int(0) },
                    { "pattern": lit_bool(false), "body": app("add", vec![lit_int(1), call_self(app("tail", vec![var("xs")]))]) },
                ]
            }
        })
    }

    #[test]
    fn proves_user_recursive_length_distributes_over_append() {
        let Some(solver) = solver() else {
            return;
        };
        // forall xs ys. self(append(xs, ys)) = add(self(xs), self(ys)), for the user-defined recursive
        // `self` above — induction over a *user-defined* recursive body, not a built-in op.
        let prop = forall(&["xs", "ys"], app("eq", vec![
            call_self(app("append", vec![var("xs"), var("ys")])),
            app("add", vec![call_self(var("xs")), call_self(var("ys"))]),
        ]));
        let (o, _) = prove_by_induction(&prop, Some(&recursive_length_body()), solver);
        assert_eq!(o, InductionOutcome::Proved, "self distributes over append should be proved, got {o:?}");
    }

    /// `self = \xs -> case null(xs) of true -> nil | false -> cons(mul(2, head xs), self(tail xs))` —
    /// a cons-recursive map (doubling). The recursive function returns a LIST, unlike `recursive_length`.
    fn recursive_double_all_body() -> J {
        json!({
            "kind": "lambda",
            "params": [{ "name": "xs" }],
            "body": {
                "kind": "case",
                "scrutinee": app("null", vec![var("xs")]),
                "arms": [
                    { "pattern": lit_bool(true),  "body": var("nil") },
                    { "pattern": lit_bool(false), "body": app("cons", vec![
                        app("mul", vec![lit_int(2), app("head", vec![var("xs")])]),
                        call_self(app("tail", vec![var("xs")]))]) },
                ]
            }
        })
    }

    #[test]
    fn proves_list_returning_self_preserves_length() {
        let Some(solver) = solver() else {
            return;
        };
        // forall xs. length(self(xs)) = length(xs) — a law where the user-defined recursive `self`
        // returns a LIST, composed under the builtin `length`. Regression: the base arm `nil` must be
        // recognized as a list so `self`'s SMT return sort is `Lst`, not the default `Int`.
        let prop = forall(&["xs"], app("eq", vec![
            app("length", vec![call_self(var("xs"))]),
            app("length", vec![var("xs")]),
        ]));
        let (o, _) = prove_by_induction(&prop, Some(&recursive_double_all_body()), solver);
        assert_eq!(o, InductionOutcome::Proved, "list-returning self should preserve length, got {o:?}");
    }

    /// `self = \xs ys -> case null(xs) of true -> ys | false -> cons(head xs, self(tail xs, ys))` —
    /// a TWO-list-parameter recursive append, recursing on the first list with the second a spectator.
    fn recursive_append_body() -> J {
        json!({
            "kind": "lambda",
            "params": [{ "name": "xs" }, { "name": "ys" }],
            "body": {
                "kind": "case",
                "scrutinee": app("null", vec![var("xs")]),
                "arms": [
                    { "pattern": lit_bool(true),  "body": var("ys") },
                    { "pattern": lit_bool(false), "body": app("cons", vec![
                        app("head", vec![var("xs")]),
                        json!({ "kind": "app", "op": "self", "args": [app("tail", vec![var("xs")]), var("ys")] })]) },
                ]
            }
        })
    }
    /// `self(xs, ys)` written as the curried apply spine `apply(apply(self, xs), ys)`.
    fn call_self2(x: J, y: J) -> J {
        app("apply", vec![app("apply", vec![var("self"), x]), y])
    }

    /// A `zipWith3` over three lists: `\xs ys zs -> case null(xs) of true -> nil
    /// | false -> cons(combiner, self(tail xs, tail ys, tail zs))`. ARITY 3 (uniform recursion — all three
    /// lists descend together); the arity-uncapped in-house prover handles it via the generalized spectator IH.
    fn zipwith3_body(combiner: J) -> J {
        json!({
            "kind": "lambda",
            "params": [{ "name": "xs" }, { "name": "ys" }, { "name": "zs" }],
            "body": {
                "kind": "case",
                "scrutinee": app("null", vec![var("xs")]),
                "arms": [
                    { "pattern": lit_bool(true),  "body": var("nil") },
                    { "pattern": lit_bool(false), "body": app("cons", vec![
                        combiner,
                        json!({ "kind": "app", "op": "self", "args": [
                            app("tail", vec![var("xs")]),
                            app("tail", vec![var("ys")]),
                            app("tail", vec![var("zs")]) ] })]) },
                ]
            }
        })
    }
    fn head_of(v: &str) -> J {
        app("head", vec![var(v)])
    }

    /// `interleave3` two ways over three lists: one CONSes the three heads, the other APPENDs a
    /// three-element chunk. Equal, but the proof needs to unfold `append` on the concrete 3-prefix each
    /// step — an inductive/definitional argument NORMALIZE does not do (it never reassociates/unfolds
    /// `append(cons(h,t), r)`), so this case reaches the solver path rather than the normalization one.
    /// `cons(head xs, cons(head ys, cons(head zs, tail)))`.
    fn three_prefix(tail: J) -> J {
        app("cons", vec![head_of("xs"), app("cons", vec![head_of("ys"), app("cons", vec![head_of("zs"), tail])])])
    }
    fn interleave3_cons() -> J {
        zipwith3_via(three_prefix(self3_tail()))
    }
    fn interleave3_append() -> J {
        zipwith3_via(app("append", vec![three_prefix(var("nil")), self3_tail()]))
    }
    fn self3_tail() -> J {
        json!({ "kind": "app", "op": "self", "args": [
            app("tail", vec![var("xs")]), app("tail", vec![var("ys")]), app("tail", vec![var("zs")]) ] })
    }
    /// A zipWith3 shell whose non-nil arm is exactly `arm` (already containing the self-call).
    fn zipwith3_via(arm: J) -> J {
        json!({
            "kind": "lambda",
            "params": [{ "name": "xs" }, { "name": "ys" }, { "name": "zs" }],
            "body": { "kind": "case", "scrutinee": app("null", vec![var("xs")]),
                "arms": [ { "pattern": lit_bool(true), "body": var("nil") },
                          { "pattern": lit_bool(false), "body": arm } ] }
        })
    }

    /// `\xs a b -> case null(xs) of true -> a+b | false -> self(tail xs, <acc_a>, <acc_b>)` — an arity-3
    /// tail-accumulator recursion; `acc_a`/`acc_b` are the two accumulator updates.
    fn accum3_body(acc_a: J, acc_b: J) -> J {
        json!({
            "kind": "lambda",
            "params": [{ "name": "xs" }, { "name": "a" }, { "name": "b" }],
            "body": { "kind": "case", "scrutinee": app("null", vec![var("xs")]),
                "arms": [
                    { "pattern": lit_bool(true), "body": app("add", vec![var("a"), var("b")]) },
                    { "pattern": lit_bool(false), "body":
                        json!({ "kind": "app", "op": "self", "args": [app("tail", vec![var("xs")]), acc_a, acc_b] }) },
                ] }
        })
    }

    fn stln(n: usize) -> J {
        let mut e = var("xs");
        for _ in 0..n {
            e = app("tail", vec![e]);
        }
        e
    }
    fn shdn(n: usize) -> J {
        app("head", vec![stln(n)])
    }
    fn ssum_exact(m: usize) -> J {
        let mut e = shdn(m - 1);
        for i in (0..m - 1).rev() {
            e = app("add", vec![shdn(i), e]);
        }
        e
    }
    /// A sum that peels `k` elements per recursive step, with base cases for the `0..k-1` remainders.
    fn sumk_body(k: usize) -> J {
        let mut rec = json!({ "kind": "app", "op": "self", "args": [stln(k)] });
        for i in (0..k).rev() {
            rec = app("add", vec![shdn(i), rec]);
        }
        let mut body = rec;
        for d in (1..k).rev() {
            body = json!({ "kind": "case", "scrutinee": app("null", vec![stln(d)]),
                "arms": [ { "pattern": lit_bool(true), "body": ssum_exact(d) },
                          { "pattern": lit_bool(false), "body": body } ] });
        }
        json!({ "kind": "lambda", "params": [{ "name": "xs" }], "body":
            { "kind": "case", "scrutinee": app("null", vec![var("xs")]),
              "arms": [ { "pattern": lit_bool(true), "body": lit_int(0) },
                        { "pattern": lit_bool(false), "body": body } ] } })
    }

    #[test]
    fn inhouse_proves_stride_3_vs_4_sum_lcm12() {
        let Some(solver) = solver() else { return };
        // Two sums peeling 3 vs 4 elements per step — alignment period lcm(3,4) = 12. Closes now that the
        // stride cap is 12 (was UNKNOWN at the old cap of 6); z3 discharges the stride-12 step directly.
        assert_eq!(prove_equiv_by_induction(&sumk_body(3), &sumk_body(4), solver), InductionOutcome::Proved);
    }

    #[test]
    fn inhouse_proves_stride_5_vs_6_sum_lcm30() {
        let Some(solver) = solver() else { return };
        // The named residual pair: sums peeling 5 vs 6 elements — alignment period lcm(5,6) = 30,
        // beyond the old cap of 24. One targeted attempt at stride 30 (31 base cases + one step).
        assert_eq!(prove_equiv_by_induction(&sumk_body(5), &sumk_body(6), solver), InductionOutcome::Proved);
    }

    #[test]
    fn inhouse_proves_stride_7_vs_8_sum_lcm56() {
        let Some(solver) = solver() else { return };
        // Near the new cap: lcm(7,8) = 56 — 57 base cases + one stride-56 step, still one attempt.
        assert_eq!(prove_equiv_by_induction(&sumk_body(7), &sumk_body(8), solver), InductionOutcome::Proved);
    }

    #[test]
    fn inhouse_proves_arity3_interleave3() {
        let Some(solver) = solver() else { return };
        // The arity>2 capability (cap lifted): interleave3 built with nested `cons` vs with `append` of a
        // 3-element chunk. The `append` first arg is concrete (length 3) so it unfolds, and the generalized
        // spectator IH (ys, zs threaded, ∀-quantified) closes the step — no external solver needed.
        let (f, g) = (interleave3_cons(), interleave3_append());
        assert_eq!(prove_equiv_by_induction(&f, &g, solver), InductionOutcome::Proved);
        // End to end (normalize can't reconcile these — it never unfolds `append` — so it's the induction).
        assert_eq!(crate::prove_equivalent(&f, &g, solver), crate::EquivVerdict::Equivalent(vec![]));
    }

    #[test]
    fn inhouse_proves_arity4_interleave4() {
        let Some(solver) = solver() else { return };
        // Arity 4 — three spectators threaded and ∀-generalized; still closes. Confirms the cap-lift is
        // genuinely arity-uncapped, not just arity-3.
        assert_eq!(prove_equiv_by_induction(&zipwith4_cons(), &zipwith4_append(), solver), InductionOutcome::Proved);
    }

    #[test]
    fn inhouse_arity3_distinct_is_refuted() {
        let Some(solver) = solver() else { return };
        // Soundness at arity 3: genuinely different combiners (x+(y+z) vs x+(y-z)) must NOT be equated — a
        // base case refutes with a counterexample, never a false Proved.
        let f = zipwith3_body(app("add", vec![head_of("xs"), app("add", vec![head_of("ys"), head_of("zs")])]));
        let g = zipwith3_body(app("add", vec![head_of("xs"), app("sub", vec![head_of("ys"), head_of("zs")])]));
        assert!(matches!(prove_equiv_by_induction(&f, &g, solver), InductionOutcome::Failed(_)));
        assert!(matches!(crate::prove_equivalent(&f, &g, solver), crate::EquivVerdict::Distinct(_)));
    }

    #[test]
    fn inhouse_reordered_accumulator_proves_via_collapse_lemma() {
        // Two tail-accumulators that move `head` into DIFFERENT accumulators (both = a+b+sum xs). The bare
        // spectator IH can't close the step; accumulator-collapse discovery proves `g(xs,a,b) = g(xs,0,a+b)`
        // for each side by its own induction, asserts them, and the two-recursive IH bridges the rest.
        let Some(solver) = solver() else { return };
        let acc_a = accum3_body(app("add", vec![var("a"), head_of("xs")]), var("b"));
        let acc_b = accum3_body(var("a"), app("add", vec![var("b"), head_of("xs")]));
        assert!(
            matches!(prove_equiv_by_induction(&acc_a, &acc_b, solver), InductionOutcome::ProvedWithLemmas(_)),
            "reordered accumulators should prove via the transfer-invariance lemma",
        );
        assert!(matches!(crate::prove_equivalent(&acc_a, &acc_b, solver), crate::EquivVerdict::Equivalent(_)));
    }

    #[test]
    fn inhouse_distinct_accumulator_is_not_equated() {
        // SOUNDNESS: two accumulators that genuinely differ — one threads `head` into `b`, the other threads
        // `2*head` — compute a+b+sum vs a+b+2·sum. The transfer machinery must NOT equate them; a base case
        // refutes with a counterexample (never a false Proved/ProvedWithLemmas).
        let Some(solver) = solver() else { return };
        let acc_a = accum3_body(var("a"), app("add", vec![var("b"), head_of("xs")]));
        let acc_b = accum3_body(var("a"), app("add", vec![var("b"), app("mul", vec![lit_int(2), head_of("xs")])]));
        assert!(matches!(prove_equiv_by_induction(&acc_a, &acc_b, solver), InductionOutcome::Failed(_)));
        assert!(matches!(crate::prove_equivalent(&acc_a, &acc_b, solver), crate::EquivVerdict::Distinct(_)));
    }
    fn four_prefix(tail: J) -> J {
        app("cons", vec![head_of("xs"), app("cons", vec![head_of("ys"),
            app("cons", vec![head_of("zs"), app("cons", vec![head_of("ws"), tail])])])])
    }
    fn self4_tail() -> J {
        json!({ "kind": "app", "op": "self", "args": [app("tail", vec![var("xs")]), app("tail", vec![var("ys")]),
            app("tail", vec![var("zs")]), app("tail", vec![var("ws")]) ] })
    }
    fn zipwith4_shell(arm: J) -> J {
        json!({ "kind": "lambda", "params": [{"name":"xs"},{"name":"ys"},{"name":"zs"},{"name":"ws"}],
            "body": { "kind": "case", "scrutinee": app("null", vec![var("xs")]),
                "arms": [ {"pattern": lit_bool(true), "body": var("nil")},
                          {"pattern": lit_bool(false), "body": arm} ] } })
    }
    fn zipwith4_cons() -> J { zipwith4_shell(four_prefix(self4_tail())) }
    fn zipwith4_append() -> J { zipwith4_shell(app("append", vec![four_prefix(var("nil")), self4_tail()])) }

    #[test]
    fn proves_two_param_self_append_is_length_additive() {
        let Some(solver) = solver() else {
            return;
        };
        // forall xs ys. length(self(xs, ys)) = length(xs) + length(ys) — induction on the first list,
        // ys carried as a spectator. Exercises the two-parameter `define-fun-rec` and curried self-call.
        let prop = forall(&["xs", "ys"], app("eq", vec![
            app("length", vec![call_self2(var("xs"), var("ys"))]),
            app("add", vec![app("length", vec![var("xs")]), app("length", vec![var("ys")])]),
        ]));
        let (o, _) = prove_by_induction(&prop, Some(&recursive_append_body()), solver);
        assert_eq!(o, InductionOutcome::Proved, "append length-additivity should be proved, got {o:?}");
    }

    #[test]
    fn false_two_param_self_law_is_not_proved() {
        let Some(solver) = solver() else {
            return;
        };
        // forall xs ys. length(self(xs, ys)) = (length(xs) + length(ys)) + 1 — off by one, so false. The
        // `length(ys)` term pins ys to a list (so the goal sort-checks), and the base case xs=nil refutes
        // it: length(ys) = length(ys) + 1 is unsatisfiable as an equation, so its negation is a model.
        let prop = forall(&["xs", "ys"], app("eq", vec![
            app("length", vec![call_self2(var("xs"), var("ys"))]),
            app("add", vec![
                app("add", vec![app("length", vec![var("xs")]), app("length", vec![var("ys")])]),
                lit_int(1)]),
        ]));
        let (o, _) = prove_by_induction(&prop, Some(&recursive_append_body()), solver);
        assert!(matches!(o, InductionOutcome::Failed(_) | InductionOutcome::Unknown),
            "a false two-param self-law must not be PROVED, got {o:?}");
    }

    #[test]
    fn false_self_law_is_not_proved() {
        let Some(solver) = solver() else {
            return;
        };
        // forall xs. self(xs) = 0 — false for non-empty lists (the base case holds, the step fails).
        let prop = forall(&["xs"], app("eq", vec![call_self(var("xs")), lit_int(0)]));
        let (o, _) = prove_by_induction(&prop, Some(&recursive_length_body()), solver);
        assert!(matches!(o, InductionOutcome::Failed(_) | InductionOutcome::Unknown),
            "a false self-law must not be PROVED, got {o:?}");
    }

    #[test]
    fn self_law_without_a_body_is_unsupported() {
        // A law mentioning `self` cannot be set up without the recursive body to encode.
        let prop = forall(&["xs"], app("eq", vec![call_self(var("xs")), lit_int(0)]));
        assert!(build_induction(&prop, None).is_err());
    }

    // ---- foldl / foldr ----

    #[test]
    fn proves_foldr_append() {
        let Some(solver) = solver() else {
            return;
        };
        // forall f z xs ys. foldr(f, z, append(xs, ys)) = foldr(f, foldr(f, z, ys), xs).
        // `f` is an uninterpreted binary fold function, so this holds for *every* f.
        let prop = forall(&["f", "z", "xs", "ys"], app("eq", vec![
            app("foldr", vec![var("f"), var("z"), app("append", vec![var("xs"), var("ys")])]),
            app("foldr", vec![var("f"), app("foldr", vec![var("f"), var("z"), var("ys")]), var("xs")]),
        ]));
        let (o, _) = prove_by_induction_with_exploration(&prop, None, solver, DEFAULT_LEMMA_DEPTH);
        assert!(matches!(o, InductionOutcome::Proved | InductionOutcome::ProvedWithLemmas(_)),
            "foldr distributes over append, got {o:?}");
    }

    #[test]
    fn proves_foldl_append() {
        let Some(solver) = solver() else {
            return;
        };
        // forall f z xs ys. foldl(f, z, append(xs, ys)) = foldl(f, foldl(f, z, xs), ys).
        // foldl threads the accumulator, so the step needs the IH generalized over `z` — the
        // accumulator-quantified induction hypothesis added for fold laws.
        let prop = forall(&["f", "z", "xs", "ys"], app("eq", vec![
            app("foldl", vec![var("f"), var("z"), app("append", vec![var("xs"), var("ys")])]),
            app("foldl", vec![var("f"), app("foldl", vec![var("f"), var("z"), var("xs")]), var("ys")]),
        ]));
        let (o, _) = prove_by_induction_with_exploration(&prop, None, solver, DEFAULT_LEMMA_DEPTH);
        assert!(matches!(o, InductionOutcome::Proved | InductionOutcome::ProvedWithLemmas(_)),
            "foldl distributes over append (needs accumulator-generalized IH), got {o:?}");
    }
}
