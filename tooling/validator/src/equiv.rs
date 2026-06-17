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
//! Scope: functions of any arity ≥ 1 with matching parameter counts, where at least one side is
//! non-recursive (the side inlined). The underlying prover already quantifies over several variables and
//! inducts on one while treating the rest as free, so a multi-argument law (e.g. `\a b -> add(a, b)` ≡
//! `\a b -> add(b, a)`) is proved with exactly the same machinery as a unary one.
//!
//! Before either path runs, an **α-equivalence** fast path checks whether the two bodies are the same
//! term up to consistent renaming of bound variables — hash-different but identical. That needs no
//! solver and, crucially, decides the case where *both* functions recurse (which the law-building path
//! can only report UNSUPPORTED), so two renamed copies of the same recursive function are recognized as
//! equivalent.
//!
//! Out of scope (reported UNSUPPORTED): nullary constants, mismatched arity, and two mutually-recursive
//! functions that are *not* α-equivalent (v0.1 inlines one side); a multi-argument *recursive list*
//! function also exceeds the inductive fragment (a single list parameter) and degrades to UNKNOWN, never
//! a false verdict. A clean DISTINCT comes only from a solver counterexample (the first-order path); a
//! non-closing induction is reported UNKNOWN, not DISTINCT, since it is not a refutation.

use serde_json::{json, Value as J};
use std::collections::{BTreeMap, BTreeSet};

