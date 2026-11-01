//! The STAGED rung (2026-10-31): disjunct elimination over contract-law instances, then a final
//! query over the conclusions with the witness terms opaque.
//!
//! Uniqueness of lowest terms (the rational tier) is a proof whose every step the solver decides
//! in milliseconds ALONE — cancellation, Euclid's lemma with its quotient named, antisymmetry of
//! divisibility, the final cancellation — and whose one-shot query it does not decide at all, in
//! any form (measured 2026-10-31, `~/sandbox/uniq_*.smt2`: the explicit witnesses beside the Bézout
//! facts, or beside the quotient constraints, or the law instances in `mod`/`div` form, all unknown
//! at 60 s; the conclusions as bare facts over opaque witnesses, unsat in 20 ms). The one-shot query
//! drowns because the nonlinear hypotheses that justify a conclusion stay in the problem after the
//! conclusion is drawn, and the solver keeps reasoning about them.
//!
//! So this rung stages. A certificate's instances-only form (contracts as instances, no quantifiers)
//! has two kinds of assertion: FACTS (a post instance with no precondition, an unfolding) and
//! CONDITIONAL instances (`(=> pre post)`, a law's `(or ¬A ¬B C)`). The negated goal is split into
//! its branches (an equivalence's two directions; a conjunction's conjuncts). Per branch:
//!
//!   1. **Disjunct elimination.** For each conditional instance `(or D₁ … Dₙ)` and each disjunct,
//!      the solver is asked whether `facts ∧ branch ∧ Dᵢ` is UNSAT — one small query, with the real
//!      operations, WITHOUT the other conditional instances (what drowned the one-shot query). When
//!      every disjunct but one is refuted, the one left is a consequence of the hypotheses and joins
//!      the facts; when all are, the branch is contradictory and closed. Rounds, to a fixpoint: a
//!      conclusion drawn in one round (`p·s = r·q` from the cross law) discharges a premise in the
//!      next (`(|p|·s) mod q = 0` for Euclid's lemma).
//!   2. **The final query.** The branch, the facts that are LINEAR (no product of two non-literal
//!      terms) and the derived conclusions, with `div`/`mod` by a non-literal divisor UNINTERPRETED
//!      — the quotient inside Euclid's lemma's witness `s = q·(x·((|p|·s) div q) + y·s)` is an
//!      opaque term, which is what lets the solver see `s = q·A ∧ q = s·B ⇒ q = s`. A model of the
//!      original is a model of this (facts dropped, operations weakened, derived facts implied), so
//!      UNSAT here is UNSAT there. If the linear final is not unsat the full facts are tried once.
//!
//! Honesty: every derived fact is a consequence the solver itself established (each refutation is
//! a recorded query in the steps script, re-checkable: every `(check-sat)` must answer unsat);
//! nothing the prover's own algebra asserts enters. SAT anywhere means nothing and ends the rung
//! with `None` — the caller keeps UNKNOWN. Budgets bound the queries (a law that will not close
//! must not pay a hundred timeouts).

use std::collections::BTreeSet;

use crate::euclid::{S, parse_all, render, expand_forms};
use crate::prove::{run_smt_secs, SatAnswer};

/// A staged proof: the final query (one `(push 1) … (check-sat) (pop 1)` block per branch, every
/// answer unsat), the stage queries as one re-checkable script, and the counts.
pub struct Staged {
    pub final_smt: String,
    pub steps_smt: String,
    pub branches: usize,
    pub derived: usize,
}

const STEP_SECS: u64 = 2;
const FINAL_SECS: u64 = 15;
const BUDGET_SECS: u64 = 150;
const MAX_ROUNDS: usize = 4;
const MAX_BRANCHES: usize = 6;
const MAX_CONDITIONALS: usize = 96;
const MAX_ASSERTS: usize = 160;
const MAX_CHARS: usize = 120_000;

fn is_form(f: &S, h: &str) -> bool {
    matches!(f, S::List(items) if matches!(items.first(), Some(S::Atom(x)) if x == h))
}

/// The disjuncts of a conditional instance, or `None` for a fact: `(or …)` with two or more
/// disjuncts, `(=> A B)` as `¬A ∨ B`, `(not (and …))` as the negated conjuncts.
fn disjuncts(s: &S) -> Option<Vec<S>> {
    let S::List(items) = s else { return None };
    let top: Vec<S> = match items.first() {
        Some(S::Atom(h)) if h == "or" && items.len() >= 3 => items[1..].to_vec(),
        Some(S::Atom(h)) if h == "=>" && items.len() == 3 => vec![neg(&items[1]), items[2].clone()],
        Some(S::Atom(h)) if h == "not" && items.len() == 2 => match &items[1] {
            S::List(inner) if matches!(inner.first(), Some(S::Atom(x)) if x == "and") && inner.len() >= 3 => inner[1..].iter().map(neg).collect(),
            _ => return None,
        },
        _ => return None,
    };
    // Flattened: a lowered law is a left-nested `(or (or (or A B) C) D)`, one disjunction.
    let mut out = Vec::new();
    for d in top {
        match disjuncts(&d) {
            Some(inner) if matches!(&d, S::List(v) if matches!(v.first(), Some(S::Atom(h)) if h == "or")) => out.extend(inner),
            _ => out.push(d),
        }
    }
    Some(out)
}

