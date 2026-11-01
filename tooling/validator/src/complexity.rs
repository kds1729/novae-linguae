//! Structural time-complexity analysis — verify a record's declared `signature.complexity` (an `O(…)`
//! upper-bound class). It is the fifth "verify declared metadata" pass, joining `typecheck` (type),
//! `check-effects` (effects), `check-refinement` (the `nat`/pre/post contracts), and `check-termination`
//! (termination). Like those, the field is *declared* and — until now — unverified: a record could claim
//! `O(n)` for a body that is really `O(n²)`. This is the conservative, **sound, no-solver** structural check.
//!
//! It infers a *sound upper bound* on the body's running time as a class in the input size `n` (list length
//! / value magnitude), then compares it to the declared class. A declared `O(f)` is an upper-bound claim, so
//! to establish it we compute a sound bound `O(g)` from the body and check `g ≤ f` asymptotically. There is
//! **no refutation path** (a computed `g > f` only means our bound is looser than the claim, not that the
//! claim is false — proving a *lower* bound is a different, harder analysis): the CLI reports it as
//! UNVERIFIABLE, never a violation. So, exactly like `check-termination`, this pass can VERIFY a bound but
//! never refute one.
//!
//! ## What it can bound (the first-order fragment)
//!
//! Over the arithmetic/boolean/comparison builtins plus the first-order list ops, each op is classified by
//! its own cost: the scalar ops and `head`/`tail`/`cons`/`null` are `O(1)`; `length`/`append`/`reverse` are
//! `O(n)`. From that:
//!
//! - **Non-recursive body** — a finite AST of first-order ops over data that stays `O(n)` (no recursion can
//!   blow the sizes up), so it is `O(1)` when it uses only constant-time ops, else `O(n)`.
//! - **Structurally recursive body** — modelled as a recurrence `T(n) = a·T(n−k) + w(n)` where `a` is the
//!   branching factor (the number of `self`-calls on the worst-case execution path — `case` arms are
//!   mutually exclusive, so it is the *max* over arms, not the textual count, which keeps `filter` at `O(n)`
//!   rather than mis-reading two arm-local calls as exponential), `k` the constant descent, and `w` the
//!   per-step non-recursive work (`O(1)` or `O(n)` by the rule above, with `self`-calls treated as `O(1)`
//!   placeholders):
//!     - one self-call, `O(1)` work → `O(n)`   (length, sum, map/filter builds)
//!     - one self-call, `O(n)` work → `O(n²)`  (naive reverse via `append`, insertion-style builds)
//!     - two or more self-calls, constant descent → **exponential** (a sound upper bound: naive `fib` is
//!       `Θ(φⁿ) ≤ O(2ⁿ)`)
//!   A **halving** descent (`div(p, c)`, `c ≥ 2`) is recognized too: one call + `O(1)` → `O(log n)`, one
//!   call + `O(n)` → `O(n)`, two calls + `O(1)` → `O(n)`, two calls + `O(n)` → `O(n log n)` (merge-sort).
//!
//! Anything else is `Opaque` (reported UNVERIFIABLE/UNKNOWN, never a false bound): a **higher-order / opaque**
//! application (`map`/`filter`/`fold`, applying a parameter or an `fn_ref`), whose cost depends on a callee
//! this local analysis cannot see (the same honesty stance `check-effects`/`check-termination` take); a
//! recursion whose argument is not a recognized structural/numeric descent, or whose self-calls descend on
//! different parameters (no single measure); or an exotic branching-plus-halving shape outside the table.
//!
//! Complexity is measured in a **single** input dimension `n` (the largest input size), matching the
//! single-variable `cost` model the `compose` path already uses; a genuinely multi-variable bound is
//! approximated in that one `n`.

use serde_json::Value as J;
use std::collections::{BTreeMap, BTreeSet};

/// A complexity class `O(n^deg · (log n)^logs)`, or `exp` for any exponential (`O(2ⁿ)` and above). Covers the
/// schema's `time` classes and the shapes the recurrence analysis produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Class {
    pub deg: u32,
    pub logs: u32,
    pub exp: bool,
}

impl Class {
    pub const CONST: Class = Class { deg: 0, logs: 0, exp: false };
    pub const LOG: Class = Class { deg: 0, logs: 1, exp: false };
    pub const EXP: Class = Class { deg: 0, logs: 0, exp: true };

    fn poly(deg: u32) -> Class {
        Class { deg, logs: 0, exp: false }
    }

    /// Asymptotic rank for ordering: exponential dominates every polynomial; among polynomials compare by
    /// degree, then by log-factor count.
    fn rank(&self) -> (u32, u32, u32) {
        (self.exp as u32, self.deg, self.logs)
    }

    /// Human form, matching the schema's `O(…)` spelling.
    pub fn display(&self) -> String {
        if self.exp {
            return "O(2^n)".to_string();
        }
        let np = match self.deg {
            0 => String::new(),
            1 => "n".to_string(),
            d => format!("n^{d}"),
        };
        let lp = match self.logs {
            0 => String::new(),
            1 => "log n".to_string(),
            l => format!("(log n)^{l}"),
        };
        match (np.is_empty(), lp.is_empty()) {
            (true, true) => "O(1)".to_string(),
            (false, true) => format!("O({np})"),
            (true, false) => format!("O({lp})"),
            (false, false) => format!("O({np} {lp})"),
        }
    }
}

impl PartialOrd for Class {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Class {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.rank().cmp(&other.rank())
    }
}

/// Parse a declared `O(…)` complexity string into a [`Class`]. Recognizes the schema's classes plus the
/// general `O(n^k)` / `O(n^k log n)` polynomials and exponential/factorial forms. Returns `None` for an
/// unrecognized spelling (the caller then reports it UNVERIFIABLE rather than guessing).
pub fn parse_class(s: &str) -> Option<Class> {
    let t = s.trim();
    let inner = t.strip_prefix("O(").and_then(|r| r.strip_suffix(')'))?.trim();
    // Exponential / factorial: any `_^n` (2^n, k^n) or a factorial.
    if inner.contains("^n") || inner.contains('!') {
        return Some(Class::EXP);
    }
    let inner = inner.replace("  ", " ");
    Some(match inner.as_str() {
        "1" => Class::CONST,
        "log n" => Class::LOG,
        "n" => Class::poly(1),
        "n log n" => Class { deg: 1, logs: 1, exp: false },
        _ => {
            // n^K optionally followed by " log n".
            let (poly, logs) = match inner.strip_suffix(" log n") {
                Some(p) => (p, 1u32),
                None => (inner.as_str(), 0u32),
            };
            let deg = if poly == "n" {
                1
            } else {
                poly.strip_prefix("n^").and_then(|d| d.parse::<u32>().ok())?
            };
            Class { deg, logs, exp: false }
        }
    })
}

/// The inferred bound, or the reason the body is out of the analyzable fragment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComplexityOutcome {
    /// A sound upper bound on the body's running time in the input size.
    Bound(Class),
    /// The body is outside the fragment this analysis can bound — never a false `Bound`.
    Opaque(String),
}

/// First-order builtin operators whose *own* work is `O(1)` (excludes the `O(n)` list ops below and the
/// higher-order combinators, which are opaque).
const CONST_OPS: &[&str] = &[
    "add", "sub", "mul", "neg", "abs", "min", "max", "mod", "div", "eq", "neq", "lt", "le", "gt", "ge",
    "and", "or", "xor", "not", "id", "head", "tail", "cons", "null", "to_float",
];
/// First-order ops whose work is linear in their input size. The string ops are input-linear
/// (`str_concat`/`str_join` are also output-linear, which is the sound `output_size` class;
/// `to_string`/`parse_int` are digit-linear).
const LINEAR_OPS: &[&str] = &[
    "length", "append", "reverse", "last", "init",
    "str_concat", "str_length", "str_contains", "str_lt", "str_lower", "url_encode", "str_split",
    "str_join", "to_string", "parse_int",
    // Map ops are O(log n) on a BTreeMap (map_keys O(n)); O(n) is the sound coarse class.
    "map_put", "map_get", "map_del", "map_size", "map_keys",
    // JSON conversions are linear in the text/tree size.
    "parse_json", "render_json",
];

/// How a `self`-call's recursion argument descends its parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Descent {
    /// `tail^k(p)` or `sub(p, c)` — a constant step. Depth is `Θ(n)`.
    Constant,
    /// `div(p, c)` with `c ≥ 2` — the parameter is halved. Depth is `Θ(log n)`.
    Halving,
}

fn head_op(node: &J) -> Option<&str> {
    node.get("op")
        .and_then(|o| o.as_str())
        .or_else(|| node.pointer("/fn/name").and_then(|n| n.as_str()))
}

/// The application-spine head name and flattened argument list of an `app` node, walking nested
/// curried `fn` applications — the form the surface parser emits for juxtaposition, so
/// `((f a) b)` reads as `("f", [a, b])`. Covers the `{op: f}` spelling too. `None` when the
/// ultimate head isn't a named variable.
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

