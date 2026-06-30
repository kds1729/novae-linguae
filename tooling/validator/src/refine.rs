//! Refinement checking — verify a function body actually satisfies the refinement implied by its
//! declared type. The HM typechecker ([`crate::typecheck`]) erases `nat` to `int` and never checks the
//! body stays non-negative, so a body declared `… -> nat` that can produce a *negative* `int` type-checks
//! clean today. This closes that hole — the third pillar of "verified by default" (principle 3) for the
//! one refinement the type language bakes in.
//!
//! A `nat` is a non-negative `int`. For a `nat`-result function this proves
//!
//! ```text
//!   ∀ params. (⋀ nat-typed params ≥ 0) ⟹ body(params) ≥ 0
//! ```
//!
//! via the [`crate::prove`] backend (first-order SMT, with a structural-induction fallback for a
//! recursive body — e.g. a recursive `length : List a -> nat` is proved ≥ 0 by induction). The `nat`-ness
//! of the parameters is the *precondition*: `double : nat -> nat` is sound because `n ≥ 0 ⟹ n + n ≥ 0`,
//! while `\a b -> sub(a, b) : (int, int) -> nat` is **violated** (`a = 0, b = 1 ⟹ −1 < 0`).
//!
//! Outcomes: SOUND (proved), VIOLATED (a solver counterexample — a real input on which the body breaks
//! its declared `nat`), UNVERIFIABLE (out of the decidable fragment / solver undecided — never a false
//! SOUND), NOT-APPLICABLE (the result type is not `nat`, so there is nothing to refine), or NO-SOLVER.
//! This is intentionally conservative: only a solver counterexample yields VIOLATED, and only a closed
//! proof yields SOUND.

use serde_json::{json, Value as J};
use std::collections::{BTreeMap, BTreeSet};

