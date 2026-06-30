//! Canonical normalization — rewrite a body-expression AST to a canonical normal form via a set of
//! **meaning-preserving** rewrites, so that functions that are reconcilable by those rewrites share one
//! normal form. This is the operable core of the README's "canonical normal form per equivalence class"
//! item: rather than only *picking* a representative (the smallest content-address), we *compute* a
//! canonical artifact, and two bodies with equal normal form are equivalent — decided structurally,
//! without the solver.
//!
//! The rewrites (each sound — it preserves the function's meaning on every input, so a false equivalence
//! is impossible):
//!   - **α-canonicalization** — bound variables (lambda params, `let`, `case` binds, `forall` vars) are
//!     renamed to positional names; free variables (builtins, the function head, `self`) are untouched.
//!   - **AC ordering** — associative+commutative operators (`add`, `mul`, `and`, `or`, `xor`, `min`,
//!     `max`) are flattened across nesting and their operands sorted by a naming-invariant key, so
//!     `add(a, b)` and `add(b, a)` — and `add(add(a, b), c)` and `add(c, add(b, a))` — coincide.
//!   - **subtraction as addition** — `sub(a, b) → add(a, neg(b))`, so subtraction folds into the additive
//!     AC group (`a - b`, `a + (-b)`, and `(-b) + a` all coincide). Sound: both subterms are retained.
//!   - **commutative ordering** — `eq`/`neq` operands are sorted (they commute but don't associate).
//!   - **constant folding** — applications all of whose operands are literals are evaluated (the total
//!     `Int`/`Bool` operators; `div`/`mod` are left alone to avoid a divide-by-zero rewrite).
//!   - **identity elimination** — the identity element of an AC operator is dropped (`add(x, 0) → x`,
//!     `mul(x, 1) → x`, `and(x, true) → x`, `or(x, false) → x`, `xor(x, false) → x`). *Absorbing*
//!     elements (`mul(x, 0)`, `and(x, false)`, …) are deliberately NOT applied: under a possibly
//!     non-terminating `x` they would not be meaning-preserving.
//!   - **idempotence** — for the idempotent AC operators (`and`, `or`, `min`, `max`) a repeated operand
//!     collapses (`and(x, x) → x`, `max(a, max(a, b)) → max(a, b)`). Sound: *one* copy is kept, so the
//!     operand is still evaluated — safe even if it could diverge (unlike `xor(x, x) → false`, which
//!     drops both copies and is therefore NOT applied).
//!   - **involution** — `neg(neg(x)) → x`, `not(not(x)) → x`; `id(x) → x`; literal `nat` → `int`.
//!
//! For the recognized arithmetic/boolean builtins the rebuilt node uses the compact `op` form, so a body
//! that wrote `{fn: {var: add}}` and one that wrote `{op: add}` normalize alike. Operators outside that
//! set (list ops, `fn_ref` application) are left structurally as-is (only their subterms and bound names
//! are normalized), so the normal form stays a valid body AST.
//!
//! It is a *normalizer*, not a decision procedure: equal normal forms imply equivalence, but unequal
//! ones say nothing (those fall through to the prover). Determinism (principle 5): every rewrite and the
//! ordering key are fixed, so a body has exactly one normal form.

use serde_json::{json, Value as J};

use crate::equiv::alpha_canonical;

/// Associative + commutative operators: flattened across nesting, operands sorted, identity dropped.
/// `min`/`max` are AC too (and idempotent, below) but have no integer identity element.
const AC_OPS: &[&str] = &["add", "mul", "and", "or", "xor", "min", "max"];
/// AC operators that are also idempotent: a repeated operand collapses to one copy. Keeping one copy is
/// sound even under a possibly-diverging operand (the operand is still evaluated). `add`/`mul`/`xor` are
/// NOT idempotent (`add(x,x)=2x`, `xor(x,x)=false` — the latter would drop both copies).
const IDEMPOTENT_OPS: &[&str] = &["and", "or", "min", "max"];
/// Commutative (but not associative) binary operators: operands sorted, not flattened.
const COMM_OPS: &[&str] = &["eq", "neq"];
/// The arithmetic/boolean builtins we recognize — rebuilt in `op` form (and constant-folded). Anything
/// else (list ops, general application) is left in its original form.
const ARITH_BOOL_OPS: &[&str] = &[
    "add", "sub", "mul", "neg", "abs", "min", "max", "mod", "div", "eq", "neq", "lt", "le", "gt", "ge",
    "and", "or", "xor", "not", "id",
];