/// If `node` heads a `self`-call (in any spelling — `{op:"self"}`, `{fn:{name:"self"}}`, a curried
/// direct application `((self a0) a1)`, or a curried `apply(apply(self, …), …)` spine), return its
/// argument list in order.
fn self_call_args(node: &J) -> Option<Vec<J>> {
    let args = node.get("args").and_then(|a| a.as_array());
    if let Some((head, spine_args)) = app_spine(node) {
        if head == "self" {
            return Some(spine_args);
        }
    }
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

/// If `arg` is `tail^k(var p)` (`k ≥ 1`), return `(p, Constant)`.
fn tail_descent(arg: &J) -> Option<(String, Descent)> {
    let mut node = arg;
    let mut k = 0;
    while head_op(node) == Some("tail") {
        node = node.get("args").and_then(|a| a.as_array()).and_then(|a| a.first())?;
        k += 1;
    }
    if k >= 1 && node.get("kind").and_then(|k| k.as_str()) == Some("var") {
        return node.get("name").and_then(|n| n.as_str()).map(|n| (n.to_string(), Descent::Constant));
    }
    None
}

/// If `arg` is a strict *numeric* descent of a variable — `sub(var, c)` with `c ≥ 1` (constant step) or
/// `div(var, c)` with `c ≥ 2` (halving) — return `(p, kind)`.
fn numeric_descent(arg: &J) -> Option<(String, Descent)> {
    let (op, args) = app_spine(arg)?;
    let op = op.as_str();
    let (lhs, rhs) = (args.first()?, args.get(1)?);
    let var_name = |n: &J| {
        (n.get("kind").and_then(|k| k.as_str()) == Some("var"))
            .then(|| n.get("name").and_then(|x| x.as_str()).map(String::from))
            .flatten()
    };
    // Euclid's step `mod(_, p)`: the slot receives a remainder modulo the parameter. The remainder
    // at least halves the measure every two steps (Lamé), so the depth is Θ(log n) — the halving row.
    if op == "mod" {
        return var_name(rhs).map(|p| (p, Descent::Halving));
    }
    let c = rhs.pointer("/value/value").and_then(|v| v.as_i64())?;
    if let Some(p) = var_name(lhs) {
        return match op {
            "sub" if c >= 1 => Some((p, Descent::Constant)),
            "div" if c >= 2 => Some((p, Descent::Halving)),
            _ => None,
        };
    }
    // `div(add(p, k), c)` with `c ≥ 2` — the ceiling-halving shape (`(n + 1) div 2`): still Θ(log n) deep.
    if op == "div" && c >= 2 {
        let (inner_op, inner) = app_spine(lhs)?;
        if inner_op == "add" && inner.len() == 2 {
            let lit = |n: &J| n.pointer("/value/value").and_then(|v| v.as_i64());
            if let (Some(p), Some(_)) = (var_name(&inner[0]), lit(&inner[1])) {
                return Some((p, Descent::Halving));
            }
            if let (Some(_), Some(p)) = (lit(&inner[0]), var_name(&inner[1])) {
                return Some((p, Descent::Halving));
            }
        }
    }
    None
}

/// The descent of a single recursion argument, if it is a recognized strict decrease of a parameter.
fn arg_descent(arg: &J) -> Option<(String, Descent)> {
    tail_descent(arg).or_else(|| numeric_descent(arg))
}

/// A linked view of other records, for folding a `fn_ref` callee's cost — and its RESULT'S size, when a
/// certified fact bounds it (a proved postcondition `result < p`, or a list result at most linear in
/// its input), so a callee's result can itself be a size-bounded argument to the next callee.
/// Everything is analyzed/proved from the callee's own body, never read from its declaration.
pub trait LinkedCallees {
    /// The callee's own inferred complexity class, or `None` when unresolvable.
    fn class_of(&self, target: &str) -> Option<Class>;
    /// The callee's parameter positions whose VALUES bound its result (empty = a constant bound), or
    /// `None` when nothing certified bounds the result.
    fn result_bounded_by(&self, target: &str) -> Option<Vec<usize>>;
    /// For a callee returning a TUPLE OF LISTS built by a `tail^k` recursion (`ntt_split`): the `k` by
    /// which every component is shorter than the input (each component ≤ n − ⌊n/k⌋, so strictly shorter
    /// once n ≥ k) — re-derived from the callee's body (`component_shrink`). `None` otherwise.
    fn components_shrink_by(&self, target: &str) -> Option<u32>;
    /// For a callee returning a TUPLE OF LISTS by a `tail^k` recursion on its list parameters
    /// (`ntt_butterfly`): the parameter positions each component is NO LONGER than (plus a constant) —
    /// re-derived from the callee's body (`component_preserve`). `None` otherwise.
    fn components_preserved_by(&self, target: &str) -> Option<Vec<usize>>;
    /// Is the callee's RESULT (every component of it) bounded by a polynomial in its arguments'
    /// values — re-derived from its body ([`result_poly_bounded`]): non-recursive arithmetic, `div`
    /// by anything, `mod` by a bounded divisor, other such callees. Enough for a polylogarithmic
    /// callee applied to it (2026-11-01). Default: nothing is known.
    fn result_poly_bounded(&self, _target: &str) -> bool {
        false
    }
}

/// Does `body`'s result — every component, through `let` and `case` — have a value bounded by a
/// polynomial in its parameters' values? Each result leaf must be [`poly_bounded_in`] its scope; a
/// recursive result is not (nothing bounds `self(…)`'s magnitude by structure).
pub fn result_poly_bounded(body: &J, callee: Option<CalleeComplexity<'_>>) -> bool {
    let (Some(params), Some(inner)) = (lambda_params(body), body.get("body")) else { return false };
    let bound: BTreeSet<String> = params.iter().cloned().collect();
    fn leaves(node: &J, bound: &BTreeSet<String>, callee: Option<CalleeComplexity<'_>>) -> bool {
        match node.get("kind").and_then(|k| k.as_str()) {
            Some("let") => {
                let (Some(v), Some(b)) = (node.get("value"), node.get("body")) else { return false };
                let mut inner = bound.clone();
                if let Some(n) = node.get("name").and_then(|n| n.as_str()) {
                    bind_poly(&mut inner, n, poly_bounded_in(v, bound, callee, false), size_bounded_in(v, bound, callee, false));
                }
                leaves(b, &inner, callee)
            }
            Some("case") => {
                let Some(s) = node.get("scrutinee") else { return false };
                let (sp, ss) = (poly_bounded_in(s, bound, callee, false), size_bounded_in(s, bound, callee, false));
                node.get("arms").and_then(|a| a.as_array()).is_some_and(|arms| {
                    arms.iter().all(|arm| {
                        let mut inner = bound.clone();
                        if let Some(pat) = arm.get("pattern") {
                            let mut names = Vec::new();
                            collect_binds(pat, &mut names);
                            for n in names {
                                bind_poly(&mut inner, &n, sp, ss);
                            }
                        }
                        arm.get("body").is_some_and(|b| leaves(b, &inner, callee))
                    })
                })
            }
            _ => poly_bounded_in(node, bound, callee, false),
        }
    }
    leaves(inner, &bound, callee)
}

/// Record a bound name in a scope set: size-bounded as itself, polynomially bounded only as `~name`
/// (which [`size_bounded_in`] never matches), else neither (shadowing an outer binding).
fn bind_poly(set: &mut BTreeSet<String>, name: &str, poly: bool, size: bool) {
    set.remove(name);
    set.remove(&format!("~{name}"));
    if size {
        set.insert(name.to_string());
    } else if poly {
        set.insert(format!("~{name}"));
    }
}

/// Does `body` return a tuple of lists each at most as long as (a constant plus) EVERY list parameter
/// it descends? Recognized shape: every self-call applies `tail^k_i` to parameter `i` at its own
/// position (the preserved positions, `k_i ≥ 1`); the recursive result is destructured by a tuple
/// pattern; every tuple component the body returns is a `cons` chain ending in `nil` (constant
/// material, a base arm) or in a pattern-bound component with at most `min k_i` conses per step. Then
/// component ≤ k·⌊n_i/k⌋ + c ≤ n_i + c for each preserved `i`. Returns those positions.
pub fn component_preserve(body: &J) -> Option<Vec<usize>> {
    let params = lambda_params(body)?;
    let inner = body.get("body")?;
    let mut calls: Vec<Vec<J>> = Vec::new();
    collect_self_call_args(inner, &mut calls);
    if calls.is_empty() {
        return None;
    }
    let mut positions = Vec::new();
    let mut k_min: Option<u32> = None;
    for (i, p) in params.iter().enumerate() {
        let mut k_i: Option<u32> = None;
        let every = calls.iter().all(|args| match args.get(i).and_then(tail_depth) {
            Some((q, d)) if q == *p => match k_i {
                None => {
                    k_i = Some(d);
                    true
                }
                Some(e) => e == d,
            },
            _ => false,
        });
        if let (true, Some(d)) = (every, k_i) {
            positions.push(i);
            k_min = Some(k_min.map_or(d, |m: u32| m.min(d)));
        }
    }
    let k = k_min?;
    fn ok(node: &J, k: u32, rec_vars: &[String]) -> bool {
        match node.get("kind").and_then(|k| k.as_str()) {
            Some("tuple") => {
                let Some(elems) = node.get("elems").and_then(|e| e.as_array()) else { return false };
                elems.iter().all(|e| match cons_chain(e) {
                    Some((_, None)) => true, // constant material (a base arm)
                    Some((count, Some(v))) => rec_vars.iter().any(|r| *r == v) && count <= k,
                    None => false,
                })
            }
            Some("case") => {
                let Some(scrut) = node.get("scrutinee") else { return false };
                let Some(arms) = node.get("arms").and_then(|a| a.as_array()) else { return false };
                let is_self_scrut = self_call_args(scrut).is_some();
                arms.iter().all(|arm| {
                    let Some(body) = arm.get("body") else { return false };
                    let mut bound: Vec<String> = rec_vars.to_vec();
                    if is_self_scrut {
                        if arm.pointer("/pattern/kind").and_then(|x| x.as_str()) != Some("tuple") {
                            return false;
                        }
                        collect_binds(arm.get("pattern").unwrap(), &mut bound);
                    }
                    ok(body, k, &bound)
                })
            }
            Some("let") => node.get("body").is_some_and(|b| ok(b, k, rec_vars)),
            _ => false,
        }
    }
    ok(inner, k, &[]).then_some(positions)
}

/// How many times the variable `name` occurs free-ish in `node` (patterns skipped; shadowing ignored —
/// the caller treats a `let` of the same name as unbounding it).
fn count_uses(node: &J, name: &str) -> u32 {
    match node {
        J::Object(m) => {
            if m.get("kind").and_then(|k| k.as_str()) == Some("var") {
                return (m.get("name").and_then(|n| n.as_str()) == Some(name)) as u32;
            }
            m.iter().filter(|(k, _)| *k != "pattern").map(|(_, v)| count_uses(v, name)).sum()
        }
        J::Array(xs) => xs.iter().map(|v| count_uses(v, name)).sum(),
        _ => 0,
    }
}

/// Does `body` — a lambda over a list parameter — return a tuple of lists each STRICTLY shorter than
/// the input (for inputs of length ≥ k)? Recognized shape, conservatively: every self-call descends the
/// same parameter by `tail^k`, `k ≥ 2`; the recursive result is destructured by a tuple pattern; every
/// tuple component the body returns is a `cons` chain ending in `nil` or a pattern-bound component
/// variable, with at most `k − 1` conses per step and, in a base arm guarded `length ≤ j` (the TRUE arm
/// of `null(tail^j(p))`), at most `j` conses. Then each component obeys S(n) ≤ (n mod k) +
/// (k − 1)·⌊n/k⌋ = n − ⌊n/k⌋ < n for n ≥ k. Returns `k`.
pub fn component_shrink(body: &J) -> Option<u32> {
    let params = lambda_params(body)?;
    let inner = body.get("body")?;
    // Every self-call: tail^k of one parameter, the same k ≥ 2.
    let mut calls: Vec<Vec<J>> = Vec::new();
    collect_self_call_args(inner, &mut calls);
    if calls.is_empty() {
        return None;
    }
    let mut k: Option<(String, u32)> = None;
    for args in &calls {
        let desc = args.iter().find_map(|a| tail_depth(a));
        match (desc, &k) {
            (Some((p, d)), None) if d >= 2 && params.contains(&p) => k = Some((p, d)),
            (Some((p, d)), Some((q, e))) if p == *q && d == *e => {}
            _ => return None,
        }
    }
    let (p, k) = k?;
    // Walk the body: base arms carry a length bound from `null(tail^j(p))` TRUE arms; recursive arms carry
    // the pattern variables bound from the self-call's tuple result.
    fn ok(node: &J, p: &str, k: u32, max_len: Option<u32>, rec_vars: &[String]) -> bool {
        match node.get("kind").and_then(|k| k.as_str()) {
            Some("tuple") => {
                let Some(elems) = node.get("elems").and_then(|e| e.as_array()) else { return false };
                elems.iter().all(|e| match cons_chain(e) {
                    Some((count, None)) => max_len.is_some_and(|m| count <= m) || (count == 0),
                    Some((count, Some(v))) => rec_vars.iter().any(|r| *r == v) && count <= k - 1,
                    None => false,
                })
            }
            Some("case") => {
                let Some(scrut) = node.get("scrutinee") else { return false };
                let Some(arms) = node.get("arms").and_then(|a| a.as_array()) else { return false };
                // `null(tail^j(p))`: the true arm knows length ≤ j.
                let null_bound: Option<u32> = match app_spine(scrut) {
                    Some((op, args)) if op == "null" && args.len() == 1 => match tail_depth(&args[0]) {
                        Some((q, j)) if q == p => Some(j),
                        None if args[0].get("name").and_then(|n| n.as_str()) == Some(p) => Some(0),
                        _ => None,
                    },
                    _ => None,
                };
                let is_self_scrut = self_call_args(scrut).is_some();
                arms.iter().all(|arm| {
                    let Some(body) = arm.get("body") else { return false };
                    let pat_true = arm.pointer("/pattern/value/value").and_then(|b| b.as_bool());
                    let mut bound: Vec<String> = rec_vars.to_vec();
                    if is_self_scrut {
                        if arm.pointer("/pattern/kind").and_then(|x| x.as_str()) != Some("tuple") {
                            return false;
                        }
                        collect_binds(arm.get("pattern").unwrap(), &mut bound);
                    }
                    let m = match (null_bound, pat_true) {
                        (Some(j), Some(true)) => Some(j),
                        _ => max_len,
                    };
                    ok(body, p, k, m, &bound)
                })
            }
            Some("let") => node.get("body").is_some_and(|b| ok(b, p, k, max_len, rec_vars)),
            _ => false,
        }
    }
    ok(inner, &p, k, None, &[]).then_some(k)
}

/// `cons(_, cons(_, … tail))`: the number of conses and the tail variable (`None` for `nil`).
fn cons_chain(node: &J) -> Option<(u32, Option<String>)> {
    if node.get("kind").and_then(|k| k.as_str()) == Some("var") {
        let n = node.get("name").and_then(|n| n.as_str())?;
        return Some((0, if n == "nil" { None } else { Some(n.to_string()) }));
    }
    match app_spine(node) {
        Some((op, args)) if op == "cons" && args.len() == 2 => {
            let (c, t) = cons_chain(&args[1])?;
            Some((c + 1, t))
        }
        _ => None,
    }
}

fn collect_binds(pat: &J, out: &mut Vec<String>) {
    match pat {
        J::Object(m) => {
            if m.get("kind").and_then(|k| k.as_str()) == Some("bind") {
                if let Some(n) = m.get("name").and_then(|n| n.as_str()) {
                    out.push(n.to_string());
                }
            }
            m.values().for_each(|v| collect_binds(v, out));
        }
        J::Array(xs) => xs.iter().for_each(|v| collect_binds(v, out)),
        _ => {}
    }
}

fn collect_self_call_args(node: &J, out: &mut Vec<Vec<J>>) {
    if let Some(args) = self_call_args(node) {
        out.push(args.clone());
        for a in &args {
            collect_self_call_args(a, out);
        }
        return;
    }
    match node {
        J::Object(m) => m.iter().filter(|(k, _)| *k != "pattern").for_each(|(_, v)| collect_self_call_args(v, out)),
        J::Array(xs) => xs.iter().for_each(|v| collect_self_call_args(v, out)),
        _ => {}
    }
}

/// `tail^k(var p)` with `k ≥ 1`: the parameter and the depth.
fn tail_depth(arg: &J) -> Option<(String, u32)> {
    let mut node = arg;
    let mut k = 0;
    while head_op(node) == Some("tail") {
        node = node.get("args").and_then(|a| a.as_array()).and_then(|a| a.first())?;
        k += 1;
    }
    if k >= 1 && node.get("kind").and_then(|k| k.as_str()) == Some("var") {
        return node.get("name").and_then(|n| n.as_str()).map(|n| (n.to_string(), k));
    }
    None
}

/// A self-call argument that is a COMPONENT of a shrinking callee's result (bound by the tuple pattern
/// of `case callee(p) of (ev, od) => …`): a halving descent of `p`.
fn component_descent(arg: &J, comps: &BTreeMap<String, String>) -> Option<(String, Descent)> {
    if arg.get("kind").and_then(|k| k.as_str()) != Some("var") {
        return None;
    }
    comps.get(arg.get("name")?.as_str()?).map(|p| (p.clone(), Descent::Halving))
}

pub type CalleeComplexity<'a> = &'a dyn LinkedCallees;

/// Ops whose RESULT is no larger than the sum of their inputs' sizes (values linear in the input): a
/// callee applied to such arguments runs in its class measured in OUR `n`. `mul` is admitted only
/// with a literal operand (a product of two inputs is quadratic in value — `self(n·n)` would make a
/// callee's `O(m)` an `O(n²)`).
const SIZE_BOUNDED_OPS: &[&str] = &[
    "add", "sub", "neg", "abs", "min", "max", "mod", "div", "length", "head", "tail", "cons", "append",
    "reverse", "concat", "init", "last", "fst", "snd", "null", "eq", "neq", "lt", "le", "gt", "ge", "and",
    "or", "not", "to_float", "str_concat", "str_length",
];

/// Is `arg`'s VALUE bounded by the input size — a parameter (or a `let` name bound to such a value), a
/// literal, a tuple of such, a size-bounded op over such, or a `fn_ref` callee's result that a CERTIFIED
/// fact bounds by size-bounded arguments? A recursive result or a pattern-bound name is not (nothing
/// bounds `self(…)`'s magnitude, and a tuple component came from a callee).
/// Is `arg` size-bounded by the parameters (`bound`)? Inside a function whose OWN list result is at most linear in its input
/// (`self_linear`): then a recursive result is size-bounded too (`ntt_rec` feeds its halves' transforms
/// to the butterfly).
fn size_bounded_in(arg: &J, bound: &BTreeSet<String>, callee: Option<CalleeComplexity<'_>>, self_linear: bool) -> bool {
    let size_bounded = |a: &J, b: &BTreeSet<String>, c: Option<CalleeComplexity<'_>>| size_bounded_in(a, b, c, self_linear);
    match arg.get("kind").and_then(|k| k.as_str()) {
        Some("var") => arg.get("name").and_then(|n| n.as_str()).is_some_and(|n| bound.contains(n) || n == "nil"),
        Some("lit") => true,
        Some("tuple") => arg.get("elems").and_then(|e| e.as_array()).is_some_and(|es| es.iter().all(|e| size_bounded(e, bound, callee))),
        Some("app") if self_call_args(arg).is_some() => {
            self_linear && self_call_args(arg).is_some_and(|sargs| sargs.iter().all(|a| size_bounded(a, bound, callee)))
        }
        Some("app") => {
            if let Some((target, cargs)) = crate::terminate::fn_ref_spine(arg) {
                // A proved `result < p` (or a list result at most linear in its input) makes the callee's
                // result as bounded as the arguments that bound it; a SHRINKING callee's tuple (each
                // component strictly shorter than its list input) is bounded by all of its arguments.
                if callee.and_then(|c| c.components_shrink_by(&target)).is_some() {
                    return cargs.iter().all(|a| size_bounded(a, bound, callee));
                }
                return callee
                    .and_then(|c| c.result_bounded_by(&target))
                    .is_some_and(|idxs| idxs.iter().all(|i| cargs.get(*i).is_some_and(|a| size_bounded(a, bound, callee))));
            }
            match app_spine(arg) {
                // `mod(x, k)` with a literal modulus is bounded by the constant whatever `x` is (`w2 = w·w mod P`).
                Some((op, args)) if op == "mod" && args.len() == 2 && args[1].get("kind").and_then(|k| k.as_str()) == Some("lit") => true,
                Some((op, args)) if op == "mul" && args.len() == 2 => {
                    let lit = |n: &J| n.get("kind").and_then(|k| k.as_str()) == Some("lit");
                    (lit(&args[0]) || lit(&args[1])) && args.iter().all(|a| size_bounded(a, bound, callee))
                }
                Some((op, args)) => SIZE_BOUNDED_OPS.contains(&op.as_str()) && args.iter().all(|a| size_bounded(a, bound, callee)),
                None => false,
            }
        }
        _ => false,
    }
}

/// Is `arg`'s value bounded by a POLYNOMIAL in the input size — anything [`size_bounded_in`] admits,
/// and products (`mul`) and the size-bounded ops over such terms? Enough for a callee whose class is
/// polylogarithmic (`deg = 0`, no exponential): `log^k(p(n)) = O(log^k n)` for any polynomial `p`, so
/// `normalize(a·d + c·b, b·d)`, `O(log m)` in its arguments' magnitude, is `O(log n)` in ours
/// (2026-11-01, the rational tier: seven folds UNVERIFIABLE for exactly this product). A callee of
/// polynomial class still requires a size-bounded argument (`O(m)` of `m = n²` is `O(n²)`).
fn poly_bounded_in(arg: &J, bound: &BTreeSet<String>, callee: Option<CalleeComplexity<'_>>, self_linear: bool) -> bool {
    if size_bounded_in(arg, bound, callee, self_linear) {
        return true;
    }
    let poly = |a: &J| poly_bounded_in(a, bound, callee, self_linear);
    match arg.get("kind").and_then(|k| k.as_str()) {
        // A name bound to a polynomially bounded value (`~name`, see `bind_poly`).
        Some("var") => return arg.get("name").and_then(|n| n.as_str()).is_some_and(|n| bound.contains(&format!("~{n}"))),
        Some("tuple") => return arg.get("elems").and_then(|e| e.as_array()).is_some_and(|es| es.iter().all(poly)),
        _ => {}
    }
    if self_call_args(arg).is_some() {
        return false;
    }
    // A callee whose result is itself polynomial in its arguments (`result_poly_bounded`).
    if let Some((target, cargs)) = crate::terminate::fn_ref_spine(arg) {
        return callee.is_some_and(|c| c.result_poly_bounded(&target)) && cargs.iter().all(poly);
    }
    match app_spine(arg) {
        // |x div y| ≤ |x| for every integer y ≠ 0, whatever the divisor (a gcd, a callee's result).
        Some((op, args)) if op == "div" && args.len() == 2 => poly(&args[0]),
        // |x mod y| < |y|: bounded by the divisor alone (NOT by x — Euclidean −1 mod 5 = 4).
        Some((op, args)) if op == "mod" && args.len() == 2 => poly(&args[1]),
        Some((op, args)) if op == "mul" || SIZE_BOUNDED_OPS.contains(&op.as_str()) => {
            args.iter().all(|a| poly_bounded_in(a, bound, callee, self_linear))
        }
        _ => false,
    }
}

/// The first opaque (higher-order / unrecognized-application) reason in `node`, if any. A `self`-call,
/// a recognized first-order builtin application, and — over a linked view — a `fn_ref` callee with an
/// inferred class applied to size-bounded arguments are fine; anything else applied is opaque.
fn find_opaque_in(node: &J, callee: Option<CalleeComplexity<'_>>, bound: &BTreeSet<String>, self_linear: bool) -> Option<String> {
    let find_opaque = |n: &J, c: Option<CalleeComplexity<'_>>, b: &BTreeSet<String>| find_opaque_in(n, c, b, self_linear);
    let size_bounded = |a: &J, b: &BTreeSet<String>, c: Option<CalleeComplexity<'_>>| size_bounded_in(a, b, c, self_linear);
    if self_call_args(node).is_some() {
        // still descend into the call's arguments below
    } else if let Some((target, cargs)) = crate::terminate::fn_ref_spine(node) {
        match callee.and_then(|c| c.class_of(&target)) {
            Some(class) => {
                // A polylogarithmic callee tolerates polynomially bounded arguments (`poly_bounded_in`).
                let polylog = class.deg == 0 && !class.exp;
                let ok = |a: &J| if polylog { poly_bounded_in(a, bound, callee, self_linear) } else { size_bounded(a, bound, callee) };
                if let Some(a) = cargs.iter().find(|a| !ok(a)) {
                    return Some(format!(
                        "applies `{target}` to an argument whose size is not bounded by the input ({}) — its class cannot be re-expressed in n",
                        a.get("kind").and_then(|k| k.as_str()).unwrap_or("?")
                    ));
                }
                return cargs.iter().find_map(|a| find_opaque(a, callee, bound));
            }
            None => {
                return Some(format!(
                    "applies the fn_ref `{target}` (its cost is unresolved here — pass --records to fold the callee's class)"
                ))
            }
        }
    } else if node.get("kind").and_then(|k| k.as_str()) == Some("app") {
        let op = app_spine(node).map(|(h, _)| h);
        let known = op
            .as_deref()
            .map(|o| CONST_OPS.contains(&o) || LINEAR_OPS.contains(&o))
            .unwrap_or(false);
        if !known {
            return Some(format!(
                "applies a higher-order/opaque callee `{}` (its cost is unchecked)",
                op.as_deref().unwrap_or("<unknown>")
            ));
        }
    }
    if node.get("kind").and_then(|k| k.as_str()) == Some("let") {
        // A `let` name bound to a size-bounded value is itself size-bounded for the body.
        if let Some(why) = node.get("value").and_then(|v| find_opaque(v, callee, bound)) {
            return Some(why);
        }
        let mut inner = bound.clone();
        if let (Some(n), Some(v)) = (node.get("name").and_then(|n| n.as_str()), node.get("value")) {
            bind_poly(&mut inner, n, poly_bounded_in(v, bound, callee, self_linear), size_bounded(v, bound, callee));
        }
        return node.get("body").and_then(|b| find_opaque(b, callee, &inner));
    }
    if node.get("kind").and_then(|k| k.as_str()) == Some("case") {
        // Destructuring a size-bounded value yields size-bounded parts: the arms' pattern names join
        // the bound set (`case split(xs) of (ev, od) => …` over a shrinking callee).
        if let Some(s) = node.get("scrutinee") {
            if let Some(why) = find_opaque(s, callee, bound) {
                return Some(why);
            }
            let scrut_bounded = size_bounded(s, bound, callee);
            // Destructuring a POLYNOMIALLY bounded value (a callee whose result is polynomial in its
            // arguments, `gi_mul`'s pair) yields polynomially bounded parts (`~name`).
            let scrut_poly = poly_bounded_in(s, bound, callee, self_linear);
            for arm in node.get("arms").and_then(|a| a.as_array()).cloned().unwrap_or_default() {
                let mut inner = bound.clone();
                if let Some(pat) = arm.get("pattern") {
                    let mut names = Vec::new();
                    collect_binds(pat, &mut names);
                    for n in names {
                        bind_poly(&mut inner, &n, scrut_poly, scrut_bounded);
                    }
                }
                if let Some(why) = arm.get("body").and_then(|b| find_opaque(b, callee, &inner)) {
                    return Some(why);
                }
            }
            return None;
        }
    }
    match node {
        J::Object(m) => {
            for (k, v) in m {
                if k == "pattern" {
                    continue;
                }
                if let Some(why) = find_opaque(v, callee, bound) {
                    return Some(why);
                }
            }
            None
        }
        J::Array(items) => items.iter().find_map(|v| find_opaque(v, callee, bound)),
        _ => None,
    }
}

/// Collect every `self`-call's descent (of its descending argument). A `None` entry marks a self-call whose
/// arguments contain no recognized descent — the recursion is not provably well-founded.
#[allow(clippy::too_many_arguments)]
fn collect_self_descents(
    node: &J,
    facts: &[crate::terminate::Guard],
    comps: &BTreeMap<String, String>,
    callee: Option<CalleeComplexity<'_>>,
    params: &[String],
    out: &mut Vec<Vec<(String, Descent)>>,
) {
    if let Some(sargs) = self_call_args(node) {
        // EVERY argument that strictly decreases a parameter (append-style spectators don't); the caller
        // intersects across self-calls to find the one measure — a call may shrink two parameters
        // (`pad_to`: the list AND the count) while another shrinks only the count. A `div(p, q)` by a
        // VARIABLE divisor halves only under a guard that floors `q` at 2 — the facts in force decide. A
        // component of a shrinking callee's result (`ntt_split`'s halves) is a halving descent of the
        // parameter the callee was applied to.
        out.push(
            sargs
                .iter()
                .filter_map(|a| arg_descent(a).or_else(|| variable_divisor_descent(a, facts)).or_else(|| component_descent(a, comps)))
                .collect(),
        );
        for a in &sargs {
            collect_self_descents(a, facts, comps, callee, params, out);
        }
        return;
    }
    if node.get("kind").and_then(|k| k.as_str()) == Some("case") {
        if let Some(s) = node.get("scrutinee") {
            collect_self_descents(s, facts, comps, callee, params, out);
            let (tf, ff) = crate::terminate::arm_facts(s);
            // `case callee(p) of (a, b) => …` over a shrinking callee: a and b are components of p.
            let shrink_of: Option<String> = crate::terminate::fn_ref_spine(s).and_then(|(target, cargs)| {
                let q = cargs.first()?.get("name")?.as_str()?.to_string();
                (cargs.first()?.get("kind")?.as_str()? == "var" && params.contains(&q)
                    && callee.and_then(|c| c.components_shrink_by(&target)).is_some_and(|k| k >= 2))
                    .then_some(q)
            });
            for arm in node.get("arms").and_then(|a| a.as_array()).cloned().unwrap_or_default() {
                let pat = arm.pointer("/pattern/value/value").and_then(|v| v.as_bool());
                let mut inner: Vec<crate::terminate::Guard> = facts.to_vec();
                match pat {
                    Some(true) => inner.extend(tf.iter().cloned()),
                    Some(false) => inner.extend(ff.iter().cloned()),
                    None => {}
                }
                let mut comps2 = comps.clone();
                if let (Some(q), Some(pat)) = (&shrink_of, arm.get("pattern")) {
                    if pat.get("kind").and_then(|k| k.as_str()) == Some("tuple") {
                        let mut names = Vec::new();
                        collect_binds(pat, &mut names);
                        for n in names {
                            comps2.insert(n, q.clone());
                        }
                    }
                }
                if let Some(b) = arm.get("body") {
                    collect_self_descents(b, &inner, &comps2, callee, params, out);
                }
            }
            return;
        }
    }
    match node {
        J::Object(m) => {
            for (k, v) in m {
                if k == "pattern" {
                    continue;
                }
                collect_self_descents(v, facts, comps, callee, params, out);
            }
        }
        J::Array(items) => items.iter().for_each(|v| collect_self_descents(v, facts, comps, callee, params, out)),
        _ => {}
    }
}

/// `div(var p, var q)` (or `div(add(p, k), q)`) with a dominating guard `q ≥ 2`: a halving descent of `p`.
fn variable_divisor_descent(arg: &J, facts: &[crate::terminate::Guard]) -> Option<(String, Descent)> {
    let (p, _, d) = crate::terminate::div_descent(arg)?;
    match d {
        crate::terminate::Divisor::Var(q) => facts
            .iter()
            .any(|g| matches!(g, crate::terminate::Guard::LowerBound(v, f) if *v == q && *f >= 2))
            .then_some((p, Descent::Halving)),
        crate::terminate::Divisor::Lit(_) => None, // literal divisors are `numeric_descent`'s
    }
}

/// The branching factor: the number of `self`-invocations on the worst-case execution path. `case` arms are
/// mutually exclusive (max over arms); everything else in an expression is evaluated (sum of children).
fn path_self_count(node: &J) -> u32 {
    if let Some(sargs) = self_call_args(node) {
        return 1 + sargs.iter().map(path_self_count).sum::<u32>();
    }
    if node.get("kind").and_then(|k| k.as_str()) == Some("case") {
        let scrut = node.get("scrutinee").map(path_self_count).unwrap_or(0);
        let arms = node
            .get("arms")
            .and_then(|a| a.as_array())
            .map(|a| a.iter().map(|arm| arm.get("body").map(path_self_count).unwrap_or(0)).max().unwrap_or(0))
            .unwrap_or(0);
        return scrut + arms;
    }
    match node {
        J::Object(m) => m.iter().filter(|(k, _)| *k != "pattern").map(|(_, v)| path_self_count(v)).sum(),
        J::Array(items) => items.iter().map(path_self_count).sum(),
        _ => 0,
    }
}

/// The per-step non-recursive work class on the worst-case path: `O(n)` if the worst path applies any
/// linear list op (`length`/`append`/`reverse`), else `O(1)`. `self`-calls are `O(1)` placeholders (their
/// cost is the recurrence's `T(·)` term), but their arguments are still evaluated. `case` arms are
/// mutually exclusive (max over arms).
fn path_work(node: &J, callee: Option<CalleeComplexity<'_>>) -> Class {
    if let Some(sargs) = self_call_args(node) {
        return sargs.iter().map(|a| path_work(a, callee)).max().unwrap_or(Class::CONST);
    }
    // Composition by content address: the callee's own class (in our n — `find_opaque` has already
    // required size-bounded arguments) is this step's work, beside the work of building the arguments.
    if let Some((target, cargs)) = crate::terminate::fn_ref_spine(node) {
        let own = callee.and_then(|c| c.class_of(&target)).unwrap_or(Class::CONST);
        return cargs.iter().map(|a| path_work(a, callee)).fold(own, Class::max);
    }
    if node.get("kind").and_then(|k| k.as_str()) == Some("case") {
        let scrut = node.get("scrutinee").map(|s| path_work(s, callee)).unwrap_or(Class::CONST);
        let arms = node
            .get("arms")
            .and_then(|a| a.as_array())
            .map(|a| a.iter().map(|arm| arm.get("body").map(|b| path_work(b, callee)).unwrap_or(Class::CONST)).max().unwrap_or(Class::CONST))
            .unwrap_or(Class::CONST);
        return scrut.max(arms);
    }
    // A linear list op makes this expression O(n) regardless of its (bounded) arguments.
    if node.get("kind").and_then(|k| k.as_str()) == Some("app") {
        if app_spine(node).map(|(o, _)| LINEAR_OPS.contains(&o.as_str())).unwrap_or(false) {
            return Class::poly(1);
        }
    }
    match node {
        J::Object(m) => m.iter().filter(|(k, _)| *k != "pattern").map(|(_, v)| path_work(v, callee)).max().unwrap_or(Class::CONST),
        J::Array(items) => items.iter().map(|v| path_work(v, callee)).max().unwrap_or(Class::CONST),
        _ => Class::CONST,
    }
}

/// Solve the recurrence `T(n) = branch·T(descent(n)) + work` for the supported shapes, or `None` for an
/// exotic branching-plus-halving combination outside the table.
fn combine(kind: Descent, branch: u32, work: Class) -> Option<Class> {
    match kind {
        Descent::Constant => Some(if branch >= 2 {
            Class::EXP // T(n) = a·T(n−k) + poly, a ≥ 2 → exponential (sound upper bound)
        } else {
            // T(n) = T(n−k) + work → degree of work plus one.
            Class::poly(work.deg + 1)
        }),
        Descent::Halving => match (branch, work.deg) {
            (1, 0) => Some(Class::LOG),      // T(n) = T(n/2) + O(1)
            (1, _) => Some(Class::poly(1)),  // T(n) = T(n/2) + O(n)   (geometric sum)
            (2, 0) => Some(Class::poly(1)),  // T(n) = 2T(n/2) + O(1)
            (2, 1) => Some(Class { deg: 1, logs: 1, exp: false }), // 2T(n/2) + O(n) — merge-sort
            _ => None,                       // branch ≥ 3 (or higher work) with halving — out of the table
        },
    }
}

/// Infer a sound upper bound on the body's running time as a complexity [`Class`], or report why it is
/// outside the analyzable fragment. Sound and conservative — see the module docs.
pub fn analyze_complexity(body: &J) -> ComplexityOutcome {
    analyze_complexity_linked(body, None)
}

/// [`analyze_complexity`] over a LINKED view: `callee` answers a `fn_ref` target's own inferred class
/// (analyzed, never declared), so composition by content address is a step of that cost — provided the
/// callee's arguments are size-bounded by our input (else the class cannot be re-expressed in `n`).
pub fn analyze_complexity_linked(body: &J, callee: Option<CalleeComplexity<'_>>) -> ComplexityOutcome {
    analyze_complexity_typed(body, None, callee)
}

/// [`analyze_complexity_linked`] knowing the function's DECLARED result type: when it is a list the
/// output-size analysis says is at most linear in the input, the function's own recursive results count
/// as size-bounded arguments (a callee applied to them folds).
pub fn analyze_complexity_typed(body: &J, result_ty: Option<&J>, callee: Option<CalleeComplexity<'_>>) -> ComplexityOutcome {
    let Some(params) = lambda_params(body) else {
        return ComplexityOutcome::Opaque("body is not a `lambda`".into());
    };
    let Some(inner) = body.get("body") else {
        return ComplexityOutcome::Opaque("lambda has no body".into());
    };
    let bound: BTreeSet<String> = params.iter().cloned().collect();
    let self_linear = result_ty.is_some_and(|rt| {
        let rt = crate::fold_canonical_type_refs(rt);
        is_list_type(&rt) && matches!(list_output_size_linked(&params, inner, callee), OutputSize::Const | OutputSize::Linear)
    });
    if let Some(why) = find_opaque_in(inner, callee, &bound, self_linear) {
        return ComplexityOutcome::Opaque(why);
    }

    let mut descents: Vec<Vec<(String, Descent)>> = Vec::new();
    collect_self_descents(inner, &[], &BTreeMap::new(), callee, &params, &mut descents);

    if descents.is_empty() {
        // Non-recursive over the first-order fragment: constant number of ops on O(n)-bounded data.
        return ComplexityOutcome::Bound(path_work(inner, callee));
    }

    // Every self-call must descend on ONE fixed parameter; the kind is Constant unless every descent halves.
    if descents.iter().any(|d| d.iter().all(|(p, _)| !params.contains(p))) {
        return ComplexityOutcome::Opaque(
            "a self-call's recursion argument is not a recognized structural/numeric descent".into(),
        );
    }
    let Some((_, kinds)) = choose_measure(&descents, &params) else {
        return ComplexityOutcome::Opaque("self-calls descend on different parameters (no single measure)".into());
    };
    let any_constant = kinds.iter().any(|k| *k == Descent::Constant);
    let any_halving = kinds.iter().any(|k| *k == Descent::Halving);
    // Mixed constant+halving descents on the same parameter: the constant step drives the depth (Θ(n)).
    let kind = if any_constant || !any_halving { Descent::Constant } else { Descent::Halving };
    let branch = path_self_count(inner).max(1);
    let work = path_work(inner, callee);
    match combine(kind, branch, work) {
        Some(c) => ComplexityOutcome::Bound(c),
        None => ComplexityOutcome::Opaque(
            "recursion shape (branching with a halving descent) is outside the analyzable table".into(),
        ),
    }
}

// --- output-size analysis (for verifying the structured `cost` metadata) ---------------------------
//
// `signature.cost` carries a `time` class (verified against `analyze_complexity` just like the flat
// `complexity` field) and an `output_size` relation — how the result's size grows with the input's. The
// `compose` precise-complexity path threads `output_size` through a pipeline to re-express each downstream
// stage's cost in the pipeline's input size, and it **trusts it blindly**: a stage declared `preserving`
// that actually expands would make the composite's time bound unsound. This analysis infers a sound upper
// bound on the output size so a declared `output_size` can be *verified*, closing that gap.

/// A sound upper bound on how a function's RESULT size grows with its input size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputSize {
    /// O(1) — a scalar result, or a fixed-size list (schema `constant`).
    Const,
    /// ≤ Θ(n) — the result is at most linear in the input (schema `preserving` / `bounded`).
    Linear,
    /// Θ(n²) — an expanding result (schema `quadratic`).
    Quadratic,
    /// Θ(n³) (schema `cubic`).
    Cubic,
    /// Not determinable here (a higher-order/opaque build, a polymorphic result, or an exotic recursion).
    Unknown,
}

