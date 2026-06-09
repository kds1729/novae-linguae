//! Theory exploration (Layer B of lemma discovery) — conjecturing auxiliary lemmas the curated
//! catalog ([`crate::lemmas`], Layer A) does not contain.
//!
//! When a curated lemma set can't close an induction, this module manufactures candidate lemmas the
//! way QuickSpec / Hipster / IsaCoSy do — by *theory exploration*:
//!
//! 1. **Enumerate** well-typed terms over the goal's operations (restricted to the goal's prelude
//!    closure, so a discovered lemma is automatically admissible) up to a small size bound, over a
//!    fixed set of variables `xs`, `ys : List` and the `nil` constant.
//! 2. **Test** every term on a fixed battery of concrete inputs, recording its result vector.
//! 3. **Bucket** terms by equal result vectors: terms that agree on every test are *conjectured equal*.
//! 4. Emit each bucket's equations as `forall`-quantified candidate lemmas.
//!
//! Testing is only a *filter* — it prunes the candidate set cheaply. Soundness comes entirely from the
//! next stage: [`crate::induct`] **proves each surviving conjecture by induction** before assuming it,
//! so a conjecture that passes the tests but isn't actually a theorem is rejected at the proof step,
//! never assumed. A false goal therefore can never be closed (assuming only proved facts), exactly as
//! in Layer A.
//!
//! Determinism (principle 5): enumeration order and the test battery are fixed — no RNG — so the same
//! goal yields the same conjectures every run, and the resulting proof certificates re-check.
//!
//! Honest scope (v0.1). The enumerated fragment is **first-order** — `reverse`, `append`, `length`,
//! `cons`, `head`, `tail`, `null`, `add` — exactly where the SMT backend is strong. `map`/`filter`
//! laws (whose function argument the backend models as an uninterpreted symbol) are out of scope here:
//! z3 rarely discharges them even when handed the lemma, so exploring for them would not pay off. Two
//! `List` variables and a size-5 term bound keep the search small; both are easy to widen later.
//! Conjectures are ranked smallest-first and capped, so over a rich operation set the larger
//! distribution laws can be truncated — but those (`reverse_append`, `length_append`, …) are exactly
//! what the Layer A catalog already provides, so exploration's job is the *un-catalogued* remainder.

use serde_json::{json, Value as J};
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// Largest term (node count) the enumerator builds. 5 reaches `reverse(append(xs, ys))` and the
/// associativity instances while keeping the term set in the low hundreds.
const MAX_TERM_SIZE: usize = 5;
/// Cap on conjectures handed back (ranked smallest-first), bounding how many proofs the caller attempts.
/// Large enough to retain the standard distribution laws (`reverse_append`, `length_append` are ~size
/// 10) while the caller's early-stop keeps the common case from paying for the whole list.
const MAX_CONJECTURES: usize = 32;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Sort {
    Lst,
    Int,
    Bool,
}

/// A concrete value used while testing a conjecture.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
enum Val {
    Int(i64),
    Lst(Vec<i64>),
    Bool(bool),
}

struct OpSig {
    name: &'static str,
    args: &'static [Sort],
    ret: Sort,
}

const ALL_OPS: &[OpSig] = &[
    OpSig { name: "reverse", args: &[Sort::Lst], ret: Sort::Lst },
    OpSig { name: "append", args: &[Sort::Lst, Sort::Lst], ret: Sort::Lst },
    OpSig { name: "length", args: &[Sort::Lst], ret: Sort::Int },
    OpSig { name: "cons", args: &[Sort::Int, Sort::Lst], ret: Sort::Lst },
    OpSig { name: "head", args: &[Sort::Lst], ret: Sort::Int },
    OpSig { name: "tail", args: &[Sort::Lst], ret: Sort::Lst },
    OpSig { name: "null", args: &[Sort::Lst], ret: Sort::Bool },
    OpSig { name: "add", args: &[Sort::Int, Sort::Int], ret: Sort::Int },
];

// --- AST builders ---------------------------------------------------------------------------------

fn var(n: &str) -> J {
    json!({ "kind": "var", "name": n })
}
fn app(op: &str, args: Vec<J>) -> J {
    json!({ "kind": "app", "op": op, "args": args })
}