/// The canonical normal form of a body: rewrite to a fixpoint, then assign canonical bound-variable
/// names over the final structure. Sound — equal normal forms imply the functions are equivalent.
pub fn normalize(body: &J) -> J {
    let mut cur = body.clone();
    // Rewrite to a fixpoint (the rewrites are reducing/idempotent, so this terminates quickly).
    for _ in 0..64 {
        let next = rewrite(&cur);
        if next == cur {
            break;
        }
        cur = next;
    }
    // Final pass: positional bound-variable names over the settled structure, so AC reordering that moved
    // a binder doesn't leave names depending on the pre-rewrite position.
    alpha_canonical(&cur)
}

/// Whether two bodies share a normal form (hence are equivalent). Sound; incomplete.
pub fn normal_equivalent(a: &J, b: &J) -> bool {
    normalize(a) == normalize(b)
}

// --- helpers --------------------------------------------------------------------------------------

fn head_op(node: &J) -> Option<String> {
    if let Some(op) = node.get("op").and_then(|o| o.as_str()) {
        return Some(op.to_string());
    }
    if node.pointer("/fn/kind").and_then(|k| k.as_str()) == Some("var") {
        return node.pointer("/fn/name").and_then(|n| n.as_str()).map(String::from);
    }
    None
}

fn as_int(node: &J) -> Option<i128> {
    if node.get("kind").and_then(|k| k.as_str()) != Some("lit") {
        return None;
    }
    let v = node.get("value")?;
    match v.get("kind").and_then(|k| k.as_str()) {
        Some("int") | Some("nat") => {
            let raw = v.get("value")?;
            raw.as_i64().map(|i| i as i128).or_else(|| raw.as_str().and_then(|s| s.parse().ok()))
        }
        _ => None,
    }
}

fn as_bool(node: &J) -> Option<bool> {
    if node.get("kind").and_then(|k| k.as_str()) != Some("lit") {
        return None;
    }
    let v = node.get("value")?;
    (v.get("kind").and_then(|k| k.as_str()) == Some("bool")).then(|| v.get("value").and_then(|b| b.as_bool()).unwrap_or(false))
}

fn int_lit(n: i128) -> J {
    json!({ "kind": "lit", "value": { "kind": "int", "value": n as i64 } })
}
fn bool_lit(b: bool) -> J {
    json!({ "kind": "lit", "value": { "kind": "bool", "value": b } })
}

fn app(op: &str, args: Vec<J>) -> J {
    json!({ "kind": "app", "op": op, "args": args })
}

/// A naming-invariant sort key: the operand's own α-normal form, canonically serialized. Two operands
/// that are α-equivalent get the same key regardless of the bound names they happen to use.
fn sort_key(node: &J) -> Vec<u8> {
    let canon = alpha_canonical(node);
    crate::canonicalize(&canon).unwrap_or_else(|_| canon.to_string().into_bytes())
}

/// The identity element of an AC operator, or `None`.
fn ac_identity(op: &str) -> Option<J> {
    match op {
        "add" | "xor" => Some(int_or_bool_identity(op)),
        "mul" => Some(int_lit(1)),
        "and" => Some(bool_lit(true)),
        "or" => Some(bool_lit(false)),
        _ => None,
    }
}
fn int_or_bool_identity(op: &str) -> J {
    match op {
        "add" => int_lit(0),
        "xor" => bool_lit(false),
        _ => unreachable!(),
    }
}

/// Flatten an AC operator's spine into a flat operand list (`add(add(a,b),c)` → `[a,b,c]`).
fn flatten_ac(op: &str, node: &J) -> Vec<J> {
    if head_op(node).as_deref() == Some(op) {
        if let Some(args) = node.get("args").and_then(|a| a.as_array()) {
            return args.iter().flat_map(|a| flatten_ac(op, a)).collect();
        }
    }
    vec![node.clone()]
}