fn neg(s: &S) -> S {
    if let S::List(items) = s {
        if items.len() == 2 && matches!(&items[0], S::Atom(h) if h == "not") {
            return items[1].clone();
        }
    }
    S::List(vec![S::Atom("not".into()), s.clone()])
}

/// Does the term look boolean (a connective, a comparison, a truth literal)?
fn boolish(s: &S) -> bool {
    match s {
        S::Atom(a) => a == "true" || a == "false",
        S::List(items) => matches!(items.first(), Some(S::Atom(h)) if matches!(h.as_str(), "and" | "or" | "not" | "=>" | "=" | "<" | "<=" | ">" | ">=" | "distinct" | "ite_bool")),
    }
}

/// The branches of the NEGATED goal, each a list of literals: an equivalence's two directions, a
/// conjunction's conjuncts, a disjunction's joint negation. Bounded; the fallback is the one
/// branch `¬goal`.
fn negated_branches(goal: &S) -> Vec<Vec<S>> {
    fn go(goal: &S, depth: usize) -> Vec<Vec<S>> {
        if depth > 3 {
            return vec![vec![neg(goal)]];
        }
        let S::List(items) = goal else { return vec![vec![neg(goal)]] };
        match items.first() {
            Some(S::Atom(h)) if h == "or" => {
                // ¬(A ∨ B) = ¬A ∧ ¬B: the product of the branches of each negation.
                let mut acc: Vec<Vec<S>> = vec![vec![]];
                for x in &items[1..] {
                    let bs = go(x, depth + 1);
                    let mut next = Vec::new();
                    for a in &acc {
                        for b in &bs {
                            let mut v = a.clone();
                            v.extend(b.iter().cloned());
                            next.push(v);
                        }
                    }
                    acc = next;
                    if acc.len() > MAX_BRANCHES {
                        return vec![vec![neg(goal)]];
                    }
                }
                acc
            }
            Some(S::Atom(h)) if h == "and" => items[1..].iter().flat_map(|x| go(x, depth + 1)).collect(),
            Some(S::Atom(h)) if h == "not" && items.len() == 2 => vec![vec![items[1].clone()]],
            Some(S::Atom(h)) if h == "=>" && items.len() == 3 => vec![vec![items[1].clone(), neg(&items[2])]],
            Some(S::Atom(h)) if h == "=" && items.len() == 3 && (boolish(&items[1]) || boolish(&items[2])) => {
                let (x, y) = (&items[1], &items[2]);
                match (x, y) {
                    // `X = true` IS X: its negation splits as X's does (an equivalence inside).
                    (S::Atom(t), _) if t == "true" => go(y, depth + 1),
                    (_, S::Atom(t)) if t == "true" => go(x, depth + 1),
                    (S::Atom(t), _) if t == "false" => vec![vec![y.clone()]],
                    (_, S::Atom(t)) if t == "false" => vec![vec![x.clone()]],
                    _ => vec![vec![x.clone(), neg(y)], vec![neg(x), y.clone()]],
                }
            }
            _ => vec![vec![neg(goal)]],
        }
    }
    let out = go(goal, 0);
    if out.len() > MAX_BRANCHES { vec![vec![neg(goal)]] } else { out }
}

/// `(mod X M)` / `(div X M)` with a non-literal divisor as `(umod X M)` / `(udiv X M)`; products
/// stay (the final query's reasoning IS the nonlinear `s = q·A ∧ q = s·B ⇒ q = s`).
fn abstract_divmod(s: &S) -> S {
    match s {
        S::Atom(_) => s.clone(),
        S::List(items) => {
            if items.len() == 3 {
                if let S::Atom(h) = &items[0] {
                    if (h == "mod" || h == "div") && !is_literal(&items[2]) {
                        return S::List(vec![S::Atom(format!("u{h}")), abstract_divmod(&items[1]), abstract_divmod(&items[2])]);
                    }
                }
            }
            S::List(items.iter().map(abstract_divmod).collect())
        }
    }
}

/// Is the term an arithmetic composite (`+`, `-` or `*` at its head) — a compound factor?
fn arith_composite(s: &S) -> bool {
    matches!(s, S::List(items) if items.len() >= 3 && matches!(&items[0], S::Atom(h) if h == "+" || h == "-" || h == "*"))
}