use crate::{
    prove_by_induction_with_exploration, prove_property, InductionOutcome, ProofOutcome, DEFAULT_LEMMA_DEPTH,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EquivVerdict {
    /// Proved `∀x. f(x) = g(x)`. Carries any auxiliary lemmas the proof needed (empty if first-order).
    Equivalent(Vec<String>),
    /// The two functions are identical up to consistent renaming of bound variables (α-equivalence),
    /// established structurally without the solver. They are hash-different but the *same term* — and
    /// because this needs no induction, it also covers the both-recursive case the solver path can only
    /// report UNSUPPORTED.
    EquivalentByRenaming,
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

/// Simultaneously substitute each parameter in `map` with its replacement throughout `node` (no
/// shadowing analysis — used only on the non-recursive inlined body, a plain expression over its
/// parameters). One traversal, and a replacement node is never re-traversed, so the substitutions are
/// independent: renaming `a -> x0` can't be clobbered by a later `x0 -> x1`.
fn subst_many(node: &J, map: &BTreeMap<String, J>) -> J {
    match node {
        J::Object(m) => {
            if m.get("kind").and_then(|k| k.as_str()) == Some("var") {
                if let Some(repl) = m.get("name").and_then(|n| n.as_str()).and_then(|n| map.get(n)) {
                    return repl.clone();
                }
            }
            J::Object(m.iter().map(|(k, v)| (k.clone(), subst_many(v, map))).collect())
        }
        J::Array(items) => J::Array(items.iter().map(|v| subst_many(v, map)).collect()),
        other => other.clone(),
    }
}

/// `n` fresh variable names (`x0`, `x1`, …) avoiding any name in `avoid`, so a synthesized quantified
/// variable can't collide with a parameter (or, transitively, a function head referenced as a `var`).
fn fresh_vars(n: usize, avoid: &BTreeSet<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(n);
    let mut i = 0;
    while out.len() < n {
        let name = format!("x{i}");
        if !avoid.contains(&name) && !out.contains(&name) {
            out.push(name);
        }
        i += 1;
    }
    out
}

/// A left-nested `apply` spine `apply(apply(self, x0), x1) …` over `args`, the form `flatten_call`
/// (in prove.rs / induct.rs) recovers back into `(self, [x0, x1, …])`.
fn apply_self_spine(args: &[J]) -> J {
    let mut node = json!({ "kind": "var", "name": "self" });
    for a in args {
        node = json!({ "kind": "app", "op": "apply", "args": [node, a.clone()] });
    }
    node
}

// --- α-equivalence -------------------------------------------------------------------------------

/// Rewrite a body so every *bound* variable is renamed to a canonical positional name (`$b0`, `$b1`,
/// …) assigned in pre-order. Binders: `lambda` params, `let` name, `case` `bind` patterns, `forall`
/// vars. Free variables (builtins, the function head of an `app`, `self`) are left untouched, so they
/// must still match by name. Two terms that are equal up to consistent bound-variable renaming map to
/// equal canonical forms — and two that aren't, don't (a bound var used in a different *position* gets a
/// different canonical name, e.g. `\a b -> add(a,b)` vs `\a b -> add(b,a)` stay distinct, left to the
/// solver). Object-key order doesn't matter: only ordered binder lists drive the counter, and the result
/// is compared as JSON values (order-independent).
fn alpha_canonical(node: &J) -> J {
    fn go(node: &J, env: &BTreeMap<String, String>, ctr: &mut usize) -> J {
        match node {
            J::Object(m) => {
                let kind = m.get("kind").and_then(|k| k.as_str());
                match kind {
                    Some("var") => {
                        let mut out = m.clone();
                        if let Some(canon) = m.get("name").and_then(|n| n.as_str()).and_then(|n| env.get(n)) {
                            out.insert("name".into(), J::String(canon.clone()));
                        }
                        J::Object(out)
                    }
                    Some("lambda") => {
                        let mut e2 = env.clone();
                        let params = m.get("params").and_then(|p| p.as_array()).cloned().unwrap_or_default();
                        let new_params: Vec<J> = params
                            .iter()
                            .map(|p| {
                                let mut po = p.as_object().cloned().unwrap_or_default();
                                if let Some(name) = p.get("name").and_then(|n| n.as_str()) {
                                    let canon = format!("$b{ctr}");
                                    *ctr += 1;
                                    e2.insert(name.to_string(), canon.clone());
                                    po.insert("name".into(), J::String(canon));
                                }
                                J::Object(po)
                            })
                            .collect();
                        let mut out = m.clone();
                        out.insert("params".into(), J::Array(new_params));
                        if let Some(b) = m.get("body") {
                            out.insert("body".into(), go(b, &e2, ctr));
                        }
                        J::Object(out)
                    }
                    Some("let") => {
                        // The value is in the outer scope; the name binds only the body.
                        let value = m.get("value").map(|v| go(v, env, ctr));
                        let mut e2 = env.clone();
                        let mut out = m.clone();
                        if let Some(name) = m.get("name").and_then(|n| n.as_str()) {
                            let canon = format!("$b{ctr}");
                            *ctr += 1;
                            e2.insert(name.to_string(), canon.clone());
                            out.insert("name".into(), J::String(canon));
                        }
                        if let Some(v) = value {
                            out.insert("value".into(), v);
                        }
                        if let Some(b) = m.get("body") {
                            out.insert("body".into(), go(b, &e2, ctr));
                        }
                        J::Object(out)
                    }
                    Some("case") => {
                        let mut out = m.clone();
                        if let Some(s) = m.get("scrutinee") {
                            out.insert("scrutinee".into(), go(s, env, ctr));
                        }
                        if let Some(arms) = m.get("arms").and_then(|a| a.as_array()) {
                            let new_arms: Vec<J> = arms
                                .iter()
                                .map(|arm| {
                                    let mut ao = arm.as_object().cloned().unwrap_or_default();
                                    let mut e2 = env.clone();
                                    // A `bind` pattern introduces a name scoped to this arm's body.
                                    if let Some(pat) = arm.get("pattern") {
                                        if pat.get("kind").and_then(|k| k.as_str()) == Some("bind") {
                                            let mut po = pat.as_object().cloned().unwrap_or_default();
                                            if let Some(name) = pat.get("name").and_then(|n| n.as_str()) {
                                                let canon = format!("$b{ctr}");
                                                *ctr += 1;
                                                e2.insert(name.to_string(), canon.clone());
                                                po.insert("name".into(), J::String(canon));
                                            }
                                            ao.insert("pattern".into(), J::Object(po));
                                        }
                                    }
                                    if let Some(b) = arm.get("body") {
                                        ao.insert("body".into(), go(b, &e2, ctr));
                                    }
                                    J::Object(ao)
                                })
                                .collect();
                            out.insert("arms".into(), J::Array(new_arms));
                        }
                        J::Object(out)
                    }
                    Some("forall") => {
                        let mut e2 = env.clone();
                        let vars = m.get("vars").and_then(|v| v.as_array()).cloned().unwrap_or_default();
                        let new_vars: Vec<J> = vars
                            .iter()
                            .map(|v| match v.as_str() {
                                Some(name) => {
                                    let canon = format!("$b{ctr}");
                                    *ctr += 1;
                                    e2.insert(name.to_string(), canon.clone());
                                    J::String(canon)
                                }
                                None => v.clone(),
                            })
                            .collect();
                        let mut out = m.clone();
                        out.insert("vars".into(), J::Array(new_vars));
                        if let Some(b) = m.get("body") {
                            out.insert("body".into(), go(b, &e2, ctr));
                        }
                        J::Object(out)
                    }
                    // Any other node (app, lit, …): recurse into every child, scope unchanged.
                    _ => J::Object(m.iter().map(|(k, v)| (k.clone(), go(v, env, ctr))).collect()),
                }
            }
            J::Array(items) => J::Array(items.iter().map(|v| go(v, env, ctr)).collect()),
            other => other.clone(),
        }
    }
    let mut ctr = 0;
    go(node, &BTreeMap::new(), &mut ctr)
}

/// Whether two bodies are equal up to consistent renaming of bound variables. Sound for equivalence: an
/// α-renaming preserves meaning exactly, so this never reports a false equivalence (and, needing no
/// solver, it decides the case where *both* functions recurse).
fn alpha_equivalent(body_f: &J, body_g: &J) -> bool {
    alpha_canonical(body_f) == alpha_canonical(body_g)
}

/// Prove (or refute) that the two bodies are extensionally equal: `∀x. f(x) = g(x)`.
pub fn prove_equivalent(body_f: &J, body_g: &J, solver: &str) -> EquivVerdict {
    let (Some(pf), Some(pg)) = (params(body_f), params(body_g)) else {
        return EquivVerdict::Unsupported("both inputs must be `lambda` bodies".into());
    };
    if pf.len() != pg.len() {
        return EquivVerdict::Unsupported(format!("arity mismatch: {} vs {}", pf.len(), pg.len()));
    }
    if pf.is_empty() {
        return EquivVerdict::Unsupported("nullary (constant) functions are not supported".into());
    }
    let (Some(if_), Some(ig)) = (inner(body_f), inner(body_g)) else {
        return EquivVerdict::Unsupported("lambda has no body".into());
    };

    // Fast path: if the two bodies are the same term up to bound-variable renaming, they are equivalent
    // structurally — no solver, and it works even when both sides recurse (the one case the law-building
    // path below reports UNSUPPORTED).
    if alpha_equivalent(body_f, body_g) {
        return EquivVerdict::EquivalentByRenaming;
    }

    // One fresh quantified variable per argument position, shared by both sides (so the two parameter
    // lists are aligned positionally). The law is `∀ x0..xk. eq(LHS, RHS)`.
    let avoid: BTreeSet<String> = pf.iter().chain(pg.iter()).cloned().collect();
    let var_names = fresh_vars(pf.len(), &avoid);
    let xs: Vec<J> = var_names.iter().map(|n| json!({ "kind": "var", "name": n })).collect();
    let f_map: BTreeMap<String, J> = pf.iter().cloned().zip(xs.iter().cloned()).collect();
    let g_map: BTreeMap<String, J> = pg.iter().cloned().zip(xs.iter().cloned()).collect();
    let eq = |lhs: J, rhs: J| {
        json!({ "kind": "forall", "vars": var_names, "body": { "kind": "app", "op": "eq", "args": [lhs, rhs] } })
    };
    let apply_self = apply_self_spine(&xs);

    // Build the equivalence law and choose the body to supply as `self`. When **both** sides are
    // non-recursive, inline *both* into the law (`eq(f_body[x…], g_body[x…])`, no `self`) so the operations
    // stay visible to lemma discovery. When one side recurses, it becomes `self` (a `define-fun-rec`) and
    // the other is inlined. Both recursive is out of scope for v0.1.
    let (f_rec, g_rec) = (references_self(if_), references_self(ig));
    let (prop, body) = if !f_rec && !g_rec {
        (eq(subst_many(if_, &f_map), subst_many(ig, &g_map)), None)
    } else if !g_rec {
        (eq(apply_self, subst_many(ig, &g_map)), Some(body_f))
    } else if !f_rec {
        (eq(apply_self, subst_many(if_, &f_map)), Some(body_g))
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

    // \a b -> op(a, b), in the `fn` application form.
    fn binop(p: &str, q: &str, op: &str, lhs: &str, rhs: &str) -> J {
        json!({ "kind": "lambda", "params": [{ "name": p }, { "name": q }], "body":
            { "kind": "app", "fn": { "kind": "var", "name": op },
              "args": [{ "kind": "var", "name": lhs }, { "kind": "var", "name": rhs }] } })
    }

    #[test]
    fn equivalent_binary_commutativity() {
        let Some(s) = solver() else { return };
        // \a b -> add(a, b) ≡ \a b -> add(b, a) — proved over every (a, b) by the first-order backend,
        // exercising the multi-argument forall the unary cap used to forbid.
        let f = binop("a", "b", "add", "a", "b");
        let g = binop("a", "b", "add", "b", "a");
        assert_eq!(prove_equivalent(&f, &g, s), EquivVerdict::Equivalent(vec![]));
    }

    #[test]
    fn distinct_binary_gives_counterexample() {
        let Some(s) = solver() else { return };
        // \a b -> add(a, b) ≢ \a b -> sub(a, b) — differ wherever b ≠ 0.
        let f = binop("a", "b", "add", "a", "b");
        let g = binop("a", "b", "sub", "a", "b");
        match prove_equivalent(&f, &g, s) {
            EquivVerdict::Distinct(_) => {}
            other => panic!("expected DISTINCT, got {other:?}"),
        }
    }

    #[test]
    fn nullary_is_unsupported() {
        // Two constant functions: arity 0 has no quantifier to range over — explicitly out of scope.
        let k0 = json!({ "kind": "lambda", "params": [], "body": { "kind": "lit", "value": { "kind": "int", "value": 1 } } });
        let k1 = json!({ "kind": "lambda", "params": [], "body": { "kind": "lit", "value": { "kind": "int", "value": 1 } } });
        assert!(matches!(prove_equivalent(&k0, &k1, "z3"), EquivVerdict::Unsupported(_)));
    }

    // \p -> case null(p) of true -> 0 | false -> add(1, self(tail(p))) — a recursive length, so both
    // sides reference `self`. Only the parameter name varies between copies.
    fn rec_len(p: &str) -> J {
        json!({ "kind": "lambda", "params": [{ "name": p }], "body": {
            "kind": "case",
            "scrutinee": { "kind": "app", "op": "null", "args": [{ "kind": "var", "name": p }] },
            "arms": [
                { "pattern": { "kind": "lit", "value": { "kind": "bool", "value": true } },
                  "body": { "kind": "lit", "value": { "kind": "int", "value": 0 } } },
                { "pattern": { "kind": "lit", "value": { "kind": "bool", "value": false } }, "body": {
                    "kind": "app", "op": "add", "args": [
                        { "kind": "lit", "value": { "kind": "int", "value": 1 } },
                        { "kind": "app", "op": "apply", "args": [
                            { "kind": "var", "name": "self" },
                            { "kind": "app", "op": "tail", "args": [{ "kind": "var", "name": p }] }] }] } }] } })
    }

    #[test]
    fn alpha_renamed_recursive_is_equivalent() {
        // Two copies of the same recursive function differing only in the bound parameter name. The
        // solver path would report UNSUPPORTED (both recursive); α-equivalence decides it — and needs no
        // solver, so this runs everywhere.
        assert_eq!(prove_equivalent(&rec_len("xs"), &rec_len("ys"), "z3"), EquivVerdict::EquivalentByRenaming);
    }

    #[test]
    fn alpha_renamed_nonrecursive_short_circuits_solver() {
        // \a b -> add(a,b) and \x y -> add(x,y) are the same term renamed — caught structurally, no solver.
        let f = binop("a", "b", "add", "a", "b");
        let g = binop("x", "y", "add", "x", "y");
        assert_eq!(prove_equivalent(&f, &g, "z3"), EquivVerdict::EquivalentByRenaming);
    }

    #[test]
    fn commutation_is_not_alpha_equivalent() {
        // Renaming is structural: `add(a,b)` and `add(b,a)` use their variables in different positions, so
        // they are NOT α-equivalent (they are still *semantically* equal — left to the solver path).
        let f = binop("a", "b", "add", "a", "b");
        let g = binop("a", "b", "add", "b", "a");
        assert!(!alpha_equivalent(&f, &g));
    }

    #[test]
    fn alpha_canonical_handles_let_shadowing() {
        // \a -> let a = a in a  vs  \z -> let z = z in z — alpha-equivalent despite shadowing.
        let shadow = |p: &str| json!({ "kind": "lambda", "params": [{ "name": p }], "body": {
            "kind": "let", "name": p, "value": { "kind": "var", "name": p }, "body": { "kind": "var", "name": p } } });
        assert!(alpha_equivalent(&shadow("a"), &shadow("z")));
    }
}