impl OutputSize {
    /// The polynomial degree in the input size, or `None` for `Unknown`.
    pub fn degree(self) -> Option<u32> {
        Some(match self {
            OutputSize::Const => 0,
            OutputSize::Linear => 1,
            OutputSize::Quadratic => 2,
            OutputSize::Cubic => 3,
            OutputSize::Unknown => return None,
        })
    }
    fn from_degree(d: u32) -> OutputSize {
        match d {
            0 => OutputSize::Const,
            1 => OutputSize::Linear,
            2 => OutputSize::Quadratic,
            _ => OutputSize::Cubic,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            OutputSize::Const => "constant",
            OutputSize::Linear => "linear (preserving/bounded)",
            OutputSize::Quadratic => "quadratic",
            OutputSize::Cubic => "cubic",
            OutputSize::Unknown => "unknown",
        }
    }
}

/// Map the schema's `cost.output_size` enum to an [`OutputSize`] degree bucket (`preserving`/`bounded`
/// both cap the result at Θ(n), so both are `Linear`).
pub fn parse_output_size(s: &str) -> OutputSize {
    match s {
        "constant" => OutputSize::Const,
        "preserving" | "bounded" => OutputSize::Linear,
        "quadratic" => OutputSize::Quadratic,
        "cubic" => OutputSize::Cubic,
        _ => OutputSize::Unknown,
    }
}

