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
pub(crate) fn tail_descent(arg: &J) -> Option<(String, usize)> {
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
pub(crate) fn int_lit(node: &J) -> Option<i64> {
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

/// The divisor of a halving-shaped descent: a literal `c ≥ 2`, or a VARIABLE whose floor the guards
/// must supply (`digits`: `div(n, b)` under `b < 2 ⇒ nil`, so `b ≥ 2` on the recursing arm).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Divisor {
    Lit(i64),
    Var(String),
}

/// If `arg` is `div(var p, d)` or `div(add(var p, lit k), d)` (either operand order of the `add`)
/// with `d` a literal `≥ 2` or a variable — a halving-shaped descent — return `(p, k, d)`. Whether it
/// DESCENDS depends on the floors in force: `(p + k) div c < p` for every `p ≥ floor` exactly when it
/// holds at the floor, since the gap grows with `p` (slope `1/c − 1 < 0`); a variable divisor needs
/// its own floor `≥ 2`, and the check runs at that floor (the smallest divisor gives the largest quotient).
pub(crate) fn div_descent(arg: &J) -> Option<(String, i64, Divisor)> {
    let (op, args) = app_spine(arg)?;
    if op != "div" || args.len() != 2 {
        return None;
    }
    let var = |n: &J| {
        (n.get("kind").and_then(|k| k.as_str()) == Some("var"))
            .then(|| n.get("name").and_then(|x| x.as_str()).map(String::from))
            .flatten()
    };
    let d = match (int_lit(&args[1]), var(&args[1])) {
        (Some(c), _) if c >= 2 => Divisor::Lit(c),
        (None, Some(q)) => Divisor::Var(q),
        _ => return None,
    };
    if let Some(p) = var(&args[0]) {
        return Some((p, 0, d));
    }
    let (inner_op, inner) = app_spine(&args[0])?;
    if inner_op != "add" || inner.len() != 2 {
        return None;
    }
    match (var(&inner[0]), int_lit(&inner[1]), int_lit(&inner[0]), var(&inner[1])) {
        (Some(p), Some(k), _, _) => Some((p, k, d)),
        (_, _, Some(k), Some(p)) => Some((p, k, d)),
        _ => None,
    }
}

/// An application whose spine head is a `fn_ref` LITERAL — composition by content address — with its
/// target and collected arguments (curried spines flattened).
pub(crate) fn fn_ref_spine(node: &J) -> Option<(String, Vec<J>)> {
    if node.get("kind").and_then(|k| k.as_str()) != Some("app") {
        return None;
    }
    let args: Vec<J> = node.get("args").and_then(|a| a.as_array()).cloned().unwrap_or_default();
    let f = node.get("fn")?;
    match f.get("kind").and_then(|k| k.as_str()) {
        Some("lit") if f.pointer("/value/kind").and_then(|k| k.as_str()) == Some("fn_ref") => {
            Some((f.pointer("/value/target")?.as_str()?.to_string(), args))
        }
        Some("app") => {
            let (t, mut inner) = fn_ref_spine(f)?;
            inner.extend(args);
            Some((t, inner))
        }
        _ => None,
    }
}

/// If `arg` is `mod(_, var p)` — Euclid's step: the next value is a remainder MODULO the parameter,
/// hence `< p` whenever `p ≥ 1` (the language's `mod` is Euclidean, so the remainder is also ≥ 0).
fn mod_descent(arg: &J) -> Option<String> {
    let (op, args) = app_spine(arg)?;
    if op != "mod" || args.len() != 2 {
        return None;
    }
    (args[1].get("kind").and_then(|k| k.as_str()) == Some("var"))
        .then(|| args[1].get("name").and_then(|x| x.as_str()).map(String::from))
        .flatten()
}

/// A branch fact a boolean `case` scrutinee establishes about a parameter — what makes a numeric
/// descent WELL-FOUNDED (a strictly decreasing integer sequence needs a lower bound, which the
/// guard supplies at every recursing activation).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Guard {
    /// `p >= floor` holds — recursion continues only while `p` stays at or above a constant, so
    /// any constant decrement terminates (integers strictly decrease toward a fixed floor), and a
    /// halving `div(p, c)` or a `mod(_, p)` descent terminates when the floor is ≥ 1 (the value
    /// then strictly shrinks and cannot do so forever above the floor).
    LowerBound(String, i64),
    /// `p != 0` holds — a floor ONLY for a `nat`-typed parameter (`p ≥ 0` by type, so `p ≥ 1`),
    /// and only for a unit decrement (`sub(p, 1)` keeps the value a nat; a larger step could
    /// tunnel below zero and recurse forever).
    NonZero(String),
    /// `length(p) ≥ m` holds — the FALSE arm of `null(tail^(m−1)(p))`: what makes a component of a
    /// shrinking callee's result (shorter than `p` once `length(p) ≥ k`) a well-founded descent.
    MinLength(String, u32),
}

