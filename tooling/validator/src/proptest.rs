//! Generative property testing — the rung above example-bound CONSISTENT.
//!
//! `check-properties` (eval.rs / interp.rs) decides a `forall` law by ranging its variables over the
//! record's worked `examples[]`: a true-on-all / false-on-none verdict is CONSISTENT, "not
//! contradicted by the examples I have" — not a search for a counterexample. This module *searches*:
//! for each quantified variable it infers a value generator from how the variable is used in the
//! predicate, samples many inputs, runs the body (interp.rs), and reports HELD (no counterexample in N
//! cases) or REFUTED with a **shrunk** minimal counterexample.
//!
//! Determinism (principle 5): the sampler is a hand-rolled, fixed-seeded xorshift PRNG — no
//! `getrandom`, no wall clock. Same record + same N → same verdict and same counterexample, so a
//! REFUTED is reproducible and replayable.
//!
//! Honest scope: a variable used in *function position* (the higher-order argument of
//! map/filter/fold/compose/apply, or as an `app` op) is UNGENERATABLE — we don't synthesize functions
//! — so e.g. map's `composition` and `length_preserving` laws (which quantify over `f`/`g`) are
//! reported UNGENERATABLE, not silently passed. Generation ranges over the inferred *type*, ignoring
//! refinements/preconditions; an input the body rejects (a runtime error) is a skipped case, never a
//! counterexample, so domain mismatches don't manufacture false refutations.

use crate::interp::{encode_value, eval_predicate_env, Val};
use serde_json::Value as J;
use std::collections::{BTreeMap, BTreeSet};

/// What kind of value to sample for a quantified variable, inferred from its usage.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GenKind {
    Int,
    /// A non-negative int: the kind of a variable fed to a `nat`-typed parameter of `self`. The
    /// signature's `nat` is a TYPE, not a refinement — a law over `gcd : nat nat -> nat` ranges over
    /// nats, and sampling negatives would manufacture counterexamples outside the function's domain
    /// (measured on the integer-tier module: Euclid is not symmetric on negative ints).
    Nat,
    Bool,
    List(Box<GenKind>),
    Str,
}

thread_local! {
    /// The function under test's DECLARED parameter kinds, by position — what a law variable
    /// that reaches the function only as its argument ranges over. Without it such a variable
    /// defaulted to Int: `pad_to`'s `length_is_n` (`forall xs n. n < 0 || length(self(xs, n)) ==
    /// n`) fed an int for `xs` in 72 of its 81 bounded cases, every one skipped on "expected a
    /// list", and read EXHAUSTIVE (9 cases) for eight rounds — the skipped-case report's first
    /// production finding (2026-10-30). Set per record by the property checker from the
    /// signature (`self_param_kinds_of_type`); `nat` positions keep working through
    /// `self_nat_positions` for callers that pass only those.
    static SELF_PARAM_KINDS: std::cell::RefCell<BTreeMap<usize, GenKind>> = const { std::cell::RefCell::new(BTreeMap::new()) };
}

/// Install the function under test's declared parameter kinds (see `SELF_PARAM_KINDS`),
/// returning the previous map so a caller can restore it.
pub fn set_self_param_kinds(kinds: BTreeMap<usize, GenKind>) -> BTreeMap<usize, GenKind> {
    SELF_PARAM_KINDS.with(|c| std::mem::replace(&mut *c.borrow_mut(), kinds))
}

/// The generator kind of a declared type, when it has one: `int`, `nat`, `bool`, `string`, and
/// `List <elem>` (a canonical `ref` to a builtin reads as the builtin). Tuples, sums, functions
/// and `Json` have none — a variable of such a type stays undecidable here, as before.
pub fn gen_kind_of_type(ty: &J) -> Option<GenKind> {
    match crate::builtin_type_name(ty) {
        Some("int") => return Some(GenKind::Int),
        Some("nat") => return Some(GenKind::Nat),
        Some("bool") => return Some(GenKind::Bool),
        Some("string") => return Some(GenKind::Str),
        _ => {}
    }
    if ty.get("kind").and_then(|k| k.as_str()) == Some("apply")
        && ty.get("ctor").and_then(crate::builtin_type_name) == Some("List")
    {
        let elem = ty.get("args").and_then(|a| a.as_array()).and_then(|a| a.first()).and_then(gen_kind_of_type).unwrap_or(GenKind::Int);
        return Some(GenKind::List(Box::new(elem)));
    }
    None
}

/// The declared parameter kinds of a function TYPE (a `forall` is looked through), by position.
pub fn self_param_kinds_of_type(ty: &J) -> BTreeMap<usize, GenKind> {
    let fn_ty = if ty.get("kind").and_then(|k| k.as_str()) == Some("forall") { ty.get("body") } else { Some(ty) };
    fn_ty
        .and_then(|t| t.get("params"))
        .and_then(|p| p.as_array())
        .map(|ps| ps.iter().enumerate().filter_map(|(i, p)| gen_kind_of_type(p).map(|k| (i, k))).collect())
        .unwrap_or_default()
}

/// The outcome of a generative check of one property.
#[derive(Debug)]
pub enum GenOutcome {
    /// Every case in the (finite, bounded) domain was checked — a proof over that domain. Stronger
    /// than `Held`: for an all-boolean property it is total; for bounded int/list domains it is
    /// exhaustive over the enumerated range. The count is of DECIDED cases; the skipped ones
    /// (the body errored, or the predicate did not evaluate to a bool) ride beside it.
    Exhaustive(usize, Skips),
    /// No counterexample found in this many *sampled* decidable cases (the domain was too large to
    /// enumerate exhaustively), with the skipped cases beside it.
    Held(usize, Skips),
    /// A counterexample: the variable bindings (as value-expression ASTs) that falsify the property.
    Refuted(Vec<(String, J)>),
    /// Couldn't generate (quantifies over a function, no `forall`, or never decided a case).
    Ungeneratable(&'static str),
}

// --- deterministic PRNG (xorshift64*) -------------------------------------------------------------

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed ^ 0x9e3779b97f4a7c15 | 1) // splatter + force nonzero
    }
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x.wrapping_mul(0x2545f4914f6cdd1d)
    }
    /// Inclusive range.
    fn int_in(&mut self, lo: i128, hi: i128) -> i128 {
        let span = (hi - lo + 1) as u64;
        lo + (self.next_u64() % span) as i128
    }
    fn bool(&mut self) -> bool {
        self.next_u64() & 1 == 0
    }
}

