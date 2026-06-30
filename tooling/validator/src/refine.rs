//! Refinement checking — verify a function body actually satisfies its declared **refinements** and the
//! refinement implied by its type. The third pillar of "verified by default" (principle 3) for the
//! contracts the metadata declares but the type checker doesn't see.
//!
//! Two sources of refinement are checked, both via the [`crate::prove`] backend (first-order SMT, with a
//! structural-induction fallback for a recursive body):
//!
//! 1. **The type-implied `nat` refinement.** A `nat` is a non-negative `int`, which the HM checker erases
//!    to `int` — so a body declared `… -> nat` that can produce a negative `int` type-checks clean. For a
//!    `nat`-result function this proves `body(params) ≥ 0`.
//! 2. **Declared `signature.refinements[]`** — `pre`/`post` predicates. A **`post`** predicate is a
//!    contract on the function's output; it refers to that output through the **reserved variable
//!    `result`** (the convention this module defines), and may also mention the parameters by name. A
//!    **`pre`** predicate is a precondition on the parameters, *assumed* when discharging the posts. So a
//!    record declaring `pre: ge(a, b)` and `post: ge(result, 0)` for `\a b -> sub(a, b)` is sound because
//!    `a ≥ b ⟹ a − b ≥ 0`. (`inv` refinements are not checked in v0.1 — their semantics for a pure
//!    function are reserved.)
//!
//! For each postcondition the obligation discharged is
//!
//! ```text
//!   ∀ params. (⋀ pre  ∧  ⋀ nat-typed params ≥ 0) ⟹ post[result := body(params)]
//! ```
//!
//! — the `nat`-ness of parameters is folded into the preconditions (a `nat` param *is* `≥ 0`). Outcomes:
//! SOUND (proved), VIOLATED (a solver counterexample — a real input on which the body breaks the
//! contract), UNVERIFIABLE (out of the decidable fragment / undecided — never a false SOUND), or
//! NOT-APPLICABLE (nothing to check). Conservative by construction: only a closed proof is SOUND, only a
//! counterexample is VIOLATED.

use serde_json::{json, Value as J};
use std::collections::BTreeMap;

use crate::equiv::{apply_self_spine, params, references_self, subst_many};
use crate::{
    prove_by_induction_with_exploration, prove_property, InductionOutcome, ProofOutcome, DEFAULT_LEMMA_DEPTH,
};

/// The reserved variable a `post` predicate uses to denote the function's output.
const RESULT_VAR: &str = "result";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefinementOutcome {
    /// The body provably satisfies the refinement on every (precondition-satisfying) input.
    Sound,
    /// A solver counterexample: a concrete input on which the body breaks the refinement.
    Violated(String),
    /// Outside the decidable fragment, or the solver could not decide — never a false SOUND.
    Unverifiable(String),
    /// There is nothing to check (no declared refinements and a non-`nat` result).
    NotApplicable,
    /// No SMT solver was available.
    NoSolver,
}

/// One refinement's verdict, with a human label naming what was checked.
#[derive(Debug, Clone)]
pub struct RefinementReport {
    pub label: String,
    pub outcome: RefinementOutcome,
}

/// Unwrap a `forall`-quantified (polymorphic) type to its inner `fn`, or return the type as-is.
fn unwrap_forall(ty: &J) -> &J {
    if ty.get("kind").and_then(|k| k.as_str()) == Some("forall") {
        ty.get("body").unwrap_or(ty)
    } else {
        ty
    }
}

/// Whether a type AST is the `nat` builtin.
fn is_nat(ty: &J) -> bool {
    ty.get("kind").and_then(|k| k.as_str()) == Some("builtin")
        && ty.get("name").and_then(|n| n.as_str()) == Some("nat")
}

fn app(op: &str, args: Vec<J>) -> J {
    json!({ "kind": "app", "op": op, "args": args })
}
fn var(name: &str) -> J {
    json!({ "kind": "var", "name": name })
}
fn int_lit(n: i64) -> J {
    json!({ "kind": "lit", "value": { "kind": "int", "value": n } })
}

/// The function's result, as a term over its parameters: a non-recursive body is inlined (its inner
/// expression already ranges over the parameter names); a recursive one applies `self`, which the prover
/// encodes as a `define-fun-rec`. Returns `(param_names, result_expr, body_for_self)`.
fn result_term<'a>(body: &'a J, pnames: &[String]) -> Option<(J, Option<&'a J>)> {
    let inner = body.get("body")?;
    if references_self(inner) {
        let xs: Vec<J> = pnames.iter().map(|n| var(n)).collect();
        Some((apply_self_spine(&xs), Some(body)))
    } else {
        Some((inner.clone(), None))
    }
}