/// Fold a list of literal operands of an AC op into a single literal (the rest are non-literal).
fn fold_ac_literals(op: &str, lits: &[J]) -> Option<J> {
    match op {
        "add" => Some(int_lit(lits.iter().filter_map(as_int).sum())),
        "mul" => Some(int_lit(lits.iter().filter_map(as_int).product())),
        "and" => Some(bool_lit(lits.iter().filter_map(as_bool).all(|b| b))),
        "or" => Some(bool_lit(lits.iter().filter_map(as_bool).any(|b| b))),
        "xor" => Some(bool_lit(lits.iter().filter_map(as_bool).fold(false, |a, b| a ^ b))),
        // min/max have no identity element, so `lits` may yield nothing — then there is no folded literal.
        "min" => lits.iter().filter_map(as_int).min().map(int_lit),
        "max" => lits.iter().filter_map(as_int).max().map(int_lit),
        _ => None,
    }
}

// --- the rewrite ----------------------------------------------------------------------------------

/// One bottom-up rewrite pass: rewrite every child, then simplify this node if it is a recognized
/// application. (Called to a fixpoint by [`normalize`].)
fn rewrite(node: &J) -> J {
    match node {
        J::Object(m) => {
            let rebuilt: serde_json::Map<String, J> =
                m.iter().map(|(k, v)| (k.clone(), rewrite(v))).collect();
            let obj = J::Object(rebuilt);
            if obj.get("kind").and_then(|k| k.as_str()) == Some("app") {
                simplify_app(&obj)
            } else if obj.get("kind").and_then(|k| k.as_str()) == Some("lit") {
                // Canonical literal kind: `nat` literals become `int` (same value), so a `nat` and an
                // `int` literal of the same value normalize alike.
                if let Some(n) = as_int(&obj) {
                    if obj.pointer("/value/kind").and_then(|k| k.as_str()) == Some("nat") {
                        return int_lit(n);
                    }
                }
                obj
            } else {
                obj
            }
        }
        J::Array(items) => J::Array(items.iter().map(rewrite).collect()),
        other => other.clone(),
    }
}

