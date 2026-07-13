//! Structural-termination analysis — verify a record's declared `signature.terminates: always`. Like
//! `nat` (see [`crate::refine`]) the field is *declared* and, until now, unverified: a record could claim
//! `always` for a body that loops forever. This is the conservative, **sound** structural check.
//!
//! Over the **first-order** fragment (the arithmetic/boolean/comparison builtins plus the first-order list
//! ops `head`/`tail`/`cons`/`null`/`length`/`append`/`reverse`), a body provably terminates when either:
//!   - it is **non-recursive** (no `self`-call) — every builtin halts on finite input; or
//!   - some ONE argument position provably descends in EVERY `self`-call (a single well-founded
//!     measure; any fixed position, not just the first — `nth(i, xs)` descends position 2), where a
//!     position `j` descends when its argument is:
//!       * `tail^k(params[j])` (`k ≥ 1`) — **structural**: a list is a finite inductive structure and
//!         `tail` strictly shrinks it; the descent must feed its OWN position (`self(tail(xs), xs)`
//!         descends nothing — the shrinking value lands in a slot whose parameter the guard never
//!         watches), and a shadowing `let`/nested-lambda/pattern binding of the parameter's name
//!         disqualifies it (a same-named binding is a different value); or
//!       * `sub(params[j], c)` (`c ≥ 1`) under a dominating lower-bound guard `gt(p, lit)`/`ge(p, lit)`
//!         (or the mirrored `lt`/`le`) — **guarded numeric**: a strictly decreasing integer sequence
//!         over a constant floor is finite, sound for plain ints; or
//!       * `sub(params[j], 1)` under a dominating `p != 0` guard (`eq(p, 0)`'s false arm / `neq(p, 0)`'s
//!         true arm) when position `j` is `nat`-typed ([`analyze_termination_typed`]) — the type
//!         supplies the floor, and the unit step keeps the value a nat (a larger step could tunnel
//!         below zero and recurse forever, so it is refused).
//!
//! Anything else is reported `Unknown`, never a false `Always`: a recursion whose argument is *not* a
//! provable descent (it might not be well-founded), `self`-calls with no common descending position,
//! or — crucially — any **higher-order / opaque** application (`map`/`filter`/`fold`, applying
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
    "and", "or", "xor", "not", "id", "head", "tail", "last", "init", "cons", "null", "length",
    "append", "reverse",
    "str_concat", "str_length", "str_contains", "str_lt", "str_lower", "url_encode", "str_split",
    "str_join", "to_string", "to_float", "parse_int",
    "map_put", "map_get", "map_del", "map_size", "map_keys", "parse_json", "render_json",
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

/// The application-spine head name and flattened argument list of an `app` node, walking nested
/// curried `fn` applications — the form the surface parser emits for juxtaposition, so
/// `((f a) b)` reads as `("f", [a, b])`. Covers the `{op: f}` spelling too. `None` when the
/// ultimate head isn't a named variable (e.g. applying a lambda or a projection).
fn app_spine(node: &J) -> Option<(String, Vec<J>)> {
    if node.get("kind").and_then(|k| k.as_str()) != Some("app") && node.get("op").is_none() {
        return None;
    }
    let args: Vec<J> = node.get("args").and_then(|a| a.as_array()).cloned().unwrap_or_default();
    if let Some(op) = node.get("op").and_then(|o| o.as_str()) {
        return Some((op.to_string(), args));
    }
    let f = node.get("fn")?;
    match f.get("kind").and_then(|k| k.as_str()) {
        Some("var") => Some((f.get("name")?.as_str()?.to_string(), args)),
        Some("app") => {
            let (head, mut inner) = app_spine(f)?;
            inner.extend(args);
            Some((head, inner))
        }
        _ => None,
    }
}

