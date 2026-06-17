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
//!   - **AC ordering** — associative+commutative operators (`add`, `mul`, `and`, `or`, `xor`) are
//!     flattened across nesting and their operands sorted by a naming-invariant key, so `add(a, b)` and
//!     `add(b, a)` — and `add(add(a, b), c)` and `add(c, add(b, a))` — coincide.
//!   - **commutative ordering** — `eq`/`neq` operands are sorted (they commute but don't associate).
//!   - **constant folding** — applications all of whose operands are literals are evaluated (the total
//!     `Int`/`Bool` operators; `div`/`mod` are left alone to avoid a divide-by-zero rewrite).
//!   - **identity elimination** — the identity element of an AC operator is dropped (`add(x, 0) → x`,
//!     `mul(x, 1) → x`, `and(x, true) → x`, `or(x, false) → x`, `xor(x, false) → x`). *Absorbing*
//!     elements (`mul(x, 0)`, `and(x, false)`, …) are deliberately NOT applied: under a possibly
//!     non-terminating `x` they would not be meaning-preserving.
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
const AC_OPS: &[&str] = &["add", "mul", "and", "or", "xor"];
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
        "neg" if args.len() == 1 => {
            if let Some(n) = as_int(&args[0]) {
                return int_lit(-n);
            }
            if head_op(&args[0]).as_deref() == Some("neg") {
                if let Some(inner) = args[0].get("args").and_then(|a| a.as_array()).and_then(|a| a.first()) {
                    return inner.clone();
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
}
