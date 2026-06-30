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
//!
//! **Relevance-guided selection.** Raising the cap to reach a truncated lemma multiplies proof cost on
//! *every* goal (each conjecture is discharged by induction — a measured 4× regression). Instead, when
//! the goal is supplied, a few conjectures from *beyond* the size cap are **promoted** because they are
//! relevant to it: they share a non-trivial operator skeleton (`reverse(append(_,_))`, …) with the terms
//! the goal equates, so they can actually fire there as a rewrite. The smallest-`MAX_CONJECTURES` base is
//! never reordered or dropped (no regression — the reverted nil-ranking experiment demoted the essential
//! `append_nil`), so promotion only *adds* reach for a goal the cap would otherwise starve, bounded at
//! `RELEVANCE_PROMOTE`. Soundness is unchanged: a promoted conjecture is still proved before it is used.

use serde_json::{json, Value as J};
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// Largest term (node count) the enumerator builds. 5 reaches `reverse(append(xs, ys))` and the
/// associativity instances while keeping the term set in the low hundreds.
const MAX_TERM_SIZE: usize = 5;
/// Cap on conjectures handed back (ranked smallest-first), bounding how many proofs the caller attempts.
/// Large enough to retain the standard distribution laws (`reverse_append`, `length_append` are ~size
/// 10) while the caller's early-stop keeps the common case from paying for the whole list.
const MAX_CONJECTURES: usize = 32;
/// How many extra conjectures, drawn from *beyond* the size cap, may be **promoted** because they are
/// relevant to the goal (share a non-trivial operator shape with it). This is the "beyond the conjecture
/// cap" lever: rather than raise `MAX_CONJECTURES` globally (which multiplies proof cost on *every* goal —
/// a measured 4× regression), reach past the cap only for the few lemmas the goal actually needs. The
/// smallest-`MAX_CONJECTURES` base is never reordered or dropped, so goals that close within it are
/// unaffected; promotion only adds reach for goals the cap would otherwise starve.
const RELEVANCE_PROMOTE: usize = 8;
/// Smallest subterm (node count) whose operator-skeleton counts toward relevance. A bare `reverse(_)`
/// (size 2) is too generic to signal that a lemma is *about* the goal; a two-operator nest like
/// `reverse(reverse(_))` or `reverse(append(_,_))` (size ≥ 3) is.
const MIN_RELEVANT_SHAPE: usize = 3;

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

// --- goal relevance -------------------------------------------------------------------------------

/// The operator-skeleton of a term: every leaf (variable, `nil`, literal) abstracted to `_`, operator
/// nesting preserved — `reverse(append(xs, ys))` → `reverse(append(_,_))`. Two terms with the same
/// skeleton have the same operator structure regardless of which variables fill the leaves, so a skeleton
/// is a cheap "same shape" key for matching a lemma against the goal.
fn skeleton(t: &J) -> String {
    match t.get("kind").and_then(|k| k.as_str()) {
        Some("app") => {
            let op = t.get("op").and_then(|o| o.as_str()).unwrap_or("?");
            let args: Vec<String> =
                t.get("args").and_then(|a| a.as_array()).map(|a| a.iter().map(skeleton).collect()).unwrap_or_default();
            format!("{op}({})", args.join(","))
        }
        _ => "_".to_string(),
    }
}

/// Collect `skeleton → size` for every subterm of `t` rooted at an application whose size is ≥
/// `MIN_RELEVANT_SHAPE` (smaller nests are too generic to signal relevance). Keeps the largest size seen
/// for a repeated skeleton.
fn collect_skeletons(t: &J, out: &mut BTreeMap<String, usize>) {
    if t.get("kind").and_then(|k| k.as_str()) == Some("app") && size(t) >= MIN_RELEVANT_SHAPE {
        let sz = size(t);
        out.entry(skeleton(t)).and_modify(|e| *e = (*e).max(sz)).or_insert(sz);
    }
    if let Some(args) = t.get("args").and_then(|a| a.as_array()) {
        for a in args {
            collect_skeletons(a, out);
        }
    }
}