use crate::equiv::{apply_self_spine, fresh_vars, params, references_self, subst_many};
use crate::{
    prove_by_induction_with_exploration, prove_property, InductionOutcome, ProofOutcome, DEFAULT_LEMMA_DEPTH,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefinementOutcome {
    /// The body provably satisfies its declared `nat` result on every (precondition-satisfying) input.
    Sound,
    /// A solver counterexample: a concrete input on which the body produces a negative value.
    Violated(String),
    /// Outside the decidable fragment, or the solver could not decide — never a false SOUND.
    Unverifiable(String),
    /// The result type is not `nat`, so the type bakes in no refinement to check here.
    NotApplicable,
    /// No SMT solver was available.
    NoSolver,
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

/// `ge(a, b)` / `and`/`or`/`not` predicate-AST builders.
fn app(op: &str, args: Vec<J>) -> J {
    json!({ "kind": "app", "op": op, "args": args })
}
fn var(name: &str) -> J {
    json!({ "kind": "var", "name": name })
}
fn int_lit(n: i64) -> J {
    json!({ "kind": "lit", "value": { "kind": "int", "value": n } })
}

/// Check that `body` honors the `nat` refinement implied by `sig_type` (the record's `signature.type`).
pub fn check_nat_refinement(sig_type: &J, body: &J, solver: &str) -> RefinementOutcome {
    let fn_ty = unwrap_forall(sig_type);
    if fn_ty.get("kind").and_then(|k| k.as_str()) != Some("fn") {
        return RefinementOutcome::Unverifiable("signature type is not a function".into());
    }
    let result_ty = match fn_ty.get("result") {
        Some(r) => r,
        None => return RefinementOutcome::Unverifiable("function type has no result".into()),
    };
    // Only a `nat` result carries a refinement to check here.
    if !is_nat(result_ty) {
        return RefinementOutcome::NotApplicable;
    }
    let param_tys: Vec<J> = fn_ty.get("params").and_then(|p| p.as_array()).cloned().unwrap_or_default();

    let Some(pnames) = params(body) else {
        return RefinementOutcome::Unverifiable("body is not a `lambda`".into());
    };
    if pnames.len() != param_tys.len() {
        return RefinementOutcome::Unverifiable(format!(
            "arity mismatch: type has {} params, body has {}",
            param_tys.len(),
            pnames.len()
        ));
    }
    if pnames.is_empty() {
        // A nullary `nat` constant: prove `body ≥ 0` with no quantifier.
        let inner = match body.get("body") {
            Some(b) => b,
            None => return RefinementOutcome::Unverifiable("lambda has no body".into()),
        };
        let goal = json!({ "kind": "forall", "vars": ["__dummy"], "body": ge_zero(inner.clone()) });
        return decide(&goal, None, solver);
    }

    // One fresh quantified variable per parameter, so the goal never collides with a parameter or builtin.
    let avoid: BTreeSet<String> = pnames.iter().cloned().collect();
    let var_names = fresh_vars(pnames.len(), &avoid);
    let xs: Vec<J> = var_names.iter().map(|n| var(n)).collect();

    // Precondition: every `nat`-typed parameter is ≥ 0 (the rest are unconstrained `int`s).
    let pre: Vec<J> = param_tys
        .iter()
        .zip(&xs)
        .filter(|(ty, _)| is_nat(ty))
        .map(|(_, x)| app("ge", vec![x.clone(), int_lit(0)]))
        .collect();

    // The function's result, as a term over the fresh variables: inline a non-recursive body; for a
    // recursive one apply `self` and hand the body to the prover as the `define-fun-rec`.
    let inner = match body.get("body") {
        Some(b) => b,
        None => return RefinementOutcome::Unverifiable("lambda has no body".into()),
    };
    let (result_expr, body_opt) = if references_self(inner) {
        (apply_self_spine(&xs), Some(body))
    } else {
        let map: BTreeMap<String, J> = pnames.iter().cloned().zip(xs.iter().cloned()).collect();
        (subst_many(inner, &map), None)
    };

    // Goal: ∀ vars. pre ⟹ result ≥ 0, i.e. ∀ vars. ¬(⋀ pre) ∨ result ≥ 0.
    let post = ge_zero(result_expr);
    let body_pred = match pre.len() {
        0 => post,
        _ => {
            let conj = if pre.len() == 1 { pre.into_iter().next().unwrap() } else { app("and", pre) };
            app("or", vec![app("not", vec![conj]), post])
        }
    };
    let goal = json!({ "kind": "forall", "vars": var_names, "body": body_pred });
    decide(&goal, body_opt, solver)
}

/// `expr ≥ 0`.
fn ge_zero(expr: J) -> J {
    app("ge", vec![expr, int_lit(0)])
}

/// Discharge the obligation: first-order SMT, then a structural-induction fallback for recursive bodies
/// (mirrors `prove`). Maps the prover's verdicts onto the refinement outcomes — a non-closing induction is
/// UNVERIFIABLE, never a false SOUND.
fn decide(goal: &J, body: Option<&J>, solver: &str) -> RefinementOutcome {
    match prove_property(goal, body, solver).0 {
        ProofOutcome::Proved => RefinementOutcome::Sound,
        ProofOutcome::Refuted(model) => RefinementOutcome::Violated(model),
        ProofOutcome::NoSolver => RefinementOutcome::NoSolver,
        ProofOutcome::Unknown => RefinementOutcome::Unverifiable("solver could not decide".into()),
        ProofOutcome::Unsupported(_) => {
            match prove_by_induction_with_exploration(goal, body, solver, DEFAULT_LEMMA_DEPTH).0 {
                InductionOutcome::Proved | InductionOutcome::ProvedWithLemmas(_) => RefinementOutcome::Sound,
                // A satisfiable induction base is a concrete short input on which the body goes negative.
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

    #[test]
    fn nat_result_with_nat_param_is_sound() {
        let Some(s) = solver() else { return };
        // double : nat -> nat, body \n -> add(n, n). n ≥ 0 ⟹ n + n ≥ 0.
        let ty = fn_ty(vec![nat()], nat());
        let body = lambda(&["n"], app("add", vec![var("n"), var("n")]));
        assert_eq!(check_nat_refinement(&ty, &body, s), RefinementOutcome::Sound);
    }

    #[test]
    fn negatable_body_declared_nat_is_violated() {
        let Some(s) = solver() else { return };
        // \a b -> sub(a, b) : (int, int) -> nat — can be negative (a=0, b=1). VIOLATED with a counterexample.
        let ty = fn_ty(vec![int(), int()], nat());
        let body = lambda(&["a", "b"], app("sub", vec![var("a"), var("b")]));
        match check_nat_refinement(&ty, &body, s) {
            RefinementOutcome::Violated(_) => {}
            other => panic!("expected VIOLATED, got {other:?}"),
        }
    }

    #[test]
    fn abs_is_sound_nat() {
        let Some(s) = solver() else { return };
        // \n -> abs(n) : int -> nat — always ≥ 0 (no precondition needed).
        let ty = fn_ty(vec![int()], nat());
        let body = lambda(&["n"], app("abs", vec![var("n")]));
        assert_eq!(check_nat_refinement(&ty, &body, s), RefinementOutcome::Sound);
    }

    #[test]
    fn non_nat_result_is_not_applicable() {
        // add : (int, int) -> int — no nat refinement to check.
        let ty = fn_ty(vec![int(), int()], int());
        let body = lambda(&["a", "b"], app("add", vec![var("a"), var("b")]));
        assert_eq!(check_nat_refinement(&ty, &body, "z3"), RefinementOutcome::NotApplicable);
    }

    #[test]
    fn recursive_length_is_sound_nat_by_induction() {
        let Some(s) = solver() else { return };
        // length : List a -> nat, recursive: \xs -> case null xs of true -> 0 | false -> add(1, self(tail xs)).
        // Proved ≥ 0 by structural induction (base 0 ≥ 0; step 1 + self(tail) ≥ 0 given the IH).
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
        assert_eq!(check_nat_refinement(&ty, &body, s), RefinementOutcome::Sound);
    }

    #[test]
    fn recursive_countdown_can_go_negative_is_violated() {
        let Some(s) = solver() else { return };
        // \n -> sub(n, 1) : nat -> nat — n ≥ 0 does NOT give n − 1 ≥ 0 (fails at n = 0). VIOLATED.
        let ty = fn_ty(vec![nat()], nat());
        let body = lambda(&["n"], app("sub", vec![var("n"), int_lit(1)]));
        match check_nat_refinement(&ty, &body, s) {
            RefinementOutcome::Violated(_) => {}
            other => panic!("expected VIOLATED, got {other:?}"),
        }
    }
}