/// Whether a type-expr denotes a `List` (builtin `List`, or `apply(List, …)`).
pub(crate) fn is_list_type(ty: &J) -> bool {
    match ty.get("kind").and_then(|k| k.as_str()) {
        Some("builtin") => ty.get("name").and_then(|n| n.as_str()) == Some("List"),
        Some("apply") => ty.pointer("/ctor/name").and_then(|n| n.as_str()) == Some("List"),
        _ => false,
    }
}
/// Whether a type-expr denotes a scalar builtin (a value of fixed, input-independent size).
fn is_scalar_type(ty: &J) -> bool {
    ty.get("kind").and_then(|k| k.as_str()) == Some("builtin")
        && matches!(
            ty.get("name").and_then(|n| n.as_str()),
            Some("int") | Some("nat") | Some("bool") | Some("float") | Some("unit") | Some("string")
        )
}

/// A sound upper bound on the size-degree (in the input size) of the LIST value `node` evaluates to. A
/// `self`-call contributes degree 0 (it is the recurrence's carried term, accounted for separately by the
/// `+1` in [`list_output_size`]); every non-recursive list expression is at most Θ(n) (degree 1), since no
/// first-order builtin expands a list super-linearly. `cons` adds one element (the tail's degree carries);
/// `append`/`reverse` take the max/same; `case` arms are mutually exclusive (max).
fn list_size_degree(node: &J, callee: Option<CalleeComplexity<'_>>) -> u32 {
    list_size_degree_in(node, &BTreeSet::new(), &BTreeSet::new(), callee)
}