/// The two terms an equation relates: the operands of the `eq` predicate of a `forall eq(L, R)` goal or a
/// bare `eq(L, R)`. Relevance is judged on these *terms* — not the `eq`/`forall` wrapper, which every
/// goal and conjecture share and which would otherwise create spurious matches.
fn equation_operands(node: &J) -> Vec<J> {
    let pred = if node.get("kind").and_then(|k| k.as_str()) == Some("forall") {
        node.get("body")
    } else {
        Some(node)
    };
    pred.filter(|p| p.get("op").and_then(|o| o.as_str()) == Some("eq"))
        .and_then(|p| p.get("args").and_then(|a| a.as_array()).cloned())
        .unwrap_or_default()
}

/// The goal's relevant operator-skeletons (`skeleton → size`), taken from the terms it equates.
fn goal_skeletons(goal: &J) -> BTreeMap<String, usize> {
    let mut out = BTreeMap::new();
    for operand in equation_operands(goal) {
        collect_skeletons(&operand, &mut out);
    }
    out
}

/// Relevance of a conjecture to the goal: the size of the largest non-trivial operator-skeleton its
/// equated terms share with the goal's (0 if none). A conjecture that manipulates a structure the goal
/// actually contains can fire there as a rewrite; one sharing nothing only bloats the SMT context.
fn relevance(conj: &J, goal_skels: &BTreeMap<String, usize>) -> usize {
    let mut cs = BTreeMap::new();
    for operand in equation_operands(conj) {
        collect_skeletons(&operand, &mut cs);
    }
    cs.keys().filter_map(|k| goal_skels.get(k)).copied().max().unwrap_or(0)
}

// --- public API -----------------------------------------------------------------------------------