/// If `node` is the application-spine head `self` (in any of its spellings — `{op:"self"}`,
/// `{fn:{var:"self"}}`, a curried direct application `((self a0) a1)`, or a curried
/// `apply(apply(self, …), …)` spine), return its argument list in order.
fn self_call_args(node: &J) -> Option<Vec<J>> {
    let args = node.get("args").and_then(|a| a.as_array());
    // Direct spellings, incl. the curried direct-application spine.
    if let Some((head, spine_args)) = app_spine(node) {
        if head == "self" {
            return Some(spine_args);
        }
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
/// Walks the curried spine, so a surface-parsed `str_split "," s` reads as first-order.
fn first_order_head(node: &J) -> Option<String> {
    let (op, _) = app_spine(node)?;
    FIRST_ORDER_OPS.contains(&op.as_str()).then_some(op)
}

/// The integer value of a `lit` int/nat node.
fn int_lit(node: &J) -> Option<i64> {
    if node.get("kind").and_then(|k| k.as_str()) != Some("lit") {
        return None;
    }
    let v = node.get("value")?;
    match v.get("kind").and_then(|k| k.as_str()) {
        Some("int") | Some("nat") => v.get("value").and_then(|n| n.as_i64()),
        _ => None,
    }
}

/// If `arg` is `sub(var p, lit c)` with `c ≥ 1` — a constant numeric decrement — return `(p, c)`.
fn sub_descent(arg: &J) -> Option<(String, i64)> {
    let (op, args) = app_spine(arg)?;
    if op != "sub" || args.len() != 2 {
        return None;
    }
    let p = (args[0].get("kind").and_then(|k| k.as_str()) == Some("var"))
        .then(|| args[0].get("name").and_then(|n| n.as_str()))
        .flatten()?;
    let c = int_lit(&args[1])?;
    (c >= 1).then(|| (p.to_string(), c))
}

/// A branch fact a boolean `case` scrutinee establishes about a parameter — what makes a constant
/// numeric decrement WELL-FOUNDED (a strictly decreasing integer sequence needs a lower bound,
/// which the guard supplies at every recursing activation).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Guard {
    /// `p > lit` / `p >= lit` holds — recursion continues only while `p` exceeds a constant, so
    /// any constant decrement terminates (integers strictly decrease toward a fixed floor).
    LowerBound(String),
    /// `p != 0` holds — a floor ONLY for a `nat`-typed parameter (`p ≥ 0` by type, so `p ≥ 1`),
    /// and only for a unit decrement (`sub(p, 1)` keeps the value a nat; a larger step could
    /// tunnel below zero and recurse forever).
    NonZero(String),
}

/// The facts a boolean scrutinee establishes for its `true` and `false` arms respectively.
fn arm_facts(scrutinee: &J) -> (Vec<Guard>, Vec<Guard>) {
    let Some((op, args)) = app_spine(scrutinee) else {
        return (vec![], vec![]);
    };
    if args.len() != 2 {
        return (vec![], vec![]);
    }
    let var = |n: &J| {
        (n.get("kind").and_then(|k| k.as_str()) == Some("var"))
            .then(|| n.get("name").and_then(|x| x.as_str()).map(String::from))
            .flatten()
    };
    match op.as_str() {
        // p > lit / p >= lit (and the mirrored lit < p / lit <= p): the true arm has the bound.
        "gt" | "ge" => match (var(&args[0]), int_lit(&args[1])) {
            (Some(p), Some(_)) => (vec![Guard::LowerBound(p)], vec![]),
            _ => (vec![], vec![]),
        },
        "lt" | "le" => match (int_lit(&args[0]), var(&args[1])) {
            (Some(_), Some(p)) => (vec![Guard::LowerBound(p)], vec![]),
            _ => (vec![], vec![]),
        },
        // eq(p, 0): the FALSE arm knows p != 0; neq(p, 0): the true arm does.
        "eq" => match (var(&args[0]), int_lit(&args[1])) {
            (Some(p), Some(0)) => (vec![], vec![Guard::NonZero(p)]),
            _ => (vec![], vec![]),
        },
        "neq" => match (var(&args[0]), int_lit(&args[1])) {
            (Some(p), Some(0)) => (vec![Guard::NonZero(p)], vec![]),
            _ => (vec![], vec![]),
        },
        _ => (vec![], vec![]),
    }
}

/// Walk the body collecting, for each `self`-call, the set of argument POSITIONS that provably
/// descend, under the branch `facts` in force at that point. Position `j` counts when the
/// argument is:
///   - `tail^k` of the parameter bound at that same position (`params[j]`) — structural descent
///     (the measure is the value a recursion slot feeds back into itself, so `self(tail(xs), xs)`
///     descends nowhere: the first slot receives a shrinking value, but it is `xs` — the second
///     slot — that the next activation keeps unchanged); or
///   - `sub(params[j], c)` with `c ≥ 1` under a dominating `params[j] > lit`/`>= lit` guard (a
///     strictly decreasing integer sequence over a constant floor is finite — sound for plain
///     ints); or
///   - `sub(params[j], 1)` under a dominating `params[j] != 0` guard when position `j` is
///     `nat`-typed (`p ≥ 0` by type, so `p != 0` ⇒ `p ≥ 1` and the decrement stays a nat).
/// Also detects any opaque/higher-order application, returning the first reason via `opaque`.
/// Every name a pattern binds (`bind` nodes, at any depth — variant payloads, tuple elements).
fn pattern_binds(pattern: &J, out: &mut std::collections::BTreeSet<String>) {
    match pattern {
        J::Object(m) => {
            if m.get("kind").and_then(|k| k.as_str()) == Some("bind") {
                if let Some(n) = m.get("name").and_then(|n| n.as_str()) {
                    out.insert(n.to_string());
                }
            }
            m.values().for_each(|v| pattern_binds(v, out));
        }
        J::Array(items) => items.iter().for_each(|v| pattern_binds(v, out)),
        _ => {}
    }
}

struct Ctx<'a> {
    params: &'a [String],
    nat_positions: &'a std::collections::BTreeSet<usize>,
}