fn simplify_app(node: &J) -> J {
    let Some(op) = head_op(node) else { return node.clone() };
    let args: Vec<J> = node.get("args").and_then(|a| a.as_array()).cloned().unwrap_or_default();

    // Involutions and id (checked before the generic builtin rebuild).
    match op.as_str() {
        "id" if args.len() == 1 => return args[0].clone(),
        // Subtraction as addition: a - b ≡ a + (-b). Folds subtraction into the additive AC group so
        // `a - b`, `a + (-b)`, `(-b) + a` coincide. Sound — both subterms are retained (negation never
        // drops `b`), and over ℤ it is an identity. `neg` is simplified first so literals fold (3 - 1 → 2).
        "sub" if args.len() == 2 => {
            let neg_b = simplify_app(&app("neg", vec![args[1].clone()]));
            return simplify_app(&app("add", vec![args[0].clone(), neg_b]));
        }
        "neg" if args.len() == 1 => {
            if let Some(n) = as_int(&args[0]) {
                return int_lit(-n);
            }
            if head_op(&args[0]).as_deref() == Some("neg") {
                if let Some(inner) = args[0].get("args").and_then(|a| a.as_array()).and_then(|a| a.first()) {
                    return inner.clone();
                }
            }
            // Distribute negation over addition: -(x + y) ≡ (-x) + (-y). Sound (every subterm retained),
            // and it completes the subtraction-as-addition canonicalization for nested `a - (b - c)`.
            if head_op(&args[0]).as_deref() == Some("add") {
                if let Some(inner) = args[0].get("args").and_then(|a| a.as_array()) {
                    let negated: Vec<J> = inner.iter().map(|t| simplify_app(&app("neg", vec![t.clone()]))).collect();
                    return simplify_app(&app("add", negated));
                }
            }
        }
        "not" if args.len() == 1 => {
            if let Some(b) = as_bool(&args[0]) {
                return bool_lit(!b);
            }
            if head_op(&args[0]).as_deref() == Some("not") {
                if let Some(inner) = args[0].get("args").and_then(|a| a.as_array()).and_then(|a| a.first()) {
                    return inner.clone();
                }
            }
        }
        _ => {}
    }

    // Associative + commutative operators: flatten, fold literals, drop identity, sort, rebuild.
    if AC_OPS.contains(&op.as_str()) {
        let flat: Vec<J> = args.iter().flat_map(|a| flatten_ac(&op, a)).collect();
        let (lits, mut rest): (Vec<J>, Vec<J>) =
            flat.into_iter().partition(|x| as_int(x).is_some() || as_bool(x).is_some());
        let folded = fold_ac_literals(&op, &lits);
        // Keep the folded literal unless it is the identity element (then it's redundant beside `rest`).
        if let Some(f) = folded {
            let is_identity = ac_identity(&op).map(|id| id == f).unwrap_or(false);
            if !is_identity || rest.is_empty() {
                rest.push(f);
            }
        }
        rest.sort_by(|a, b| sort_key(a).cmp(&sort_key(b)));
        // Idempotence: for and/or/min/max, collapse repeated operands (now adjacent after the sort). One
        // copy is kept, so the operand is still evaluated — sound even if it could diverge.
        if IDEMPOTENT_OPS.contains(&op.as_str()) {
            rest.dedup_by(|a, b| sort_key(a) == sort_key(b));
        }
        return match rest.len() {
            0 => ac_identity(&op).unwrap_or_else(|| int_lit(0)),
            1 => rest.into_iter().next().unwrap(),
            // Rebuild as a right-nested binary spine over the sorted operands (canonical association).
            _ => rest.into_iter().rev().reduce(|acc, x| app(&op, vec![x, acc])).unwrap(),
        };
    }

    // Commutative binary (eq/neq): fold if both literal, else sort the two operands.
    if COMM_OPS.contains(&op.as_str()) && args.len() == 2 {
        if let (Some(x), Some(y)) = (as_int(&args[0]), as_int(&args[1])) {
            return bool_lit(if op == "eq" { x == y } else { x != y });
        }
        if let (Some(x), Some(y)) = (as_bool(&args[0]), as_bool(&args[1])) {
            return bool_lit(if op == "eq" { x == y } else { x != y });
        }
        let mut ab = args.clone();
        ab.sort_by(|a, b| sort_key(a).cmp(&sort_key(b)));
        return app(&op, ab);
    }

    // Other recognized arithmetic/boolean builtins: constant-fold when all operands are literal, else
    // rebuild in `op` form (unifying the `fn`/`op` spellings). div/mod are never folded (divide-by-zero).
    if ARITH_BOOL_OPS.contains(&op.as_str()) {
        if let Some(folded) = fold_binary(&op, &args) {
            return folded;
        }
        return app(&op, args);
    }

    // Unrecognized head (list op, fn_ref application): leave the application form untouched.
    node.clone()
}