/// The string-generation alphabet: letters, a comma and a space (so separator-shaped laws —
/// split/join/contains — exercise interesting cases), a digit, and a non-ASCII scalar (so
/// length-in-scalars semantics is exercised). Deterministic like everything else here.
const STR_ALPHABET: &[char] = &['a', 'b', ',', ' ', 'x', '7', 'é'];

/// A magnitude-spread integer for the WIDE mode: log-uniform over 1..2^60 (so a law meets every scale
/// up to the evaluator's exact range, not just the small numbers), with a zero or a small value one
/// time in eight. Signed for ints, non-negative for nats.
fn wide_magnitude(rng: &mut Rng, signed: bool) -> i128 {
    let v = if rng.int_in(0, 7) == 0 {
        rng.int_in(0, 4)
    } else {
        let e = rng.int_in(0, 59) as u32;
        rng.int_in(1i128 << e, (1i128 << (e + 1)) - 1)
    };
    if signed && rng.bool() {
        -v
    } else {
        v
    }
}

fn gen_value(kind: &GenKind, rng: &mut Rng, wide: bool) -> Val {
    match kind {
        GenKind::Int if wide => Val::Int(wide_magnitude(rng, true)),
        GenKind::Nat if wide => Val::Int(wide_magnitude(rng, false)),
        GenKind::Int => Val::Int(rng.int_in(-32, 32)),
        GenKind::Nat => Val::Int(rng.int_in(0, 64)),
        GenKind::Bool => Val::Bool(rng.bool()),
        GenKind::List(elem) => {
            let n = rng.int_in(0, 6) as usize;
            Val::List((0..n).map(|_| gen_value(elem, rng, wide)).collect())
        }
        GenKind::Str => {
            let n = rng.int_in(0, 6) as usize;
            Val::Str((0..n).map(|_| STR_ALPHABET[rng.int_in(0, STR_ALPHABET.len() as i128 - 1) as usize]).collect())
        }
    }
}

// --- usage-directed kind inference ----------------------------------------------------------------

/// Infer a generator for every name in `vars` from how it is used in `node`. Returns `Err(reason)`
/// if any variable is used in function position (we can't synthesize functions).
fn infer_kinds(
    node: &J,
    vars: &BTreeSet<String>,
    self_nat_positions: &BTreeSet<usize>,
) -> Result<BTreeMap<String, GenKind>, &'static str> {
    let mut kinds: BTreeMap<String, GenKind> = vars.iter().map(|v| (v.clone(), GenKind::Int)).collect();
    let mut function_vars: BTreeSet<String> = BTreeSet::new();
    collect(node, vars, self_nat_positions, &mut kinds, &mut function_vars);
    if !function_vars.is_empty() {
        return Err("property quantifies over a function (higher-order) — cannot generate");
    }
    Ok(kinds)
}

/// If `node` is `{kind:var, name}` and `name` is a quantified var, return it.
fn as_qvar<'a>(node: &'a J, vars: &BTreeSet<String>) -> Option<&'a str> {
    let name = node.get("kind").filter(|k| *k == "var").and(node.get("name")).and_then(|n| n.as_str())?;
    vars.contains(name).then_some(name)
}

fn set_kind(kinds: &mut BTreeMap<String, GenKind>, name: &str, k: GenKind) {
    // List/Bool/Str/Nat evidence is more specific than the default Int; don't let a later Int
    // overwrite it (a nat fed to `add` is still a nat).
    match kinds.get(name) {
        Some(GenKind::List(_)) | Some(GenKind::Bool) | Some(GenKind::Str) | Some(GenKind::Nat) if k == GenKind::Int => {}
        _ => {
            kinds.insert(name.to_string(), k);
        }
    }
}

