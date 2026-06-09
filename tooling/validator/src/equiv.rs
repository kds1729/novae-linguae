//! Semantic-equivalence proving — decide whether two functions compute the same thing, `∀x. f(x) =
//! g(x)`, over the unbounded domain. This addresses the README's named open problem ("semantic
//! equivalence vs hash equivalence"): two records can be hash-different yet behaviorally identical, and
//! until now the commons could only dedupe *byte*-identical artifacts. With this, behaviorally-equal
//! functions can be recognized and clustered, upgrading principle 2's "perfect deduplication".
//!
//! It reuses the existing property prover rather than introducing a new two-function encoding. The key
//! move: **inline the non-recursive function's body into a property of the other** (taken as `self`),
//! turning `f ≡ g` into the single-`self` law `∀x. self(x) = g_body[x]`, which `prove`/induction/lemma
//! discovery already handle. So `f`/`g` equivalence is proved with the full strength of the SMT +
//! structural-induction + lemma-discovery pipeline — including list laws (e.g. `\xs. reverse(reverse
//! xs)` ≡ `\xs. xs`).
//!
//! Honest scope (v0.1): unary functions, and at least one side non-recursive (the side inlined). Two
//! mutually-recursive functions, or arity ≠ 1, are reported UNSUPPORTED. A clean DISTINCT verdict comes
//! only with a solver counterexample (the first-order path); an induction that fails to *prove*
//! equivalence is reported UNKNOWN, not DISTINCT, since a non-closing induction is not a refutation.

use serde_json::{json, Value as J};