/// Constant-fold a non-AC arithmetic/boolean operator applied to all-literal operands. `None` if not
/// all-literal, the arity is wrong, or the operator is intentionally never folded (`div`/`mod`).
fn fold_binary(op: &str, args: &[J]) -> Option<J> {
    match op {
        "sub" if args.len() == 2 => Some(int_lit(as_int(&args[0])? - as_int(&args[1])?)),
        "abs" if args.len() == 1 => Some(int_lit(as_int(&args[0])?.abs())),
        "min" if args.len() == 2 => Some(int_lit(as_int(&args[0])?.min(as_int(&args[1])?))),
        "max" if args.len() == 2 => Some(int_lit(as_int(&args[0])?.max(as_int(&args[1])?))),
        "lt" if args.len() == 2 => Some(bool_lit(as_int(&args[0])? < as_int(&args[1])?)),
        "le" if args.len() == 2 => Some(bool_lit(as_int(&args[0])? <= as_int(&args[1])?)),
        "gt" if args.len() == 2 => Some(bool_lit(as_int(&args[0])? > as_int(&args[1])?)),
        "ge" if args.len() == 2 => Some(bool_lit(as_int(&args[0])? >= as_int(&args[1])?)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(n: &str) -> J {
        json!({ "kind": "var", "name": n })
    }

    #[test]
    fn commutative_add_normalizes_equal() {
        // add(a, b) and add(b, a) share a normal form (no solver).
        assert!(normal_equivalent(&app("add", vec![v("a"), v("b")]), &app("add", vec![v("b"), v("a")])));
    }

    #[test]
    fn associative_commutative_add_normalizes_equal() {
        // add(add(a, b), c) ≡ add(c, add(b, a)) under AC.
        let l = app("add", vec![app("add", vec![v("a"), v("b")]), v("c")]);
        let r = app("add", vec![v("c"), app("add", vec![v("b"), v("a")])]);
        assert!(normal_equivalent(&l, &r));
    }

    #[test]
    fn constant_folding_and_identity() {
        // add(x, 0) → x ; add(2, 3) → 5 ; mul(x, 1) → x.
        assert_eq!(normalize(&app("add", vec![v("x"), int_lit(0)])), normalize(&v("x")));
        assert_eq!(normalize(&app("add", vec![int_lit(2), int_lit(3)])), int_lit(5));
        assert_eq!(normalize(&app("mul", vec![v("x"), int_lit(1)])), normalize(&v("x")));
    }

    #[test]
    fn absorbing_element_is_not_applied() {
        // mul(x, 0) must NOT fold to 0 — unsound if x can diverge. It normalizes to mul(0, x) (sorted),
        // distinct from the literal 0.
        let n = normalize(&app("mul", vec![v("x"), int_lit(0)]));
        assert_ne!(n, int_lit(0), "mul-by-zero must not collapse to 0");
    }

    #[test]
    fn fn_form_and_op_form_unify() {
        // {fn: {var: add}} and {op: add} normalize alike for a recognized builtin.
        let fn_form = json!({ "kind": "app", "fn": { "kind": "var", "name": "add" }, "args": [v("a"), v("b")] });
        let op_form = app("add", vec![v("a"), v("b")]);
        assert!(normal_equivalent(&fn_form, &op_form));
    }

    #[test]
    fn involution_neg_and_not() {
        assert_eq!(normalize(&app("neg", vec![app("neg", vec![v("x")])])), normalize(&v("x")));
        assert_eq!(normalize(&app("not", vec![app("not", vec![v("b")])])), normalize(&v("b")));
    }

    #[test]
    fn nat_and_int_literals_unify() {
        let nat5 = json!({ "kind": "lit", "value": { "kind": "nat", "value": 5 } });
        assert_eq!(normalize(&nat5), int_lit(5));
    }

    #[test]
    fn distinct_functions_do_not_collapse() {
        // add(a, b) and sub(a, b) are genuinely different — different normal forms.
        assert!(!normal_equivalent(&app("add", vec![v("a"), v("b")]), &app("sub", vec![v("a"), v("b")])));
        // Non-commutative sub is not reordered: sub(a,b) ≠ sub(b,a).
        assert!(!normal_equivalent(&app("sub", vec![v("a"), v("b")]), &app("sub", vec![v("b"), v("a")])));
    }

    #[test]
    fn lambda_bodies_normalize_under_binders() {
        // \a b -> add(a, b)  vs  \x y -> add(y, x): same normal form (α + AC).
        let f = json!({ "kind": "lambda", "params": [{ "name": "a" }, { "name": "b" }], "body": app("add", vec![v("a"), v("b")]) });
        let g = json!({ "kind": "lambda", "params": [{ "name": "x" }, { "name": "y" }], "body": app("add", vec![v("y"), v("x")]) });
        assert!(normal_equivalent(&f, &g));
    }

    // --- new rewrites (subtraction-as-addition, neg distribution, min/max AC, idempotence) -----------

    #[test]
    fn subtraction_folds_into_addition() {
        // a - b ≡ a + (-b) ≡ (-b) + a.
        assert!(normal_equivalent(&app("sub", vec![v("a"), v("b")]),
                                  &app("add", vec![v("a"), app("neg", vec![v("b")])])));
        assert!(normal_equivalent(&app("sub", vec![v("a"), v("b")]),
                                  &app("add", vec![app("neg", vec![v("b")]), v("a")])));
        // a - (b - c) ≡ a - b + c  (needs neg distribution over add).
        let lhs = app("sub", vec![v("a"), app("sub", vec![v("b"), v("c")])]);
        let rhs = app("add", vec![app("sub", vec![v("a"), v("b")]), v("c")]);
        assert!(normal_equivalent(&lhs, &rhs));
        // a - a is NOT collapsed to 0 (would be unsound under a diverging a — same stance as mul-by-0).
        assert_ne!(normalize(&app("sub", vec![v("a"), v("a")])), int_lit(0));
    }

    #[test]
    fn min_max_are_ac_and_idempotent() {
        // commutative + associative
        assert!(normal_equivalent(&app("max", vec![v("a"), v("b")]), &app("max", vec![v("b"), v("a")])));
        assert!(normal_equivalent(&app("min", vec![app("min", vec![v("a"), v("b")]), v("c")]),
                                  &app("min", vec![v("c"), app("min", vec![v("b"), v("a")])])));
        // idempotent: max(a, a) → a, min(a, min(a, b)) → min(a, b)
        assert_eq!(normalize(&app("max", vec![v("a"), v("a")])), normalize(&v("a")));
        assert!(normal_equivalent(&app("min", vec![v("a"), app("min", vec![v("a"), v("b")])]),
                                  &app("min", vec![v("a"), v("b")])));
        // literal folding: max(5, a, 3) ≡ max(a, 5)
        assert!(normal_equivalent(&app("max", vec![int_lit(5), v("a"), int_lit(3)]),
                                  &app("max", vec![v("a"), int_lit(5)])));
        // min and max stay distinct
        assert!(!normal_equivalent(&app("min", vec![v("a"), v("b")]), &app("max", vec![v("a"), v("b")])));
    }

    #[test]
    fn and_or_idempotence() {
        assert_eq!(normalize(&app("and", vec![v("p"), v("p")])), normalize(&v("p")));
        assert_eq!(normalize(&app("or", vec![v("p"), v("p")])), normalize(&v("p")));
        // or(p, or(p, q)) → or(p, q)
        assert!(normal_equivalent(&app("or", vec![v("p"), app("or", vec![v("p"), v("q")])]),
                                  &app("or", vec![v("p"), v("q")])));
        // xor is NOT idempotent: xor(p, p) must NOT collapse to p (it is false, but dropping both copies
        // is unsound under divergence, so it stays as the un-collapsed form).
        assert_ne!(normalize(&app("xor", vec![v("p"), v("p")])), normalize(&v("p")));
    }

    // --- soundness property test: a reference evaluator over the fragment must agree on a body and its
    // normal form for every input. A single unsound rewrite (a false equivalence) would be caught here.

    #[derive(Clone, Copy, PartialEq, Debug)]
    enum Val {
        I(i128),
        B(bool),
    }

    fn ei(v: &Val) -> Option<i128> {
        if let Val::I(x) = v { Some(*x) } else { None }
    }
    fn eb(v: &Val) -> Option<bool> {
        if let Val::B(x) = v { Some(*x) } else { None }
    }

    /// Reference evaluator over the normal-form fragment. `None` on a partial op (div/mod by zero) — those
    /// envs are skipped. div/mod use any fixed semantics: they are never rewritten, so the choice only has
    /// to be *consistent* across a body and its normal form.
    fn eval_ref(node: &J, env: &std::collections::HashMap<String, Val>) -> Option<Val> {
        if let Some(i) = as_int(node) {
            return Some(Val::I(i));
        }
        if let Some(b) = as_bool(node) {
            return Some(Val::B(b));
        }
        if node.get("kind").and_then(|k| k.as_str()) == Some("var") {
            return env.get(node.get("name")?.as_str()?).copied();
        }
        let op = head_op(node)?;
        let args = node.get("args")?.as_array()?;
        let ev: Vec<Val> = args.iter().map(|a| eval_ref(a, env)).collect::<Option<_>>()?;
        // n-ary fold for the AC ops (the normal form rebuilds them as nested binaries; inputs may be n-ary).
        let int_fold = |f: fn(i128, i128) -> i128| -> Option<Val> {
            let mut it = ev.iter().map(ei);
            let first = it.next()??;
            it.try_fold(first, |a, b| Some(f(a, b?))).map(Val::I)
        };
        let bool_fold = |f: fn(bool, bool) -> bool| -> Option<Val> {
            let mut it = ev.iter().map(eb);
            let first = it.next()??;
            it.try_fold(first, |a, b| Some(f(a, b?))).map(Val::B)
        };
        Some(match op.as_str() {
            "add" => int_fold(|a, b| a + b)?,
            "mul" => int_fold(|a, b| a * b)?,
            "min" => int_fold(|a, b| a.min(b))?,
            "max" => int_fold(|a, b| a.max(b))?,
            "and" => bool_fold(|a, b| a && b)?,
            "or" => bool_fold(|a, b| a || b)?,
            "xor" => bool_fold(|a, b| a ^ b)?,
            "sub" => Val::I(ei(&ev[0])? - ei(&ev[1])?),
            "neg" => Val::I(-ei(&ev[0])?),
            "abs" => Val::I(ei(&ev[0])?.abs()),
            "id" => ev[0],
            "not" => Val::B(!eb(&ev[0])?),
            "div" => {
                let d = ei(&ev[1])?;
                if d == 0 { return None; }
                Val::I(ei(&ev[0])? / d)
            }
            "mod" => {
                let d = ei(&ev[1])?;
                if d == 0 { return None; }
                Val::I(ei(&ev[0])? % d)
            }
            "eq" => Val::B(ev[0] == ev[1]),
            "neq" => Val::B(ev[0] != ev[1]),
            "lt" => Val::B(ei(&ev[0])? < ei(&ev[1])?),
            "le" => Val::B(ei(&ev[0])? <= ei(&ev[1])?),
            "gt" => Val::B(ei(&ev[0])? > ei(&ev[1])?),
            "ge" => Val::B(ei(&ev[0])? >= ei(&ev[1])?),
            _ => return None,
        })
    }

    #[test]
    fn normalization_preserves_meaning() {
        use std::collections::HashMap;
        let a = || v("a");
        let b = || v("b");
        let c = || v("c");
        let p = || v("p");
        let q = || v("q");
        // A battery exercising every new rewrite plus the existing ones, with free vars.
        let int_exprs: Vec<J> = vec![
            app("sub", vec![a(), b()]),
            app("sub", vec![a(), app("sub", vec![b(), c()])]),
            app("sub", vec![app("sub", vec![a(), b()]), c()]),
            app("add", vec![app("neg", vec![b()]), a()]),
            app("neg", vec![app("add", vec![a(), app("neg", vec![b()])])]),
            app("max", vec![int_lit(5), a(), int_lit(3)]),
            app("min", vec![a(), app("min", vec![a(), b()])]),
            app("max", vec![app("max", vec![a(), b()]), a()]),
            app("add", vec![app("mul", vec![a(), int_lit(2)]), app("sub", vec![b(), a()])]),
            app("min", vec![app("add", vec![a(), int_lit(1)]), app("sub", vec![a(), b()])]),
            app("abs", vec![app("sub", vec![a(), b()])]),
        ];
        let bool_exprs: Vec<J> = vec![
            app("and", vec![p(), p()]),
            app("or", vec![p(), app("or", vec![p(), q()])]),
            app("xor", vec![p(), p()]),
            app("eq", vec![app("sub", vec![a(), b()]), int_lit(0)]),
            app("lt", vec![app("min", vec![a(), b()]), app("max", vec![a(), b()])]),
            app("and", vec![app("lt", vec![a(), b()]), app("lt", vec![a(), b()])]),
        ];
        let ivals = [-3i128, -1, 0, 2, 5];
        let bvals = [false, true];
        let mut checked = 0u64;
        for expr in int_exprs.iter().chain(bool_exprs.iter()) {
            let norm = normalize(expr);
            for &av in &ivals {
                for &bv in &ivals {
                    for &cv in &ivals {
                        for &pv in &bvals {
                            for &qv in &bvals {
                                let env: HashMap<String, Val> = [
                                    ("a".into(), Val::I(av)), ("b".into(), Val::I(bv)),
                                    ("c".into(), Val::I(cv)), ("p".into(), Val::B(pv)),
                                    ("q".into(), Val::B(qv)),
                                ].into_iter().collect();
                                let before = eval_ref(expr, &env);
                                let after = eval_ref(&norm, &env);
                                // Skip envs that make a subexpression partial (div/mod by zero) on either side.
                                if before.is_none() || after.is_none() {
                                    continue;
                                }
                                assert_eq!(before, after,
                                    "normalize changed meaning of {expr} -> {norm} at env a={av} b={bv} c={cv} p={pv} q={qv}");
                                checked += 1;
                            }
                        }
                    }
                }
            }
        }
        assert!(checked > 1000, "soundness battery should exercise many envs (got {checked})");
    }
}