fn walk(
    node: &J,
    ctx: &Ctx,
    shadowed: &std::collections::BTreeSet<String>,
    facts: &[Guard],
    descents: &mut Vec<std::collections::BTreeSet<usize>>,
    opaque: &mut Option<String>,
) {
    if opaque.is_some() {
        return;
    }
    if let Some(sargs) = self_call_args(node) {
        let call_descents: std::collections::BTreeSet<usize> = sargs
            .iter()
            .enumerate()
            .filter(|(j, a)| {
                // A descent must track the PARAMETER at this position — a shadowing `let`/pattern
                // binding of the same name is a different value (`let xs = cons(1, xs) in
                // self(tail(xs))` descends nothing).
                let Some(pj) = ctx.params.get(*j) else { return false };
                if shadowed.contains(pj) {
                    return false;
                }
                if tail_descent(a).is_some_and(|(p, _)| p == *pj) {
                    return true;
                }
                sub_descent(a).is_some_and(|(p, c)| {
                    p == *pj
                        && (facts.contains(&Guard::LowerBound(p.clone()))
                            || (c == 1
                                && ctx.nat_positions.contains(j)
                                && facts.contains(&Guard::NonZero(p.clone()))))
                })
            })
            .map(|(j, _)| j)
            .collect();
        descents.push(call_descents);
        for a in &sargs {
            walk(a, ctx, shadowed, facts, descents, opaque);
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
    match node.get("kind").and_then(|k| k.as_str()) {
        // A `let` rebinding a parameter name SHADOWS it for the rest of the body.
        Some("let") => {
            if let Some(v) = node.get("value") {
                walk(v, ctx, shadowed, facts, descents, opaque);
            }
            let mut inner = shadowed.clone();
            if let Some(n) = node.get("name").and_then(|n| n.as_str()) {
                if ctx.params.contains(&n.to_string()) {
                    inner.insert(n.to_string());
                }
            }
            if let Some(b) = node.get("body") {
                walk(b, ctx, &inner, facts, descents, opaque);
            }
        }
        // A nested lambda's parameters shadow too.
        Some("lambda") => {
            let mut inner = shadowed.clone();
            for p in lambda_params(node).unwrap_or_default() {
                if ctx.params.contains(&p) {
                    inner.insert(p);
                }
            }
            if let Some(b) = node.get("body") {
                walk(b, ctx, &inner, facts, descents, opaque);
            }
        }
        // A `case` over a boolean guard: each arm walks under the facts its pattern establishes
        // (and under any names its pattern binds, which shadow).
        Some("case") => {
            if let Some(scrutinee) = node.get("scrutinee") {
                walk(scrutinee, ctx, shadowed, facts, descents, opaque);
            }
            let (true_facts, false_facts) = node.get("scrutinee").map(arm_facts).unwrap_or_default();
            for arm in node.get("arms").and_then(|a| a.as_array()).into_iter().flatten() {
                let mut arm_facts_ctx = facts.to_vec();
                match arm.pointer("/pattern/value/value").and_then(|b| b.as_bool()) {
                    Some(true) => arm_facts_ctx.extend(true_facts.iter().cloned()),
                    Some(false) => arm_facts_ctx.extend(false_facts.iter().cloned()),
                    None => {}
                }
                let mut binds = std::collections::BTreeSet::new();
                if let Some(p) = arm.get("pattern") {
                    pattern_binds(p, &mut binds);
                }
                let mut inner = shadowed.clone();
                inner.extend(binds.into_iter().filter(|n| ctx.params.contains(n)));
                if let Some(body) = arm.get("body") {
                    walk(body, ctx, &inner, &arm_facts_ctx, descents, opaque);
                }
            }
        }
        // Everything else: descend into every child (args, body, value, …).
        _ => match node {
            J::Object(m) => {
                for (k, v) in m {
                    if k == "pattern" {
                        continue; // patterns bind, they don't compute
                    }
                    walk(v, ctx, shadowed, facts, descents, opaque);
                }
            }
            J::Array(items) => items.iter().for_each(|v| walk(v, ctx, shadowed, facts, descents, opaque)),
            _ => {}
        },
    }
}

/// Decide whether `body` provably terminates. Sound and conservative — see the module docs.
/// Type-blind: without the record's parameter types, only the structural and the
/// explicitly-guarded numeric descents apply (see [`analyze_termination_typed`] for the
/// `nat`-aware form).
pub fn analyze_termination(body: &J) -> TerminationOutcome {
    analyze_termination_typed(body, &std::collections::BTreeSet::new())
}

/// The 0-based positions of a record's `nat`-typed parameters (unwrapping a `forall`), feeding
/// [`analyze_termination_typed`]'s nat-guarded descent.
pub fn nat_param_positions(record: &J) -> std::collections::BTreeSet<usize> {
    let ty = record.pointer("/signature/type");
    let fn_ty = ty
        .and_then(|t| if t.get("kind").and_then(|k| k.as_str()) == Some("forall") { t.get("body") } else { Some(t) });
    fn_ty
        .and_then(|t| t.get("params"))
        .and_then(|p| p.as_array())
        .map(|ps| {
            ps.iter()
                .enumerate()
                .filter(|(_, p)| {
                    p.get("kind").and_then(|k| k.as_str()) == Some("builtin")
                        && p.get("name").and_then(|n| n.as_str()) == Some("nat")
                })
                .map(|(i, _)| i)
                .collect()
        })
        .unwrap_or_default()
}

/// [`analyze_termination`] with the record's `nat`-typed parameter POSITIONS supplied, enabling
/// the guarded numeric descent `case eq(n, 0) of { … ; false => … self(…, sub(n, 1), …) }` — a
/// `nat` is bounded below by type, so a non-zero test plus a unit decrement is a well-founded
/// measure (the `range_from` / `follow_redirect` shape). Plain-int parameters get the same
/// treatment under an explicit lower-bound guard (`gt(p, lit)` / `ge(p, lit)`), where the guard
/// itself is the floor.
pub fn analyze_termination_typed(
    body: &J,
    nat_positions: &std::collections::BTreeSet<usize>,
) -> TerminationOutcome {
    let Some(params) = lambda_params(body) else {
        return TerminationOutcome::Unknown("body is not a `lambda`".into());
    };
    let Some(inner) = body.get("body") else {
        return TerminationOutcome::Unknown("lambda has no body".into());
    };

    let mut descents: Vec<std::collections::BTreeSet<usize>> = Vec::new();
    let mut opaque: Option<String> = None;
    let ctx = Ctx { params: &params, nat_positions };
    walk(inner, &ctx, &std::collections::BTreeSet::new(), &[], &mut descents, &mut opaque);

    if let Some(why) = opaque {
        return TerminationOutcome::Unknown(why);
    }
    if descents.is_empty() {
        // Non-recursive over the first-order fragment: every builtin halts on finite input.
        return TerminationOutcome::Always;
    }
    // Recursive: some ONE argument position must provably descend in EVERY self-call — a single
    // well-founded measure. (Any fixed position works, not just the first: `nth(i, xs)` descends
    // position 2; `range_from(a, n)` descends its guarded nat at position 2.)
    let common = descents
        .iter()
        .skip(1)
        .fold(descents[0].clone(), |acc, s| acc.intersection(s).cloned().collect());
    if common.is_empty() {
        return TerminationOutcome::Unknown(
            "no single argument position provably descends in every self-call (structural `tail`, \
             or a guarded constant decrement — `gt(p, lit)`-guarded `sub(p, c)`, or a nat's \
             non-zero-guarded `sub(p, 1)`)".into(),
        );
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
    fn curried_first_order_application_terminates() {
        // The surface parser emits juxtaposition `str_split "," s` as a CURRIED spine
        // ((str_split ",") s); the analysis must read through it to the first-order head
        // rather than reporting an opaque callee.
        let curried = json!({ "kind": "app",
            "fn": { "kind": "app", "fn": v("str_split"),
                    "args": [{ "kind": "lit", "value": { "kind": "string", "value": "," } }] },
            "args": [v("s")] });
        let body = lambda(&["s"], app("head", vec![curried]));
        assert_eq!(analyze_termination(&body), TerminationOutcome::Always);
        // And a curried self-call spine still reads as a structural recursion: ((self (tail xs)) n).
        let curried_self = json!({ "kind": "app",
            "fn": { "kind": "app", "fn": v("self"), "args": [app("tail", vec![v("xs")])] },
            "args": [v("n")] });
        let step = app("add", vec![lit_i(1), curried_self]);
        let body = lambda(&["xs", "n"], rec_case("xs", lit_i(0), step));
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
    fn descent_in_a_later_position_terminates() {
        // nth: \i xs -> case null xs of true -> 0 | false -> self(sub(i, 1), tail(xs)).
        // The descent is at position 2 (its own parameter) — any FIXED position carries the measure.
        let rec = json!({ "kind": "app", "op": "self",
                          "args": [app("sub", vec![v("i"), lit_i(1)]), app("tail", vec![v("xs")])] });
        let body = lambda(&["i", "xs"], rec_case("xs", lit_i(0), rec));
        assert_eq!(analyze_termination(&body), TerminationOutcome::Always);
    }

    #[test]
    fn descent_of_another_positions_parameter_is_unknown() {
        // \i xs -> case null xs of true -> 0 | false -> self(tail(xs), xs): the FIRST slot receives
        // a shrinking value, but it is xs — the SECOND slot — that the next activation keeps
        // unchanged, so the guard never progresses (an infinite loop the old first-arg check
        // wrongly accepted as Always).
        let rec = json!({ "kind": "app", "op": "self", "args": [app("tail", vec![v("xs")]), v("xs")] });
        let body = lambda(&["i", "xs"], rec_case("xs", lit_i(0), rec));
        assert!(matches!(analyze_termination(&body), TerminationOutcome::Unknown(_)),
                "a descent must feed its own position");
    }

    /// `case eq(n, 0) of true -> base | false -> step` — the nat count-down guard.
    fn eq_zero_case(p: &str, base: J, step: J) -> J {
        json!({
            "kind": "case",
            "scrutinee": app("eq", vec![v(p), lit_i(0)]),
            "arms": [
                { "pattern": lit_b(true), "body": base },
                { "pattern": lit_b(false), "body": step }
            ]
        })
    }

    #[test]
    fn nat_nonzero_guarded_unit_decrement_terminates() {
        // range_from: \a n -> case eq(n, 0) of true -> nil | false -> cons(a, self(add(a,1), sub(n,1))).
        // Position 2 is nat-typed: n != 0 plus n >= 0 (by type) is a well-founded unit descent.
        let rec = json!({ "kind": "app", "op": "self",
                          "args": [app("add", vec![v("a"), lit_i(1)]), app("sub", vec![v("n"), lit_i(1)])] });
        let step = app("cons", vec![v("a"), rec]);
        let body = lambda(&["a", "n"], eq_zero_case("n", v("a"), step));
        let nat1: std::collections::BTreeSet<usize> = [1].into();
        assert_eq!(analyze_termination_typed(&body, &nat1), TerminationOutcome::Always);
        // Without the nat typing the same shape is UNKNOWN — an int can pass 0 going down forever.
        assert!(matches!(analyze_termination_typed(&body, &Default::default()),
                         TerminationOutcome::Unknown(_)),
                "a non-zero guard is only a floor for a nat");
    }

    #[test]
    fn nat_nonzero_guard_with_larger_step_is_unknown() {
        // sub(n, 2) under eq(n, 0)=false: from n=1 the step tunnels below zero — not well-founded.
        let rec = json!({ "kind": "app", "op": "self", "args": [v("a"), app("sub", vec![v("n"), lit_i(2)])] });
        let body = lambda(&["a", "n"], eq_zero_case("n", v("a"), rec));
        let nat1: std::collections::BTreeSet<usize> = [1].into();
        assert!(matches!(analyze_termination_typed(&body, &nat1), TerminationOutcome::Unknown(_)));
    }

    #[test]
    fn lower_bound_guarded_decrement_terminates_for_ints() {
        // \n -> case gt(n, 0) of true -> self(sub(n, 1)) | false -> 0: strictly decreasing ints
        // over a constant floor — sound with NO nat typing (recursion stops once n <= 0).
        let rec = json!({ "kind": "app", "op": "self", "args": [app("sub", vec![v("n"), lit_i(1)])] });
        let body = lambda(&["n"], json!({
            "kind": "case",
            "scrutinee": app("gt", vec![v("n"), lit_i(0)]),
            "arms": [
                { "pattern": lit_b(true), "body": rec },
                { "pattern": lit_b(false), "body": lit_i(0) }
            ]
        }));
        assert_eq!(analyze_termination(&body), TerminationOutcome::Always);
    }

    #[test]
    fn unguarded_decrement_is_unknown() {
        // self(sub(n, 1)) with no dominating bound: n never meets a floor. Must be Unknown.
        let rec = json!({ "kind": "app", "op": "self", "args": [app("sub", vec![v("n"), lit_i(1)])] });
        let body = lambda(&["n"], app("add", vec![lit_i(1), rec]));
        assert!(matches!(analyze_termination(&body), TerminationOutcome::Unknown(_)));
    }

    #[test]
    fn shadowed_parameter_descent_is_unknown() {
        // let xs = cons(1, xs) in case null xs of true -> 0 | false -> self(tail(xs)): the
        // descent tracks the LET-bound xs, not the parameter — self receives the original xs
        // back, an infinite loop. The shadow must disqualify the descent.
        let rec = app("apply", vec![v("self"), app("tail", vec![v("xs")])]);
        let body = lambda(&["xs"], json!({
            "kind": "let", "name": "xs",
            "value": app("cons", vec![lit_i(1), v("xs")]),
            "body": rec_case("xs", lit_i(0), rec)
        }));
        assert!(matches!(analyze_termination(&body), TerminationOutcome::Unknown(_)),
                "a shadowing let must disqualify same-name descent");
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