/// Node count of a term or conjecture (used to rank and to choose a bucket's representative). Descends
/// through both `args` (applications) and `body` (a `forall` conjecture), so a whole `forall` equation
/// is measured by its contents — not treated as a single node.
fn size(t: &J) -> usize {
    let mut n = 1;
    if let Some(args) = t.get("args").and_then(|a| a.as_array()) {
        n += args.iter().map(size).sum::<usize>();
    }
    if let Some(b) = t.get("body") {
        n += size(b);
    }
    n
}

// --- enumeration ----------------------------------------------------------------------------------

/// All well-typed terms (any sort) up to `MAX_TERM_SIZE`, built from `ops` plus the variables
/// `xs`/`ys` and the `nil` constant. Deduplicated by canonical JSON.
fn enumerate(ops: &[&OpSig]) -> Vec<J> {
    // terms[sort][size] = list of terms.
    let mut by: BTreeMap<(Sort, usize), Vec<J>> = BTreeMap::new();
    by.insert((Sort::Lst, 1), vec![var("xs"), var("ys"), var("nil")]);
    by.entry((Sort::Int, 1)).or_default();
    by.entry((Sort::Bool, 1)).or_default();

    for sz in 2..=MAX_TERM_SIZE {
        for op in ops {
            let mut produced: Vec<J> = Vec::new();
            match op.args {
                [a0] => {
                    for sub in by.get(&(*a0, sz - 1)).into_iter().flatten() {
                        produced.push(app(op.name, vec![sub.clone()]));
                    }
                }
                [a0, a1] => {
                    for left_sz in 1..=(sz - 2) {
                        let right_sz = sz - 1 - left_sz;
                        let lefts = by.get(&(*a0, left_sz)).cloned().unwrap_or_default();
                        let rights = by.get(&(*a1, right_sz)).cloned().unwrap_or_default();
                        for l in &lefts {
                            for r in &rights {
                                produced.push(app(op.name, vec![l.clone(), r.clone()]));
                            }
                        }
                    }
                }
                _ => {}
            }
            by.entry((op.ret, sz)).or_default().extend(produced);
        }
        // Dedup each (sort, size) bucket by canonical JSON.
        for ((_, s), terms) in by.iter_mut() {
            if *s == sz {
                let mut seen = BTreeSet::new();
                terms.retain(|t| seen.insert(t.to_string()));
            }
        }
    }
    by.into_values().flatten().collect()
}

// --- evaluation (the test oracle) -----------------------------------------------------------------

type Env = HashMap<&'static str, Val>;

fn eval(t: &J, env: &Env) -> Option<Val> {
    match t.get("kind").and_then(|k| k.as_str()) {
        Some("var") => {
            let name = t.get("name").and_then(|n| n.as_str())?;
            if name == "nil" {
                Some(Val::Lst(Vec::new()))
            } else {
                env.get(name).cloned()
            }
        }
        Some("app") => {
            let op = t.get("op").and_then(|o| o.as_str())?;
            let args = t.get("args").and_then(|a| a.as_array())?;
            let ev = |i: usize| eval(args.get(i)?, env);
            match op {
                "reverse" => match ev(0)? {
                    Val::Lst(mut xs) => {
                        xs.reverse();
                        Some(Val::Lst(xs))
                    }
                    _ => None,
                },
                "append" => match (ev(0)?, ev(1)?) {
                    (Val::Lst(mut a), Val::Lst(b)) => {
                        a.extend(b);
                        Some(Val::Lst(a))
                    }
                    _ => None,
                },
                "length" => match ev(0)? {
                    Val::Lst(xs) => Some(Val::Int(xs.len() as i64)),
                    _ => None,
                },
                "cons" => match (ev(0)?, ev(1)?) {
                    (Val::Int(h), Val::Lst(mut t)) => {
                        t.insert(0, h);
                        Some(Val::Lst(t))
                    }
                    _ => None,
                },
                "head" => match ev(0)? {
                    Val::Lst(xs) => xs.first().map(|h| Val::Int(*h)), // None on empty: a partial term
                    _ => None,
                },
                "tail" => match ev(0)? {
                    Val::Lst(xs) if !xs.is_empty() => Some(Val::Lst(xs[1..].to_vec())),
                    Val::Lst(_) => None, // tail of nil is partial
                    _ => None,
                },
                "null" => match ev(0)? {
                    Val::Lst(xs) => Some(Val::Bool(xs.is_empty())),
                    _ => None,
                },
                "add" => match (ev(0)?, ev(1)?) {
                    (Val::Int(a), Val::Int(b)) => Some(Val::Int(a + b)),
                    _ => None,
                },
                _ => None,
            }
        }
        _ => None,
    }
}

