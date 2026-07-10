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
    if lhs.get("kind").and_then(|k| k.as_str()) != Some("var") {
        return None;
    }
    let p = lhs.get("name").and_then(|n| n.as_str())?.to_string();
    let c = rhs.pointer("/value/value").and_then(|v| v.as_i64())?;
    match op {
        "sub" if c >= 1 => Some((p, Descent::Constant)),
        "div" if c >= 2 => Some((p, Descent::Halving)),
        _ => None,
    }
}

/// The descent of a single recursion argument, if it is a recognized strict decrease of a parameter.
fn arg_descent(arg: &J) -> Option<(String, Descent)> {
    tail_descent(arg).or_else(|| numeric_descent(arg))
}

/// The first opaque (higher-order / unrecognized-application) reason in `node`, if any. A `self`-call and a
/// recognized first-order builtin application are fine; anything else applied is opaque.
fn find_opaque(node: &J) -> Option<String> {
    if self_call_args(node).is_some() {
        // still descend into the call's arguments below
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
    match node {
        J::Object(m) => {
            for (k, v) in m {
                if k == "pattern" {
                    continue;
                }
                if let Some(why) = find_opaque(v) {
                    return Some(why);
                }
            }
            None
        }
        J::Array(items) => items.iter().find_map(find_opaque),
        _ => None,
    }
}

/// Collect every `self`-call's descent (of its descending argument). A `None` entry marks a self-call whose
/// arguments contain no recognized descent — the recursion is not provably well-founded.
fn collect_self_descents(node: &J, out: &mut Vec<Option<(String, Descent)>>) {
    if let Some(sargs) = self_call_args(node) {
        // The descending argument is whichever one strictly decreases a parameter (append-style spectators
        // don't, so the first descending arg is the recursion position).
        out.push(sargs.iter().find_map(arg_descent));
        for a in &sargs {
            collect_self_descents(a, out);
        }
        return;
    }
    match node {
        J::Object(m) => {
            for (k, v) in m {
                if k == "pattern" {
                    continue;
                }
                collect_self_descents(v, out);
            }
        }
        J::Array(items) => items.iter().for_each(|v| collect_self_descents(v, out)),
        _ => {}
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
fn path_work(node: &J) -> Class {
    if let Some(sargs) = self_call_args(node) {
        return sargs.iter().map(path_work).max().unwrap_or(Class::CONST);
    }
    if node.get("kind").and_then(|k| k.as_str()) == Some("case") {
        let scrut = node.get("scrutinee").map(path_work).unwrap_or(Class::CONST);
        let arms = node
            .get("arms")
            .and_then(|a| a.as_array())
            .map(|a| a.iter().map(|arm| arm.get("body").map(path_work).unwrap_or(Class::CONST)).max().unwrap_or(Class::CONST))
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
        J::Object(m) => m.iter().filter(|(k, _)| *k != "pattern").map(|(_, v)| path_work(v)).max().unwrap_or(Class::CONST),
        J::Array(items) => items.iter().map(path_work).max().unwrap_or(Class::CONST),
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
    let Some(params) = lambda_params(body) else {
        return ComplexityOutcome::Opaque("body is not a `lambda`".into());
    };
    let Some(inner) = body.get("body") else {
        return ComplexityOutcome::Opaque("lambda has no body".into());
    };
    if let Some(why) = find_opaque(inner) {
        return ComplexityOutcome::Opaque(why);
    }

    let mut descents: Vec<Option<(String, Descent)>> = Vec::new();
    collect_self_descents(inner, &mut descents);

    if descents.is_empty() {
        // Non-recursive over the first-order fragment: constant number of ops on O(n)-bounded data.
        return ComplexityOutcome::Bound(path_work(inner));
    }

    // Every self-call must descend on ONE fixed parameter; the kind is Constant unless every descent halves.
    let mut on: Option<String> = None;
    let mut any_constant = false;
    let mut any_halving = false;
    for d in &descents {
        match d {
            Some((p, kind)) if params.contains(p) => {
                match &on {
                    None => on = Some(p.clone()),
                    Some(prev) if prev == p => {}
                    Some(_) => {
                        return ComplexityOutcome::Opaque(
                            "self-calls descend on different parameters (no single measure)".into(),
                        )
                    }
                }
                match kind {
                    Descent::Constant => any_constant = true,
                    Descent::Halving => any_halving = true,
                }
            }
            _ => {
                return ComplexityOutcome::Opaque(
                    "a self-call's recursion argument is not a recognized structural/numeric descent".into(),
                )
            }
        }
    }
    // Mixed constant+halving descents on the same parameter: the constant step drives the depth (Θ(n)).
    let kind = if any_constant || !any_halving { Descent::Constant } else { Descent::Halving };
    let branch = path_self_count(inner).max(1);
    let work = path_work(inner);
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
fn is_list_type(ty: &J) -> bool {
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
fn list_size_degree(node: &J) -> u32 {
    if self_call_args(node).is_some() {
        return 0; // the carried recursion term
    }
    match node.get("kind").and_then(|k| k.as_str()) {
        Some("var") => {
            // `nil` is the empty-list constant (degree 0); any other list-typed variable is a Θ(n) list.
            if node.get("name").and_then(|n| n.as_str()) == Some("nil") { 0 } else { 1 }
        }
        Some("lit") | Some("list") => 0, // a literal / fixed-size list
        Some("case") => node
            .get("arms")
            .and_then(|a| a.as_array())
            .map(|a| a.iter().map(|arm| arm.get("body").map(list_size_degree).unwrap_or(0)).max().unwrap_or(0))
            .unwrap_or(0),
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
fn single_param_constant_recursion(descents: &[Option<(String, Descent)>], params: &[String]) -> bool {
    let mut on: Option<&str> = None;
    for d in descents {
        match d {
            Some((p, Descent::Constant)) if params.contains(p) => match on {
                None => on = Some(p),
                Some(prev) if prev == p => {}
                Some(_) => return false,
            },
            _ => return false,
        }
    }
    true
}

/// Infer a sound upper bound on the size of a **list-returning** body's result.
fn list_output_size(params: &[String], inner: &J) -> OutputSize {
    if find_opaque(inner).is_some() {
        return OutputSize::Unknown; // a map/filter/fold/opaque build — can't see the result's size
    }
    let mut descents: Vec<Option<(String, Descent)>> = Vec::new();
    collect_self_descents(inner, &mut descents);
    if descents.is_empty() {
        // Non-recursive list expression: at most Θ(n) (Const for nil/literal).
        return OutputSize::from_degree(list_size_degree(inner));
    }
    if !single_param_constant_recursion(&descents, params) {
        return OutputSize::Unknown; // halving / mixed / non-structural — outside the S(n)=S(n−1)+chunk shape
    }
    if path_self_count(inner) >= 2 {
        return OutputSize::Unknown; // two builds appended per step could expand super-polynomially
    }
    // S(n) = S(n−1) + chunk(n): the per-step non-recursive list material has degree `list_size_degree`
    // (self-calls count 0), and summing it over the n levels adds one to the degree.
    OutputSize::from_degree(list_size_degree(inner).saturating_add(1))
}

/// Infer a sound upper bound on how a function's result size grows with its input, given its declared
/// result type and body. A scalar result is `Const`; a concrete `List` result is analyzed structurally; a
/// polymorphic (type-variable) or otherwise-unrecognized result is `Unknown` (its size could be as large
/// as the input — never claimed smaller).
pub fn analyze_output_size(result_ty: &J, body: &J) -> OutputSize {
    let Some(params) = lambda_params(body) else { return OutputSize::Unknown };
    let Some(inner) = body.get("body") else { return OutputSize::Unknown };
    if is_scalar_type(result_ty) {
        OutputSize::Const
    } else if is_list_type(result_ty) {
        list_output_size(&params, inner)
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
