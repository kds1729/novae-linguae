//! Structural-termination analysis — verify a record's declared `signature.terminates: always`. Like
//! `nat` (see [`crate::refine`]) the field is *declared* and, until now, unverified: a record could claim
//! `always` for a body that loops forever. This is the conservative, **sound** structural check.
//!
//! Over the **first-order** fragment (the arithmetic/boolean/comparison builtins plus the first-order list
//! ops `head`/`tail`/`cons`/`null`/`length`/`append`/`reverse`), a body provably terminates when either:
//!   - it is **non-recursive** (no `self`-call) — every builtin halts on finite input; or
//!   - it is **structurally recursive**: every `self`-call's recursion argument is `tail^k(p)` (`k ≥ 1`)
//!     of one fixed parameter `p`. A list is a finite inductive structure and `tail` strictly shrinks it,
//!     so the recursion is well-founded and halts (normally, or with an error at `nil` — either way it
//!     terminates).
//!
//! Anything else is reported `Unknown`, never a false `Always`: a recursion whose argument is *not* a
//! strict structural descent (it might not be well-founded), `self`-calls descending on different
//! parameters, or — crucially — any **higher-order / opaque** application (`map`/`filter`/`fold`, applying
//! a parameter or an `fn_ref`), whose termination depends on a callee this local analysis cannot see (the
//! same honesty stance `check-effects` takes for opaque callees). So `Always` here is sound but
//! incomplete; the unverifiable cases are flagged, not waved through.

use serde_json::Value as J;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminationOutcome {
    /// The body provably halts on every input (non-recursive, or structurally recursive).
    Always,
    /// Could not prove termination — carries the reason. Never a false `Always`.
    Unknown(String),
}

/// First-order builtin operators whose application terminates on finite arguments. Deliberately EXCLUDES
/// the higher-order combinators (`map`/`filter`/`foldl`/`foldr`/`compose`) and `apply` — those take a
/// function whose termination this analysis can't see, so a body using them is `Unknown`.
const FIRST_ORDER_OPS: &[&str] = &[
    "add", "sub", "mul", "neg", "abs", "min", "max", "mod", "div", "eq", "neq", "lt", "le", "gt", "ge",
    "and", "or", "xor", "not", "id", "head", "tail", "cons", "null", "length", "append", "reverse",
];