fn collect(
    node: &J,
    vars: &BTreeSet<String>,
    self_nat_positions: &BTreeSet<usize>,
    kinds: &mut BTreeMap<String, GenKind>,
    func_vars: &mut BTreeSet<String>,
) {
    let Some(kind) = node.get("kind").and_then(|k| k.as_str()) else { return };
    match kind {
        "app" => {
            let op = node.get("op").and_then(|o| o.as_str()).unwrap_or_default();
            let args = node.get("args").and_then(|a| a.as_array()).cloned().unwrap_or_default();
            // The op itself being a quantified variable ⇒ that variable is a function.
            if vars.contains(op) {
                func_vars.insert(op.to_string());
            }
            let int_list = || GenKind::List(Box::new(GenKind::Int));
            let arg = |i: usize| args.get(i);
            // A quantified variable fed to a `nat`-typed parameter of the function under test ranges
            // over nats: `self(a, b)` binds positionally; `apply(self, a, b)` shifts by the head.
            let self_arg_position = |i: usize| -> Option<usize> {
                if op == "self" {
                    Some(i)
                } else if op == "apply" && i >= 1 && as_qvar(&args[0], vars).is_none()
                    && args[0].get("kind").and_then(|k| k.as_str()) == Some("var")
                    && args[0].get("name").and_then(|n| n.as_str()) == Some("self")
                {
                    Some(i - 1)
                } else {
                    None
                }
            };
            for (i, a) in args.iter().enumerate() {
                if let Some(name) = as_qvar(a, vars) {
                    if let Some(p) = self_arg_position(i) {
                        // The declared parameter kind when the checker was told it (a list, a
                        // string, a nat…); else the nat positions alone, as before.
                        match SELF_PARAM_KINDS.with(|m| m.borrow().get(&p).cloned()) {
                            Some(k) => set_kind(kinds, name, k),
                            None if self_nat_positions.contains(&p) => set_kind(kinds, name, GenKind::Nat),
                            None => {}
                        }
                    }
                    match (op, i) {
                        // Higher-order function positions ⇒ ungeneratable function variable.
                        ("map" | "filter" | "foldl" | "foldr" | "apply", 0) => {
                            func_vars.insert(name.to_string());
                        }
                        ("compose", 0 | 1) => {
                            func_vars.insert(name.to_string());
                        }
                        // List positions.
                        ("length" | "reverse" | "null" | "head" | "tail", 0) => set_kind(kinds, name, int_list()),
                        ("map" | "filter", 1) => set_kind(kinds, name, int_list()),
                        ("foldl" | "foldr", 2) => set_kind(kinds, name, int_list()),
                        ("append" | "concat", 0 | 1) => set_kind(kinds, name, int_list()),
                        ("cons", 1) => set_kind(kinds, name, int_list()),
                        // String positions (spec/expressiveness.md phase 1): a var consumed by a
                        // string op is a String; str_join's second argument is a list OF strings.
                        ("str_concat" | "str_contains" | "str_split", 0 | 1)
                        | ("str_length" | "url_encode" | "parse_int", 0)
                        | ("str_join", 0) => set_kind(kinds, name, GenKind::Str),
                        ("str_join", 1) => set_kind(kinds, name, GenKind::List(Box::new(GenKind::Str))),
                        ("to_string", 0) => set_kind(kinds, name, GenKind::Int),
                        // Boolean positions.
                        ("and" | "or" | "xor" | "implies" | "iff", _) | ("not", 0) => set_kind(kinds, name, GenKind::Bool),
                        // Integer positions.
                        ("add" | "sub" | "mul" | "div" | "mod" | "neg" | "min" | "max", _)
                        | ("lt" | "le" | "gt" | "ge", _) => set_kind(kinds, name, GenKind::Int),
                        _ => {}
                    }
                }
            }
            let _ = arg; // (kept for readability of the match above)
            for a in &args {
                collect(a, vars, self_nat_positions, kinds, func_vars);
            }
        }
        "forall" | "exists" => {
            if let Some(body) = node.get("body") {
                collect(body, vars, self_nat_positions, kinds, func_vars);
            }
        }
        _ => {}
    }
}

// --- the generative check -------------------------------------------------------------------------

fn is_false(body: &J, binding: &BTreeMap<String, Val>, self_fn: &Option<Val>) -> bool {
    matches!(eval_predicate_env(body, binding, self_fn), Some(Val::Bool(false)))
}

/// Smaller candidate values to try when shrinking a counterexample.
fn shrink_candidates(v: &Val) -> Vec<Val> {
    match v {
        Val::Int(0) => vec![],
        Val::Int(i) => {
            let mut out = vec![Val::Int(0), Val::Int(i / 2)];
            out.push(Val::Int(i - i.signum())); // step one toward zero
            out.retain(|c| !matches!(c, Val::Int(x) if x == i));
            out
        }
        Val::Bool(true) => vec![Val::Bool(false)],
        Val::List(xs) if !xs.is_empty() => {
            // Drop each single element (smaller lists first).
            (0..xs.len())
                .map(|drop| {
                    let mut ys = xs.clone();
                    ys.remove(drop);
                    Val::List(ys)
                })
                .collect()
        }
        Val::Str(s) if !s.is_empty() => {
            // Drop each single character (as with lists — smaller strings first).
            let chars: Vec<char> = s.chars().collect();
            (0..chars.len())
                .map(|drop| {
                    Val::Str(chars.iter().enumerate().filter(|(i, _)| *i != drop).map(|(_, c)| c).collect())
                })
                .collect()
        }
        _ => vec![],
    }
}

/// Greedily shrink a failing binding to a locally-minimal one that still falsifies the property.
fn shrink(
    body: &J,
    self_fn: &Option<Val>,
    names: &[String],
    mut binding: BTreeMap<String, Val>,
) -> BTreeMap<String, Val> {
    let mut improved = true;
    let mut guard = 0;
    while improved && guard < 2000 {
        improved = false;
        guard += 1;
        for name in names {
            let current = binding[name].clone();
            for cand in shrink_candidates(&current) {
                let mut trial = binding.clone();
                trial.insert(name.clone(), cand);
                if is_false(body, &trial, self_fn) {
                    binding = trial;
                    improved = true;
                    break;
                }
            }
        }
    }
    binding
}

// Bounded-exhaustive enumeration: when the whole domain is finite and small, check *every* case
// instead of sampling. `bool` is total; `int`/`list` use a bounded range so the verdict is exhaustive
// over that range (not a universal proof — see GenOutcome::Exhaustive).
const EXHAUSTIVE_BUDGET: usize = 4096;
/// Closure applications one generated case may spend before it is SKIPPED with the reason
/// "evaluation budget exceeded" — enough for any polynomial body at the sampled magnitudes, and
/// a wall for an exponential one on a wide sample (`binomial` by Pascal's rule at n ≈ 2^60 never
/// returned; 2026-10-30). Deterministic, so the verdict stays reproducible.
const CASE_FUEL: u64 = 2_000_000;

/// Evaluate one generated case under the fuel budget.
fn eval_case(body: &J, binding: &BTreeMap<String, Val>, self_fn: &Option<Val>) -> Option<Val> {
    let prev = crate::interp::set_eval_fuel(Some(CASE_FUEL));
    let out = eval_predicate_env(body, binding, self_fn);
    crate::interp::set_eval_fuel(prev);
    out
}
const INT_LO: i128 = -4;
const INT_HI: i128 = 4;
const LIST_MAX_LEN: usize = 3;