/// The facts a boolean scrutinee establishes for its `true` and `false` arms respectively.
pub(crate) fn arm_facts(scrutinee: &J) -> (Vec<Guard>, Vec<Guard>) {
    let Some((op, args)) = app_spine(scrutinee) else {
        return (vec![], vec![]);
    };
    // `null(tail^j(p))`: the FALSE arm knows length(p) ≥ j + 1.
    if op == "null" && args.len() == 1 {
        if let Some((p, j)) = tail_descent(&args[0]) {
            return (vec![], vec![Guard::MinLength(p, j as u32 + 1)]);
        }
        if args[0].get("kind").and_then(|k| k.as_str()) == Some("var") {
            if let Some(p) = args[0].get("name").and_then(|n| n.as_str()) {
                return (vec![], vec![Guard::MinLength(p.to_string(), 1)]);
            }
        }
        return (vec![], vec![]);
    }
    if args.len() != 2 {
        return (vec![], vec![]);
    }
    let var = |n: &J| {
        (n.get("kind").and_then(|k| k.as_str()) == Some("var"))
            .then(|| n.get("name").and_then(|x| x.as_str()).map(String::from))
            .flatten()
    };
    let lb = |p: String, floor: i64| vec![Guard::LowerBound(p, floor)];
    match op.as_str() {
        // p > lit / p >= lit: the TRUE arm has the floor (lit+1 / lit). lit > p / lit >= p: the
        // FALSE arm does (p >= lit / p >= lit+1).
        "gt" => match (var(&args[0]), int_lit(&args[1]), int_lit(&args[0]), var(&args[1])) {
            (Some(p), Some(l), _, _) => (lb(p, l.saturating_add(1)), vec![]),
            (_, _, Some(l), Some(p)) => (vec![], lb(p, l)),
            _ => (vec![], vec![]),
        },
        "ge" => match (var(&args[0]), int_lit(&args[1]), int_lit(&args[0]), var(&args[1])) {
            (Some(p), Some(l), _, _) => (lb(p, l), vec![]),
            (_, _, Some(l), Some(p)) => (vec![], lb(p, l.saturating_add(1))),
            _ => (vec![], vec![]),
        },
        // lit < p / lit <= p: the TRUE arm has the floor. p < lit / p <= lit: the FALSE arm does
        // (the `case lt(n, 2) of { true => base; false => step }` shape of a halving recursion).
        "lt" => match (int_lit(&args[0]), var(&args[1]), var(&args[0]), int_lit(&args[1])) {
            (Some(l), Some(p), _, _) => (lb(p, l.saturating_add(1)), vec![]),
            (_, _, Some(p), Some(l)) => (vec![], lb(p, l)),
            _ => (vec![], vec![]),
        },
        "le" => match (int_lit(&args[0]), var(&args[1]), var(&args[0]), int_lit(&args[1])) {
            (Some(l), Some(p), _, _) => (lb(p, l), vec![]),
            (_, _, Some(p), Some(l)) => (vec![], lb(p, l.saturating_add(1))),
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

/// What a self-call's descending position tells the induction principle: a structural (list) descent,
/// or a numeric one with the FLOOR the recursion is bounded below by (the guard's, or the nat type's).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DescentKind {
    Structural,
    Numeric { floor: i64 },
}

/// The well-founded measure a recursive body descends on — the one argument position every
/// `self`-call provably shrinks — with the floor it is bounded below by. Numeric (the parameter
/// itself, bounded by `floor`) or structural (`structural`: the parameter is a list every call
/// tails — the measure is its `length`, floor 0). `None` when no single position descends the same
/// way in every call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Measure {
    pub position: usize,
    pub floor: i64,
    pub structural: bool,
}

/// A linked view of other records, for folding a `fn_ref` callee into the analysis: does the callee
/// (itself analyzed, transitively) provably terminate? `None` = unresolvable (no records view, or a
/// cycle) — the application stays opaque.
pub trait LinkedTermination {
    /// Does the callee at `target` provably terminate (its own analysis, over the same view)?
    fn terminates(&self, target: &str) -> Option<bool>;
    /// For a callee returning a tuple of lists each strictly shorter than its list input (for inputs of
    /// length ≥ k) — re-derived from its body — the `k`; see `complexity::component_shrink`.
    fn components_shrink_by(&self, target: &str) -> Option<u32>;
}

pub type CalleeTerminates<'a> = &'a dyn LinkedTermination;

struct Ctx<'a> {
    params: &'a [String],
    nat_positions: &'a std::collections::BTreeSet<usize>,
    callee: Option<CalleeTerminates<'a>>,
}