/// [`list_size_degree`] with `small` = the list variables a dominating `null(tail^j p)` TRUE arm has
/// shown to hold at most `j` elements (a base case returning its own short input is O(1) material), and
/// `carried` = pattern-bound components of a PRESERVING callee's tuple, each no longer than a distinct
/// recursive result it was applied to (they are the recurrence's carried terms, degree 0, provided each
/// is used at most once — `case butterfly(self ev, self od) of (lo, hi) => append lo hi`).
fn list_size_degree_in(node: &J, small: &BTreeSet<String>, carried: &BTreeSet<String>, callee: Option<CalleeComplexity<'_>>) -> u32 {
    let list_size_degree = |n: &J| list_size_degree_in(n, small, carried, callee);
    if self_call_args(node).is_some() {
        return 0; // the carried recursion term
    }
    match node.get("kind").and_then(|k| k.as_str()) {
        Some("var") => {
            // `nil` is the empty-list constant (degree 0); any other list-typed variable is a Θ(n) list.
            let name = node.get("name").and_then(|n| n.as_str()).unwrap_or("");
            if name == "nil" || small.contains(name) || carried.contains(name) { 0 } else { 1 }
        }
        Some("lit") | Some("list") => 0, // a literal / fixed-size list
        Some("let") => {
            // The body decides the list; the bound name is no longer known small/carried.
            let name = node.get("name").and_then(|n| n.as_str()).unwrap_or("");
            let mut s2 = small.clone();
            let mut c2 = carried.clone();
            s2.remove(name);
            c2.remove(name);
            node.get("body").map(|b| list_size_degree_in(b, &s2, &c2, callee)).unwrap_or(1)
        }
        Some("case") => {
            // `case null(tail^j p) of true => …`: in the TRUE arm `p` has at most j elements.
            let short: Option<String> = node.get("scrutinee").and_then(|s| {
                if head_op(s) != Some("null") {
                    return None;
                }
                let a = s.get("args")?.as_array()?.first()?;
                if a.get("kind")?.as_str()? == "var" {
                    return a.get("name")?.as_str().map(String::from);
                }
                tail_depth(a).map(|(p, _)| p)
            });
            // `case F(self a, self b) of (lo, hi) => …` over a PRESERVING callee: the distinct self-call
            // arguments at F's preserved positions each bound one component.
            let avail: usize = node
                .get("scrutinee")
                .and_then(|s| {
                    let (target, cargs) = crate::terminate::fn_ref_spine(s)?;
                    let positions = callee?.components_preserved_by(&target)?;
                    let mut distinct: Vec<&J> = Vec::new();
                    for i in positions {
                        if let Some(a) = cargs.get(i) {
                            if self_call_args(a).is_some() && !distinct.contains(&a) {
                                distinct.push(a);
                            }
                        }
                    }
                    Some(distinct.len())
                })
                .unwrap_or(0);
            node.get("arms")
                .and_then(|a| a.as_array())
                .map(|a| {
                    a.iter()
                        .map(|arm| {
                            let is_true = arm.pointer("/pattern/value/value").and_then(|v| v.as_bool()) == Some(true);
                            let mut inner = small.clone();
                            if let (true, Some(p)) = (is_true, &short) {
                                inner.insert(p.clone());
                            }
                            let mut c2 = carried.clone();
                            let body = arm.get("body");
                            if avail > 0 && arm.pointer("/pattern/kind").and_then(|x| x.as_str()) == Some("tuple") {
                                let mut names = Vec::new();
                                collect_binds(arm.get("pattern").unwrap(), &mut names);
                                let uses: Vec<u32> = names.iter().map(|n| body.map(|b| count_uses(b, n)).unwrap_or(0)).collect();
                                let used = uses.iter().filter(|u| **u >= 1).count();
                                if uses.iter().all(|u| *u <= 1) && used <= avail {
                                    c2.extend(names);
                                }
                            }
                            body.map(|b| list_size_degree_in(b, &inner, &c2, callee)).unwrap_or(0)
                        })
                        .max()
                        .unwrap_or(0)
                })
                .unwrap_or(0)
        }
        Some("app") => match app_spine(node) {
            Some((op, sargs)) => match op.as_str() {
                "cons" => sargs.get(1).map(list_size_degree).unwrap_or(0),
                "append" => sargs.iter().map(list_size_degree).max().unwrap_or(0),
                "reverse" => sargs.first().map(list_size_degree).unwrap_or(0),
                // A first-order builtin returning a scalar (head/length/…) can't appear in list position
                // other than as an already-handled sub-part; conservatively bound anything else at Θ(n).
                _ => 1,
            },
            None => 1,
        },
        _ => 1,
    }
}