/// The finite value list of a kind for exhaustive enumeration.
fn domain_of(kind: &GenKind) -> Vec<Val> {
    match kind {
        GenKind::Bool => vec![Val::Bool(false), Val::Bool(true)],
        GenKind::Int => (INT_LO..=INT_HI).map(Val::Int).collect(),
        // Same domain SIZE as Int, shifted onto the nats (so the exhaustive budget behaves alike).
        GenKind::Nat => (0..=(INT_HI - INT_LO)).map(Val::Int).collect(),
        // A curated bounded string domain (edge shapes: empty, singletons, separators, repeats,
        // a non-ASCII scalar) — like Int's bounded range, "exhaustive" means over THIS domain.
        GenKind::Str => ["", "a", "b", ",", " ", "ab", "a,b", "a,,b", "é"]
            .iter()
            .map(|s| Val::Str((*s).to_string()))
            .collect(),
        GenKind::List(elem) => {
            let ev = domain_of(elem);
            let mut out = vec![Val::List(vec![])];
            let mut current = vec![Vec::<Val>::new()]; // lists of the current length
            for _ in 0..LIST_MAX_LEN {
                let mut next = Vec::new();
                for prefix in &current {
                    for v in &ev {
                        let mut l = prefix.clone();
                        l.push(v.clone());
                        next.push(l);
                    }
                }
                out.extend(next.iter().map(|l| Val::List(l.clone())));
                current = next;
            }
            out
        }
    }
}

/// Every binding in the cross-product of the variables' finite domains, or `None` if that product
/// exceeds `budget` (then the caller samples instead).
fn enumerate_domain(names: &[String], kinds: &BTreeMap<String, GenKind>, budget: usize) -> Option<Vec<BTreeMap<String, Val>>> {
    let per_var: Vec<Vec<Val>> = names.iter().map(|n| domain_of(&kinds[n])).collect();
    let mut total: usize = 1;
    for d in &per_var {
        total = total.checked_mul(d.len())?;
        if total > budget {
            return None;
        }
    }
    if total == 0 {
        return None;
    }
    let mut acc: Vec<BTreeMap<String, Val>> = vec![BTreeMap::new()];
    for (name, dom) in names.iter().zip(&per_var) {
        let mut next = Vec::with_capacity(acc.len() * dom.len());
        for binding in &acc {
            for v in dom {
                let mut b = binding.clone();
                b.insert(name.clone(), v.clone());
                next.push(b);
            }
        }
        acc = next;
    }
    Some(acc)
}

fn report_binding(names: &[String], binding: &BTreeMap<String, Val>) -> Vec<(String, J)> {
    names.iter().map(|n| (n.clone(), encode_value(&binding[n]))).collect()
}

/// The cases a generative run could not decide: how many, and the first one's reason with its
/// binding — reported with the verdict, because a skipped case is where an unguarded law hides
/// (2026-10-30: `gcd`'s divisibility and `lcm`'s multiple-of-both both read EXHAUSTIVE while
/// their `0 mod 0` case was being skipped; the solver's model found it later each time).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Skips {
    pub count: usize,
    /// `"<runtime error> at a = 0, b = 0"` for the first skipped case; `None` when nothing was skipped.
    pub first: Option<String>,
}

impl Skips {
    fn note(&mut self, names: &[String], binding: &BTreeMap<String, Val>) {
        self.count += 1;
        if self.first.is_none() {
            let why = crate::interp::take_predicate_skip_reason()
                .unwrap_or_else(|| "undecided (the predicate did not evaluate to a bool)".to_string());
            let at = names
                .iter()
                .map(|n| format!("{n} = {}", short_value(&encode_value(&binding[n]))))
                .collect::<Vec<_>>()
                .join(", ");
            let mut text = format!("{why} at {at}");
            if text.chars().count() > 160 {
                text = text.chars().take(157).collect::<String>() + "…";
            }
            self.first = Some(text);
        }
    }

    /// The report suffix: empty when nothing was skipped, else `; N skipped: <first reason>`.
    pub fn suffix(&self) -> String {
        match &self.first {
            None => String::new(),
            Some(first) => format!("; {} skipped: {first}", self.count),
        }
    }
}

/// A value-expression AST as a short token for a report: a scalar's `value`, else its JSON.
fn short_value(v: &J) -> String {
    match v.get("value") {
        Some(J::String(s)) => format!("{s:?}"),
        Some(x) if !x.is_object() && !x.is_array() => x.to_string(),
        _ => v.to_string(),
    }
}

/// Generatively check one property's predicate. `expr` is the property AST; `self_fn` is the
/// executable function-under-test (bound to `self`); `cases` is how many inputs to sample when the
/// domain is too large to enumerate; `seed` makes a sampled run deterministic.
pub fn generative_check(
    expr: &J,
    self_fn: &Option<Val>,
    self_nat_positions: &BTreeSet<usize>,
    cases: usize,
    seed: u64,
) -> GenOutcome {
    generative_check_mode(expr, self_fn, self_nat_positions, cases, seed, false)
}