/// Discharge one postcondition: `∀ params. (⋀ pre) ⟹ post[result := result_expr]`.
fn check_post(
    pnames: &[String],
    pre: &[J],
    post: &J,
    result_expr: &J,
    body_opt: Option<&J>,
    solver: &str,
) -> RefinementOutcome {
    // Substitute the reserved `result` variable with the body's actual result term.
    let subst: BTreeMap<String, J> = [(RESULT_VAR.to_string(), result_expr.clone())].into_iter().collect();
    let post_sub = subst_many(post, &subst);

    // (⋀ pre) ⟹ post  ≡  ¬(⋀ pre) ∨ post.
    let body_pred = if pre.is_empty() {
        post_sub
    } else {
        let conj = if pre.len() == 1 { pre[0].clone() } else { app("and", pre.to_vec()) };
        app("or", vec![app("not", vec![conj]), post_sub])
    };
    // A nullary contract has no parameter to range over — a lone dummy makes it a well-formed `forall`.
    let vars: Vec<String> = if pnames.is_empty() { vec!["__dummy".into()] } else { pnames.to_vec() };
    let goal = json!({ "kind": "forall", "vars": vars, "body": body_pred });
    decide(&goal, body_opt, solver)
}

/// Check every refinement of a record — the declared `pre`/`post` plus the type-implied `nat`. Returns one
/// report per checked refinement (empty-ish: a single NOT-APPLICABLE report when there is nothing to check).
pub fn check_refinements(sig_type: &J, refinements: &[J], body: &J, solver: &str) -> Vec<RefinementReport> {
    let one = |label: String, outcome: RefinementOutcome| vec![RefinementReport { label, outcome }];

    let fn_ty = unwrap_forall(sig_type);
    if fn_ty.get("kind").and_then(|k| k.as_str()) != Some("fn") {
        return one("signature".into(), RefinementOutcome::Unverifiable("signature type is not a function".into()));
    }
    let result_ty = match fn_ty.get("result") {
        Some(r) => r,
        None => return one("signature".into(), RefinementOutcome::Unverifiable("function type has no result".into())),
    };
    let param_tys: Vec<J> = fn_ty.get("params").and_then(|p| p.as_array()).cloned().unwrap_or_default();

    let Some(pnames) = params(body) else {
        return one("signature".into(), RefinementOutcome::Unverifiable("body is not a `lambda`".into()));
    };
    if pnames.iter().any(|p| p == RESULT_VAR) {
        return one(
            "signature".into(),
            RefinementOutcome::Unverifiable(format!("a parameter is named `{RESULT_VAR}`, the reserved output variable")),
        );
    }
    if pnames.len() != param_tys.len() {
        return one(
            "signature".into(),
            RefinementOutcome::Unverifiable(format!(
                "arity mismatch: type has {} params, body has {}",
                param_tys.len(),
                pnames.len()
            )),
        );
    }
    let Some((result_expr, body_opt)) = result_term(body, &pnames) else {
        return one("signature".into(), RefinementOutcome::Unverifiable("lambda has no body".into()));
    };

    // Preconditions assumed for *every* postcondition: the declared `pre` predicates plus the implicit
    // non-negativity of every `nat`-typed parameter.
    let mut pre: Vec<J> = refinements
        .iter()
        .filter(|r| r.get("kind").and_then(|k| k.as_str()) == Some("pre"))
        .filter_map(|r| r.get("expr").cloned())
        .collect();
    for (ty, name) in param_tys.iter().zip(&pnames) {
        if is_nat(ty) {
            pre.push(app("ge", vec![var(name), int_lit(0)]));
        }
    }

    let mut reports: Vec<RefinementReport> = Vec::new();
    // Declared postconditions.
    for (i, post) in refinements
        .iter()
        .filter(|r| r.get("kind").and_then(|k| k.as_str()) == Some("post"))
        .filter_map(|r| r.get("expr"))
        .enumerate()
    {
        reports.push(RefinementReport {
            label: format!("post[{i}]"),
            outcome: check_post(&pnames, &pre, post, &result_expr, body_opt, solver),
        });
    }
    // The type-implied `nat` refinement: result ≥ 0.
    if is_nat(result_ty) {
        let post = app("ge", vec![var(RESULT_VAR), int_lit(0)]);
        reports.push(RefinementReport {
            label: "nat-result (≥ 0)".into(),
            outcome: check_post(&pnames, &pre, &post, &result_expr, body_opt, solver),
        });
    }
    // `inv` refinements are declared but not yet checked — surface that honestly rather than ignore them.
    for _ in refinements.iter().filter(|r| r.get("kind").and_then(|k| k.as_str()) == Some("inv")) {
        reports.push(RefinementReport {
            label: "inv".into(),
            outcome: RefinementOutcome::Unverifiable("`inv` refinements are not checked in v0.1".into()),
        });
    }

    if reports.is_empty() {
        return one("refinements".into(), RefinementOutcome::NotApplicable);
    }
    reports
}