/// Pattern variables bound as COMPONENTS of a shrinking callee's result: name → (the list parameter the
/// callee was applied to, the callee's `k`). Such a component is a well-founded descent of that parameter
/// wherever the guards establish `length ≥ k`.
type Components = std::collections::BTreeMap<String, (String, u32)>;

#[allow(clippy::too_many_arguments)]
fn walk(
    node: &J,
    ctx: &Ctx,
    shadowed: &std::collections::BTreeSet<String>,
    facts: &[Guard],
    comps: &Components,
    descents: &mut Vec<std::collections::BTreeMap<usize, DescentKind>>,
    opaque: &mut Option<String>,
) {
    if opaque.is_some() {
        return;
    }
    if let Some(sargs) = self_call_args(node) {
        let mut call_descents: std::collections::BTreeMap<usize, DescentKind> = Default::default();
        for (j, a) in sargs.iter().enumerate() {
            // A descent must track the PARAMETER at this position — a shadowing `let`/pattern
            // binding of the same name is a different value (`let xs = cons(1, xs) in
            // self(tail(xs))` descends nothing).
            let Some(pj) = ctx.params.get(j) else { continue };
            if shadowed.contains(pj) {
                continue;
            }
            if tail_descent(a).is_some_and(|(p, _)| p == *pj) {
                call_descents.insert(j, DescentKind::Structural);
                continue;
            }
            // A component of a shrinking callee's result, where the guards say the input is long enough.
            if let Some((q, k)) = a.get("name").and_then(|n| n.as_str()).and_then(|n| comps.get(n)) {
                if q == pj && facts.iter().any(|g| matches!(g, Guard::MinLength(v, m) if v == q && *m >= *k)) {
                    call_descents.insert(j, DescentKind::Structural);
                    continue;
                }
            }
            // The floor a GUARD establishes for a parameter (the tightest one in force), and the
            // floor nat typing adds under a non-zero guard (`p ≥ 0` by type, so `p != 0` ⇒ `p ≥ 1`).
            let guard_floor = |p: &str| {
                facts
                    .iter()
                    .filter_map(|g| match g {
                        Guard::LowerBound(q, f) if q == p => Some(*f),
                        _ => None,
                    })
                    .max()
            };
            let nat_nonzero = ctx.nat_positions.contains(&j) && facts.contains(&Guard::NonZero(pj.clone()));
            let positive_floor = guard_floor(pj).into_iter().chain(nat_nonzero.then_some(1)).max();
            let type_floor = ctx.nat_positions.contains(&j).then_some(0);
            // The floor the induction principle may use: the guard's, else the nat type's 0.
            let induction_floor = |f: Option<i64>| f.or(type_floor).unwrap_or(0);
            if sub_descent(a).is_some_and(|(p, c)| p == *pj && (guard_floor(pj).is_some() || (c == 1 && nat_nonzero))) {
                call_descents.insert(j, DescentKind::Numeric { floor: induction_floor(guard_floor(pj)) });
                continue;
            }
            // Halving-shaped descent: `(p + k) div c < p` must hold at the floor (then everywhere above
            // it). A variable divisor contributes its own floor (≥ 2) as the divisor to check at.
            if let Some((p, k, d)) = div_descent(a) {
                if p == *pj {
                    let c = match &d {
                        Divisor::Lit(c) => Some(*c),
                        Divisor::Var(q) => guard_floor(q).filter(|f| *f >= 2),
                    };
                    if let (Some(f), Some(c)) = (positive_floor, c) {
                        if (f.saturating_add(k)).div_euclid(c) < f {
                            call_descents.insert(j, DescentKind::Numeric { floor: induction_floor(guard_floor(pj)) });
                            continue;
                        }
                    }
                }
            }
            // Euclid's step: the slot receives a remainder modulo its own parameter, `< p` for `p ≥ 1`.
            if mod_descent(a).is_some_and(|p| p == *pj && positive_floor.is_some_and(|f| f >= 1)) {
                call_descents.insert(j, DescentKind::Numeric { floor: induction_floor(guard_floor(pj)) });
            }
        }
        descents.push(call_descents);
        for a in &sargs {
            walk(a, ctx, shadowed, facts, comps, descents, opaque);
        }
        return;
    }
    if node.get("kind").and_then(|k| k.as_str()) == Some("app") {
        if first_order_head(node).is_none() {
            // Composition by content address: a `fn_ref` callee folds in when the linked view can
            // vouch that IT terminates (analyzed, not declared) — then the application is one more
            // terminating first-order step and only its arguments need walking.
            if let Some((target, cargs)) = fn_ref_spine(node) {
                match ctx.callee.and_then(|c| c.terminates(&target)) {
                    Some(true) => {
                        for a in &cargs {
                            walk(a, ctx, shadowed, facts, comps, descents, opaque);
                        }
                        return;
                    }
                    Some(false) => {
                        *opaque = Some(format!("applies `{target}`, whose own termination is not provable"));
                        return;
                    }
                    None => {
                        *opaque = Some(format!(
                            "applies the fn_ref `{target}` (unresolved here — pass --records to fold the callee's termination)"
                        ));
                        return;
                    }
                }
            }
            // An application that is neither a self-call nor a recognized first-order builtin: a
            // higher-order combinator (map/filter/fold), or applying a parameter. Opaque.
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
                walk(v, ctx, shadowed, facts, comps, descents, opaque);
            }
            let mut inner = shadowed.clone();
            if let Some(n) = node.get("name").and_then(|n| n.as_str()) {
                if ctx.params.contains(&n.to_string()) {
                    inner.insert(n.to_string());
                }
            }
            if let Some(b) = node.get("body") {
                walk(b, ctx, &inner, facts, comps, descents, opaque);
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
                walk(b, ctx, &inner, facts, comps, descents, opaque);
            }
        }
        // A `case` over a boolean guard: each arm walks under the facts its pattern establishes
        // (and under any names its pattern binds, which shadow).
        Some("case") => {
            if let Some(scrutinee) = node.get("scrutinee") {
                walk(scrutinee, ctx, shadowed, facts, comps, descents, opaque);
            }
            let (true_facts, false_facts) = node.get("scrutinee").map(arm_facts).unwrap_or_default();
            // `case callee(p) of (a, b) => …` over a shrinking callee (linked view): a and b are
            // components of p, shorter than it once the guards say length(p) ≥ k.
            let shrink_of: Option<(String, u32)> = node.get("scrutinee").and_then(fn_ref_spine).and_then(|(target, cargs)| {
                let first = cargs.first()?;
                if first.get("kind").and_then(|k| k.as_str()) != Some("var") {
                    return None;
                }
                let q = first.get("name")?.as_str()?.to_string();
                if !ctx.params.contains(&q) || shadowed.contains(&q) {
                    return None;
                }
                let k = ctx.callee.and_then(|c| c.components_shrink_by(&target))?;
                (k >= 2).then_some((q, k))
            });
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
                let mut inner_comps = comps.clone();
                if let (Some((q, k)), Some(p)) = (&shrink_of, arm.get("pattern")) {
                    if p.get("kind").and_then(|x| x.as_str()) == Some("tuple") {
                        for n in &binds {
                            inner_comps.insert(n.clone(), (q.clone(), *k));
                        }
                    }
                }
                let mut inner = shadowed.clone();
                inner.extend(binds.into_iter().filter(|n| ctx.params.contains(n)));
                if let Some(body) = arm.get("body") {
                    walk(body, ctx, &inner, &arm_facts_ctx, &inner_comps, descents, opaque);
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
                    walk(v, ctx, shadowed, facts, comps, descents, opaque);
                }
            }
            J::Array(items) => items.iter().for_each(|v| walk(v, ctx, shadowed, facts, comps, descents, opaque)),
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
    record.pointer("/signature/type").map(nat_positions_of_type).unwrap_or_default()
}