/// QUOTIENT ABSTRACTION (2026-11-01): a fact `A = B·C` (either orientation) whose factor `C` (else
/// `B`) is an arithmetic composite — Euclid's lemma's quotient `u·(p·s div q) + v·s` — with that
/// factor a FRESH constant `wq_k`, declared in `decls`. Sound: the fact still holds with the
/// constant set to the factor, so the abstracted set is weaker and its unsat is the original's.
/// Measured on uniqueness as a law on `rat_normalize`: `q = s·K ∧ s = q·K′ ∧ p·s = r·q ∧ q, s ≥ 1
/// ⇒ p = r` is unknown at 120 s with `K`, `K′` the degree-3 quotients and unsat at once with them
/// opaque (the `q = s` step needs only `K·K′ = 1`).
fn abstract_quotient(s: &S, k: &mut usize, decls: &mut String) -> S {
    let S::List(items) = s else { return s.clone() };
    if items.len() != 3 || !matches!(&items[0], S::Atom(h) if h == "=") {
        return s.clone();
    }
    let fresh = |k: &mut usize, decls: &mut String| {
        *k += 1;
        decls.push_str(&format!("(declare-const wq_{k} Int)\n"));
        S::Atom(format!("wq_{k}"))
    };
    for side in [2usize, 1] {
        let S::List(prod) = &items[side] else { continue };
        if prod.len() != 3 || !matches!(&prod[0], S::Atom(h) if h == "*") {
            continue;
        }
        let pos = if arith_composite(&prod[2]) { 2 } else if arith_composite(&prod[1]) { 1 } else { continue };
        let mut p = prod.clone();
        p[pos] = fresh(k, decls);
        let mut out = items.clone();
        out[side] = S::List(p);
        return S::List(out);
    }
    s.clone()
}

fn is_literal(s: &S) -> bool {
    match s {
        S::Atom(a) => !a.is_empty() && a.chars().all(|c| c.is_ascii_digit()),
        S::List(items) => items.len() == 2 && matches!(&items[0], S::Atom(h) if h == "-") && is_literal(&items[1]),
    }
}

/// The symbols a term mentions — constants and function symbols, not operators or numerals — for
/// the relevance slice of a refutation (the facts sharing a symbol with the disjunct). Measured
/// (2026-10-31): the mod premise of Euclid's lemma closes in 0.17 s from the three facts that
/// mention its symbols and is unknown at 30 s with four unrelated inequalities beside them.
fn symbols(s: &S, out: &mut BTreeSet<String>) {
    const OPS: &[&str] = &["and", "or", "not", "=>", "=", "<", "<=", ">", ">=", "+", "-", "*", "div", "mod", "abs", "ite", "true", "false", "distinct", "let", "umod", "udiv"];
    match s {
        S::Atom(a) => {
            if !is_literal(s) && !OPS.contains(&a.as_str()) {
                out.insert(a.clone());
            }
        }
        S::List(items) => items.iter().for_each(|x| symbols(x, out)),
    }
}

/// For a disjunct `(not (= (mod T m) 0))`, the one fact the solver is measured to miss: when a fact
/// in force says `T = Q·m` (either orientation, either factor order), the VALID formula
/// `m ≠ 0 ∧ T = Q·m ⇒ T mod m = 0` — an instance of the division-uniqueness lemma at residue 0,
/// true in SMT-LIB's own semantics, so asserting it needs no discharge. Measured (2026-10-31): the
/// mirror premise `(|r|·q) mod s = 0` from `|p|·s = |r|·q` is unknown at 30 s as stated and unsat
/// in 16 s with the equation flipped; with the hint it is linear.
fn divisibility_hint(d: &S, facts: &[&S]) -> Option<String> {
    let S::List(items) = d else { return None };
    if items.len() != 2 || !matches!(&items[0], S::Atom(h) if h == "not") {
        return None;
    }
    let S::List(eq) = &items[1] else { return None };
    if eq.len() != 3 || !matches!(&eq[0], S::Atom(h) if h == "=") {
        return None;
    }
    let (modt, zero) = if is_literal(&eq[2]) { (&eq[1], &eq[2]) } else { (&eq[2], &eq[1]) };
    if render(zero) != "0" {
        return None;
    }
    let S::List(mt) = modt else { return None };
    if mt.len() != 3 || !matches!(&mt[0], S::Atom(h) if h == "mod") {
        return None;
    }
    let (t, m) = (&mt[1], &mt[2]);
    for f in facts {
        let S::List(fe) = f else { continue };
        if fe.len() != 3 || !matches!(&fe[0], S::Atom(h) if h == "=") {
            continue;
        }
        for (lhs, rhs) in [(&fe[1], &fe[2]), (&fe[2], &fe[1])] {
            if lhs != t {
                continue;
            }
            let S::List(prod) = rhs else { continue };
            if prod.len() != 3 || !matches!(&prod[0], S::Atom(h) if h == "*") {
                continue;
            }
            let quotient = if &prod[1] == m { &prod[2] } else if &prod[2] == m { &prod[1] } else { continue };
            return Some(format!(
                "(assert (=> (and (not (= {m} 0)) (= {t} (* {q} {m}))) (= (mod {t} {m}) 0)))\n",
                m = render(m),
                t = render(t),
                q = render(quotient)
            ));
        }
    }
    None
}