/// Whether every `self`-call in `inner` descends `tail^k`/`sub(p,c)` (a **constant** step) on ONE fixed
/// parameter — the shape for which the output-size recurrence `S(n) = S(n−1) + chunk(n)` holds. A halving
/// descent or a mixed/non-descending recursion returns `false` (the caller then reports `Unknown`).
fn single_param_constant_recursion(descents: &[Vec<(String, Descent)>], params: &[String]) -> bool {
    matches!(choose_measure(descents, params), Some((_, kinds)) if kinds.iter().all(|k| *k == Descent::Constant))
}

/// The one parameter EVERY self-call descends (the first such in parameter order), with the descent kind
/// each call establishes for it. `None` when no parameter is common to all calls.
fn choose_measure(descents: &[Vec<(String, Descent)>], params: &[String]) -> Option<(String, Vec<Descent>)> {
    if descents.is_empty() {
        return None;
    }
    params.iter().find_map(|p| {
        let kinds: Option<Vec<Descent>> = descents
            .iter()
            .map(|d| d.iter().find(|(q, _)| q == p).map(|(_, k)| *k))
            .collect();
        kinds.map(|ks| (p.clone(), ks))
    })
}

/// Infer a sound upper bound on the size of a **list-returning** body's result.
fn list_output_size_linked(params: &[String], inner: &J, callee: Option<CalleeComplexity<'_>>) -> OutputSize {
    // The opaque check here asks only whether every applied callee is RESOLVED (its result size is then
    // read as linear in its size-bounded arguments); a self result inside is fine for a size analysis.
    if find_opaque_in(inner, callee, &params.iter().cloned().collect(), true).is_some() {
        return OutputSize::Unknown; // a map/filter/fold/opaque build — can't see the result's size
    }
    let mut descents: Vec<Vec<(String, Descent)>> = Vec::new();
    collect_self_descents(inner, &[], &BTreeMap::new(), callee, params, &mut descents);
    if descents.is_empty() {
        // Non-recursive list expression: at most Θ(n) (Const for nil/literal).
        return OutputSize::from_degree(list_size_degree(inner, callee));
    }
    if !single_param_constant_recursion(&descents, params) {
        // A HALVING recursion that conses O(1) material per level builds Θ(log n) ≤ Θ(n) elements
        // (`digits`: one digit per halving) — Linear is the sound coarse class. Anything else
        // (mixed / non-structural / expanding per level) is outside the S(n)=S(n−1)+chunk shape.
        // A HALVING recursion that conses O(1) material per level (`digits`) builds Θ(log n); a TWO-WAY
        // halving recursion that only appends its two results (`ntt_rec`) builds S(n) = 2S(n/2) + O(1) =
        // Θ(n). Both are Linear, the sound coarse class.
        let halving = matches!(choose_measure(&descents, params), Some((_, kinds)) if kinds.iter().all(|k| *k == Descent::Halving));
        if halving && path_self_count(inner) <= 2 && list_size_degree(inner, callee) == 0 {
            return OutputSize::Linear;
        }
        return OutputSize::Unknown;
    }
    if path_self_count(inner) >= 2 {
        return OutputSize::Unknown; // two builds appended per step could expand super-polynomially
    }
    // S(n) = S(n−1) + chunk(n): the per-step non-recursive list material has degree `list_size_degree`
    // (self-calls count 0), and summing it over the n levels adds one to the degree.
    OutputSize::from_degree(list_size_degree(inner, callee).saturating_add(1))
}

/// Infer a sound upper bound on how a function's result size grows with its input, given its declared
/// result type and body. A scalar result is `Const`; a concrete `List` result is analyzed structurally; a
/// polymorphic (type-variable) or otherwise-unrecognized result is `Unknown` (its size could be as large
/// as the input — never claimed smaller).
pub fn analyze_output_size(result_ty: &J, body: &J) -> OutputSize {
    analyze_output_size_linked(result_ty, body, None)
}