/// The parameter names of a `lambda` body, or `None` if it isn't one.
fn lambda_params(body: &J) -> Option<Vec<String>> {
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

/// If `node` is the application-spine head `self` (in any of its spellings — `{op:"self"}`,
/// `{fn:{var:"self"}}`, or a curried `apply(apply(self, …), …)` spine), return its argument list in order.
fn self_call_args(node: &J) -> Option<Vec<J>> {
    let args = node.get("args").and_then(|a| a.as_array());
    if node.get("op").and_then(|o| o.as_str()) == Some("self") {
        return Some(args.cloned().unwrap_or_default());
    }
    if node.pointer("/fn/name").and_then(|n| n.as_str()) == Some("self") {
        return Some(args.cloned().unwrap_or_default());
    }
    // Curried apply spine: apply(apply(self, a0), a1) → [a0, a1].
    if node.get("op").and_then(|o| o.as_str()) == Some("apply") {
        let args = args?;
        let head = args.first()?;
        if head.get("kind").and_then(|k| k.as_str()) == Some("var")
            && head.get("name").and_then(|n| n.as_str()) == Some("self")
        {
            return Some(args[1..].to_vec());
        }
        if let Some(mut inner) = self_call_args(head) {
            inner.extend(args[1..].iter().cloned());
            return Some(inner);
        }
    }
    None
}

/// If `arg` is `tail^k(var p)` (`k ≥ 1`) — a strict structural descent of a variable — return `(p, k)`.
fn tail_descent(arg: &J) -> Option<(String, usize)> {
    let mut node = arg;
    let mut k = 0;
    while node.get("op").and_then(|o| o.as_str()) == Some("tail")
        || node.pointer("/fn/name").and_then(|n| n.as_str()) == Some("tail")
    {
        node = node.get("args").and_then(|a| a.as_array()).and_then(|a| a.first())?;
        k += 1;
    }
    if k >= 1 && node.get("kind").and_then(|k| k.as_str()) == Some("var") {
        return node.get("name").and_then(|n| n.as_str()).map(|n| (n.to_string(), k));
    }
    None
}

/// The recognized first-order op at this application's head, or `None` if the node isn't a builtin
/// first-order application (it may be a `self`-call, a higher-order/opaque call, or a non-application).
fn first_order_head(node: &J) -> Option<String> {
    let op = node
        .get("op")
        .and_then(|o| o.as_str())
        .or_else(|| node.pointer("/fn/name").and_then(|n| n.as_str()))?;
    FIRST_ORDER_OPS.contains(&op).then(|| op.to_string())
}

/// Walk the body collecting each `self`-call's recursion descent (`tail_descent` of its first argument)
/// and detecting any opaque/higher-order application. Returns the first opaque reason via `opaque`.
fn walk(node: &J, descents: &mut Vec<Option<(String, usize)>>, opaque: &mut Option<String>) {
    if opaque.is_some() {
        return;
    }
    if let Some(sargs) = self_call_args(node) {
        // A self-call: its first argument is the recursion position.
        descents.push(sargs.first().and_then(tail_descent));
        for a in &sargs {
            walk(a, descents, opaque);
        }
        return;
    }
    if node.get("kind").and_then(|k| k.as_str()) == Some("app") {
        if first_order_head(node).is_none() {
            // An application that is neither a self-call nor a recognized first-order builtin: a
            // higher-order combinator (map/filter/fold), or applying a parameter / fn_ref. Opaque.
            let head = node
                .get("op")
                .and_then(|o| o.as_str())
                .or_else(|| node.pointer("/fn/name").and_then(|n| n.as_str()))
                .unwrap_or("<unknown>");
            *opaque = Some(format!("applies a higher-order/opaque callee `{head}` (its termination is unchecked)"));
            return;
        }
    }
    // Descend into every child (args, body, value, scrutinee, case arms).
    match node {
        J::Object(m) => {
            for (k, v) in m {
                if k == "pattern" {
                    continue; // patterns bind, they don't compute
                }
                walk(v, descents, opaque);
            }
        }
        J::Array(items) => items.iter().for_each(|v| walk(v, descents, opaque)),
        _ => {}
    }
}

/// Decide whether `body` provably terminates. Sound and conservative — see the module docs.
pub fn analyze_termination(body: &J) -> TerminationOutcome {
    let Some(params) = lambda_params(body) else {
        return TerminationOutcome::Unknown("body is not a `lambda`".into());
    };
    let Some(inner) = body.get("body") else {
        return TerminationOutcome::Unknown("lambda has no body".into());
    };

    let mut descents: Vec<Option<(String, usize)>> = Vec::new();
    let mut opaque: Option<String> = None;
    walk(inner, &mut descents, &mut opaque);

    if let Some(why) = opaque {
        return TerminationOutcome::Unknown(why);
    }
    if descents.is_empty() {
        // Non-recursive over the first-order fragment: every builtin halts on finite input.
        return TerminationOutcome::Always;
    }
    // Structurally recursive: every self-call must descend `tail^k` (k ≥ 1) on ONE fixed parameter.
    let mut on: Option<String> = None;
    for d in descents {
        match d {
            Some((p, _)) if params.contains(&p) => match &on {
                None => on = Some(p),
                Some(prev) if *prev == p => {}
                Some(_) => {
                    return TerminationOutcome::Unknown(
                        "self-calls descend on different parameters (no single well-founded measure)".into(),
                    )
                }
            },
            _ => {
                return TerminationOutcome::Unknown(
                    "a self-call's recursion argument is not a structural `tail` descent of a parameter".into(),
                )
            }
        }
    }
    TerminationOutcome::Always
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn v(n: &str) -> J {
        json!({ "kind": "var", "name": n })
    }
    fn app(op: &str, args: Vec<J>) -> J {
        json!({ "kind": "app", "op": op, "args": args })
    }
    fn lambda(ps: &[&str], inner: J) -> J {
        let params: Vec<J> = ps.iter().map(|p| json!({ "name": p })).collect();
        json!({ "kind": "lambda", "params": params, "body": inner })
    }
    fn lit_b(b: bool) -> J {
        json!({ "kind": "lit", "value": { "kind": "bool", "value": b } })
    }
    fn lit_i(n: i64) -> J {
        json!({ "kind": "lit", "value": { "kind": "int", "value": n } })
    }
    /// `case null(p) of true -> base | false -> step`.
    fn rec_case(p: &str, base: J, step: J) -> J {
        json!({
            "kind": "case",
            "scrutinee": app("null", vec![v(p)]),
            "arms": [
                { "pattern": lit_b(true), "body": base },
                { "pattern": lit_b(false), "body": step }
            ]
        })
    }

    #[test]
    fn non_recursive_first_order_terminates() {
        let body = lambda(&["a", "b"], app("add", vec![v("a"), v("b")]));
        assert_eq!(analyze_termination(&body), TerminationOutcome::Always);
    }

    #[test]
    fn structural_recursion_terminates() {
        // length: \xs -> case null xs of true -> 0 | false -> add(1, self(tail xs)).
        let step = app("add", vec![lit_i(1), app("apply", vec![v("self"), app("tail", vec![v("xs")])])]);
        let body = lambda(&["xs"], rec_case("xs", lit_i(0), step));
        assert_eq!(analyze_termination(&body), TerminationOutcome::Always);
    }

    #[test]
    fn two_step_descent_terminates() {
        // self(tail(tail xs)) — a stride-2 structural descent is still well-founded.
        let step = app("add", vec![lit_i(2), app("apply", vec![v("self"), app("tail", vec![app("tail", vec![v("xs")])])])]);
        let body = lambda(&["xs"], rec_case("xs", lit_i(0), step));
        assert_eq!(analyze_termination(&body), TerminationOutcome::Always);
    }

    #[test]
    fn two_param_spectator_recursion_terminates() {
        // append: \xs ys -> case null xs of true -> ys | false -> cons(head xs, self(tail xs, ys)).
        let rec = json!({ "kind": "app", "op": "self", "args": [app("tail", vec![v("xs")]), v("ys")] });
        let step = app("cons", vec![app("head", vec![v("xs")]), rec]);
        let body = lambda(&["xs", "ys"], rec_case("xs", v("ys"), step));
        assert_eq!(analyze_termination(&body), TerminationOutcome::Always);
    }

    #[test]
    fn non_structural_recursion_is_unknown() {
        // self(xs) — recurses on the SAME argument, not a descent: would loop. Must be Unknown.
        let step = app("add", vec![lit_i(1), app("apply", vec![v("self"), v("xs")])]);
        let body = lambda(&["xs"], rec_case("xs", lit_i(0), step));
        match analyze_termination(&body) {
            TerminationOutcome::Unknown(_) => {}
            other => panic!("expected Unknown, got {other:?}"),
        }
    }

    #[test]
    fn higher_order_body_is_unknown() {
        // \xs -> map(f, xs): map's termination depends on the opaque `f`, so we can't prove it here.
        let body = lambda(&["f", "xs"], app("map", vec![v("f"), v("xs")]));
        match analyze_termination(&body) {
            TerminationOutcome::Unknown(_) => {}
            other => panic!("expected Unknown, got {other:?}"),
        }
    }

    #[test]
    fn applying_a_parameter_is_unknown() {
        // \g x -> apply(g, x): applying an opaque parameter — termination unknown.
        let body = lambda(&["g", "x"], app("apply", vec![v("g"), v("x")]));
        match analyze_termination(&body) {
            TerminationOutcome::Unknown(_) => {}
            other => panic!("expected Unknown, got {other:?}"),
        }
    }
}