/// Is the disjunct `(not (= (mod T m) 0))` (a divisibility premise, negated)?
fn is_mod_zero_disjunct(d: &S) -> bool {
    let S::List(items) = d else { return false };
    if items.len() != 2 || !matches!(&items[0], S::Atom(h) if h == "not") {
        return false;
    }
    let S::List(eq) = &items[1] else { return false };
    eq.len() == 3
        && matches!(&eq[0], S::Atom(h) if h == "=")
        && eq[1..].iter().any(|x| matches!(x, S::List(m) if m.len() == 3 && matches!(&m[0], S::Atom(h) if h == "mod")))
        && eq[1..].iter().any(|x| render(x) == "0")
}

/// Does the term divide or take a remainder by a non-literal anywhere (a witness equation)?
fn has_divmod(s: &S) -> bool {
    match s {
        S::Atom(_) => false,
        S::List(items) => {
            if items.len() == 3 && matches!(&items[0], S::Atom(h) if h == "div" || h == "mod") && !is_literal(&items[2]) {
                return true;
            }
            items.iter().any(has_divmod)
        }
    }
}

/// Does the term multiply two non-literal factors anywhere?
fn nonlinear(s: &S) -> bool {
    match s {
        S::Atom(_) => false,
        S::List(items) => {
            if matches!(items.first(), Some(S::Atom(h)) if h == "*") && items[1..].iter().filter(|x| !is_literal(x)).count() >= 2 {
                return true;
            }
            items.iter().any(nonlinear)
        }
    }
}

/// Run a query GROUNDED when the ground rung's rewrite applies — every uninterpreted application
/// (a callee, `udiv`/`umod`, a projection of a callee's tuple) a constant per distinct term, logic
/// `QF_NIA` — else as written. Measured (2026-10-31): the final query with the witness quotients as
/// `udiv` applications is unknown at 30 s under `ALL` and unsat in 20 ms with them constants.
/// Returns whether it was unsat and the script that ran (for the re-checkable record).
fn check(script: &str, solver: &str, secs: u64) -> (bool, String) {
    let ran = crate::ground::ackermannize(script).unwrap_or_else(|| script.to_string());
    let ok = matches!(run_smt_secs(&ran, solver, secs), Ok(SatAnswer::Unsat));
    (ok, ran)
}

/// Debug aid (`NL_EUCLID_DEBUG_DIR=<dir>`): the rung's input and why it declined, when it did.
fn debug_note(what: &str, text: &str) {
    if let Ok(dir) = std::env::var("NL_EUCLID_DEBUG_DIR") {
        let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
        let _ = std::fs::write(format!("{dir}/staged-{n}.{what}"), text);
    }
}