/// The fixed test battery: assignments to `xs` and `ys`. Asymmetric pairs (so `xs`/`ys` and
/// `append(xs, ys)`/`append(ys, xs)` are distinguished) and edge cases (`nil`, singletons, repeats).
fn test_envs() -> Vec<Env> {
    let lists: &[(&[i64], &[i64])] = &[
        (&[], &[]),
        (&[1], &[]),
        (&[], &[2]),
        (&[1], &[2]),
        (&[1, 2], &[3]),
        (&[3], &[1, 2]),
        (&[1, 2, 3], &[4, 5]),
        (&[5, 4], &[3, 2, 1]),
        (&[1, 1], &[2, 2]),
        (&[7, 8, 9], &[7, 8, 9]),
        (&[2, 1], &[1, 2]),
        (&[0], &[0, 0]),
    ];
    lists
        .iter()
        .map(|(xs, ys)| {
            let mut e: Env = HashMap::new();
            e.insert("xs", Val::Lst(xs.to_vec()));
            e.insert("ys", Val::Lst(ys.to_vec()));
            e
        })
        .collect()
}

/// Variables a term mentions (from `{xs, ys}`), in `forall`-binding order.
fn free_vars(t: &J, out: &mut Vec<String>) {
    match t.get("kind").and_then(|k| k.as_str()) {
        Some("var") => {
            if let Some(n) = t.get("name").and_then(|n| n.as_str()) {
                if (n == "xs" || n == "ys") && !out.iter().any(|v| v == n) {
                    out.push(n.to_string());
                }
            }
        }
        Some("app") => {
            if let Some(args) = t.get("args").and_then(|a| a.as_array()) {
                for a in args {
                    free_vars(a, out);
                }
            }
        }
        _ => {}
    }
}

// --- public API -----------------------------------------------------------------------------------