/// Convenience: the type-implied `nat` refinement only (no declared `pre`/`post`). Returns the single
/// outcome — `NotApplicable` when the result type is not `nat`.
pub fn check_nat_refinement(sig_type: &J, body: &J, solver: &str) -> RefinementOutcome {
    let fn_ty = unwrap_forall(sig_type);
    let not_nat = fn_ty.get("result").map(|r| !is_nat(r)).unwrap_or(true);
    if not_nat {
        return RefinementOutcome::NotApplicable;
    }
    check_refinements(sig_type, &[], body, solver)
        .into_iter()
        .find(|r| r.label == "nat-result (≥ 0)")
        .map(|r| r.outcome)
        .unwrap_or_else(|| RefinementOutcome::Unverifiable("could not build the nat obligation".into()))
}

/// Discharge the obligation: first-order SMT, then a structural-induction fallback for recursive bodies
/// (mirrors `prove`). A non-closing induction is UNVERIFIABLE, never a false SOUND.
fn decide(goal: &J, body: Option<&J>, solver: &str) -> RefinementOutcome {
    match prove_property(goal, body, solver).0 {
        ProofOutcome::Proved => RefinementOutcome::Sound,
        ProofOutcome::Refuted(model) => RefinementOutcome::Violated(model),
        ProofOutcome::NoSolver => RefinementOutcome::NoSolver,
        ProofOutcome::Unknown => RefinementOutcome::Unverifiable("solver could not decide".into()),
        ProofOutcome::Unsupported(_) => {
            match prove_by_induction_with_exploration(goal, body, solver, DEFAULT_LEMMA_DEPTH).0 {
                InductionOutcome::Proved | InductionOutcome::ProvedWithLemmas(_) => RefinementOutcome::Sound,
                // A satisfiable induction base is a concrete short input on which the body breaks the contract.
                InductionOutcome::Failed(model) => RefinementOutcome::Violated(model),
                InductionOutcome::NoSolver => RefinementOutcome::NoSolver,
                InductionOutcome::Unknown => RefinementOutcome::Unverifiable("induction did not close".into()),
                InductionOutcome::Unsupported(why) => RefinementOutcome::Unverifiable(why),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solver() -> Option<&'static str> {
        for s in ["z3", "cvc5"] {
            if std::process::Command::new(s).arg("--version").output().map(|o| o.status.success()).unwrap_or(false) {
                return Some(s);
            }
        }
        None
    }

    fn nat() -> J {
        json!({ "kind": "builtin", "name": "nat" })
    }
    fn int() -> J {
        json!({ "kind": "builtin", "name": "int" })
    }
    fn fn_ty(params: Vec<J>, result: J) -> J {
        json!({ "kind": "fn", "params": params, "result": result })
    }
    fn lambda(ps: &[&str], inner: J) -> J {
        let params: Vec<J> = ps.iter().map(|p| json!({ "name": p })).collect();
        json!({ "kind": "lambda", "params": params, "body": inner })
    }
    fn refinement(kind: &str, expr: J) -> J {
        json!({ "kind": kind, "expr": expr })
    }
    fn nat_outcome(ty: &J, body: &J, s: &str) -> RefinementOutcome {
        check_nat_refinement(ty, body, s)
    }

    // ---- the type-implied nat refinement (unchanged behavior) ----

    #[test]
    fn nat_result_with_nat_param_is_sound() {
        let Some(s) = solver() else { return };
        let ty = fn_ty(vec![nat()], nat());
        let body = lambda(&["n"], app("add", vec![var("n"), var("n")]));
        assert_eq!(nat_outcome(&ty, &body, s), RefinementOutcome::Sound);
    }

    #[test]
    fn negatable_body_declared_nat_is_violated() {
        let Some(s) = solver() else { return };
        let ty = fn_ty(vec![int(), int()], nat());
        let body = lambda(&["a", "b"], app("sub", vec![var("a"), var("b")]));
        match nat_outcome(&ty, &body, s) {
            RefinementOutcome::Violated(_) => {}
            other => panic!("expected VIOLATED, got {other:?}"),
        }
    }

    #[test]
    fn abs_is_sound_nat() {
        let Some(s) = solver() else { return };
        let ty = fn_ty(vec![int()], nat());
        let body = lambda(&["n"], app("abs", vec![var("n")]));
        assert_eq!(nat_outcome(&ty, &body, s), RefinementOutcome::Sound);
    }

    #[test]
    fn non_nat_result_is_not_applicable() {
        let ty = fn_ty(vec![int(), int()], int());
        let body = lambda(&["a", "b"], app("add", vec![var("a"), var("b")]));
        assert_eq!(nat_outcome(&ty, &body, "z3"), RefinementOutcome::NotApplicable);
    }

    #[test]
    fn recursive_length_is_sound_nat_by_induction() {
        let Some(s) = solver() else { return };
        let ty = fn_ty(
            vec![json!({ "kind": "apply", "ctor": { "kind": "builtin", "name": "List" }, "args": [{ "kind": "var", "name": "a" }] })],
            nat(),
        );
        let body = lambda(
            &["xs"],
            json!({
                "kind": "case",
                "scrutinee": app("null", vec![var("xs")]),
                "arms": [
                    { "pattern": { "kind": "lit", "value": { "kind": "bool", "value": true } }, "body": int_lit(0) },
                    { "pattern": { "kind": "lit", "value": { "kind": "bool", "value": false } }, "body":
                        app("add", vec![int_lit(1), app("apply", vec![var("self"), app("tail", vec![var("xs")])])]) },
                ]
            }),
        );
        assert_eq!(nat_outcome(&ty, &body, s), RefinementOutcome::Sound);
    }

    #[test]
    fn recursive_countdown_can_go_negative_is_violated() {
        let Some(s) = solver() else { return };
        let ty = fn_ty(vec![nat()], nat());
        let body = lambda(&["n"], app("sub", vec![var("n"), int_lit(1)]));
        match nat_outcome(&ty, &body, s) {
            RefinementOutcome::Violated(_) => {}
            other => panic!("expected VIOLATED, got {other:?}"),
        }
    }

    // ---- declared pre/post refinements (the `result` convention) ----

    #[test]
    fn declared_post_matches_body_is_sound() {
        let Some(s) = solver() else { return };
        // add : (int,int) -> int, post: result = a + b. The body IS add(a,b), so sound.
        let ty = fn_ty(vec![int(), int()], int());
        let body = lambda(&["a", "b"], app("add", vec![var("a"), var("b")]));
        let post = refinement("post", app("eq", vec![var("result"), app("add", vec![var("a"), var("b")])]));
        let reports = check_refinements(&ty, &[post], &body, s);
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].outcome, RefinementOutcome::Sound);
    }

    #[test]
    fn declared_post_contradicted_by_body_is_violated() {
        let Some(s) = solver() else { return };
        // post: result = a + b, but the body is mul(a,b) — violated (e.g. a=1,b=2: 2 ≠ 3).
        let ty = fn_ty(vec![int(), int()], int());
        let body = lambda(&["a", "b"], app("mul", vec![var("a"), var("b")]));
        let post = refinement("post", app("eq", vec![var("result"), app("add", vec![var("a"), var("b")])]));
        let reports = check_refinements(&ty, &[post], &body, s);
        match &reports[0].outcome {
            RefinementOutcome::Violated(_) => {}
            other => panic!("expected VIOLATED, got {other:?}"),
        }
    }

    #[test]
    fn precondition_gates_the_postcondition() {
        let Some(s) = solver() else { return };
        // \a b -> sub(a,b), post: result >= 0. Without a pre it's VIOLATED (a<b); with pre a>=b it's SOUND.
        let ty = fn_ty(vec![int(), int()], int());
        let body = lambda(&["a", "b"], app("sub", vec![var("a"), var("b")]));
        let post = refinement("post", app("ge", vec![var("result"), int_lit(0)]));
        // No precondition → violated.
        let bare = check_refinements(&ty, std::slice::from_ref(&post), &body, s);
        assert!(matches!(bare[0].outcome, RefinementOutcome::Violated(_)));
        // With `pre: a >= b` → sound.
        let pre = refinement("pre", app("ge", vec![var("a"), var("b")]));
        let guarded = check_refinements(&ty, &[pre, post], &body, s);
        assert_eq!(guarded[0].outcome, RefinementOutcome::Sound);
    }

    #[test]
    fn post_and_implicit_nat_both_reported() {
        let Some(s) = solver() else { return };
        // double : nat -> nat, post: result = n + n. Two reports: the post (sound) and the nat (sound).
        let ty = fn_ty(vec![nat()], nat());
        let body = lambda(&["n"], app("add", vec![var("n"), var("n")]));
        let post = refinement("post", app("eq", vec![var("result"), app("add", vec![var("n"), var("n")])]));
        let reports = check_refinements(&ty, &[post], &body, s);
        assert_eq!(reports.len(), 2, "{reports:?}");
        assert!(reports.iter().all(|r| r.outcome == RefinementOutcome::Sound));
    }
}