/// The rung as a retry over an instances-only certificate (contracts as instances, no quantified
/// axioms, the negated goal the LAST assertion). `Some` when every branch closed.
pub fn retry_staged(smt: &str, solver: &str) -> Option<Staged> {
    let forms = parse_all(smt).ok()?;
    if forms.iter().any(|f| is_form(f, "assert") && render(f).contains("(forall ")) {
        debug_note("declined.txt", "quantified axioms present");
        return None; // quantified axioms present: not the instances-only form this rung stages
    }
    debug_note("input.smt2", smt);
    let header: String = forms
        .iter()
        .filter(|f| !is_form(f, "assert") && !is_form(f, "check-sat") && !is_form(f, "get-model"))
        .map(render)
        .collect::<Vec<_>>()
        .join("\n");
    let asserted: Vec<S> = expand_forms(&forms);
    // A size gate: the rung is for a checker's instance-shaped certificate, not for a recursive
    // definition unfolded into every assertion (megabytes; measured inside a callee's own law
    // derivations, where it cost minutes and could not close).
    let chars: usize = asserted.iter().map(|s| render(s).len()).sum();
    if asserted.len() > MAX_ASSERTS || chars > MAX_CHARS {
        debug_note("declined.txt", &format!("size gate: {} assertions, {chars} chars", asserted.len()));
        return None;
    }
    let (goal_neg, hyps) = asserted.split_last()?;
    // The goal: the last assertion is `(not G)`.
    let goal = match goal_neg {
        S::List(items) if items.len() == 2 && matches!(&items[0], S::Atom(h) if h == "not") => items[1].clone(),
        other => neg(other),
    };
    let branches = negated_branches(&goal);
    let mut facts: Vec<S> = Vec::new();
    let mut conditionals: Vec<Vec<S>> = Vec::new();
    for h in hyps {
        match disjuncts(h) {
            Some(ds) if conditionals.len() < MAX_CONDITIONALS => conditionals.push(ds),
            Some(_) => facts.push(h.clone()),
            None => facts.push(h.clone()),
        }
    }
    if conditionals.is_empty() {
        debug_note("declined.txt", "no conditional instances");
        return None;
    }
    // The conditionals that share a symbol with the goal first: the budget goes to the instances
    // the goal's terms appear in before the ones about other terms (measured: the gcd laws' many
    // instances exhausted a budget before Euclid's lemma's instance was reached).
    let mut goal_syms = BTreeSet::new();
    symbols(&goal, &mut goal_syms);
    conditionals.sort_by_key(|ds| {
        let mut fs = BTreeSet::new();
        ds.iter().for_each(|d| symbols(d, &mut fs));
        if fs.iter().any(|x| goal_syms.contains(x)) { 0 } else { 1 }
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(BUDGET_SECS);
    let mut steps = String::new();
    steps.push_str(";; the staged rung: each script below (separated by (reset)) refutes one disjunct of a conditional instance under\n;; the facts in force (a relevance slice of the certificate's unconditional facts, the goal's branch, the conclusions\n;; drawn before — grounded to QF_NIA where the ground rung's rewrite applies); every (check-sat) must answer unsat.\n;; A conditional all of whose disjuncts but one are refuted yields the one left as a fact.\n");
    let mut fin = String::new();
    fin.push_str(";; the staged rung's final queries, one script per branch of the negated goal (separated by (reset)): the branch,\n;; the certificate's LINEAR facts and the conclusions drawn in the stages (steps script), with div/mod by a non-literal\n;; divisor uninterpreted and every uninterpreted application a constant (the ground rung's rewrite).\n");
    let udecl = "(declare-fun umod (Int Int) Int)\n(declare-fun udiv (Int Int) Int)\n";
    let mut derived_total = 0usize;
    for (bi, branch) in branches.iter().enumerate() {
        let mut in_force: Vec<S> = facts.clone();
        in_force.extend(branch.iter().cloned());
        let mut derived: Vec<S> = Vec::new();
        let mut resolved: BTreeSet<usize> = BTreeSet::new();
        let mut closed = false;
        // A disjunct that could not be refuted, with the number of conclusions in force then: it is
        // tried again only once more have been drawn (measured: the retries cost a round its budget).
        let mut failed_at: std::collections::BTreeMap<(usize, usize), usize> = std::collections::BTreeMap::new();
        steps.push_str(&format!(";; branch {} of {}: {}\n", bi + 1, branches.len(), branch.iter().map(render).collect::<Vec<_>>().join(" ∧ ")));
        'rounds: for _ in 0..MAX_ROUNDS {
            let mut progress = false;
            // Conditionals a round derives (a law instance behind a refuted domain guard), added
            // for the next round.
            let mut pending: Vec<Vec<S>> = Vec::new();
            // Two fact sets per refutation: the LINEAR facts with the conclusions drawn so far first
            // (the mod premise of Euclid's lemma closes in 0.4 s from `p·s = r·q` alone and drowns
            // beside the Bézout products — measured), the full set second.
            let full: String = in_force.iter().chain(derived.iter()).map(|f| format!("(assert {})\n", render(f))).collect();
            let small: String = in_force
                .iter()
                .filter(|f| !nonlinear(f) || branch.contains(f))
                .chain(derived.iter())
                .map(|f| format!("(assert {})\n", render(f)))
                .collect();
            // Smaller still: without the goal's branch literals (the negated conjunction `¬(p = r ∧
            // q = s)` alone slows the mod premise from 0.16 s to 9 s — measured).
            let tiny: String = in_force
                .iter()
                .filter(|f| !nonlinear(f) && !branch.contains(f))
                .chain(derived.iter())
                .map(|f| format!("(assert {})\n", render(f)))
                .collect();
            for (ci, ds) in conditionals.iter().enumerate() {
                if resolved.contains(&ci) {
                    continue;
                }
                if std::time::Instant::now() >= deadline {
                    steps.push_str(";; the budget ran out; the remaining conditionals were not attempted\n");
                    break 'rounds;
                }
                let mut left: Vec<&S> = Vec::new();
                let mut refuted = 0usize;
                let started = std::time::Instant::now();
                let mut queries = 0usize;
                for (di, d) in ds.iter().enumerate() {
                    // A disjunct already a fact in force is not refutable; a disjunct whose negation
                    // is in force is refuted without a query.
                    if in_force.iter().chain(derived.iter()).any(|f| f == d) {
                        left.push(d);
                        continue;
                    }
                    let nd = neg(d);
                    if in_force.iter().chain(derived.iter()).any(|f| *f == nd) {
                        refuted += 1;
                        continue;
                    }
                    // One disjunct stands already and this one is nonlinear (an instance's
                    // conclusion, typically): refuting it is the expensive query the rung does not
                    // need — the instance is undecided this round either way.
                    if left.len() == 1 && nonlinear(d) {
                        left.push(d);
                        break;
                    }
                    // Failed before with the same conclusions in force: nothing new to refute it with.
                    if failed_at.get(&(ci, di)) == Some(&derived.len()) {
                        left.push(d);
                        if left.len() > 1 {
                            break;
                        }
                        continue;
                    }
                    let mut done = false;
                    let mut tried: Vec<&String> = Vec::new();
                    // The relevance slice first: the linear facts and conclusions that share a
                    // symbol with the disjunct.
                    let mut syms = BTreeSet::new();
                    symbols(d, &mut syms);
                    let slice = |with_witnesses: bool| -> String {
                        in_force
                            .iter()
                            .filter(|f| !nonlinear(f) && !branch.contains(f))
                            .chain(derived.iter().filter(|f| with_witnesses || !has_divmod(f)))
                            .filter(|f| {
                                let mut fs = BTreeSet::new();
                                symbols(f, &mut fs);
                                fs.iter().any(|x| syms.contains(x))
                            })
                            .map(|f| format!("(assert {})\n", render(f)))
                            .collect()
                    };
                    // Without the witness equations drawn so far first (one Euclid instance's
                    // `s = q·(x·((|p|·s) div q) + y·s)` poisons the next instance's mod premise).
                    let sliced_plain = slice(false);
                    let sliced = slice(true);
                    let all_facts: Vec<&S> = in_force.iter().chain(derived.iter()).collect();
                    let hint_opt = divisibility_hint(d, &all_facts);
                    let hint = hint_opt.clone().unwrap_or_default();
                    // A `mod … = 0` premise without a product fact to read it from (a law instance at
                    // a candidate that is not the one) gets one cheap tier, not five — measured: six
                    // such instances cost the budget before the right one was reached.
                    let tiers: Vec<&String> = if is_mod_zero_disjunct(d) && hint_opt.is_none() {
                        vec![&sliced_plain]
                    } else {
                        vec![&sliced_plain, &sliced, &tiny, &small, &full]
                    };
                    for base in tiers {
                        if tried.iter().any(|t| *t == base) {
                            continue;
                        }
                        tried.push(base);
                        let base = format!("{base}{hint}");
                        let q = format!("{header}\n{base}(assert {})\n(check-sat)\n", render(d));
                        queries += 1;
                        let (ok, ran) = check(&q, solver, STEP_SECS);
                        if ok {
                            refuted += 1;
                            steps.push_str(&format!("{ran}(reset)\n"));
                            done = true;
                            break;
                        }
                    }
                    if !done {
                        failed_at.insert((ci, di), derived.len());
                        left.push(d);
                    }
                    if left.len() > 1 {
                        break; // two disjuncts stand: nothing to conclude from this instance yet
                    }
                }
                let ms = started.elapsed().as_millis();
                if ms > 500 {
                    steps.push_str(&format!(";; timing: conditional {ci} ({} disjuncts) {ms} ms, {queries} queries, {refuted} refuted, {} left — {}\n", ds.len(), left.len(), render(&ds[ds.len() - 1]).chars().take(120).collect::<String>()));
                }
                if refuted == 0 {
                    continue;
                }
                match left.len() {
                    0 => {
                        // Every disjunct refuted: the branch's facts are contradictory.
                        steps.push_str(";; every disjunct refuted — the branch is closed\n");
                        resolved.insert(ci);
                        closed = true;
                        break 'rounds;
                    }
                    1 if refuted == ds.len() - 1 => {
                        let c = left[0].clone();
                        steps.push_str(&format!(";; derived: {}\n", render(&c)));
                        resolved.insert(ci);
                        derived_total += 1;
                        progress = true;
                        // A conclusion that is itself a disjunction — a law instance whose domain guard
                        // was the refuted disjunct (`(=> (and (>= a 0) (>= b 0)) (or …))`, Euclid's
                        // lemma at a nat callee) — is one more conditional to eliminate.
                        match disjuncts(&c) {
                            Some(more) if conditionals.len() + pending.len() < MAX_CONDITIONALS => pending.push(more),
                            _ => derived.push(c),
                        }
                    }
                    _ => {}
                }
            }
            conditionals.extend(pending);
            if !progress {
                break;
            }
        }
        // The final query of this branch, in tiers (each over opaque div/mod): the branch, the
        // linear facts and the conclusions that are linear, carry a witness (`div`/`mod`) or equate
        // two products (the cross law's `p·s = r·q`) — the shape measured to close in 20 ms; then
        // every conclusion; then every fact.
        let linear: Vec<&S> = in_force.iter().filter(|f| !nonlinear(f) || branch.contains(f)).collect();
        // `quotients`: the second pass, with compound quotient factors opaque (`abstract_quotient`).
        let tier_with = |extra: &[&S], quotients: bool| -> String {
            let mut block = String::new();
            let mut decls = String::new();
            let mut k = 0usize;
            for f in linear.iter().chain(extra.iter()) {
                let f = abstract_divmod(f);
                let f = if quotients { abstract_quotient(&f, &mut k, &mut decls) } else { f };
                block.push_str(&format!("(assert {})\n", render(&f)));
            }
            format!("{header}\n{udecl}{decls}{block}(check-sat)\n")
        };
        let tier = |extra: &[&S]| tier_with(extra, false);
        // The conclusions drawn LAST are the deepest consequences (the witness equations and the
        // cross equality come after the posts and the callee facts they rest on): the tiers grow
        // from them — every linear conclusion plus the last 4, 8, 16 nonlinear ones, then every
        // conclusion, then every fact. Measured: the six-fact final closes in 20 ms, the same with
        // every conclusion beside it does not.
        let lin_derived: Vec<&S> = derived.iter().filter(|f| !nonlinear(f)).collect();
        let nonlin_derived: Vec<&S> = derived.iter().filter(|f| nonlinear(f)).collect();
        let mut tiers: Vec<Vec<&S>> = Vec::new();
        for k in [4usize, 8, 16] {
            let start = nonlin_derived.len().saturating_sub(k);
            let mut t = lin_derived.clone();
            t.extend(nonlin_derived[start..].iter().cloned());
            if tiers.last().map(|p: &Vec<&S>| p.len()) != Some(t.len()) {
                tiers.push(t);
            }
        }
        tiers.push(derived.iter().collect());
        tiers.push(in_force.iter().filter(|f| nonlinear(f) && !branch.contains(f)).chain(derived.iter()).collect());
        // A branch whose facts the eliminations found contradictory (`closed`) still gets its final
        // run: the written final must stand on its own for a re-checker, and the tier with every
        // fact holds the contradiction the refutations found (measured: a final written unrun for
        // a closed branch re-checked `sat`).
        let _ = closed;
        let mut ok = false;
        let mut ran = tier(&tiers[0]);
        let mut first_ran: Option<String> = None;
        if !ok {
            for extra in &tiers {
                let q = tier(extra);
                let (k, r) = check(&q, solver, FINAL_SECS);
                if first_ran.is_none() {
                    first_ran = Some(r.clone());
                }
                ran = r;
                if k {
                    ok = true;
                    break;
                }
            }
        }
        // The second pass: the same tiers with compound quotients opaque — only when the first
        // closed none (a weaker query; the first pass's finals stay the ones written when they close).
        if !ok {
            for extra in &tiers {
                let q = tier_with(extra, true);
                if q == tier(extra) {
                    continue;
                }
                let (k, r) = check(&q, solver, FINAL_SECS);
                ran = r;
                if k {
                    ok = true;
                    break;
                }
            }
        }
        if !ok {
            if let Some(f) = &first_ran {
                debug_note("final0.smt2", f);
            }
        }
        if !ok {
            if let Ok(dir) = std::env::var("NL_EUCLID_DEBUG_DIR") {
                let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
                let _ = std::fs::write(format!("{dir}/staged-{n}.final.smt2"), &ran);
                let _ = std::fs::write(format!("{dir}/staged-{n}.steps.smt2"), &steps);
            }
            return None;
        }
        fin.push_str(&format!(";; branch {} of {}\n{ran}(reset)\n", bi + 1, branches.len()));
    }
    Some(Staged { final_smt: fin, steps_smt: steps, branches: branches.len(), derived: derived_total })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_compound_quotient_becomes_a_fresh_constant_and_the_mutual_division_final_closes() {
        let (mut k, mut decls) = (0usize, String::new());
        let f = parse_all("(= q (* s (+ (* u w) (* v q))))").unwrap().remove(0);
        assert_eq!(render(&abstract_quotient(&f, &mut k, &mut decls)), "(= q (* s wq_1))");
        let f2 = parse_all("(= (* (+ a b) s) q)").unwrap().remove(0);
        assert_eq!(render(&abstract_quotient(&f2, &mut k, &mut decls)), "(= (* wq_2 s) q)");
        // A product of atoms, or no product, is left alone.
        let f3 = parse_all("(= (* p s) (* r q))").unwrap().remove(0);
        assert_eq!(render(&abstract_quotient(&f3, &mut k, &mut decls)), "(= (* p s) (* r q))");
        assert_eq!(decls, "(declare-const wq_1 Int)\n(declare-const wq_2 Int)\n");
        // The measured shape: unsat with the quotients opaque (q = s follows from K·K′ = 1).
        let smt = "(declare-const p Int)(declare-const q Int)(declare-const r Int)(declare-const s Int)\
                   (declare-const wq_1 Int)(declare-const wq_2 Int)(assert (>= q 1))(assert (>= s 1))\
                   (assert (= (* p s) (* r q)))(assert (not (= p r)))(assert (= q (* s wq_1)))(assert (= s (* q wq_2)))(check-sat)";
        assert!(matches!(run_smt_secs(smt, "z3", 10), Ok(SatAnswer::Unsat)));
    }

    #[test]
    fn an_equivalence_splits_into_its_two_directions() {
        let g = parse_all("(= (f a) (and (= p r) (= q s)))").unwrap().remove(0);
        let bs = negated_branches(&g);
        assert_eq!(bs.len(), 2);
        assert_eq!(render(&bs[0][0]), "(f a)");
        assert_eq!(render(&bs[0][1]), "(not (and (= p r) (= q s)))");
        assert_eq!(render(&bs[1][0]), "(not (f a))");
        // A guarded law: ¬(b < 1 ∨ d < 1 ∨ G) = b ≥ 1 ∧ d ≥ 1 ∧ ¬G, with G's own split.
        let g2 = parse_all("(or (< b 1) (< d 1) (= (f a) (and (= p r) (= q s))))").unwrap().remove(0);
        let bs2 = negated_branches(&g2);
        assert_eq!(bs2.len(), 2);
        assert_eq!(bs2[0].len(), 4);
        assert_eq!(render(&bs2[0][0]), "(not (< b 1))");
        // The law as the prover states it: `… || self(…) == true` with self expanded — `X = true`
        // splits as X does (one branch here hid the equivalence's two directions, measured).
        let g3 = parse_all("(or (or (< b 1) (< d 1)) (= (= (f a) (and (= p r) (= q s))) true))").unwrap().remove(0);
        let bs3 = negated_branches(&g3);
        assert_eq!(bs3.len(), 2);
        assert_eq!(render(&bs3[0][2]), "(f a)");
        assert_eq!(render(&bs3[1][2]), "(not (f a))");
    }

    #[test]
    fn conditionals_are_disjunctions_and_implications() {
        let imp = parse_all("(=> (not (= d 0)) (>= q 1))").unwrap().remove(0);
        assert_eq!(disjuncts(&imp).unwrap().iter().map(render).collect::<Vec<_>>(), vec!["(= d 0)", "(>= q 1)"]);
        let law = parse_all("(or (= d 0) (not (= (* a d) (* c b))) (= (* p s) (* r q)))").unwrap().remove(0);
        assert_eq!(disjuncts(&law).unwrap().len(), 3);
        // A lowered law is left-nested: one disjunction of four, not two of two.
        let nested = parse_all("(or (or (or (= b 0) (= d 0)) (not (= (* a d) (* c b)))) (= (* p s) (* r q)))").unwrap().remove(0);
        assert_eq!(disjuncts(&nested).unwrap().len(), 4);
        let fact = parse_all("(= (* p b) (* a q))").unwrap().remove(0);
        assert!(disjuncts(&fact).is_none());
        assert!(nonlinear(&fact));
        assert!(!nonlinear(&parse_all("(>= q 1)").unwrap().remove(0)));
    }

    /// The hand-measured uniqueness chain as this rung sees it (`~/sandbox/uniq_h2.smt2`, unknown at
    /// 60 s in one shot): the cross law and Euclid's lemma as conditional instances, the goal an
    /// equivalence. Staged, both branches close.
    #[test]
    fn uniqueness_of_lowest_terms_closes_staged() {
        let Some(solver) = ["z3", "cvc5"].into_iter().find(|s| std::process::Command::new(s).arg("--version").output().map(|o| o.status.success()).unwrap_or(false)) else { return };
        let smt = "(set-logic ALL)
(declare-const a Int) (declare-const b Int) (declare-const c Int) (declare-const d Int)
(declare-const p Int) (declare-const q Int) (declare-const r Int) (declare-const s Int)
(declare-const x1 Int) (declare-const y1 Int) (declare-const x2 Int) (declare-const y2 Int)
(declare-const g1 Int) (declare-const g2 Int)
(declare-const e Bool)
(assert (not (= b 0)))
(assert (not (= d 0)))
(assert (=> (not (= b 0)) (>= q 1)))
(assert (=> (not (= d 0)) (>= s 1)))
(assert (= (* p b) (* a q)))
(assert (= (* r d) (* c s)))
(assert (= e (= (* a d) (* c b))))
(assert (= g1 1)) (assert (= g2 1))
(assert (= (+ (* (abs p) x1) (* q y1)) g1))
(assert (= (+ (* (abs r) x2) (* s y2)) g2))
(assert (or (= b 0) (= d 0) (not (= (* a d) (* c b))) (= (* p s) (* r q))))
(assert (or (= b 0) (= d 0) (not (= (* a d) (* c b))) (= (* (abs p) s) (* (abs r) q))))
(assert (or (= q 0) (not (= g1 1)) (not (= (mod (* (abs p) s) q) 0)) (= s (* q (+ (* x1 (div (* (abs p) s) q)) (* y1 s))))))
(assert (or (= s 0) (not (= g2 1)) (not (= (mod (* (abs r) q) s) 0)) (= q (* s (+ (* x2 (div (* (abs r) q) s)) (* y2 q))))))
(assert (not (= e (and (= p r) (= q s)))))
(check-sat)
";
        let st = retry_staged(smt, solver).expect("the staged rung closes uniqueness");
        assert_eq!(st.branches, 2);
        assert!(st.derived >= 3, "derived {}", st.derived);
        assert!(st.final_smt.contains("(udiv "), "{}", st.final_smt);
    }
}