use crate::{
    prove_by_induction_with_exploration, prove_property, InductionOutcome, ProofOutcome, DEFAULT_LEMMA_DEPTH,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EquivVerdict {
    /// Proved `∀x. f(x) = g(x)`. Carries any auxiliary lemmas the proof needed (empty if first-order).
    Equivalent(Vec<String>),
    /// A solver counterexample shows the functions differ; carries the model.
    Distinct(String),
    /// Could not decide (solver gave up, or induction did not close).
    Unknown,
    /// Outside the supported fragment (arity ≠ 1, both recursive, malformed).
    Unsupported(String),
    /// No SMT solver was available.
    NoSolver,
}

/// The lambda parameter names of a body, or `None` if it isn't a lambda.
fn params(body: &J) -> Option<Vec<String>> {
    if body.get("kind").and_then(|k| k.as_str()) != Some("lambda") {
        return None;
    }
    Some(
        body.get("params")
            .and_then(|p| p.as_array())
            .map(|a| a.iter().filter_map(|x| x.get("name").and_then(|n| n.as_str()).map(String::from)).collect())
            .unwrap_or_default(),
    )
}

fn inner(body: &J) -> Option<&J> {
    body.get("body")
}

/// Does the expression refer to `self` (i.e. recurse)? Checks the `self` var and the `self`/apply-of-self
/// call forms, descending the AST.
fn references_self(node: &J) -> bool {
    if node.get("kind").and_then(|k| k.as_str()) == Some("var")
        && node.get("name").and_then(|n| n.as_str()) == Some("self")
    {
        return true;
    }
    if node.get("op").and_then(|o| o.as_str()) == Some("self") {
        return true;
    }
    for key in ["body", "value", "scrutinee", "fn"] {
        if let Some(c) = node.get(key) {
            if references_self(c) {
                return true;
            }
        }
    }
    for key in ["args", "arms"] {
        if let Some(arr) = node.get(key).and_then(|a| a.as_array()) {
            if arr.iter().any(|c| references_self(c.get("body").unwrap_or(c))) {
                return true;
            }
        }
    }
    false
}

/// Substitute the variable `name` with `repl` throughout `node` (no shadowing analysis — used only on
/// the non-recursive inlined body, which is a plain expression over its single parameter).
fn subst(node: &J, name: &str, repl: &J) -> J {
    match node {
        J::Object(map) => {
            if map.get("kind").and_then(|k| k.as_str()) == Some("var")
                && map.get("name").and_then(|n| n.as_str()) == Some(name)
            {
                return repl.clone();
            }
            J::Object(map.iter().map(|(k, v)| (k.clone(), subst(v, name, repl))).collect())
        }
        J::Array(items) => J::Array(items.iter().map(|v| subst(v, name, repl)).collect()),
        other => other.clone(),
    }
}

/// Prove (or refute) that the two bodies are extensionally equal: `∀x. f(x) = g(x)`.
pub fn prove_equivalent(body_f: &J, body_g: &J, solver: &str) -> EquivVerdict {
    let (Some(pf), Some(pg)) = (params(body_f), params(body_g)) else {
        return EquivVerdict::Unsupported("both inputs must be `lambda` bodies".into());
    };
    if pf.len() != pg.len() {
        return EquivVerdict::Unsupported(format!("arity mismatch: {} vs {}", pf.len(), pg.len()));
    }
    if pf.len() != 1 {
        return EquivVerdict::Unsupported(format!("only unary functions are supported (got arity {})", pf.len()));
    }
    let (Some(if_), Some(ig)) = (inner(body_f), inner(body_g)) else {
        return EquivVerdict::Unsupported("lambda has no body".into());
    };
    let x = json!({ "kind": "var", "name": "x" });
    let eq = |lhs: J, rhs: J| {
        json!({ "kind": "forall", "vars": ["x"], "body": { "kind": "app", "op": "eq", "args": [lhs, rhs] } })
    };
    let apply_self = json!({ "kind": "app", "op": "apply", "args": [{ "kind": "var", "name": "self" }, x.clone()] });

    // Build the equivalence law and choose the body to supply as `self`. When **both** sides are
    // non-recursive, inline *both* into the law (`eq(f_body[x], g_body[x])`, no `self`) so the operations
    // stay visible to lemma discovery. When one side recurses, it becomes `self` (a `define-fun-rec`) and
    // the other is inlined. Both recursive is out of scope for v0.1.
    let (f_rec, g_rec) = (references_self(if_), references_self(ig));
    let (prop, body) = if !f_rec && !g_rec {
        (eq(subst(if_, &pf[0], &x), subst(ig, &pg[0], &x)), None)
    } else if !g_rec {
        (eq(apply_self, subst(ig, &pg[0], &x)), Some(body_f))
    } else if !f_rec {
        (eq(apply_self, subst(if_, &pf[0], &x)), Some(body_g))
    } else {
        return EquivVerdict::Unsupported("both functions are recursive (v0.1 inlines one side)".into());
    };

    // First-order SMT first; fall back to induction + lemma discovery (mirrors `prove`).
    match prove_property(&prop, body, solver).0 {
        ProofOutcome::Proved => EquivVerdict::Equivalent(vec![]),
        ProofOutcome::Refuted(model) => EquivVerdict::Distinct(model),
        ProofOutcome::NoSolver => EquivVerdict::NoSolver,
        ProofOutcome::Unknown => EquivVerdict::Unknown,
        ProofOutcome::Unsupported(_) => {
            match prove_by_induction_with_exploration(&prop, body, solver, DEFAULT_LEMMA_DEPTH).0 {
                InductionOutcome::Proved => EquivVerdict::Equivalent(vec![]),
                InductionOutcome::ProvedWithLemmas(ls) => EquivVerdict::Equivalent(ls),
                InductionOutcome::NoSolver => EquivVerdict::NoSolver,
                // A non-closing induction is not a refutation — report UNKNOWN, never a false DISTINCT.
                InductionOutcome::Unknown | InductionOutcome::Failed(_) => EquivVerdict::Unknown,
                InductionOutcome::Unsupported(why) => EquivVerdict::Unsupported(why),
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

    // \n -> add(n, n)   (fn form: app with `fn`)
    fn double_add() -> J {
        json!({ "kind": "lambda", "params": [{ "name": "n" }], "body":
            { "kind": "app", "fn": { "kind": "var", "name": "add" }, "args": [{ "kind": "var", "name": "n" }, { "kind": "var", "name": "n" }] } })
    }
    // \m -> mul(2, m)
    fn double_mul() -> J {
        json!({ "kind": "lambda", "params": [{ "name": "m" }], "body":
            { "kind": "app", "fn": { "kind": "var", "name": "mul" }, "args": [{ "kind": "lit", "value": { "kind": "int", "value": 2 } }, { "kind": "var", "name": "m" }] } })
    }
    // \k -> add(k, 1)
    fn succ() -> J {
        json!({ "kind": "lambda", "params": [{ "name": "k" }], "body":
            { "kind": "app", "fn": { "kind": "var", "name": "add" }, "args": [{ "kind": "var", "name": "k" }, { "kind": "lit", "value": { "kind": "int", "value": 1 } }] } })
    }
    // \xs -> reverse(reverse(xs))
    fn rev_rev() -> J {
        json!({ "kind": "lambda", "params": [{ "name": "xs" }], "body":
            { "kind": "app", "fn": { "kind": "var", "name": "reverse" }, "args": [
                { "kind": "app", "fn": { "kind": "var", "name": "reverse" }, "args": [{ "kind": "var", "name": "xs" }] }] } })
    }
    // \ys -> ys   (identity)
    fn ident() -> J {
        json!({ "kind": "lambda", "params": [{ "name": "ys" }], "body": { "kind": "var", "name": "ys" } })
    }

    #[test]
    fn equivalent_first_order() {
        let Some(s) = solver() else { return };
        // double-via-add ≡ double-via-mul.
        assert_eq!(prove_equivalent(&double_add(), &double_mul(), s), EquivVerdict::Equivalent(vec![]));
    }

    #[test]
    fn distinct_first_order_gives_counterexample() {
        let Some(s) = solver() else { return };
        match prove_equivalent(&double_add(), &succ(), s) {
            EquivVerdict::Distinct(_) => {}
            other => panic!("expected DISTINCT, got {other:?}"),
        }
    }

    #[test]
    fn equivalent_list_law_via_induction() {
        let Some(s) = solver() else { return };
        // \xs. reverse(reverse(xs)) ≡ \xs. xs — both non-recursive (builtin reverse), proved by the
        // inductive lemma-discovery path.
        match prove_equivalent(&rev_rev(), &ident(), s) {
            EquivVerdict::Equivalent(_) => {}
            other => panic!("expected EQUIVALENT, got {other:?}"),
        }
    }

    #[test]
    fn arity_mismatch_is_unsupported() {
        let bin = json!({ "kind": "lambda", "params": [{ "name": "a" }, { "name": "b" }], "body": { "kind": "var", "name": "a" } });
        assert!(matches!(prove_equivalent(&bin, &ident(), "z3"), EquivVerdict::Unsupported(_)));
    }
}