/// Conjecture candidate lemmas for a goal whose prelude closure is `closure`. Returns named
/// `forall`-quantified equations (`name`, AST). The smallest [`MAX_CONJECTURES`] always come first
/// (ranked smallest-first — the original behavior, never reordered or dropped). When `goal` is supplied,
/// up to [`RELEVANCE_PROMOTE`] further conjectures from *beyond* that cap are appended **because they are
/// relevant to the goal** (they share a non-trivial operator shape with the terms it equates) — the
/// relevance-guided way to reach a lemma the cap would otherwise starve, without globally raising it.
/// Each conjecture is *tested-valid* (holds on the whole battery) but not yet *proved* — the caller must
/// discharge it by induction before assuming it, so promotion can only add reach, never unsoundness.
pub fn explore_lemmas(closure: &BTreeSet<String>, goal: Option<&J>) -> Vec<(String, J)> {
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

    // Rank smallest-first (cheapest, most general first), dedup.
    conjectures.sort_by_key(|c| (size(c), c.to_string()));
    conjectures.dedup_by_key(|c| c.to_string());

    // Base: the smallest `MAX_CONJECTURES` — exactly the original output. Never reordered or dropped, so a
    // goal that already closed within the cap is byte-for-byte unaffected (no regression, the lesson from
    // the reverted nil-ranking experiment that demoted the essential `append_nil`).
    let mut selected: Vec<J> = conjectures.iter().take(MAX_CONJECTURES).cloned().collect();

    // Relevance promotion: append up to `RELEVANCE_PROMOTE` conjectures from BEYOND the cap that are
    // relevant to the goal — most relevant first, smallest to break ties. These run only after the base is
    // exhausted (the caller tries conjectures in order and early-stops), so they cost nothing on goals that
    // close within the cap, and reach the few extra lemmas a starved goal needs.
    if let (Some(goal), true) = (goal, conjectures.len() > MAX_CONJECTURES) {
        let goal_skels = goal_skeletons(goal);
        if !goal_skels.is_empty() {
            let mut promoted: Vec<(usize, usize, String, J)> = conjectures[MAX_CONJECTURES..]
                .iter()
                .filter_map(|c| {
                    let r = relevance(c, &goal_skels);
                    (r > 0).then(|| (r, size(c), c.to_string(), c.clone()))
                })
                .collect();
            promoted.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)).then_with(|| a.2.cmp(&b.2)));
            promoted.truncate(RELEVANCE_PROMOTE);
            selected.extend(promoted.into_iter().map(|(_, _, _, c)| c));
        }
    }
    selected.into_iter().enumerate().map(|(i, c)| (format!("discovered_{i}"), c)).collect()
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
        let found = explore_lemmas(&closure(&["reverse", "append"]), None);
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
        let found = explore_lemmas(&closure(&["reverse", "append"]), None);
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
        assert!(explore_lemmas(&closure(&[]), None).is_empty());
    }

    #[test]
    fn relevance_promotes_beyond_the_cap_without_demoting_the_base() {
        // A rich closure yields more conjectures than the size cap, so the goal-agnostic output is exactly
        // `MAX_CONJECTURES` smallest-first. A goal that mentions a distinctive nested shape
        // (`reverse(append(_,_))` / `append(reverse(_),_)`) must (a) leave that base byte-for-byte intact —
        // never demote it (the nil-ranking lesson) — and (b) APPEND relevant conjectures from beyond the
        // cap, each genuinely sharing a non-trivial skeleton with the goal.
        let cl = closure(&["reverse", "append", "length", "add"]);
        let base = explore_lemmas(&cl, None);
        assert_eq!(base.len(), MAX_CONJECTURES, "rich closure should fill the cap (got {})", base.len());

        // `reverse(append(xs, ys)) = append(reverse(ys), reverse(xs))` — the reverse/append distribution
        // shape, whose conjectures are large (~size 10) and land beyond the smallest-cap base.
        let goal = json!({ "kind": "forall", "vars": ["xs", "ys"], "body": app("eq", vec![
            app("reverse", vec![app("append", vec![var("xs"), var("ys")])]),
            app("append", vec![app("reverse", vec![var("ys")]), app("reverse", vec![var("xs")])]),
        ]) });
        let promoted = explore_lemmas(&cl, Some(&goal));

        // (a) Base preserved exactly: the first MAX_CONJECTURES conjecture ASTs are identical (only the
        // `discovered_i` names, assigned by position, are the same too).
        let base_asts: Vec<&J> = base.iter().map(|(_, c)| c).collect();
        let promoted_base: Vec<&J> = promoted.iter().take(MAX_CONJECTURES).map(|(_, c)| c).collect();
        assert_eq!(base_asts, promoted_base, "the smallest-cap base must be untouched");

        // (b) Promotion happened, and every promoted conjecture is genuinely goal-relevant.
        assert!(promoted.len() > base.len(), "expected goal-relevant conjectures to be promoted beyond the cap");
        assert!(promoted.len() <= MAX_CONJECTURES + RELEVANCE_PROMOTE);
        let goal_skels = goal_skeletons(&goal);
        for (_, c) in promoted.iter().skip(MAX_CONJECTURES) {
            assert!(relevance(c, &goal_skels) > 0, "a promoted conjecture must share a shape with the goal: {c}");
        }
    }

    #[test]
    fn no_goal_is_identical_to_before() {
        // Without a goal, output is exactly the smallest-first cap — the original behavior, so existing
        // callers/paths are unchanged.
        let cl = closure(&["reverse", "append", "length", "add"]);
        let a = explore_lemmas(&cl, None);
        let b = explore_lemmas(&cl, None);
        assert_eq!(a, b);
        assert!(a.len() <= MAX_CONJECTURES);
    }

    #[test]
    fn every_conjecture_holds_on_the_battery() {
        // Soundness of the *filter*: every returned conjecture must actually agree on all test envs
        // (it's only a conjecture — proof happens later — but it must at least pass the tests).
        let envs = test_envs();
        for (_, c) in explore_lemmas(&closure(&["reverse", "append"]), None) {
            let body = c.get("body").unwrap();
            let (a, b) = (&body.get("args").unwrap()[0], &body.get("args").unwrap()[1]);
            for e in &envs {
                assert_eq!(eval(a, e), eval(b, e), "conjecture sides disagree on a test env: {c}");
            }
        }
    }
}
