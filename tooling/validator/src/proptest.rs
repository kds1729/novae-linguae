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
enum GenKind {
    Int,
    Bool,
    List(Box<GenKind>),
    Str,
}

/// The outcome of a generative check of one property.
#[derive(Debug)]
pub enum GenOutcome {
    /// Every case in the (finite, bounded) domain was checked — a proof over that domain. Stronger
    /// than `Held`: for an all-boolean property it is total; for bounded int/list domains it is
    /// exhaustive over the enumerated range.
    Exhaustive(usize),
    /// No counterexample found in this many *sampled* decidable cases (the domain was too large to
    /// enumerate exhaustively).
    Held(usize),
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

fn gen_value(kind: &GenKind, rng: &mut Rng) -> Val {
    match kind {
        GenKind::Int => Val::Int(rng.int_in(-32, 32)),
        GenKind::Bool => Val::Bool(rng.bool()),
        GenKind::List(elem) => {
            let n = rng.int_in(0, 6) as usize;
            Val::List((0..n).map(|_| gen_value(elem, rng)).collect())
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
fn infer_kinds(node: &J, vars: &BTreeSet<String>) -> Result<BTreeMap<String, GenKind>, &'static str> {
    let mut kinds: BTreeMap<String, GenKind> = vars.iter().map(|v| (v.clone(), GenKind::Int)).collect();
    let mut function_vars: BTreeSet<String> = BTreeSet::new();
    collect(node, vars, &mut kinds, &mut function_vars);
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
    // List/Bool/Str evidence is more specific than the default Int; don't let a later Int overwrite it.
    match kinds.get(name) {
        Some(GenKind::List(_)) | Some(GenKind::Bool) | Some(GenKind::Str) if k == GenKind::Int => {}
        _ => {
            kinds.insert(name.to_string(), k);
        }
    }
}

fn collect(
    node: &J,
    vars: &BTreeSet<String>,
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
            for (i, a) in args.iter().enumerate() {
                if let Some(name) = as_qvar(a, vars) {
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
                collect(a, vars, kinds, func_vars);
            }
        }
        "forall" | "exists" => {
            if let Some(body) = node.get("body") {
                collect(body, vars, kinds, func_vars);
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
const INT_LO: i128 = -4;
const INT_HI: i128 = 4;
const LIST_MAX_LEN: usize = 3;

/// The finite value list of a kind for exhaustive enumeration.
fn domain_of(kind: &GenKind) -> Vec<Val> {
    match kind {
        GenKind::Bool => vec![Val::Bool(false), Val::Bool(true)],
        GenKind::Int => (INT_LO..=INT_HI).map(Val::Int).collect(),
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

/// Generatively check one property's predicate. `expr` is the property AST; `self_fn` is the
/// executable function-under-test (bound to `self`); `cases` is how many inputs to sample when the
/// domain is too large to enumerate; `seed` makes a sampled run deterministic.
pub fn generative_check(expr: &J, self_fn: &Option<Val>, cases: usize, seed: u64) -> GenOutcome {
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

    let kinds = match infer_kinds(body, &vars) {
        Ok(k) => k,
        Err(reason) => return GenOutcome::Ungeneratable(reason),
    };

    // Prefer EXHAUSTIVE checking when the bounded domain fits the budget — a proof over that domain.
    if let Some(domain) = enumerate_domain(&names, &kinds, EXHAUSTIVE_BUDGET) {
        let mut decided = 0usize;
        for binding in domain {
            match eval_predicate_env(body, &binding, self_fn) {
                Some(Val::Bool(true)) => decided += 1,
                Some(Val::Bool(false)) => {
                    let minimal = shrink(body, self_fn, &names, binding);
                    return GenOutcome::Refuted(report_binding(&names, &minimal));
                }
                _ => {}
            }
        }
        return if decided == 0 {
            GenOutcome::Ungeneratable("no case was decidable (predicate never evaluated to a bool)")
        } else {
            GenOutcome::Exhaustive(decided)
        };
    }

    // Otherwise SAMPLE the (too-large) domain.
    let mut rng = Rng::new(seed);
    let mut decided = 0usize;
    for _ in 0..cases {
        let binding: BTreeMap<String, Val> =
            names.iter().map(|n| (n.clone(), gen_value(&kinds[n], &mut rng))).collect();
        match eval_predicate_env(body, &binding, self_fn) {
            Some(Val::Bool(true)) => decided += 1,
            Some(Val::Bool(false)) => {
                let minimal = shrink(body, self_fn, &names, binding);
                return GenOutcome::Refuted(report_binding(&names, &minimal));
            }
            _ => {} // undecidable on this input (out of domain / unresolved) — skip
        }
    }
    if decided == 0 {
        GenOutcome::Ungeneratable("no case was decidable (predicate never evaluated to a bool)")
    } else {
        GenOutcome::Held(decided)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interp::self_fn_from_body;
    use serde_json::json;

    // \n -> add(n, n)
    fn double_body() -> J {
        json!({ "kind": "lambda", "params": [{ "name": "n" }],
            "body": { "kind": "app", "fn": { "kind": "var", "name": "add" },
                      "args": [{ "kind": "var", "name": "n" }, { "kind": "var", "name": "n" }] } })
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
        match generative_check(&expr, &self_fn, 200, 1) {
            GenOutcome::Exhaustive(n) | GenOutcome::Held(n) => assert!(n > 0),
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
        match generative_check(&expr, &self_fn, 500, 1) {
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
        match generative_check(&expr, &None, 200, 7) {
            GenOutcome::Exhaustive(n) | GenOutcome::Held(n) => assert!(n > 0),
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
        match generative_check(&expr, &None, 200, 7) {
            GenOutcome::Exhaustive(n) | GenOutcome::Held(n) => assert!(n > 0),
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
        match generative_check(&expr, &None, 200, 7) {
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
        match generative_check(&expr, &None, 100, 3) {
            GenOutcome::Exhaustive(n) | GenOutcome::Held(n) => assert!(n > 0),
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
        assert!(matches!(generative_check(&expr, &None, 50, 1), GenOutcome::Ungeneratable(_)));
    }

    #[test]
    fn small_boolean_domain_is_exhaustive() {
        // forall b. eq(not(not(b)), b): `b` is a bool, so both cases are enumerated — a real proof.
        let expr = json!({ "kind": "forall", "vars": ["b"], "body": {
            "kind": "app", "op": "eq", "args": [
                { "kind": "app", "op": "not", "args": [
                    { "kind": "app", "op": "not", "args": [{ "kind": "var", "name": "b" }] }] },
                { "kind": "var", "name": "b" }] } });
        match generative_check(&expr, &None, 200, 1) {
            GenOutcome::Exhaustive(n) => assert_eq!(n, 2),
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
        assert!(matches!(generative_check(&expr, &None, 200, 1), GenOutcome::Held(_)));
    }
}