/// [`generative_check`] with the WIDE option: skip the bounded exhaustive domain and SAMPLE
/// integers across magnitudes (log-uniform to 2^60) — number theory's cases live far above −4..4,
/// and a law that holds there is evidence the small domain cannot give (still not a proof).
pub fn generative_check_mode(
    expr: &J,
    self_fn: &Option<Val>,
    self_nat_positions: &BTreeSet<usize>,
    cases: usize,
    seed: u64,
    wide: bool,
) -> GenOutcome {
    // Only a `forall` gives a domain to range over.
    if expr.get("kind").and_then(|k| k.as_str()) != Some("forall") {
        return GenOutcome::Ungeneratable("not a forall — no domain to sample");
    }
    let Some(var_arr) = expr.get("vars").and_then(|v| v.as_array()) else {
        return GenOutcome::Ungeneratable("forall has no vars");
    };
    let names: Vec<String> = var_arr.iter().filter_map(|v| v.as_str().map(String::from)).collect();
    let vars: BTreeSet<String> = names.iter().cloned().collect();
    let Some(body) = expr.get("body") else {
        return GenOutcome::Ungeneratable("forall has no body");
    };

    let kinds = match infer_kinds(body, &vars, self_nat_positions) {
        Ok(k) => k,
        Err(reason) => return GenOutcome::Ungeneratable(reason),
    };

    // Prefer EXHAUSTIVE checking when the bounded domain fits the budget — a proof over that domain.
    if let Some(domain) = (!wide).then(|| enumerate_domain(&names, &kinds, EXHAUSTIVE_BUDGET)).flatten() {
        let mut decided = 0usize;
        let mut skips = Skips::default();
        for binding in domain {
            crate::interp::clear_predicate_skip_reason();
            match eval_case(body, &binding, self_fn) {
                Some(Val::Bool(true)) => decided += 1,
                Some(Val::Bool(false)) => {
                    let minimal = shrink(body, self_fn, &names, binding);
                    return GenOutcome::Refuted(report_binding(&names, &minimal));
                }
                _ => skips.note(&names, &binding),
            }
        }
        return if decided == 0 {
            GenOutcome::Ungeneratable("no case was decidable (predicate never evaluated to a bool)")
        } else {
            GenOutcome::Exhaustive(decided, skips)
        };
    }

    // Otherwise SAMPLE the (too-large) domain.
    let mut rng = Rng::new(seed);
    let mut decided = 0usize;
    let mut skips = Skips::default();
    for _ in 0..cases {
        let binding: BTreeMap<String, Val> =
            names.iter().map(|n| (n.clone(), gen_value(&kinds[n], &mut rng, wide))).collect();
        crate::interp::clear_predicate_skip_reason();
        match eval_case(body, &binding, self_fn) {
            Some(Val::Bool(true)) => decided += 1,
            Some(Val::Bool(false)) => {
                let minimal = shrink(body, self_fn, &names, binding);
                return GenOutcome::Refuted(report_binding(&names, &minimal));
            }
            _ => skips.note(&names, &binding), // undecidable on this input (out of domain / errored / over budget)
        }
    }
    if decided == 0 {
        GenOutcome::Ungeneratable("no case was decidable (predicate never evaluated to a bool)")
    } else {
        GenOutcome::Held(decided, skips)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interp::self_fn_from_body;
    use serde_json::json;

    /// A law variable that reaches the function ONLY as its argument ranges over the DECLARED
    /// parameter type once the checker is told it: `\xs n -> add(length(xs), n)` under
    /// `forall xs n. eq(self(xs, n), self(xs, n))` — with no kinds installed `xs` is an int and
    /// every case errors ("expected a list"); with `List int, nat` installed every case decides.
    #[test]
    fn a_self_argument_ranges_over_the_declared_parameter_type() {
        let body = json!({ "kind": "lambda", "params": [{ "name": "xs" }, { "name": "n" }],
            "body": { "kind": "app", "fn": { "kind": "var", "name": "add" }, "args": [
                { "kind": "app", "fn": { "kind": "var", "name": "length" }, "args": [{ "kind": "var", "name": "xs" }] },
                { "kind": "var", "name": "n" } ] } });
        let self_fn = self_fn_from_body(&body);
        let call = json!({ "kind": "app", "op": "self", "args": [{ "kind": "var", "name": "xs" }, { "kind": "var", "name": "n" }] });
        let law = json!({ "kind": "forall", "vars": ["xs", "n"], "body": { "kind": "app", "op": "eq", "args": [call, call] } });
        let nats: BTreeSet<usize> = [1].into();
        let prev = set_self_param_kinds(BTreeMap::new());
        let bare = generative_check(&law, &self_fn, &nats, 200, 1);
        assert!(matches!(bare, GenOutcome::Ungeneratable(_)), "an int for xs errors every case: {bare:?}");
        let ty = json!({ "kind": "fn", "params": [
            { "kind": "apply", "ctor": { "kind": "builtin", "name": "List" }, "args": [{ "kind": "builtin", "name": "int" }] },
            { "kind": "builtin", "name": "nat" } ], "result": { "kind": "builtin", "name": "int" } });
        let kinds = self_param_kinds_of_type(&ty);
        assert_eq!(kinds.get(&0), Some(&GenKind::List(Box::new(GenKind::Int))));
        assert_eq!(kinds.get(&1), Some(&GenKind::Nat));
        set_self_param_kinds(kinds);
        let typed = generative_check(&law, &self_fn, &nats, 200, 1);
        set_self_param_kinds(prev);
        // Lists × nats outgrow the exhaustive budget, so this SAMPLES — what matters is that
        // every case decides (no "expected a list") where before none did.
        match typed {
            GenOutcome::Exhaustive(n, skips) | GenOutcome::Held(n, skips) => {
                assert!(n > 0);
                assert_eq!(skips, Skips::default(), "{skips:?}");
            }
            other => panic!("expected every case to decide over lists, got {other:?}"),
        }
    }

    /// An exponential body on a WIDE sample — Pascal's-rule binomials at n near 2^60 — must not
    /// hang the checker: each case has a budget, and a case over it is SKIPPED with the reason.
    #[test]
    fn a_case_over_the_evaluation_budget_is_skipped_not_waited_for() {
        // \n k -> case gt k 0 of { true => case gt n 0 of { true => add (self (sub n 1) (sub k 1)) (self (sub n 1) k); false => 0 }; false => 1 }
        let pascal = json!({ "kind": "lambda", "params": [{ "name": "n" }, { "name": "k" }], "body": { "kind": "case",
            "scrutinee": { "kind": "app", "fn": { "kind": "var", "name": "gt" }, "args": [{ "kind": "var", "name": "k" }, { "kind": "lit", "value": { "kind": "int", "value": 0 } }] },
            "arms": [
                { "pattern": { "kind": "lit", "value": { "kind": "bool", "value": true } }, "body": { "kind": "case",
                    "scrutinee": { "kind": "app", "fn": { "kind": "var", "name": "gt" }, "args": [{ "kind": "var", "name": "n" }, { "kind": "lit", "value": { "kind": "int", "value": 0 } }] },
                    "arms": [
                        { "pattern": { "kind": "lit", "value": { "kind": "bool", "value": true } }, "body": { "kind": "app", "fn": { "kind": "var", "name": "add" }, "args": [
                            { "kind": "app", "fn": { "kind": "var", "name": "self" }, "args": [ { "kind": "app", "fn": { "kind": "var", "name": "sub" }, "args": [{ "kind": "var", "name": "n" }, { "kind": "lit", "value": { "kind": "int", "value": 1 } }] }, { "kind": "app", "fn": { "kind": "var", "name": "sub" }, "args": [{ "kind": "var", "name": "k" }, { "kind": "lit", "value": { "kind": "int", "value": 1 } }] } ] },
                            { "kind": "app", "fn": { "kind": "var", "name": "self" }, "args": [ { "kind": "app", "fn": { "kind": "var", "name": "sub" }, "args": [{ "kind": "var", "name": "n" }, { "kind": "lit", "value": { "kind": "int", "value": 1 } }] }, { "kind": "var", "name": "k" } ] } ] } },
                        { "pattern": { "kind": "lit", "value": { "kind": "bool", "value": false } }, "body": { "kind": "lit", "value": { "kind": "int", "value": 0 } } } ] } },
                { "pattern": { "kind": "lit", "value": { "kind": "bool", "value": false } }, "body": { "kind": "lit", "value": { "kind": "int", "value": 1 } } } ] } });
        // On a CLI-sized stack: the recursion is as deep as `n`, and a wide `n` reaches the depth
        // bound (100,000) long before a test thread's 2 MiB stack would allow.
        let out = std::thread::Builder::new()
            .stack_size(2 << 30)
            .spawn(move || {
                let self_fn = self_fn_from_body(&pascal);
                let nats: BTreeSet<usize> = [0, 1].into();
                // C(n, 0) = 1 returns at once: every wide case decides.
                let law = json!({ "kind": "forall", "vars": ["n"], "body": { "kind": "app", "op": "eq", "args": [
                    { "kind": "app", "op": "self", "args": [{ "kind": "var", "name": "n" }, { "kind": "lit", "value": 0 }] }, { "kind": "lit", "value": 1 } ] } });
                let quick = generative_check_mode(&law, &self_fn, &nats, 20, 1, true);
                // C(n, n div 2) ≥ 1: the tree is exponential in n at bounded depth (n ≈ 60 was the
                // 33-minute case), and a wide `n` hits the depth bound; either way every sampled case
                // must come back — skipped and SAID, or decided — within the budget.
                let law = json!({ "kind": "forall", "vars": ["n"], "body": { "kind": "app", "op": "ge", "args": [
                    { "kind": "app", "op": "self", "args": [{ "kind": "var", "name": "n" }, { "kind": "app", "op": "div", "args": [{ "kind": "var", "name": "n" }, { "kind": "lit", "value": 2 }] }] }, { "kind": "lit", "value": 1 } ] } });
                let t = std::time::Instant::now();
                let slow = generative_check_mode(&law, &self_fn, &nats, 12, 1, true);
                (quick, slow, t.elapsed())
            })
            .unwrap()
            .join()
            .unwrap();
        let (quick, slow, elapsed) = out;
        assert!(matches!(quick, GenOutcome::Held(_, Skips { count: 0, .. })), "{quick:?}");
        assert!(elapsed.as_secs() < 120, "the budget must bound the wall clock: {elapsed:?}");
        match slow {
            GenOutcome::Held(_, skips) => {
                let why = skips.first.as_deref().unwrap_or("");
                assert!(skips.count == 0 || why.contains("evaluation budget exceeded") || why.contains("recursion depth exceeded"), "{skips:?}");
            }
            GenOutcome::Ungeneratable(_) => {} // every sampled case over budget: also honest
            other => panic!("{other:?}"),
        }
    }

    /// The same law behind its guard, `forall a b. or(eq(b, 0), eq(mod(a, b), mod(a, b)))`: the
    /// guard decides the `b = 0` cases, so NOTHING is skipped — a guarded law reads as the clean
    /// EXHAUSTIVE it is, which strict evaluation of `or` denied it.
    #[test]
    fn a_true_guard_decides_the_case_instead_of_skipping_it() {
        let m = json!({ "kind": "app", "op": "mod", "args": [ { "kind": "var", "name": "a" }, { "kind": "var", "name": "b" } ] });
        let guard = json!({ "kind": "app", "op": "eq", "args": [ { "kind": "var", "name": "b" }, { "kind": "lit", "value": 0 } ] });
        let law = json!({ "kind": "forall", "vars": ["a", "b"], "body": { "kind": "app", "op": "or", "args": [
            guard, { "kind": "app", "op": "eq", "args": [m, m] } ] } });
        match generative_check(&law, &None, &Default::default(), 200, 1) {
            GenOutcome::Exhaustive(n, skips) => {
                assert!(n > 0);
                assert_eq!(skips, Skips::default(), "a true guard must decide the case: {skips:?}");
            }
            other => panic!("expected a clean EXHAUSTIVE, got {other:?}"),
        }
        // And the guard on the RIGHT decides too (an erroring side beside a deciding one).
        let m2 = json!({ "kind": "app", "op": "mod", "args": [ { "kind": "var", "name": "a" }, { "kind": "var", "name": "b" } ] });
        let guard2 = json!({ "kind": "app", "op": "eq", "args": [ { "kind": "var", "name": "b" }, { "kind": "lit", "value": 0 } ] });
        let law2 = json!({ "kind": "forall", "vars": ["a", "b"], "body": { "kind": "app", "op": "or", "args": [
            { "kind": "app", "op": "eq", "args": [m2.clone(), m2] }, guard2 ] } });
        assert!(matches!(generative_check(&law2, &None, &Default::default(), 200, 1), GenOutcome::Exhaustive(_, Skips { count: 0, .. })));
    }

    /// `forall a b. eq(mod(a, b), mod(a, b))` over the bounded int domain: true wherever it is
    /// defined, and every case with `b = 0` errors in the interpreter and is SKIPPED — the verdict
    /// must say so, with the first reason and binding, instead of reading as a clean EXHAUSTIVE.
    #[test]
    fn skipped_cases_are_counted_and_the_first_reason_is_reported() {
        let m = json!({ "kind": "app", "op": "mod", "args": [ { "kind": "var", "name": "a" }, { "kind": "var", "name": "b" } ] });
        let law = json!({ "kind": "forall", "vars": ["a", "b"], "body": { "kind": "app", "op": "eq", "args": [m, m] } });
        match generative_check(&law, &None, &Default::default(), 200, 1) {
            GenOutcome::Exhaustive(n, skips) => {
                assert!(n > 0);
                assert!(skips.count > 0, "the b = 0 cases must be skipped, not decided");
                let first = skips.first.clone().expect("a reason for the first skipped case");
                assert!(first.contains("b = 0"), "{first}");
                assert!(first.contains(" at "), "{first}");
                let text = skips.suffix();
                assert!(text.starts_with(&format!("; {} skipped: ", skips.count)), "{text}");
            }
            other => panic!("expected EXHAUSTIVE with skips, got {other:?}"),
        }
    }

    // \n -> add(n, n)
    fn double_body() -> J {
        json!({ "kind": "lambda", "params": [{ "name": "n" }],
            "body": { "kind": "app", "fn": { "kind": "var", "name": "add" },
                      "args": [{ "kind": "var", "name": "n" }, { "kind": "var", "name": "n" }] } })
    }

    #[test]
    fn nat_typed_self_parameters_bound_the_domain_to_nats() {
        // gcd: \a b -> case eq(b, 0) of true -> a | false -> self(b, mod(a, b)). Euclid with the
        // language's Euclidean `mod` is NOT symmetric on negative ints (gcd(-2, -1) = -1 but
        // gcd(-1, -2) = 1), while over the declared `nat nat -> nat` domain commutativity holds.
        let gcd = json!({ "kind": "lambda", "params": [{ "name": "a" }, { "name": "b" }],
            "body": { "kind": "case",
                      "scrutinee": { "kind": "app", "fn": { "kind": "var", "name": "eq" },
                                     "args": [{ "kind": "var", "name": "b" }, { "kind": "lit", "value": { "kind": "int", "value": 0 } }] },
                      "arms": [
                          { "pattern": { "kind": "lit", "value": { "kind": "bool", "value": true } }, "body": { "kind": "var", "name": "a" } },
                          { "pattern": { "kind": "lit", "value": { "kind": "bool", "value": false } },
                            "body": { "kind": "app", "fn": { "kind": "var", "name": "self" },
                                      "args": [{ "kind": "var", "name": "b" },
                                               { "kind": "app", "fn": { "kind": "var", "name": "mod" },
                                                 "args": [{ "kind": "var", "name": "a" }, { "kind": "var", "name": "b" }] }] } }] } });
        let self_fn = self_fn_from_body(&gcd);
        // The surface spelling `self(a, b)` — an app with op "self".
        let law = json!({ "kind": "forall", "vars": ["a", "b"],
            "body": { "kind": "app", "op": "eq", "args": [
                { "kind": "app", "op": "self", "args": [{ "kind": "var", "name": "a" }, { "kind": "var", "name": "b" }] },
                { "kind": "app", "op": "self", "args": [{ "kind": "var", "name": "b" }, { "kind": "var", "name": "a" }] }] } });
        let nats: BTreeSet<usize> = [0, 1].into();
        assert!(matches!(generative_check(&law, &self_fn, &nats, 200, 1), GenOutcome::Exhaustive(..)),
                "over nats the law holds exhaustively");
        assert!(matches!(generative_check(&law, &self_fn, &Default::default(), 200, 1), GenOutcome::Refuted(_)),
                "over ints the generator finds the negative counterexample");
    }

    #[test]
    fn holds_for_a_true_law() {
        // forall n. eq(self(n), add(n, n))  — true for double, over generated ints.
        let expr = json!({ "kind": "forall", "vars": ["n"], "body": {
            "kind": "app", "op": "eq", "args": [
                { "kind": "app", "op": "apply", "args": [{ "kind": "var", "name": "self" }, { "kind": "var", "name": "n" }] },
                { "kind": "app", "op": "add", "args": [{ "kind": "var", "name": "n" }, { "kind": "var", "name": "n" }] }] } });
        let self_fn = self_fn_from_body(&double_body());
        // `n` is an int → the bounded domain is small, so this is checked EXHAUSTIVELY.
        match generative_check(&expr, &self_fn, &Default::default(), 200, 1) {
            GenOutcome::Exhaustive(n, _) | GenOutcome::Held(n, _) => assert!(n > 0),
            _ => panic!("double's doubling law should hold"),
        }
    }

    #[test]
    fn refutes_and_shrinks_a_false_law() {
        // forall n. gt(self(n), n)  — false for double at n = 0 (double(0) = 0, not > 0).
        let expr = json!({ "kind": "forall", "vars": ["n"], "body": {
            "kind": "app", "op": "gt", "args": [
                { "kind": "app", "op": "apply", "args": [{ "kind": "var", "name": "self" }, { "kind": "var", "name": "n" }] },
                { "kind": "var", "name": "n" }] } });
        let self_fn = self_fn_from_body(&double_body());
        match generative_check(&expr, &self_fn, &Default::default(), 500, 1) {
            GenOutcome::Refuted(b) => {
                // The minimal counterexample is n = 0.
                assert_eq!(b.len(), 1);
                assert_eq!(b[0].0, "n");
                assert_eq!(b[0].1, json!({ "kind": "int", "value": 0 }));
            }
            _ => panic!("a strictly-increasing claim about double should be REFUTED at n=0"),
        }
    }

    #[test]
    fn reverse_involution_holds_over_generated_lists() {
        // forall xs. eq(reverse(reverse(xs)), xs)  — xs inferred as a list, no self needed.
        let expr = json!({ "kind": "forall", "vars": ["xs"], "body": {
            "kind": "app", "op": "eq", "args": [
                { "kind": "app", "op": "reverse", "args": [
                    { "kind": "app", "op": "reverse", "args": [{ "kind": "var", "name": "xs" }] }] },
                { "kind": "var", "name": "xs" }] } });
        match generative_check(&expr, &None, &Default::default(), 200, 7) {
            GenOutcome::Exhaustive(n, _) | GenOutcome::Held(n, _) => assert!(n > 0),
            _ => panic!("reverse∘reverse = id should hold"),
        }
    }

    #[test]
    fn split_join_round_trip_holds_over_generated_strings() {
        // forall s. eq(str_join(",", str_split(",", s)), s) — a TRUE string law the SMT string
        // fragment cannot reach (split/join have no theory counterpart); the generative checker
        // covers it: s is inferred a String and the law holds over the generated/bounded domain.
        let comma = json!({ "kind": "lit", "value": { "kind": "string", "value": "," } });
        let expr = json!({ "kind": "forall", "vars": ["s"], "body": {
            "kind": "app", "op": "eq", "args": [
                { "kind": "app", "op": "str_join", "args": [comma.clone(),
                    { "kind": "app", "op": "str_split", "args": [comma, { "kind": "var", "name": "s" }] }] },
                { "kind": "var", "name": "s" }] } });
        match generative_check(&expr, &None, &Default::default(), 200, 7) {
            GenOutcome::Exhaustive(n, _) | GenOutcome::Held(n, _) => assert!(n > 0),
            other => panic!("join . split should hold, got {other:?}"),
        }
    }

    #[test]
    fn false_string_law_is_refuted_and_shrunk() {
        // forall s. str_contains("a", s) — false; shrinking should land on the empty string.
        let expr = json!({ "kind": "forall", "vars": ["s"], "body": {
            "kind": "app", "op": "str_contains", "args": [
                { "kind": "lit", "value": { "kind": "string", "value": "a" } },
                { "kind": "var", "name": "s" }] } });
        match generative_check(&expr, &None, &Default::default(), 200, 7) {
            GenOutcome::Refuted(bind) => {
                assert_eq!(bind.len(), 1);
                assert_eq!(bind[0].1, json!({ "kind": "string", "value": "" }),
                           "shrinking should reach the empty string");
            }
            other => panic!("contains(\"a\", s) should be refuted, got {other:?}"),
        }
    }

    #[test]
    fn string_list_position_infers_list_of_strings() {
        // forall xs. eq(str_join("", xs), str_join("", xs)) — xs must generate as List(Str), not
        // List(Int) (a List(Int) would make str_join error → every case undecided → UNGENERATABLE).
        let empty = json!({ "kind": "lit", "value": { "kind": "string", "value": "" } });
        let join = json!({ "kind": "app", "op": "str_join", "args": [empty, { "kind": "var", "name": "xs" }] });
        let expr = json!({ "kind": "forall", "vars": ["xs"], "body": {
            "kind": "app", "op": "eq", "args": [join.clone(), join] } });
        match generative_check(&expr, &None, &Default::default(), 100, 3) {
            GenOutcome::Exhaustive(n, _) | GenOutcome::Held(n, _) => assert!(n > 0),
            other => panic!("str_join over generated List(Str) should hold, got {other:?}"),
        }
    }

    #[test]
    fn function_quantified_law_is_ungeneratable() {
        // forall f xs. eq(length(map(f, xs)), length(xs))  — f is a function ⇒ ungeneratable.
        let expr = json!({ "kind": "forall", "vars": ["f", "xs"], "body": {
            "kind": "app", "op": "eq", "args": [
                { "kind": "app", "op": "length", "args": [
                    { "kind": "app", "op": "map", "args": [{ "kind": "var", "name": "f" }, { "kind": "var", "name": "xs" }] }] },
                { "kind": "app", "op": "length", "args": [{ "kind": "var", "name": "xs" }] }] } });
        assert!(matches!(generative_check(&expr, &None, &Default::default(), 50, 1), GenOutcome::Ungeneratable(_)));
    }

    #[test]
    fn small_boolean_domain_is_exhaustive() {
        // forall b. eq(not(not(b)), b): `b` is a bool, so both cases are enumerated — a real proof.
        let expr = json!({ "kind": "forall", "vars": ["b"], "body": {
            "kind": "app", "op": "eq", "args": [
                { "kind": "app", "op": "not", "args": [
                    { "kind": "app", "op": "not", "args": [{ "kind": "var", "name": "b" }] }] },
                { "kind": "var", "name": "b" }] } });
        match generative_check(&expr, &None, &Default::default(), 200, 1) {
            GenOutcome::Exhaustive(n, _) => assert_eq!(n, 2),
            _ => panic!("a boolean law should be checked exhaustively over both cases"),
        }
    }

    #[test]
    fn large_domain_falls_back_to_sampling() {
        // forall a b c d. eq(add(add(add(a,b),c),d), add(add(add(d,c),b),a)) — 4 ints exceed the
        // exhaustive budget, so it is SAMPLED (still holds).
        let sum = |order: [&str; 4]| {
            let mut acc = json!({ "kind": "var", "name": order[0] });
            for v in &order[1..] {
                acc = json!({ "kind": "app", "op": "add", "args": [acc, { "kind": "var", "name": v }] });
            }
            acc
        };
        let expr = json!({ "kind": "forall", "vars": ["a", "b", "c", "d"],
            "body": { "kind": "app", "op": "eq", "args": [sum(["a", "b", "c", "d"]), sum(["d", "c", "b", "a"])] } });
        assert!(matches!(generative_check(&expr, &None, &Default::default(), 200, 1), GenOutcome::Held(..)));
    }
}