/// Conjecture candidate lemmas for a goal whose prelude closure is `closure`. Returns named
/// `forall`-quantified equations (`name`, AST), ranked smallest-first and capped at [`MAX_CONJECTURES`].
/// Each is *tested-valid* (holds on the whole battery) but not yet *proved* — the caller must discharge
/// it by induction before assuming it.
pub fn explore_lemmas(closure: &BTreeSet<String>) -> Vec<(String, J)> {
    // Enumerate over exactly the goal's closure operations (so conjectures stay admissible), within the
    // first-order fragment this module supports.
    let ops: Vec<&OpSig> = ALL_OPS.iter().filter(|o| closure.contains(o.name)).collect();
    if ops.is_empty() {
        return Vec::new();
    }
    let terms = enumerate(&ops);
    let envs = test_envs();

    // Bucket *total* terms (defined on every env) by their result vector.
    let mut buckets: HashMap<Vec<Val>, Vec<J>> = HashMap::new();
    for t in terms {
        let sig: Option<Vec<Val>> = envs.iter().map(|e| eval(&t, e)).collect();
        if let Some(sig) = sig {
            buckets.entry(sig).or_default().push(t);
        }
    }

    // Within each bucket of ≥2 agreeing terms, conjecture (representative = other).
    let mut conjectures: Vec<J> = Vec::new();
    for (_sig, mut group) in buckets {
        if group.len() < 2 {
            continue;
        }
        group.sort_by_key(|t| (size(t), t.to_string()));
        let rep = group[0].clone();
        for other in group.into_iter().skip(1) {
            if other == rep {
                continue;
            }
            let mut vars = Vec::new();
            free_vars(&rep, &mut vars);
            free_vars(&other, &mut vars);
            // A useful lemma is universally quantified; a closed equation (no vars) is not a rewrite.
            if vars.is_empty() {
                continue;
            }
            conjectures.push(json!({
                "kind": "forall",
                "vars": vars,
                "body": app("eq", vec![rep.clone(), other]),
            }));
        }
    }

    // Rank smallest-first (cheapest, most general first), dedup, cap.
    conjectures.sort_by_key(|c| (size(c), c.to_string()));
    conjectures.dedup_by_key(|c| c.to_string());
    conjectures.truncate(MAX_CONJECTURES);
    conjectures.into_iter().enumerate().map(|(i, c)| (format!("discovered_{i}"), c)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn closure(ops: &[&str]) -> BTreeSet<String> {
        ops.iter().map(|s| s.to_string()).collect()
    }

    /// Does `t` have the shape `reverse(reverse(v))` for a variable `v`?
    fn is_double_reverse(t: &J) -> bool {
        let one = |x: &J| x.get("op").and_then(|o| o.as_str()) == Some("reverse");
        one(t)
            && t.pointer("/args/0").map(one).unwrap_or(false)
            && t.pointer("/args/0/args/0/kind").and_then(|k| k.as_str()) == Some("var")
    }

    #[test]
    fn rediscovers_reverse_involution_and_reverse_append() {
        // Over {reverse, append}, exploration must conjecture both reverse∘reverse = id and the
        // reverse/append distribution law — the lemmas Layer A hand-codes, here derived from scratch.
        let found = explore_lemmas(&closure(&["reverse", "append"]));
        // reverse-involution: a conjecture equating reverse(reverse(v)) with a bare variable.
        assert!(
            found.iter().any(|(_, c)| {
                let (a, b) = (c.pointer("/body/args/0"), c.pointer("/body/args/1"));
                match (a, b) {
                    (Some(a), Some(b)) => {
                        (is_double_reverse(a) && b.get("kind").and_then(|k| k.as_str()) == Some("var"))
                            || (is_double_reverse(b) && a.get("kind").and_then(|k| k.as_str()) == Some("var"))
                    }
                    _ => false,
                }
            }),
            "expected a reverse-involution conjecture (reverse(reverse(v)) = v)"
        );
        // reverse/append distribution: reverse(append(.,.)) equated with append(reverse(.), reverse(.)).
        assert!(
            found.iter().any(|(_, c)| {
                let s = c.to_string();
                s.contains(r#""op":"reverse"}],"kind":"app","op":"append""#) // append(reverse..,reverse..)
                    && s.contains(r#""op":"append"}],"kind":"app","op":"reverse""#) // reverse(append..)
            }),
            "expected a reverse-over-append distribution conjecture"
        );
    }

    #[test]
    fn conjectures_are_universally_quantified_equations() {
        let found = explore_lemmas(&closure(&["reverse", "append"]));
        assert!(!found.is_empty());
        for (name, c) in &found {
            assert!(name.starts_with("discovered_"));
            assert_eq!(c.get("kind").and_then(|k| k.as_str()), Some("forall"));
            let vars = c.get("vars").and_then(|v| v.as_array()).unwrap();
            assert!(!vars.is_empty(), "a conjecture must quantify at least one variable");
            assert_eq!(c.pointer("/body/op").and_then(|o| o.as_str()), Some("eq"));
        }
    }

    #[test]
    fn empty_closure_yields_nothing() {
        assert!(explore_lemmas(&closure(&[])).is_empty());
    }

    #[test]
    fn every_conjecture_holds_on_the_battery() {
        // Soundness of the *filter*: every returned conjecture must actually agree on all test envs
        // (it's only a conjecture — proof happens later — but it must at least pass the tests).
        let envs = test_envs();
        for (_, c) in explore_lemmas(&closure(&["reverse", "append"])) {
            let body = c.get("body").unwrap();
            let (a, b) = (&body.get("args").unwrap()[0], &body.get("args").unwrap()[1]);
            for e in &envs {
                assert_eq!(eval(a, e), eval(b, e), "conjecture sides disagree on a test env: {c}");
            }
        }
    }
}