/// [`analyze_output_size`] over a linked view (an applied callee's result reads as linear in its
/// size-bounded arguments).
pub fn analyze_output_size_linked(result_ty: &J, body: &J, callee: Option<CalleeComplexity<'_>>) -> OutputSize {
    // A `ref` to a canonical builtin type artifact IS the builtin (the v0.2 fold) — normalize so
    // e.g. a ref-spelled scalar result is recognized as `Const` rather than falling to `Unknown`.
    let result_ty = &crate::fold_canonical_type_refs(result_ty);
    let Some(params) = lambda_params(body) else { return OutputSize::Unknown };
    let Some(inner) = body.get("body") else { return OutputSize::Unknown };
    if is_scalar_type(result_ty) {
        OutputSize::Const
    } else if is_list_type(result_ty) {
        list_output_size_linked(&params, inner, callee)
    } else {
        OutputSize::Unknown
    }
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
    fn self_call(args: Vec<J>) -> J {
        json!({ "kind": "app", "op": "self", "args": args })
    }
    /// `case cond of true -> a | false -> b`.
    fn case2(cond: J, a: J, b: J) -> J {
        json!({
            "kind": "case",
            "scrutinee": cond,
            "arms": [
                { "pattern": lit_b(true), "body": a },
                { "pattern": lit_b(false), "body": b }
            ]
        })
    }

    fn bound(body: &J) -> Class {
        match analyze_complexity(body) {
            ComplexityOutcome::Bound(c) => c,
            ComplexityOutcome::Opaque(why) => panic!("expected a bound, got Opaque: {why}"),
        }
    }

    // ---- the class type + parser ----

    #[test]
    fn parse_and_order_classes() {
        assert_eq!(parse_class("O(1)"), Some(Class::CONST));
        assert_eq!(parse_class("O(log n)"), Some(Class::LOG));
        assert_eq!(parse_class("O(n)"), Some(Class::poly(1)));
        assert_eq!(parse_class("O(n log n)"), Some(Class { deg: 1, logs: 1, exp: false }));
        assert_eq!(parse_class("O(n^2)"), Some(Class::poly(2)));
        assert_eq!(parse_class("O(n^2 log n)"), Some(Class { deg: 2, logs: 1, exp: false }));
        assert_eq!(parse_class("O(n^3)"), Some(Class::poly(3)));
        assert_eq!(parse_class("O(2^n)"), Some(Class::EXP));
        assert_eq!(parse_class("O(n!)"), Some(Class::EXP));
        assert_eq!(parse_class("O(weird)"), None);
        // ordering: 1 < log n < n < n log n < n^2 < 2^n
        assert!(Class::CONST < Class::LOG);
        assert!(Class::LOG < Class::poly(1));
        assert!(Class::poly(1) < Class { deg: 1, logs: 1, exp: false });
        assert!(Class { deg: 1, logs: 1, exp: false } < Class::poly(2));
        assert!(Class::poly(3) < Class::EXP);
    }

    #[test]
    fn class_round_trips_through_display() {
        for s in ["O(1)", "O(log n)", "O(n)", "O(n log n)", "O(n^2)", "O(n^2 log n)", "O(n^3)"] {
            assert_eq!(parse_class(s).unwrap().display(), s, "round-trip {s}");
        }
    }

    // ---- non-recursive ----

    #[test]
    fn constant_scalar_body_is_o1() {
        // \a b -> add(a, b) — only O(1) ops.
        let body = lambda(&["a", "b"], app("add", vec![v("a"), v("b")]));
        assert_eq!(bound(&body), Class::CONST);
    }

    #[test]
    fn nonrecursive_reverse_is_on() {
        // \xs -> reverse(xs) — one linear list op, no recursion.
        let body = lambda(&["xs"], app("reverse", vec![v("xs")]));
        assert_eq!(bound(&body), Class::poly(1));
    }

    // ---- linear recursion ----

    #[test]
    fn length_recursion_is_on() {
        // length: \xs -> case null xs of true -> 0 | false -> add(1, self(tail xs)).
        let step = app("add", vec![lit_i(1), self_call(vec![app("tail", vec![v("xs")])])]);
        let body = lambda(&["xs"], case2(app("null", vec![v("xs")]), lit_i(0), step));
        assert_eq!(bound(&body), Class::poly(1));
    }

    #[test]
    fn factorial_numeric_recursion_is_on() {
        // factorial: \n -> case le(n,1) of true -> 1 | false -> mul(n, self(sub(n,1))).
        let step = app("mul", vec![v("n"), self_call(vec![app("sub", vec![v("n"), lit_i(1)])])]);
        let body = lambda(&["n"], case2(app("le", vec![v("n"), lit_i(1)]), lit_i(1), step));
        assert_eq!(bound(&body), Class::poly(1));
    }

    #[test]
    fn filter_two_arm_recursion_stays_on() {
        // filter-like: exactly ONE self-call fires per call, but it is written in BOTH arms. The textual
        // count is 2; the path count is 1, so this must stay O(n), not exponential.
        let keep = app("cons", vec![app("head", vec![v("xs")]), self_call(vec![app("tail", vec![v("xs")])])]);
        let drop = self_call(vec![app("tail", vec![v("xs")])]);
        let body = lambda(&["xs"], case2(app("null", vec![v("xs")]), json!({ "kind": "var", "name": "xs" }), case2(app("head", vec![v("xs")]), keep, drop)));
        assert_eq!(bound(&body), Class::poly(1));
    }

    // ---- quadratic recursion ----

    #[test]
    fn naive_reverse_via_append_is_on2() {
        // \xs -> case null xs of true -> nil | false -> append(self(tail xs), cons(head xs, nil)).
        let step = app(
            "append",
            vec![
                self_call(vec![app("tail", vec![v("xs")])]),
                app("cons", vec![app("head", vec![v("xs")]), v("nil")]),
            ],
        );
        let body = lambda(&["xs"], case2(app("null", vec![v("xs")]), v("nil"), step));
        assert_eq!(bound(&body), Class::poly(2), "one self-call with O(n) append work per step → O(n^2)");
    }

    // ---- exponential recursion ----

    #[test]
    fn naive_fib_is_exponential() {
        // \n -> case le(n,1) of true -> n | false -> add(self(sub(n,1)), self(sub(n,2))).
        let step = app(
            "add",
            vec![
                self_call(vec![app("sub", vec![v("n"), lit_i(1)])]),
                self_call(vec![app("sub", vec![v("n"), lit_i(2)])]),
            ],
        );
        let body = lambda(&["n"], case2(app("le", vec![v("n"), lit_i(1)]), v("n"), step));
        assert_eq!(bound(&body), Class::EXP, "two self-calls on a constant descent → exponential");
    }

    // ---- halving recursion ----

    /// ntt_split: \xs -> case null(xs) of true -> (nil, nil) | false -> case null(tail xs) of
    ///   true -> (cons(head xs, nil), nil) | false -> case self(tail(tail xs)) of (ev, od) -> (cons(head xs, ev), cons(head(tail xs), od))
    fn split_body(second_cons: bool) -> J {
        let hd = app("head", vec![v("xs")]);
        let hd2 = app("head", vec![app("tail", vec![v("xs")])]);
        let rec = self_call(vec![app("tail", vec![app("tail", vec![v("xs")])])]);
        let od_elem = if second_cons { app("cons", vec![hd2.clone(), app("cons", vec![hd2.clone(), v("od")])]) } else { app("cons", vec![hd2, v("od")]) };
        let step = json!({ "kind": "case", "scrutinee": rec, "arms": [{
            "pattern": { "kind": "tuple", "elems": [{ "kind": "bind", "name": "ev" }, { "kind": "bind", "name": "od" }] },
            "body": { "kind": "tuple", "elems": [app("cons", vec![hd.clone(), v("ev")]), od_elem] } }] });
        let one = json!({ "kind": "tuple", "elems": [app("cons", vec![hd, v("nil")]), v("nil")] });
        let empty = json!({ "kind": "tuple", "elems": [v("nil"), v("nil")] });
        lambda(&["xs"], case2(app("null", vec![v("xs")]), empty, case2(app("null", vec![app("tail", vec![v("xs")])]), one, step)))
    }

    /// Every callee has `class`; `poly_result` answers `result_poly_bounded`.
    struct Magnitude {
        class: Class,
        poly_result: bool,
    }
    impl LinkedCallees for Magnitude {
        fn class_of(&self, _: &str) -> Option<Class> {
            Some(self.class)
        }
        fn result_bounded_by(&self, _: &str) -> Option<Vec<usize>> {
            None
        }
        fn components_shrink_by(&self, _: &str) -> Option<u32> {
            None
        }
        fn components_preserved_by(&self, _: &str) -> Option<Vec<usize>> {
            None
        }
        fn result_poly_bounded(&self, _: &str) -> bool {
            self.poly_result
        }
    }

    #[test]
    fn a_polylog_callee_folds_over_products_and_polynomial_callee_results_a_linear_one_does_not() {
        let fref = |s: &str| json!({ "kind": "lit", "value": { "kind": "fn_ref", "target": format!("fn_{}", s.repeat(32)) } });
        let call = |s: &str, args: Vec<J>| json!({ "kind": "app", "fn": fref(s), "args": args });
        // normalize(a·d, b·d): a product of two inputs as the argument.
        let product = lambda(&["a", "b", "d"], call("ab", vec![app("mul", vec![v("a"), v("d")]), app("mul", vec![v("b"), v("d")])]));
        let log = Magnitude { class: Class::LOG, poly_result: false };
        assert_eq!(analyze_complexity_linked(&product, Some(&log)), ComplexityOutcome::Bound(Class::LOG));
        // O(m) of m = a·d is O(n²) in ours: still refused for a polynomial callee.
        let linear = Magnitude { class: Class::poly(1), poly_result: true };
        assert!(matches!(analyze_complexity_linked(&product, Some(&linear)), ComplexityOutcome::Opaque(_)));
        // case G(a, b) of (p, q) => F(p, q): G's components are polynomial only if G's result is.
        let comp = lambda(&["a", "b"], json!({ "kind": "case", "scrutinee": call("cd", vec![v("a"), v("b")]), "arms": [{
            "pattern": { "kind": "tuple", "elems": [{ "kind": "bind", "name": "p" }, { "kind": "bind", "name": "q" }] },
            "body": call("ab", vec![v("p"), v("q")]) }] }));
        let poly = Magnitude { class: Class::LOG, poly_result: true };
        assert_eq!(analyze_complexity_linked(&comp, Some(&poly)), ComplexityOutcome::Bound(Class::LOG));
        assert!(matches!(analyze_complexity_linked(&comp, Some(&log)), ComplexityOutcome::Opaque(_)));
    }

    #[test]
    fn a_result_is_polynomial_through_div_by_anything_and_mod_by_a_bounded_divisor_only() {
        let none = Magnitude { class: Class::LOG, poly_result: false };
        let fref = json!({ "kind": "lit", "value": { "kind": "fn_ref", "target": format!("fn_{}", "ab".repeat(32)) } });
        let opaque = json!({ "kind": "app", "fn": fref, "args": [v("x"), v("y")] });
        // (x·y div g(x, y), x mod y): the quotient is bounded by its dividend whatever the divisor.
        let ok = lambda(&["x", "y"], json!({ "kind": "tuple", "elems": [
            app("div", vec![app("mul", vec![v("x"), v("y")]), opaque.clone()]), app("mod", vec![v("x"), v("y")])] }));
        assert!(result_poly_bounded(&ok, Some(&none)));
        // x mod g(x, y) is bounded only by the (unknown) divisor — not by x (Euclidean −1 mod 5 = 4).
        let bad = lambda(&["x", "y"], app("mod", vec![v("x"), opaque]));
        assert!(!result_poly_bounded(&bad, Some(&none)));
        // A recursive result is never polynomial by structure.
        let rec = lambda(&["x"], self_call(vec![app("sub", vec![v("x"), lit_i(1)])]));
        assert!(!result_poly_bounded(&rec, Some(&none)));
    }

    struct Shrinks;
    impl LinkedCallees for Shrinks {
        fn class_of(&self, _: &str) -> Option<Class> {
            Some(Class::poly(1))
        }
        fn result_bounded_by(&self, _: &str) -> Option<Vec<usize>> {
            None
        }
        fn components_shrink_by(&self, _: &str) -> Option<u32> {
            Some(2)
        }
        fn components_preserved_by(&self, _: &str) -> Option<Vec<usize>> {
            Some(vec![0, 1])
        }
    }

    /// ntt_butterfly: \e o wk w -> case null e of true -> (nil, nil) | false -> case null o of true -> (nil, nil)
    ///   | false -> case self(tail e, tail o, wk, w) of (lo, hi) -> (cons(head e, lo), cons(head o, hi))
    fn butterfly_body(double_cons: bool) -> J {
        let rec = self_call(vec![app("tail", vec![v("e")]), app("tail", vec![v("o")]), v("wk"), v("w")]);
        let lo = app("cons", vec![app("head", vec![v("e")]), v("lo")]);
        let hi = if double_cons {
            app("cons", vec![app("head", vec![v("o")]), app("cons", vec![app("head", vec![v("o")]), v("hi")])])
        } else {
            app("cons", vec![app("head", vec![v("o")]), v("hi")])
        };
        let step = json!({ "kind": "case", "scrutinee": rec, "arms": [{
            "pattern": { "kind": "tuple", "elems": [{ "kind": "bind", "name": "lo" }, { "kind": "bind", "name": "hi" }] },
            "body": { "kind": "tuple", "elems": [lo, hi] } }] });
        let empty = json!({ "kind": "tuple", "elems": [v("nil"), v("nil")] });
        lambda(&["e", "o", "wk", "w"], case2(app("null", vec![v("e")]), empty.clone(), case2(app("null", vec![v("o")]), empty, step)))
    }

    #[test]
    fn a_tail_recursion_with_one_cons_per_component_preserves_its_list_inputs() {
        assert_eq!(component_preserve(&butterfly_body(false)), Some(vec![0, 1]));
        // Two conses per step onto one component: that component may outgrow the input — refused.
        assert_eq!(component_preserve(&butterfly_body(true)), None);
        // The deinterleave preserves (and shrinks) its one list input.
        assert_eq!(component_preserve(&split_body(false)), Some(vec![0]));
        // Two conses per TWO tail steps still preserves (it just no longer shrinks).
        assert_eq!(component_preserve(&split_body(true)), Some(vec![0]));
    }

    #[test]
    fn appending_a_preserving_callees_components_of_two_halves_is_a_linear_output() {
        // ntt_rec's shape: case split(xs) of (ev, od) => case F(self ev, self od) of (lo, hi) => append lo hi.
        let fref = |s: &str| json!({ "kind": "lit", "value": { "kind": "fn_ref", "target": format!("fn_{}", s.repeat(32)) } });
        let list_int = json!({ "kind": "apply", "ctor": { "kind": "builtin", "name": "List" }, "args": [{ "kind": "builtin", "name": "int" }] });
        let shape = |result: J| {
            let f = json!({ "kind": "app", "fn": fref("ef"), "args": [self_call(vec![v("ev")]), self_call(vec![v("od")])] });
            let inner = json!({ "kind": "case", "scrutinee": f, "arms": [{
                "pattern": { "kind": "tuple", "elems": [{ "kind": "bind", "name": "lo" }, { "kind": "bind", "name": "hi" }] },
                "body": result }] });
            let split = json!({ "kind": "app", "fn": fref("cd"), "args": [v("xs")] });
            let rec = json!({ "kind": "case", "scrutinee": split, "arms": [{
                "pattern": { "kind": "tuple", "elems": [{ "kind": "bind", "name": "ev" }, { "kind": "bind", "name": "od" }] },
                "body": inner }] });
            lambda(&["xs"], case2(app("null", vec![app("tail", vec![v("xs")])]), v("xs"), rec))
        };
        let good = shape(app("append", vec![v("lo"), v("hi")]));
        assert_eq!(analyze_output_size_linked(&list_int, &good, Some(&Shrinks)), OutputSize::Linear);
        match analyze_complexity_typed(&good, Some(&list_int), Some(&Shrinks)) {
            ComplexityOutcome::Bound(c) => assert_eq!(c, Class { deg: 1, logs: 1, exp: false }),
            other => panic!("{other:?}"),
        }
        // Using one component twice doubles the material: S(n) = 2S(n/2) + S(n/2)… — not linear, Unknown.
        let twice = shape(app("append", vec![app("append", vec![v("lo"), v("lo")]), v("hi")]));
        assert_eq!(analyze_output_size_linked(&list_int, &twice, Some(&Shrinks)), OutputSize::Unknown);
        // Without the view nothing is known about F's components.
        assert_eq!(analyze_output_size_linked(&list_int, &good, None), OutputSize::Unknown);
    }

    #[test]
    fn a_tail2_split_shrinks_its_components_and_drives_a_halving_descent() {
        // The deinterleave: each component gets ONE cons per two tail steps — strictly shorter for n ≥ 2.
        assert_eq!(component_shrink(&split_body(false)), Some(2));
        // Two conses onto one component per step: that component may be as long as the input — refused.
        assert_eq!(component_shrink(&split_body(true)), None);
        // A caller recursing on the components of a shrinking callee (linked view): T(n) = 2T(n/2) + O(n).
        let callee = json!({ "kind": "lit", "value": { "kind": "fn_ref", "target": format!("fn_{}", "cd".repeat(32)) } });
        let split = json!({ "kind": "app", "fn": callee, "args": [v("xs")] });
        let rec = json!({ "kind": "case", "scrutinee": split, "arms": [{
            "pattern": { "kind": "tuple", "elems": [{ "kind": "bind", "name": "ev" }, { "kind": "bind", "name": "od" }] },
            "body": app("append", vec![self_call(vec![v("ev")]), self_call(vec![v("od")])]) }] });
        let body = lambda(&["xs"], case2(app("null", vec![app("tail", vec![v("xs")])]), v("xs"), rec));
        match analyze_complexity_linked(&body, Some(&Shrinks)) {
            ComplexityOutcome::Bound(c) => assert_eq!(c, Class { deg: 1, logs: 1, exp: false }),
            other => panic!("{other:?}"),
        }
        // Without the view the components are opaque.
        assert!(matches!(analyze_complexity(&body), ComplexityOutcome::Opaque(_)));
    }

    #[test]
    fn a_callee_applied_to_a_recursive_result_folds_when_the_own_output_is_linear() {
        // \xs -> case null(tail xs) of true -> xs | false -> case split(xs) of (ev, od) -> cons(g(self ev), self od)
        // The output is a two-way halving build of O(1) material per level: S(n) = 2S(n/2) + O(1) = Θ(n),
        // so the recursive result `self ev` is size-bounded and `g`'s O(m) reads as O(n): T = 2T(n/2) + O(n).
        let split_ref = json!({ "kind": "lit", "value": { "kind": "fn_ref", "target": format!("fn_{}", "cd".repeat(32)) } });
        let g_ref = json!({ "kind": "lit", "value": { "kind": "fn_ref", "target": format!("fn_{}", "ef".repeat(32)) } });
        let split = json!({ "kind": "app", "fn": split_ref, "args": [v("xs")] });
        let g_of_rec = json!({ "kind": "app", "fn": g_ref, "args": [self_call(vec![v("ev")])] });
        let rec = json!({ "kind": "case", "scrutinee": split, "arms": [{
            "pattern": { "kind": "tuple", "elems": [{ "kind": "bind", "name": "ev" }, { "kind": "bind", "name": "od" }] },
            "body": app("cons", vec![g_of_rec, self_call(vec![v("od")])]) }] });
        let body = lambda(&["xs"], case2(app("null", vec![app("tail", vec![v("xs")])]), v("xs"), rec));
        let list_int = json!({ "kind": "apply", "ctor": { "kind": "builtin", "name": "List" }, "args": [{ "kind": "builtin", "name": "int" }] });
        assert_eq!(analyze_output_size_linked(&list_int, &body, Some(&Shrinks)), OutputSize::Linear);
        match analyze_complexity_typed(&body, Some(&list_int), Some(&Shrinks)) {
            ComplexityOutcome::Bound(c) => assert_eq!(c, Class { deg: 1, logs: 1, exp: false }),
            other => panic!("{other:?}"),
        }
        // Without the declared result type nothing bounds `self ev`: the fold refuses.
        match analyze_complexity_linked(&body, Some(&Shrinks)) {
            ComplexityOutcome::Opaque(why) => assert!(why.contains("not bounded"), "{why}"),
            other => panic!("{other:?}"),
        }
        // A callee's output (degree-1 material) appended per level is NOT a linear build — the output
        // stays Unknown and the fold still refuses with the type.
        let rec2 = json!({ "kind": "case", "scrutinee": json!({ "kind": "app", "fn": json!({ "kind": "lit", "value": { "kind": "fn_ref", "target": format!("fn_{}", "cd".repeat(32)) } }), "args": [v("xs")] }), "arms": [{
            "pattern": { "kind": "tuple", "elems": [{ "kind": "bind", "name": "ev" }, { "kind": "bind", "name": "od" }] },
            "body": app("append", vec![json!({ "kind": "app", "fn": json!({ "kind": "lit", "value": { "kind": "fn_ref", "target": format!("fn_{}", "ef".repeat(32)) } }), "args": [self_call(vec![v("ev")])] }), self_call(vec![v("od")])]) }] });
        let body2 = lambda(&["xs"], case2(app("null", vec![app("tail", vec![v("xs")])]), v("xs"), rec2));
        assert_eq!(analyze_output_size_linked(&list_int, &body2, Some(&Shrinks)), OutputSize::Unknown);
        assert!(matches!(analyze_complexity_typed(&body2, Some(&list_int), Some(&Shrinks)), ComplexityOutcome::Opaque(_)));
    }

    #[test]
    fn euclid_mod_recursion_is_log() {
        // gcd: \a b -> case eq(b,0) of true -> a | false -> self(b, mod(a,b)) — Lamé: Θ(log n) steps, O(1) each.
        let step = self_call(vec![v("b"), app("mod", vec![v("a"), v("b")])]);
        let body = lambda(&["a", "b"], case2(app("eq", vec![v("b"), lit_i(0)]), v("a"), step));
        assert_eq!(bound(&body), Class::LOG);
    }

    #[test]
    fn ceiling_halving_recursion_is_log() {
        // next_pow2: \n -> case lt(n,2) of true -> 1 | false -> mul(2, self(div(add(n,1), 2))).
        let step = app("mul", vec![lit_i(2), self_call(vec![app("div", vec![app("add", vec![v("n"), lit_i(1)]), lit_i(2)])])]);
        let body = lambda(&["n"], case2(app("lt", vec![v("n"), lit_i(2)]), lit_i(1), step));
        assert_eq!(bound(&body), Class::LOG);
    }

    #[test]
    fn halving_recursion_is_log() {
        // \n -> case le(n,1) of true -> 0 | false -> add(1, self(div(n,2))) — binary-search depth.
        let step = app("add", vec![lit_i(1), self_call(vec![app("div", vec![v("n"), lit_i(2)])])]);
        let body = lambda(&["n"], case2(app("le", vec![v("n"), lit_i(1)]), lit_i(0), step));
        assert_eq!(bound(&body), Class::LOG);
    }

    // ---- opaque / unrecognized ----

    #[test]
    fn higher_order_body_is_opaque() {
        // \f xs -> map(f, xs): map's cost depends on the opaque f.
        let body = lambda(&["f", "xs"], app("map", vec![v("f"), v("xs")]));
        assert!(matches!(analyze_complexity(&body), ComplexityOutcome::Opaque(_)));
    }

    #[test]
    fn non_descending_recursion_is_opaque() {
        // self(xs) — no descent; not provably well-founded, so we cannot bound its cost.
        let step = app("add", vec![lit_i(1), self_call(vec![v("xs")])]);
        let body = lambda(&["xs"], case2(app("null", vec![v("xs")]), lit_i(0), step));
        assert!(matches!(analyze_complexity(&body), ComplexityOutcome::Opaque(_)));
    }

    // ---- output-size analysis (for `cost.output_size`) ----

    fn int_t() -> J {
        json!({ "kind": "builtin", "name": "int" })
    }
    fn list_int_t() -> J {
        json!({ "kind": "apply", "ctor": { "kind": "builtin", "name": "List" }, "args": [{ "kind": "builtin", "name": "int" }] })
    }
    fn nil() -> J {
        json!({ "kind": "var", "name": "nil" })
    }

    #[test]
    fn output_size_parse_and_degree() {
        assert_eq!(parse_output_size("constant"), OutputSize::Const);
        assert_eq!(parse_output_size("preserving"), OutputSize::Linear);
        assert_eq!(parse_output_size("bounded"), OutputSize::Linear);
        assert_eq!(parse_output_size("quadratic"), OutputSize::Quadratic);
        assert_eq!(parse_output_size("cubic"), OutputSize::Cubic);
        assert_eq!(parse_output_size("unknown"), OutputSize::Unknown);
        assert_eq!(OutputSize::Const.degree(), Some(0));
        assert_eq!(OutputSize::Quadratic.degree(), Some(2));
        assert_eq!(OutputSize::Unknown.degree(), None);
    }

    #[test]
    fn scalar_result_is_constant_output() {
        // A recursive `length : List int -> int` returns a scalar → constant output size.
        let step = app("add", vec![lit_i(1), self_call(vec![app("tail", vec![v("xs")])])]);
        let body = lambda(&["xs"], case2(app("null", vec![v("xs")]), lit_i(0), step));
        assert_eq!(analyze_output_size(&int_t(), &body), OutputSize::Const);
    }

    #[test]
    fn map_build_is_size_preserving() {
        // \xs -> case null xs of true -> nil | false -> cons(mul(2,head xs), self(tail xs)) — Θ(n) output.
        let step = app("cons", vec![app("mul", vec![lit_i(2), app("head", vec![v("xs")])]), self_call(vec![app("tail", vec![v("xs")])])]);
        let body = lambda(&["xs"], case2(app("null", vec![v("xs")]), nil(), step));
        assert_eq!(analyze_output_size(&list_int_t(), &body), OutputSize::Linear);
    }

    #[test]
    fn naive_reverse_is_size_preserving_not_quadratic() {
        // append(self(tail), cons(head, nil)): O(n^2) TIME but Θ(n) OUTPUT SIZE — the two are independent.
        let step = app("append", vec![self_call(vec![app("tail", vec![v("xs")])]), app("cons", vec![app("head", vec![v("xs")]), nil()])]);
        let body = lambda(&["xs"], case2(app("null", vec![v("xs")]), nil(), step));
        assert_eq!(analyze_output_size(&list_int_t(), &body), OutputSize::Linear);
        // (and its TIME is quadratic, confirming the two analyses are distinct)
        assert_eq!(bound(&body), Class::poly(2));
    }

    #[test]
    fn appending_a_whole_param_each_step_is_quadratic_output() {
        // \xs -> case null xs of true -> nil | false -> append(xs, self(tail xs)) — adds |xs|=Θ(n) per step.
        let step = app("append", vec![v("xs"), self_call(vec![app("tail", vec![v("xs")])])]);
        let body = lambda(&["xs"], case2(app("null", vec![v("xs")]), nil(), step));
        assert_eq!(analyze_output_size(&list_int_t(), &body), OutputSize::Quadratic);
    }

    #[test]
    fn nonrecursive_append_is_linear_output() {
        // \xs ys -> append(xs, ys) — Θ(n) output, no recursion.
        let body = lambda(&["xs", "ys"], app("append", vec![v("xs"), v("ys")]));
        assert_eq!(analyze_output_size(&list_int_t(), &body), OutputSize::Linear);
    }

    #[test]
    fn opaque_list_build_is_unknown_output() {
        // \f xs -> map(f, xs): the result's size is unseen through the opaque map.
        let body = lambda(&["f", "xs"], app("map", vec![v("f"), v("xs")]));
        assert_eq!(analyze_output_size(&list_int_t(), &body), OutputSize::Unknown);
    }
}