/// The `nat`-typed parameter positions of a function TYPE (the record-level helper above reads the
/// signature; the refinement checker holds only the type).
pub fn nat_positions_of_type(ty: &J) -> std::collections::BTreeSet<usize> {
    let fn_ty = if ty.get("kind").and_then(|k| k.as_str()) == Some("forall") { ty.get("body") } else { Some(ty) };
    fn_ty
        .and_then(|t| t.get("params"))
        .and_then(|p| p.as_array())
        .map(|ps| {
            ps.iter()
                .enumerate()
                // `nat` spelled directly or as a `ref` to the canonical `nat` type artifact
                // (the v0.2 builtin↔ref interchange): both are bounded below by type.
                .filter(|(_, p)| crate::builtin_type_name(p) == Some("nat"))
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
    analyze_termination_linked(body, nat_positions, None)
}

/// [`analyze_termination_typed`] over a LINKED view: `callee` answers whether a `fn_ref` target
/// provably terminates (the caller analyzes the callee's own body — a declaration is never read as
/// a fact), so composition by content address is a first-order step rather than an opaque call.
pub fn analyze_termination_linked(
    body: &J,
    nat_positions: &std::collections::BTreeSet<usize>,
    callee: Option<CalleeTerminates<'_>>,
) -> TerminationOutcome {
    match descent_analysis(body, nat_positions, callee) {
        Err(why) => TerminationOutcome::Unknown(why),
        Ok(_) => TerminationOutcome::Always,
    }
}

/// The numeric well-founded measure of a recursive body, for a strong-induction principle: the
/// argument position every `self`-call descends numerically, with the floor the recursion is bounded
/// below by (the smallest floor any recursing activation relies on — any lower bound makes the
/// order well-founded). `None` for a non-recursive body, a structural (list) recursion, or an
/// unprovable one.
pub fn induction_measure(body: &J, nat_positions: &std::collections::BTreeSet<usize>) -> Option<Measure> {
    let common = descent_analysis(body, nat_positions, None).ok()??;
    // A numeric position first (the historical principle), else a position every call descends
    // structurally; a position mixing the two kinds across calls is no single measure.
    let numeric = common.iter().find_map(|(position, kinds)| {
        let floors: Option<Vec<i64>> = kinds
            .iter()
            .map(|k| match k {
                DescentKind::Numeric { floor } => Some(*floor),
                DescentKind::Structural => None,
            })
            .collect();
        floors.and_then(|fs| fs.into_iter().min()).map(|floor| Measure { position: *position, floor, structural: false })
    });
    numeric.or_else(|| {
        common
            .iter()
            .find(|(_, kinds)| !kinds.is_empty() && kinds.iter().all(|k| matches!(k, DescentKind::Structural)))
            .map(|(position, _)| Measure { position: *position, floor: 0, structural: true })
    })
}

/// Shared core: `Ok(None)` for a non-recursive body, `Ok(Some(common))` with, per commonly-descending
/// position, the descent kinds the self-calls establish there; `Err(reason)` when not provably
/// well-founded.
#[allow(clippy::type_complexity)]
fn descent_analysis(
    body: &J,
    nat_positions: &std::collections::BTreeSet<usize>,
    callee: Option<CalleeTerminates<'_>>,
) -> Result<Option<std::collections::BTreeMap<usize, Vec<DescentKind>>>, String> {
    let Some(params) = lambda_params(body) else {
        return Err("body is not a `lambda`".into());
    };
    let Some(inner) = body.get("body") else {
        return Err("lambda has no body".into());
    };

    let mut descents: Vec<std::collections::BTreeMap<usize, DescentKind>> = Vec::new();
    let mut opaque: Option<String> = None;
    let ctx = Ctx { params: &params, nat_positions, callee };
    walk(inner, &ctx, &std::collections::BTreeSet::new(), &[], &Components::new(), &mut descents, &mut opaque);

    if let Some(why) = opaque {
        return Err(why);
    }
    if descents.is_empty() {
        // Non-recursive over the first-order fragment: every builtin halts on finite input.
        return Ok(None);
    }
    // Recursive: some ONE argument position must provably descend in EVERY self-call — a single
    // well-founded measure. (Any fixed position works, not just the first: `nth(i, xs)` descends
    // position 2; `range_from(a, n)` descends its guarded nat at position 2.)
    let positions: std::collections::BTreeSet<usize> = descents
        .iter()
        .skip(1)
        .fold(descents[0].keys().cloned().collect(), |acc: std::collections::BTreeSet<usize>, s| {
            acc.intersection(&s.keys().cloned().collect()).cloned().collect()
        });
    if positions.is_empty() {
        return Err(
            "no single argument position provably descends in every self-call (structural `tail`; \
             a guarded constant decrement — `gt(p, lit)`-guarded `sub(p, c)`, or a nat's \
             non-zero-guarded `sub(p, 1)`; a halving `div(p, c)` / `div(add(p, k), c)` under a floor \
             it descends from, the divisor a literal ≥ 2 or a variable guarded ≥ 2; or Euclid's \
             `mod(_, p)` under a floor ≥ 1)".into(),
        );
    }
    Ok(Some(
        positions
            .into_iter()
            .map(|j| (j, descents.iter().map(|d| d[&j]).collect::<Vec<_>>()))
            .collect(),
    ))
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
    fn euclid_mod_descent_terminates_for_nats() {
        // gcd: \a b -> case eq(b, 0) of true -> a | false -> self(b, mod(a, b)). Position 2 receives
        // a remainder modulo its own parameter: < b whenever b >= 1 (nat + non-zero guard).
        let rec = json!({ "kind": "app", "op": "self", "args": [v("b"), app("mod", vec![v("a"), v("b")])] });
        let body = lambda(&["a", "b"], eq_zero_case("b", v("a"), rec));
        let nats: std::collections::BTreeSet<usize> = [0, 1].into();
        assert_eq!(analyze_termination_typed(&body, &nats), TerminationOutcome::Always);
        // Plain ints: a negative b has no floor — the remainder jumps above it. UNKNOWN.
        assert!(matches!(analyze_termination_typed(&body, &Default::default()), TerminationOutcome::Unknown(_)));
    }

    struct SplitView;
    impl LinkedTermination for SplitView {
        fn terminates(&self, _: &str) -> Option<bool> {
            Some(true)
        }
        fn components_shrink_by(&self, _: &str) -> Option<u32> {
            Some(2)
        }
    }

    #[test]
    fn recursion_on_a_shrinking_callees_components_terminates_under_the_length_guard() {
        // \xs -> case null(tail xs) of true -> xs | false -> case split(xs) of (ev, od) -> append(self(ev), self(od))
        let callee = json!({ "kind": "lit", "value": { "kind": "fn_ref", "target": format!("fn_{}", "cd".repeat(32)) } });
        let split = json!({ "kind": "app", "fn": callee, "args": [v("xs")] });
        let rec = json!({ "kind": "case", "scrutinee": split, "arms": [{
            "pattern": { "kind": "tuple", "elems": [{ "kind": "bind", "name": "ev" }, { "kind": "bind", "name": "od" }] },
            "body": app("append", vec![
                json!({ "kind": "app", "op": "self", "args": [v("ev")] }),
                json!({ "kind": "app", "op": "self", "args": [v("od")] })]) }] });
        let guarded = lambda(&["xs"], json!({ "kind": "case",
            "scrutinee": app("null", vec![app("tail", vec![v("xs")])]),
            "arms": [{ "pattern": lit_b(true), "body": v("xs") }, { "pattern": lit_b(false), "body": rec.clone() }] }));
        assert_eq!(analyze_termination_linked(&guarded, &Default::default(), Some(&SplitView)), TerminationOutcome::Always);
        // Without the length ≥ 2 guard a one-element input's components are not shorter: UNKNOWN.
        let unguarded = lambda(&["xs"], json!({ "kind": "case",
            "scrutinee": app("null", vec![v("xs")]),
            "arms": [{ "pattern": lit_b(true), "body": v("xs") }, { "pattern": lit_b(false), "body": rec }] }));
        assert!(matches!(analyze_termination_linked(&unguarded, &Default::default(), Some(&SplitView)), TerminationOutcome::Unknown(_)));
        // Without the view the callee is opaque.
        assert!(matches!(analyze_termination_typed(&guarded, &Default::default()), TerminationOutcome::Unknown(_)));
    }

    #[test]
    fn halving_descent_terminates_under_a_positive_floor() {
        // isqrt shape: \n -> case lt(n, 2) of true -> n | false -> self(div(n, 4)): the FALSE arm of
        // `n < 2` is the floor n >= 2, and 2 div 4 = 0 < 2 — strictly shrinking above the floor.
        let rec = json!({ "kind": "app", "op": "self", "args": [app("div", vec![v("n"), lit_i(4)])] });
        let body = lambda(&["n"], json!({
            "kind": "case",
            "scrutinee": app("lt", vec![v("n"), lit_i(2)]),
            "arms": [{ "pattern": lit_b(true), "body": v("n") }, { "pattern": lit_b(false), "body": rec }]
        }));
        assert_eq!(analyze_termination_typed(&body, &Default::default()), TerminationOutcome::Always);
        // next_pow2 shape: self(div(add(n, 1), 2)) under the same floor: (2+1) div 2 = 1 < 2.
        let rec2 = json!({ "kind": "app", "op": "self",
                           "args": [app("div", vec![app("add", vec![v("n"), lit_i(1)]), lit_i(2)])] });
        let body2 = lambda(&["n"], json!({
            "kind": "case",
            "scrutinee": app("lt", vec![v("n"), lit_i(2)]),
            "arms": [{ "pattern": lit_b(true), "body": lit_i(1) }, { "pattern": lit_b(false), "body": rec2 }]
        }));
        assert_eq!(analyze_termination_typed(&body2, &Default::default()), TerminationOutcome::Always);
        // A floor of 0 does NOT make halving descend (0 div 2 = 0): `ge(n, 0)`-guarded div is UNKNOWN.
        let body3 = lambda(&["n"], json!({
            "kind": "case",
            "scrutinee": app("ge", vec![v("n"), lit_i(0)]),
            "arms": [{ "pattern": lit_b(true), "body": rec.clone() }, { "pattern": lit_b(false), "body": v("n") }]
        }));
        assert!(matches!(analyze_termination_typed(&body3, &Default::default()), TerminationOutcome::Unknown(_)));
        // A variable divisor is not a recognized descent (it could be 1).
        let rec4 = json!({ "kind": "app", "op": "self", "args": [v("b"), app("div", vec![v("n"), v("b")])] });
        let body4 = lambda(&["b", "n"], json!({
            "kind": "case",
            "scrutinee": app("lt", vec![v("n"), lit_i(1)]),
            "arms": [{ "pattern": lit_b(true), "body": lit_i(0) }, { "pattern": lit_b(false), "body": rec4 }]
        }));
        assert!(matches!(analyze_termination_typed(&body4, &Default::default()), TerminationOutcome::Unknown(_)));
    }

    #[test]
    fn mirrored_lower_bound_guards_read_the_false_arm() {
        // \n d -> case lt(d, 2) of true -> true | false -> self(n, sub(d, 1)): the false arm of
        // `d < 2` is d >= 2 — a floor for the constant decrement (trial division counting down).
        let rec = json!({ "kind": "app", "op": "self", "args": [v("n"), app("sub", vec![v("d"), lit_i(1)])] });
        let body = lambda(&["n", "d"], json!({
            "kind": "case",
            "scrutinee": app("lt", vec![v("d"), lit_i(2)]),
            "arms": [{ "pattern": lit_b(true), "body": lit_b(true) }, { "pattern": lit_b(false), "body": rec }]
        }));
        assert_eq!(analyze_termination_typed(&body, &Default::default()), TerminationOutcome::Always);
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
