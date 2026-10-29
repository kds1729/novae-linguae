//! The Euclidean rung: residue normal forms through instances of the division-uniqueness lemma.
//!
//! Every modular identity this project's number-theory records state — `(a + b) mod m =
//! ((a mod m) + (b mod m)) mod m`, the product congruence, `gcd` dividing both of its arguments,
//! a digit sum bounded by the number — is a consequence of ONE fact about Euclidean division:
//!
//! ```text
//! m ≠ 0 ∧ x = q·m + r ∧ 0 ≤ r < |m|  ⇒  x div m = q ∧ x mod m = r        (uniqueness)
//! ```
//!
//! Measured 2026-10-18: z3 4.13 and 4.16 and cvc5 1.3 all prove the lemma in this form (z3 4.16
//! answers unknown on the `m > 0` form and on the plain shift `(x + m) mod m = x mod m`, so the
//! form is the measurement, not a nicety), and NONE of them proves the sum or product congruence
//! directly: `mod` by a variable is nonlinear, and the solvers do not find the quotient witness.
//! Given the witness — the lemma INSTANTIATED at the right `(x, q, r)` — every one of those goals
//! closes in milliseconds by linear reasoning plus the solver's own `div`/`mod` axioms.
//!
//! So this rung computes the witnesses. For every `(mod t m)` and `(div t m)` in a certificate
//! (with `m` not a literal — literal moduli are linear and the solver's own), `t` is read as a
//! polynomial over the atoms it mentions and each atom `y` is replaced by its own Euclidean
//! decomposition `(div y m)·m + (mod y m)`; expanding, `t = P + m·Q` where `P` (the residue part)
//! mentions only `(mod y m)` atoms and constants. The instance asserted is the lemma at
//! `x = t, q = Q + (P div m), r = P mod m`. The solver verifies the premise (a polynomial identity
//! over its own `div`/`mod` axioms — measured: it does, to degree three) and gains the conclusion
//! `(mod t m) = (mod P m)`: the residue of a polynomial is the residue of the polynomial of the
//! residues, which is the ring homomorphism ℤ → ℤ/mℤ, stated at exactly the terms the goal has.
//! A hypothesis that an atom's residue vanishes (`b mod g = 0`, an induction hypothesis) then
//! kills it inside `P` — the divisibility step of Euclid's algorithm. For such goals the atom
//! must first be rewritten by its decomposition against ANOTHER modulus the certificate mentions
//! (`a = (a div b)·b + (a mod b)` before reducing modulo `g`): the cross-modulus variants below.
//!
//! Honesty: the lemma is discharged by the solver FIRST (cached per solver); no instance is
//! asserted unless it is. Each instance keeps the lemma's premises, so a wrong witness makes the
//! instance vacuous, never unsound — the prover's algebra is not trusted, only the solver's
//! check of it. Instances are true statements, so a model of the augmented certificate is a model
//! of the original: SAT still means what it meant. The rung is a retry on UNKNOWN only.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::prove::{run_smt_secs, SatAnswer};

/// The uniqueness lemma, as the certificate the solver discharges before any instance is used.
pub const LEMMA_SMT: &str = "(set-logic ALL)\n\
;; Euclidean division is unique: m ≠ 0 ∧ x = q·m + r ∧ 0 ≤ r < |m| ⇒ x div m = q ∧ x mod m = r\n\
(declare-const x Int)\n(declare-const m Int)\n(declare-const q Int)\n(declare-const r Int)\n\
(assert (not (= m 0)))\n(assert (= x (+ (* q m) r)))\n(assert (>= r 0))\n(assert (< r (abs m)))\n\
(assert (not (and (= (div x m) q) (= (mod x m) r))))\n(check-sat)\n";

const LEMMA_NAME: &str = "euclid_unique";
const MAX_INSTANCES: usize = 96;
const MAX_TERMS: usize = 64;
const MAX_DEGREE: usize = 6;
const EXPAND_DEPTH: usize = 4;

thread_local! {
    static DISCHARGED: RefCell<HashMap<String, bool>> = RefCell::new(HashMap::new());
}

/// Has `solver` proved the lemma? Run once per solver per thread; a solver that cannot (or is
/// absent) makes the rung unavailable, stated — never assumed.
pub fn lemma_discharged(solver: &str) -> bool {
    if let Some(v) = DISCHARGED.with(|d| d.borrow().get(solver).copied()) {
        return v;
    }
    let ok = matches!(run_smt_secs(LEMMA_SMT, solver, 5), Ok(SatAnswer::Unsat));
    DISCHARGED.with(|d| d.borrow_mut().insert(solver.to_string(), ok));
    ok
}

/// A residue is its own residue: the second lemma the stepwise checker rests on, discharged by
/// the solver before its instances are used in the abstracted final query.
pub const IDEM_SMT: &str = "(set-logic ALL)\n\
;; a residue is its own residue: m ≠ 0 ⇒ (mod (mod x m) m) = (mod x m)\n\
(declare-const x Int)\n(declare-const m Int)\n\
(assert (not (= m 0)))\n(assert (not (= (mod (mod x m) m) (mod x m))))\n(check-sat)\n";

/// Has `solver` proved the idempotence lemma? Cached per solver per thread.
pub fn idem_discharged(solver: &str) -> bool {
    let key = format!("idem:{solver}");
    if let Some(v) = DISCHARGED.with(|d| d.borrow().get(&key).copied()) {
        return v;
    }
    let ok = matches!(run_smt_secs(IDEM_SMT, solver, 5), Ok(SatAnswer::Unsat));
    DISCHARGED.with(|d| d.borrow_mut().insert(key, ok));
    ok
}

/// A stepwise proof: the final query (over uninterpreted `mod`/`div`, with the verified instances'
/// conclusions as facts), the premise queries as one re-checkable script (every `(check-sat)` in
/// it must answer unsat), and how many premises verified of how many were generated.
pub struct Stepwise {
    pub final_smt: String,
    pub steps_smt: String,
    pub verified: usize,
    pub total: usize,
}

/// The STEPWISE checker: the rung for a goal whose residues meet only through the certificate's
/// equations (the fast-exponentiation step), where one query drowns (measured). Three kinds of
/// query, each decidable in practice: (1) every instance's PREMISE — a ring identity over the
/// solver's own `div`/`mod` axioms — checked in isolation, with nothing but the declarations and
/// `m ≠ 0` (measured: the same identity is unsat in 20 ms alone and unknown beside the
/// hypotheses); (2) the lemma and the idempotence lemma, discharged once per solver; (3) the FINAL
/// query: the hypotheses, the verified instances' conclusions as facts (`m ≠ 0 ⇒ …`), the
/// idempotence lemma instantiated at every residue, and the negated goal — with every `mod`/`div`
/// by a non-literal modulus ABSTRACTED to an uninterpreted function. The abstraction is sound (a
/// model of the original is a model of the abstraction, so unsat there is unsat here) and makes
/// the final query congruence plus linear arithmetic, which the solver decides. The instances
/// come with the certificate's equations substituted into the goal (`instances(_, true)`).
pub fn retry_stepwise(smt: &str, solver: &str) -> Option<Stepwise> {
    let insts = instances(smt, true)?;
    if !lemma_discharged(solver) || !idem_discharged(solver) {
        return None;
    }
    let forms = parse_all(smt).ok()?;
    let is_form = |f: &S, h: &str| matches!(f, S::List(items) if matches!(items.first(), Some(S::Atom(x)) if x == h));
    let header: String = forms
        .iter()
        .filter(|f| !is_form(f, "assert") && !is_form(f, "check-sat") && !is_form(f, "get-model"))
        .map(render)
        .collect::<Vec<_>>()
        .join("\n");
    // (1) The premises, each alone — beside the certificate's small facts (preconditions, testers,
    // domain bounds) and, for a witness-rung instance, the hypotheses its rule came from.
    // (Read with `let`s substituted and definitions unfolded, as the instances are: a goal's
    // `let`-bound result hides its tester otherwise.)
    let defs = definitions(&forms);
    let asserted: Vec<S> = forms
        .iter()
        .filter(|f| is_form(f, "assert"))
        .filter_map(|f| match f {
            S::List(items) if items.len() == 2 => Some(expand(&items[1], &BTreeMap::new(), &defs, 0)),
            _ => None,
        })
        .collect();
    let small: String = small_facts(&asserted).iter().map(|f| format!("(assert {f})\n")).collect();
    let mut steps = String::new();
    steps.push_str(";; the Euclidean rung, stepwise: each instance's premise (a ring identity over the solver's div/mod axioms)\n;; checked in isolation — with the certificate's small facts below and, for a witness-rung instance, the\n;; hypotheses its rule came from; every (check-sat) below must answer unsat for the instance to enter the final query.\n");
    steps.push_str(&header);
    steps.push('\n');
    let mut facts: Vec<(String, String)> = Vec::new();
    // A budget: a premise the solver cannot verify costs its whole timeout, and a law that is
    // not going to close must not pay hundreds of them. Past the deadline the remaining
    // instances are left out (stated in the steps script) and the final query runs on what
    // verified.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(STEPWISE_BUDGET_SECS);
    // The witness rung's premises (listed last) have their own, smaller budget: a premise that
    // rests on a hypothesis costs its whole timeout when it does not hold, and a law that is not
    // going to close must not pay thirty of them in every ladder it reaches.
    let mut witness_deadline: Option<std::time::Instant> = None;
    let mut skipped = 0usize;
    for i in &insts {
        let now = std::time::Instant::now();
        if i.witness && witness_deadline.is_none() {
            witness_deadline = Some(now + std::time::Duration::from_secs(WITNESS_BUDGET_SECS));
        }
        if now >= deadline || (i.witness && witness_deadline.is_some_and(|d| now >= d)) {
            skipped += 1;
            continue;
        }
        let uses: String = i.uses.iter().map(|u| format!("(assert {u})\n")).collect();
        let facts_here = if i.witness { small.as_str() } else { "" };
        let q = format!("{header}\n{facts_here}(assert (not (= {} 0)))\n{uses}(assert (not {}))\n(check-sat)\n", i.modulus, i.premise);
        let ok = matches!(run_smt_secs(&q, solver, STEP_SECS), Ok(SatAnswer::Unsat));
        steps.push_str(&format!(
            "(push 1)\n{facts_here}(assert (not (= {} 0)))\n{uses}(assert (not {}))\n(check-sat){}\n(pop 1)\n",
            i.modulus,
            i.premise,
            if ok { "" } else { " ; NOT discharged by the proving solver — the instance was left out of the final query" }
        ));
        if ok {
            facts.push((i.modulus.clone(), i.conclusion.clone()));
        }
    }
    if skipped > 0 {
        steps.push_str(&format!(";; {skipped} instance(s) not attempted: the {STEPWISE_BUDGET_SECS} s budget for premises (or the {WITNESS_BUDGET_SECS} s one for the witness rung's) ran out\n"));
    }
    if facts.is_empty() {
        return None;
    }
    // (3) The final query, abstracted.
    let asserts: Vec<&S> = forms.iter().filter(|f| is_form(f, "assert")).collect();
    let (goal, hyps) = asserts.split_last()?;
    let mut fin = String::new();
    let mut umod_terms: BTreeSet<String> = BTreeSet::new();
    for f in &forms {
        if is_form(f, "set-logic") {
            fin.push_str(&render(f));
            fin.push_str("\n;; the Euclidean rung, stepwise: mod/div by a non-literal modulus are UNINTERPRETED here (a sound abstraction);\n;; the facts below are the verified instances' conclusions and the idempotence lemma at every residue.\n(declare-fun umod (Int Int) Int)\n(declare-fun udiv (Int Int) Int)\n(declare-fun umul (Int Int) Int)\n");
        } else if is_form(f, "assert") || is_form(f, "check-sat") || is_form(f, "get-model") {
            continue;
        } else {
            fin.push_str(&render(&abstract_mod(f)));
            fin.push('\n');
        }
    }
    for h in hyps {
        let a = abstract_mod(h);
        collect_umods(&expand(&a, &BTreeMap::new(), &BTreeMap::new(), 0), &mut umod_terms);
        fin.push_str(&render(&a));
        fin.push('\n');
    }
    for (m, c) in &facts {
        let c = parse_all(c).ok().and_then(|v| v.into_iter().next())?;
        let a = abstract_mod(&c);
        collect_umods(&a, &mut umod_terms);
        let m = render(&abstract_mod(&parse_all(m).ok().and_then(|v| v.into_iter().next())?));
        fin.push_str(&format!("(assert (=> (not (= {m} 0)) {}))\n", render(&a)));
    }
    let goal_a = abstract_mod(goal);
    collect_umods(&expand(&goal_a, &BTreeMap::new(), &BTreeMap::new(), 0), &mut umod_terms);
    for t in &umod_terms {
        let Some(S::List(items)) = parse_all(t).ok().and_then(|v| v.into_iter().next()) else { continue };
        let m = render(&items[2]);
        fin.push_str(&format!("(assert (=> (not (= {m} 0)) (= (umod {t} {m}) {t})))\n"));
    }
    fin.push_str(&render(&goal_a));
    fin.push_str("\n(check-sat)\n");
    // Debug aid: `NL_EUCLID_DEBUG_DIR=<dir>` writes the final and the steps scripts there whether
    // or not the final closes (what a re-checker would see on a failure).
    if let Ok(dir) = std::env::var("NL_EUCLID_DEBUG_DIR") {
        let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
        let _ = std::fs::write(format!("{dir}/stepwise-{n}.final.smt2"), &fin);
        let _ = std::fs::write(format!("{dir}/stepwise-{n}.steps.smt2"), &steps);
    }
    match run_smt_secs(&fin, solver, 10) {
        Ok(SatAnswer::Unsat) => Some(Stepwise { final_smt: fin, steps_smt: steps, verified: facts.len(), total: insts.len() }),
        _ => None,
    }
}

/// `(mod X M)` / `(div X M)` with a non-literal `M` as `(umod X M)` / `(udiv X M)`, and a product
/// of two or more non-literal factors as nested `(umul a b)` (literal coefficients stay, so the
/// arithmetic left is linear), throughout. What remains is congruence plus linear arithmetic —
/// decidable, so the final query answers rather than times out; every identity the instances
/// rest on was verified with the real operations in its own query. Terms are rendered the same
/// way on both sides (the engine's canonical monomials, the certificate's own spellings), so the
/// abstraction keeps matching terms identical.
fn abstract_mod(s: &S) -> S {
    match s {
        S::Atom(_) => s.clone(),
        S::List(items) => {
            if items.len() == 3 {
                if let S::Atom(h) = &items[0] {
                    if (h == "mod" || h == "div") && !is_literal(&items[2]) {
                        return S::List(vec![S::Atom(format!("u{h}")), abstract_mod(&items[1]), abstract_mod(&items[2])]);
                    }
                }
            }
            if matches!(items.first(), Some(S::Atom(h)) if h == "*") {
                let args: Vec<S> = items[1..].iter().map(abstract_mod).collect();
                let (lits, non): (Vec<S>, Vec<S>) = args.into_iter().partition(is_literal);
                if non.len() >= 2 {
                    let mut t = non[0].clone();
                    for x in &non[1..] {
                        t = S::List(vec![S::Atom("umul".into()), t, x.clone()]);
                    }
                    if lits.is_empty() {
                        return t;
                    }
                    let mut v = vec![S::Atom("*".into())];
                    v.extend(lits);
                    v.push(t);
                    return S::List(v);
                }
                let mut v = vec![S::Atom("*".into())];
                v.extend(lits);
                v.extend(non);
                return S::List(v);
            }
            S::List(items.iter().map(abstract_mod).collect())
        }
    }
}

/// Every `(umod X M)` term outside a binder (a term over bound names cannot be instantiated at).
fn collect_umods(s: &S, out: &mut BTreeSet<String>) {
    let S::List(items) = s else { return };
    if let Some(S::Atom(h)) = items.first() {
        if matches!(h.as_str(), "forall" | "exists" | "lambda" | "let") {
            return;
        }
        if h == "umod" && items.len() == 3 {
            out.insert(render(s));
        }
    }
    for x in items {
        collect_umods(x, out);
    }
}

/// The rung as a retry: the certificate augmented with the lemma's instances, when the solver has
/// discharged the lemma, the certificate has `mod`/`div` terms with non-literal moduli, and the
/// augmented certificate is UNSAT. `None` otherwise (the caller keeps its verdict).
pub fn retry(smt: &str, solver: &str) -> Option<String> {
    if !lemma_discharged(solver) {
        return None;
    }
    // The plain instances first (the measured set: three close Euclid's divisibility in 0.1 s),
    // then with the witness rung's — which cost the solver more beside them (the same
    // certificate with two rule instances added: 1.9 s) and are a further retry, not the first.
    let mut last: Option<String> = None;
    for with_rules in [false, true] {
        let Some(augmented) = augment_with(smt, with_rules) else { continue };
        if last.as_deref() == Some(augmented.as_str()) {
            break; // no witness instance to add: the same query again would only cost its budget
        }
        if let Ok(SatAnswer::Unsat) = run_smt_secs(&augmented, solver, 5) {
            return Some(augmented);
        }
        last = Some(augmented);
    }
    None
}

/// The certificate with the uniqueness-lemma instances for its `mod`/`div` terms appended before
/// `(check-sat)`, the witness rung's included. `None` when there is nothing to add.
pub fn augment(smt: &str) -> Option<String> {
    augment_with(smt, true)
}

fn augment_with(smt: &str, with_rules: bool) -> Option<String> {
    let out = instances_with(smt, false, with_rules)?;
    if !with_rules && out.iter().any(|i| !i.uses.is_empty()) {
        unreachable!("plain instances carry no hypotheses");
    }
    let idx = smt.rfind("(check-sat)")?;
    let mut s = String::with_capacity(smt.len() + out.iter().map(|x| x.text.len() + 1).sum::<usize>() + 128);
    s.push_str(&smt[..idx]);
    s.push_str(&format!(";; the Euclidean rung: instances of `{LEMMA_NAME}` (discharged separately) at the certificate's mod/div terms\n"));
    for i in &out {
        s.push_str(&i.text);
        s.push('\n');
    }
    s.push_str(&smt[idx..]);
    Some(s)
}

/// One lemma instance: the modulus, the identity premise `x = (q·m + r)`, the conclusion, and the
/// whole assertion as the one-query rung states it. `uses` lists the certificate's hypotheses the
/// premise rests on (the witness rung below: a quotient that is a combination of a hypothesis —
/// the Bézout identity — holds only modulo that hypothesis), rendered as the assertions they are;
/// the stepwise checker verifies such a premise WITH them, and nothing else beyond the
/// declarations, `m ≠ 0` and the certificate's small facts.
#[derive(Clone, Debug)]
pub struct Instance {
    pub modulus: String,
    pub premise: String,
    pub conclusion: String,
    pub text: String,
    pub uses: Vec<String>,
    /// The witness rung's: after a rule step, or through a foreign-modulus lift — the instances
    /// listed after the plain ones, and the only ones whose premise query carries the
    /// certificate's small facts (a plain premise is a ring identity and needs none; beside the
    /// facts an unprovable one costs its whole timeout instead of failing fast — measured on
    /// `gcd`'s open law: 1.5 min → 5 min for its ladder).
    pub witness: bool,
}

/// A rewrite rule the certificate's hypotheses license — `lead = rest` as polynomials over atoms,
/// with `lead` a monomial of degree ≥ 2 and unit coefficient: the witness rung's search space.
/// From a polynomial equation `L = R` (the Bézout identity `g = r·x + m·y` instantiated by
/// `ext_gcd`'s certified contract: lead `r·x`, rest `g − m·y`) or from a residue equation
/// `(mod T m) = R` (the inverse's own contract `(m1·inv) mod m2 = 1 mod m2`), read through the
/// exact identity `T = R + m·(div T m)` (lead `m1·inv`). `uses` is the assertion it came from.
#[derive(Clone, Debug)]
struct Rule {
    lead: Mono,
    rest: Poly,
    uses: String,
}

const MAX_RULES: usize = 64;
const MIN_LEAD_DEGREE: usize = 2;
/// A certificate fact small enough to ride along in every premise query (the preconditions, a
/// case's tester, the domain bounds — never an unfolding or a quantified axiom, which drown the
/// solver beside a ring identity; measured 2026-10-18).
const SMALL_FACT_CHARS: usize = 120;

/// The lemma instances for a certificate's `mod`/`div` terms. With `with_variants`, the goal is
/// also read with the certificate's equations (`A = E` from unfoldings, hypothesis conclusions
/// and `ite` branches) substituted into it to a small depth — the terms a goal's two sides meet
/// at when they are formed only through such equations (the fast-exponentiation step). The
/// variants are for the STEPWISE checker only: in one query they drown the solver (measured).
pub fn instances(smt: &str, with_variants: bool) -> Option<Vec<Instance>> {
    instances_with(smt, with_variants, true)
}

/// Cap on the witness rung's instances (those a rule produced, and the chain steps after one),
/// which are listed AFTER every plain instance so a budget cuts them first.
const MAX_WITNESS_INSTANCES: usize = 32;

/// [`instances`], with the witness rung's rules on or off.
/// The certificate's `define-fun`s: name → (parameters, body).
fn definitions(forms: &[S]) -> BTreeMap<String, (Vec<String>, S)> {
    let mut defs: BTreeMap<String, (Vec<String>, S)> = BTreeMap::new();
    for f in forms {
        let S::List(items) = f else { continue };
        if !matches!(items.first(), Some(S::Atom(h)) if h == "define-fun") || items.len() != 5 {
            continue;
        }
        let S::Atom(name) = &items[1] else { continue };
        let S::List(ps) = &items[2] else { continue };
        let params: Option<Vec<String>> = ps
            .iter()
            .map(|p| match p {
                S::List(pv) if pv.len() == 2 => match &pv[0] {
                    S::Atom(n) => Some(n.clone()),
                    _ => None,
                },
                _ => None,
            })
            .collect();
        if let Some(params) = params {
            defs.insert(name.clone(), (params, items[4].clone()));
        }
    }
    defs
}

fn instances_with(smt: &str, with_variants: bool, with_rules: bool) -> Option<Vec<Instance>> {
    let forms = parse_all(smt).ok()?;
    let defs = definitions(&forms);
    let asserted: Vec<S> = forms
        .iter()
        .filter_map(|f| match f {
            S::List(items) if items.len() == 2 && matches!(items.first(), Some(S::Atom(h)) if h == "assert") => Some(items[1].clone()),
            _ => None,
        })
        .collect();
    // The goal's terms first (the last assertion — the negated obligation), then the hypotheses':
    // the goal's get the cross-modulus variants, the hypotheses' only their plain instance. The
    // whole set would close too, but slowly (measured on Euclid's divisibility: 14 instances,
    // 18 s; the two the proof needs, well under a second) — a prover's budget is part of its reach.
    let mut candidates: Vec<(S, S)> = Vec::new(); // (t, m) for each (mod t m) / (div t m)
    let mut decompositions: BTreeSet<(String, String)> = BTreeSet::new(); // (symbol, modulus key) available
    let mut mod_keys: BTreeMap<String, S> = BTreeMap::new();
    // (Instances are generated at SYNTACTIC terms only. A goal whose two sides meet at residues
    // the solver forms through equations — a `self` application an unfolding EQUATES with a
    // `mod` term, the fast-exponentiation step — is out of reach: substituting such equations
    // into the goal to generate the missing terms was built and measured 2026-10-18, and the
    // instance set it produces (108 for Euclid's divisibility, 15 without) leaves z3 at unknown
    // on certificates it otherwise closes in 0.1 s. A chain of nonlinear premises is a proof to
    // be checked stepwise, not one query.)
    let mut goal_count = 0;
    let expanded: Vec<S> = asserted.iter().map(|a| expand(a, &BTreeMap::new(), &defs, 0)).collect();
    // The witness rung's rules: every equation the hypotheses (and the goal's own facts) license.
    let rules = if with_rules { rules_of(&asserted, &expanded) } else { Vec::new() };
    if let Some(goal) = expanded.last() {
        // The goal and its accessor variants (a `case` on a result that is itself a `case`: the
        // payload accessor pushed through the `ite` to the constructor's payload) — the terms a
        // contract over a sum-typed result reaches only through the solver's datatype reasoning.
        let bases = accessor_variants(goal);
        if with_variants {
            let mut equations: Vec<(String, S)> = Vec::new();
            for e in &expanded {
                harvest(e, &mut equations, true);
            }
            for base in &bases {
                for v in variants(base, &equations) {
                    collect(&v, &mut candidates, &mut decompositions, &mut mod_keys);
                }
            }
        } else {
            for base in &bases {
                collect(base, &mut candidates, &mut decompositions, &mut mod_keys);
            }
        }
        goal_count = candidates.len();
    }
    for e in expanded.iter().rev().skip(1) {
        collect(e, &mut candidates, &mut decompositions, &mut mod_keys);
    }
    if candidates.is_empty() {
        return None;
    }
    let max = if with_variants { MAX_INSTANCES_STEPWISE } else { MAX_INSTANCES };
    let mut out: Vec<Instance> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for (i, (t, m)) in candidates.iter().enumerate() {
        if out.len() >= max {
            break;
        }
        let Some(tp) = to_poly(t) else { continue };
        for inst in instance_chain(t, m, &tp, &BTreeMap::new(), &rules) {
            if seen.insert(inst.text.clone()) {
                out.push(inst);
            }
        }
        if i >= goal_count {
            continue;
        }
        // Cross-modulus variants: a symbol of `t` the certificate also divides by another modulus
        // is first rewritten by THAT decomposition (one symbol, one other modulus per variant).
        let mkey = render(m);
        let symbols: BTreeSet<String> = tp.keys().flat_map(|mono| mono.iter().cloned()).filter(|a| !a.starts_with('(')).collect();
        for y in symbols {
            for (sym, nkey) in &decompositions {
                if sym != &y || nkey == &mkey {
                    continue;
                }
                let Some(n) = mod_keys.get(nkey) else { continue };
                let mut rewrite: BTreeMap<String, Poly> = BTreeMap::new();
                rewrite.insert(y.clone(), decomposition_of(&S::Atom(y.clone()), n));
                for inst in instance_chain(t, m, &tp, &rewrite, &rules) {
                    if seen.insert(inst.text.clone()) {
                        out.push(inst);
                    }
                }
                if out.len() >= max {
                    break;
                }
            }
        }
    }
    if out.is_empty() {
        return None;
    }
    // The plain instances first, the witness rung's after them (capped): the one-query retry
    // and the stepwise budget both read the list in order.
    let (witness, plain): (Vec<Instance>, Vec<Instance>) = out.into_iter().partition(|i| i.witness);
    let mut out = plain;
    out.extend(witness.into_iter().take(MAX_WITNESS_INSTANCES));
    Some(out)
}

// --- the witness rung: rules from hypotheses -------------------------------------------------------------
//
// A quotient witness the chain cannot compute from the goal's terms alone: `a·x ≡ 1 (mod m)` for
// the modular inverse rests on the Bézout identity `g = r·x + m·y` that `ext_gcd`'s certified
// contract states — the residue part `r·(x mod m)` of the goal's term reduces to `g` only THROUGH
// that equation. The 2026-10-18 measurement: with that one instance added by hand z3 closes the
// law instantly; the engine never searched for it. This rung searches a bounded space: every
// equation the hypotheses state is read as a polynomial identity and oriented at a monomial of
// degree ≥ 2 (a lead `r·x`), and when the chain's residue polynomial is stuck (nothing left to
// lift) a monomial that contains a lead — each of the lead's atoms either itself or its residue,
// the residue UN-LIFTED through `(mod y m) = y − m·(div y m)` to expose the atom — is rewritten by
// the rule; the chain then decomposes the result as it does any term. The instance's premise is a
// ring identity modulo the rule's hypothesis, so the instance records it (`uses`), and the
// stepwise checker verifies the premise beside exactly that assertion. Soundness is unchanged:
// a rule only proposes a term; the solver checks every premise, and a premise it cannot discharge
// leaves its instance out (stepwise) or vacuous (one query).

/// Every assertion-level fact the certificate states, with polarity: the hypotheses (positive)
/// and the literals of the negated goal (`¬(A ∨ B)` gives `¬A` and `¬B`, `¬(p ⇒ q)` gives `p` and
/// `¬q`, `¬(ite c a true)` gives `c` and `¬a`). Facts the final query holds in every model.
fn literals(s: &S, positive: bool, out: &mut Vec<(S, bool)>) {
    let is_lit = |x: &S, v: &str| matches!(x, S::Atom(a) if a == v);
    if let S::List(items) = s {
        if let Some(S::Atom(h)) = items.first() {
            match h.as_str() {
                "not" if items.len() == 2 => return literals(&items[1], !positive, out),
                "and" if positive => return items[1..].iter().for_each(|x| literals(x, true, out)),
                "or" if !positive => return items[1..].iter().for_each(|x| literals(x, false, out)),
                "=>" if !positive && items.len() == 3 => {
                    literals(&items[1], true, out);
                    literals(&items[2], false, out);
                    return;
                }
                "ite" if items.len() == 4 => {
                    let (c, a, b) = (&items[1], &items[2], &items[3]);
                    let (absorb, other_branch, cond_pol) = if positive {
                        if is_lit(b, "false") { (true, a, true) } else if is_lit(a, "false") { (true, b, false) } else { (false, a, true) }
                    } else if is_lit(b, "true") {
                        (true, a, true)
                    } else if is_lit(a, "true") {
                        (true, b, false)
                    } else {
                        (false, a, true)
                    };
                    if absorb {
                        literals(c, cond_pol, out);
                        literals(other_branch, positive, out);
                        return;
                    }
                }
                _ => {}
            }
        }
    }
    out.push((s.clone(), positive));
}

/// Equations `L = R` in a hypothesis, read through implications' consequents, conjunctions,
/// disjunctions and both branches of an `ite` (a conditional equation is still a candidate rule:
/// the solver's check of the premise decides whether it applies).
fn equations_in(s: &S, positive: bool, out: &mut Vec<(S, S)>) {
    let S::List(items) = s else { return };
    let Some(S::Atom(h)) = items.first() else { return };
    match h.as_str() {
        "=" if items.len() == 3 && positive => out.push((items[1].clone(), items[2].clone())),
        "not" if items.len() == 2 => equations_in(&items[1], !positive, out),
        "=>" if items.len() == 3 => equations_in(&items[2], positive, out),
        "and" | "or" => items[1..].iter().for_each(|x| equations_in(x, positive, out)),
        "ite" if items.len() == 4 => {
            equations_in(&items[2], positive, out);
            equations_in(&items[3], positive, out);
        }
        _ => {}
    }
}

/// The rules a certificate licenses (see the module comment above): from every positive equation
/// of the hypotheses and every literal of the negated goal.
fn rules_of(asserted: &[S], expanded: &[S]) -> Vec<Rule> {
    let mut out: Vec<Rule> = Vec::new();
    let n = expanded.len();
    for (i, e) in expanded.iter().enumerate() {
        let uses = render(&asserted[i]);
        let mut eqs: Vec<(S, S)> = Vec::new();
        if i + 1 == n {
            let mut lits = Vec::new();
            literals(e, true, &mut lits);
            for (l, pos) in lits {
                equations_in(&l, pos, &mut eqs);
            }
        } else {
            equations_in(e, true, &mut eqs);
        }
        for (l, r) in eqs {
            for (a, b) in [(&l, &r), (&r, &l)] {
                let Some(identity) = identity_of(a, b) else { continue };
                for (mono, c) in &identity {
                    if mono.len() < MIN_LEAD_DEGREE || (*c != 1 && *c != -1) {
                        continue;
                    }
                    // lead = rest:  c·lead + rest' = 0  ⇒  lead = −rest'/c
                    let mut rest_p = identity.clone();
                    rest_p.remove(mono);
                    let Some(rest) = p_scale(&rest_p, -*c) else { continue };
                    if rest.keys().flat_map(|m| m.iter()).any(|a| a.starts_with("(ite")) {
                        continue;
                    }
                    if out.iter().any(|x| x.lead == *mono && x.rest == rest) {
                        continue;
                    }
                    out.push(Rule { lead: mono.clone(), rest, uses: uses.clone() });
                    if out.len() >= MAX_RULES {
                        return out;
                    }
                }
            }
        }
    }
    out
}

/// `a = b` as the polynomial `a − b` (both sides read as polynomials over atoms), or, when `a` is a
/// residue `(mod T m)` with a non-literal modulus, the exact identity `T = b + m·(div T m)` as the
/// polynomial `T − b − m·(div T m)`.
fn identity_of(a: &S, b: &S) -> Option<Poly> {
    if let S::List(items) = a {
        if items.len() == 3 && matches!(&items[0], S::Atom(h) if h == "mod") && !is_literal(&items[2]) {
            let t = to_poly(&items[1])?;
            let bp = to_poly(b)?;
            let mp = to_poly(&items[2])?;
            let q = p_atom(format!("(div {} {})", render(&items[1]), render(&items[2])));
            let mq = p_mul(&mp, &q)?;
            let d = p_add(&t, &p_scale(&bp, -1)?)?;
            return p_add(&d, &p_scale(&mq, -1)?);
        }
    }
    let ap = to_poly(a)?;
    let bp = to_poly(b)?;
    let d = p_add(&ap, &p_scale(&bp, -1)?)?;
    (!d.is_empty()).then_some(d)
}

/// The certificate's small facts, as assertions for every premise query of the stepwise checker:
/// the literals of the hypotheses and of the negated goal that are quantifier-free and short.
fn small_facts(asserted: &[S]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for a in asserted {
        let mut lits = Vec::new();
        literals(a, true, &mut lits);
        for (l, pos) in lits {
            let (l, pos) = tester_through_ite(&l, pos);
            let s = render(&l);
            if s.len() > SMALL_FACT_CHARS || s.contains("(forall") || s.contains("(exists") {
                continue;
            }
            let f = if pos { s } else { format!("(not {s})") };
            if !out.contains(&f) {
                out.push(f);
            }
        }
    }
    out
}

/// A tester on an `ite` whose branches construct — `((_ is dt0_Just) (ite c (dt0_Just v) dt0_None))`,
/// the post's `case` on a result the body built by its own `case` — is the condition itself (or
/// its negation): the short fact the premise queries need (`c` is the callee's tester, under which
/// the callee's contract speaks).
fn tester_through_ite(l: &S, positive: bool) -> (S, bool) {
    let S::List(items) = l else { return (l.clone(), positive) };
    if items.len() != 2 {
        return (l.clone(), positive);
    }
    let S::List(tester) = &items[0] else { return (l.clone(), positive) };
    if tester.len() != 3 || !matches!(&tester[0], S::Atom(x) if x == "_") || !matches!(&tester[1], S::Atom(x) if x == "is") {
        return (l.clone(), positive);
    }
    let S::Atom(tag) = &tester[2] else { return (l.clone(), positive) };
    let S::List(ite) = &items[1] else { return (l.clone(), positive) };
    if ite.len() != 4 || !matches!(&ite[0], S::Atom(x) if x == "ite") {
        return (l.clone(), positive);
    }
    let head = |b: &S| match b {
        S::Atom(a) => Some(a.clone()),
        S::List(v) => match v.first() {
            Some(S::Atom(a)) => Some(a.clone()),
            _ => None,
        },
    };
    let (ha, hb) = (head(&ite[2]), head(&ite[3]));
    let constructs = |h: &Option<String>| h.as_deref().is_some_and(|h| h.starts_with("dt"));
    if !constructs(&ha) || !constructs(&hb) {
        return (l.clone(), positive);
    }
    match (ha.as_deref() == Some(tag.as_str()), hb.as_deref() == Some(tag.as_str())) {
        (true, false) => tester_through_ite(&ite[1], positive),
        (false, true) => tester_through_ite(&ite[1], !positive),
        _ => (l.clone(), positive),
    }
}

/// A datatype accessor applied to an `ite` whose branches construct: the accessor pushed into the
/// branch that matches it. `(dt0_Just_v (ite c (dt0_Just v) dt0_None))` yields `v`; the solver
/// identifies the two under `c`, and an instance about `v`'s terms is what the goal needs.
fn accessor_variants(goal: &S) -> Vec<S> {
    let mut out = vec![goal.clone()];
    let mut seen: BTreeSet<String> = [render(goal)].into_iter().collect();
    let mut frontier = vec![goal.clone()];
    for _ in 0..VARIANT_DEPTH {
        let mut next = Vec::new();
        for g in &frontier {
            for v in push_accessor_once(g) {
                if seen.insert(render(&v)) {
                    out.push(v.clone());
                    next.push(v);
                }
            }
        }
        if next.is_empty() || out.len() >= MAX_VARIANTS {
            break;
        }
        frontier = next;
    }
    out
}

/// Every term obtained from `s` by pushing ONE accessor through an `ite` to a constructing branch.
fn push_accessor_once(s: &S) -> Vec<S> {
    let mut out = Vec::new();
    let S::List(items) = s else { return out };
    if let Some(S::Atom(h)) = items.first() {
        if matches!(h.as_str(), "forall" | "exists" | "lambda") {
            return out;
        }
        if items.len() == 2 && h.starts_with("dt") {
            if let S::List(ite) = &items[1] {
                if ite.len() == 4 && matches!(&ite[0], S::Atom(x) if x == "ite") {
                    for branch in [&ite[2], &ite[3]] {
                        if let Some(r) = accessor_of_constructor(h, branch) {
                            out.push(r);
                        }
                    }
                }
            }
        }
    }
    for (i, x) in items.iter().enumerate() {
        for v in push_accessor_once(x) {
            let mut items2 = items.clone();
            items2[i] = v;
            out.push(S::List(items2));
        }
    }
    out
}

/// `(dtK_Tag_v (dtK_Tag v))` → `v`; `(dtK_i (mk_dtK v0 … vn))` → `vi`.
fn accessor_of_constructor(acc: &str, term: &S) -> Option<S> {
    let S::List(items) = term else { return None };
    let S::Atom(ctor) = items.first()? else { return None };
    if let Some(tag) = acc.strip_suffix("_v") {
        if ctor == tag && items.len() == 2 {
            return Some(items[1].clone());
        }
        return None;
    }
    let base = acc.rsplit_once('_')?;
    let i: usize = base.1.parse().ok()?;
    if ctor == &format!("mk_{}", base.0) {
        return items.get(i + 1).cloned();
    }
    None
}

/// Does the monomial contain the rule's lead — each lead atom either as itself or as its residue
/// `(mod atom m)`? Returns the residue atoms to un-lift (residue → atom) and the remaining atoms.
fn match_lead(lead: &Mono, mono: &Mono, mkey: &str) -> Option<(BTreeMap<String, String>, Mono)> {
    let mut rest: Vec<Option<String>> = mono.iter().cloned().map(Some).collect();
    let mut unlift: BTreeMap<String, String> = BTreeMap::new();
    for l in lead {
        let residue = format!("(mod {l} {mkey})");
        let pos = rest.iter().position(|a| a.as_deref() == Some(l.as_str())).or_else(|| {
            let p = rest.iter().position(|a| a.as_deref() == Some(residue.as_str()))?;
            unlift.insert(residue.clone(), l.clone());
            Some(p)
        })?;
        rest[pos] = None;
    }
    Some((unlift, rest.into_iter().flatten().collect()))
}

/// The polynomial with every monomial that contains a rule's lead rewritten by it (one rule per
/// monomial), and the assertions used. `None` when no rule applies.
fn apply_rules(cur: &Poly, rules: &[Rule], mkey: &str) -> Option<(Poly, Vec<String>)> {
    let mut out = cur.clone();
    let mut uses: Vec<String> = Vec::new();
    let mut any = false;
    for (mono, c) in cur {
        for rule in rules {
            if rule.lead.iter().any(|a| a == mkey) {
                continue;
            }
            let Some((unlift, remaining)) = match_lead(&rule.lead, mono, mkey) else { continue };
            // U = c · Π atoms, the matched residues un-lifted: its principal term is c·lead·M'.
            let mut u = p_const(*c);
            for atom in mono {
                let factor = match unlift.get(atom) {
                    Some(y) => {
                        let mut f = p_atom(y.clone());
                        let mut dm = vec![mkey.to_string(), format!("(div {y} {mkey})")];
                        dm.sort();
                        f.insert(dm, -1);
                        f
                    }
                    None => p_atom(atom.clone()),
                };
                u = p_mul(&u, &factor)?;
            }
            // + c·(rest − lead)·M'
            let mut lead_p = Poly::new();
            lead_p.insert(rule.lead.clone(), 1);
            let diff = p_add(&rule.rest, &p_scale(&lead_p, -1)?)?;
            let mut mp = p_const(*c);
            for a in &remaining {
                mp = p_mul(&mp, &p_atom(a.clone()))?;
            }
            let add = p_mul(&diff, &mp)?;
            let mut mono_p = Poly::new();
            mono_p.insert(mono.clone(), *c);
            out = p_add(&out, &p_scale(&mono_p, -1)?)?;
            out = p_add(&out, &u)?;
            out = p_add(&out, &add)?;
            if !uses.contains(&rule.uses) {
                uses.push(rule.uses.clone());
            }
            any = true;
            break;
        }
    }
    any.then_some((out, uses))
}

const MAX_INSTANCES_STEPWISE: usize = 512;
/// Per-premise solver budget (a verifiable premise answers in milliseconds) and the budget for
/// all of them (measured: the agreement of fast exponentiation verifies 250 premises in ~15 s).
const STEP_SECS: u64 = 2;
const STEPWISE_BUDGET_SECS: u64 = 60;
const WITNESS_BUDGET_SECS: u64 = 20;
const VARIANT_DEPTH: usize = 3;
const MAX_VARIANTS: usize = 256;

const OPS: &[&str] = &["+", "-", "*", "mod", "div", "abs", "=", "<", "<=", ">", ">=", "distinct", "and", "or", "not", "=>", "ite", "let", "forall", "exists", "!", "_", "is", "true", "false"];

/// An application atom: a list whose head is a symbol that is not an operator — `self`, a callee,
/// a datatype accessor — the kind of term an unfolding equates with an expression.
fn is_app_atom(s: &S) -> bool {
    matches!(s, S::List(items) if matches!(items.first(), Some(S::Atom(h)) if !OPS.contains(&h.as_str()) && !h.starts_with('(')))
}

/// A term's key with its arithmetic arguments normalized (`(self b (- (- (* 2 e) 1) 1) m)` and
/// `(self b (* 2 (- e 1)) m)` are one application), so an equation about one spelling applies to
/// the other — the solver identifies them by arithmetic and congruence.
fn atom_key(s: &S) -> String {
    match s {
        S::Atom(_) => render(s),
        S::List(items) => {
            let parts: Vec<String> = items
                .iter()
                .enumerate()
                .map(|(i, x)| if i == 0 { render(x) } else { to_poly(x).map(|p| render_poly(&p)).unwrap_or_else(|| atom_key(x)) })
                .collect();
            format!("({})", parts.join(" "))
        }
    }
}

/// Equations `A = E` with `A` an application atom, read positively: top-level, under `=>` (its
/// consequent), `and`, `or`, and through the branches of an `ite` on the right-hand side.
fn harvest(s: &S, out: &mut Vec<(String, S)>, positive: bool) {
    let S::List(items) = s else { return };
    let Some(S::Atom(h)) = items.first() else { return };
    match h.as_str() {
        "=" if items.len() == 3 && positive => {
            for (a, e) in [(&items[1], &items[2]), (&items[2], &items[1])] {
                if is_app_atom(a) {
                    let mut rhs = vec![e.clone()];
                    let mut i = 0;
                    while i < rhs.len() {
                        if let S::List(ri) = &rhs[i] {
                            if ri.len() == 4 && matches!(&ri[0], S::Atom(x) if x == "ite") {
                                let (t, f) = (ri[2].clone(), ri[3].clone());
                                rhs.remove(i);
                                rhs.push(t);
                                rhs.push(f);
                                continue;
                            }
                        }
                        i += 1;
                    }
                    for r in rhs {
                        if !is_app_atom(&r) || atom_key(&r) != atom_key(a) {
                            out.push((atom_key(a), r));
                        }
                    }
                }
            }
        }
        "=>" if items.len() == 3 => harvest(&items[2], out, positive),
        "and" | "or" => items[1..].iter().for_each(|x| harvest(x, out, positive)),
        "not" if items.len() == 2 => harvest(&items[1], out, !positive),
        _ => {}
    }
}

/// The goal with up to `VARIANT_DEPTH` equation substitutions applied, one atom occurrence at a
/// time, breadth first and capped — the original first, then the variants whose atoms match an
/// equation EXACTLY (to full depth), then those matched loosely (`keys_may_coincide`), from every
/// variant so far. The order is the measurement: the loose matches flood the list, and a cap that
/// cuts an exact one loses the proof (the doubling law closed on 29 exact instances and not on
/// 192 mixed ones).
fn variants(goal: &S, eqs: &[(String, S)]) -> Vec<S> {
    let mut out: Vec<S> = vec![goal.clone()];
    let mut seen: BTreeSet<String> = [render(goal)].into_iter().collect();
    for strict in [true, false] {
        let mut frontier: Vec<S> = out.clone();
        for _ in 0..VARIANT_DEPTH {
            let mut next = Vec::new();
            for g in &frontier {
                for v in substitute_once(g, eqs, strict) {
                    if out.len() >= MAX_VARIANTS {
                        return out;
                    }
                    if seen.insert(render(&v)) {
                        out.push(v.clone());
                        next.push(v);
                    }
                }
            }
            frontier = next;
            if frontier.is_empty() {
                break;
            }
        }
    }
    out
}

/// Do two application keys name applications the solver may identify — the same head and arity,
/// each argument either identical or, on at least one side, a compound arithmetic term (whose
/// equality to the other may hold under the case the solver is in: `e − 1 = 2·(e div 2)` when `e`
/// is odd)? A variant is only a candidate term — an instance about it is a true fact about that
/// term, connected to the goal by the solver or not — so matching loosely is safe, only costlier.
fn keys_may_coincide(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    let (Ok(pa), Ok(pb)) = (parse_all(a), parse_all(b)) else { return false };
    let (Some(S::List(xa)), Some(S::List(xb))) = (pa.first(), pb.first()) else { return false };
    if xa.len() != xb.len() || xa.first() != xb.first() {
        return false;
    }
    xa.iter().zip(xb).skip(1).all(|(x, y)| x == y || matches!(x, S::List(_)) || matches!(y, S::List(_)))
}

/// Every term obtained from `s` by replacing ONE application atom with a term an equation says it
/// equals — keys equal exactly (`strict`), or as `keys_may_coincide`.
fn substitute_once(s: &S, eqs: &[(String, S)], strict: bool) -> Vec<S> {
    let mut out = Vec::new();
    if is_app_atom(s) {
        let k = atom_key(s);
        for (a, e) in eqs {
            if if strict { a == &k } else { keys_may_coincide(a, &k) } {
                out.push(e.clone());
            }
        }
    }
    if let S::List(items) = s {
        if matches!(items.first(), Some(S::Atom(h)) if matches!(h.as_str(), "forall" | "exists" | "lambda")) {
            return out;
        }
        for (i, x) in items.iter().enumerate() {
            for v in substitute_once(x, eqs, strict) {
                let mut items2 = items.clone();
                items2[i] = v;
                out.push(S::List(items2));
            }
        }
    }
    out
}

// --- s-expressions ------------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum S {
    Atom(String),
    List(Vec<S>),
}

fn parse_all(src: &str) -> Result<Vec<S>, String> {
    let chars: Vec<char> = src.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    loop {
        skip_ws(&chars, &mut i);
        if i >= chars.len() {
            break;
        }
        out.push(parse_one(&chars, &mut i)?);
    }
    Ok(out)
}

fn skip_ws(c: &[char], i: &mut usize) {
    while *i < c.len() {
        if c[*i].is_whitespace() {
            *i += 1;
        } else if c[*i] == ';' {
            while *i < c.len() && c[*i] != '\n' {
                *i += 1;
            }
        } else {
            break;
        }
    }
}

fn parse_one(c: &[char], i: &mut usize) -> Result<S, String> {
    skip_ws(c, i);
    if *i >= c.len() {
        return Err("unexpected end".into());
    }
    match c[*i] {
        '(' => {
            *i += 1;
            let mut items = Vec::new();
            loop {
                skip_ws(c, i);
                if *i >= c.len() {
                    return Err("unclosed list".into());
                }
                if c[*i] == ')' {
                    *i += 1;
                    return Ok(S::List(items));
                }
                items.push(parse_one(c, i)?);
            }
        }
        ')' => Err("unexpected `)`".into()),
        '"' => {
            let start = *i;
            *i += 1;
            while *i < c.len() {
                if c[*i] == '"' {
                    if *i + 1 < c.len() && c[*i + 1] == '"' {
                        *i += 2;
                        continue;
                    }
                    *i += 1;
                    break;
                }
                *i += 1;
            }
            Ok(S::Atom(c[start..*i].iter().collect()))
        }
        '|' => {
            let start = *i;
            *i += 1;
            while *i < c.len() && c[*i] != '|' {
                *i += 1;
            }
            *i = (*i + 1).min(c.len());
            Ok(S::Atom(c[start..*i].iter().collect()))
        }
        _ => {
            let start = *i;
            while *i < c.len() && !c[*i].is_whitespace() && c[*i] != '(' && c[*i] != ')' && c[*i] != ';' {
                *i += 1;
            }
            Ok(S::Atom(c[start..*i].iter().collect()))
        }
    }
}

pub fn render(s: &S) -> String {
    match s {
        S::Atom(a) => a.clone(),
        S::List(items) => format!("({})", items.iter().map(render).collect::<Vec<_>>().join(" ")),
    }
}

/// `let`s substituted (simultaneous binding), `define-fun` applications unfolded to a bounded
/// depth, `!` annotations dropped; quantifiers left as they are (their bodies range over bound
/// variables — nothing to instantiate at).
fn expand(s: &S, env: &BTreeMap<String, S>, defs: &BTreeMap<String, (Vec<String>, S)>, depth: usize) -> S {
    match s {
        S::Atom(a) => env.get(a).cloned().unwrap_or_else(|| s.clone()),
        S::List(items) => {
            let Some(S::Atom(head)) = items.first() else {
                return S::List(items.iter().map(|x| expand(x, env, defs, depth)).collect());
            };
            match head.as_str() {
                "let" if items.len() == 3 => {
                    let S::List(binds) = &items[1] else { return s.clone() };
                    let mut inner = env.clone();
                    for b in binds {
                        if let S::List(bv) = b {
                            if bv.len() == 2 {
                                if let S::Atom(n) = &bv[0] {
                                    inner.insert(n.clone(), expand(&bv[1], env, defs, depth));
                                }
                            }
                        }
                    }
                    expand(&items[2], &inner, defs, depth)
                }
                "forall" | "exists" | "lambda" => s.clone(),
                "!" if items.len() >= 2 => expand(&items[1], env, defs, depth),
                _ => {
                    let args: Vec<S> = items[1..].iter().map(|x| expand(x, env, defs, depth)).collect();
                    if let Some((params, body)) = defs.get(head) {
                        if params.len() == args.len() && depth < EXPAND_DEPTH {
                            let inner: BTreeMap<String, S> = params.iter().cloned().zip(args).collect();
                            return expand(body, &inner, defs, depth + 1);
                        }
                    }
                    let mut v = Vec::with_capacity(args.len() + 1);
                    v.push(S::Atom(head.clone()));
                    v.extend(args);
                    S::List(v)
                }
            }
        }
    }
}

fn is_numeral(a: &str) -> bool {
    !a.is_empty() && a.chars().all(|c| c.is_ascii_digit())
}

fn is_literal(s: &S) -> bool {
    match s {
        S::Atom(a) => is_numeral(a),
        S::List(items) => items.len() == 2 && matches!(&items[0], S::Atom(h) if h == "-") && is_literal(&items[1]),
    }
}

fn collect(s: &S, cands: &mut Vec<(S, S)>, decs: &mut BTreeSet<(String, String)>, keys: &mut BTreeMap<String, S>) {
    let S::List(items) = s else { return };
    if let Some(S::Atom(h)) = items.first() {
        if matches!(h.as_str(), "forall" | "exists" | "lambda") {
            return;
        }
        if (h == "mod" || h == "div") && items.len() == 3 && !is_literal(&items[2]) {
            let m = &items[2];
            let mk = render(m);
            keys.entry(mk.clone()).or_insert_with(|| m.clone());
            if let S::Atom(y) = &items[1] {
                if !is_numeral(y) {
                    decs.insert((y.clone(), mk));
                }
            }
            let pair = (items[1].clone(), m.clone());
            if !cands.contains(&pair) {
                cands.push(pair);
            }
        }
    }
    for x in items {
        collect(x, cands, decs, keys);
    }
}

// --- polynomials over atoms -----------------------------------------------------------------------------

/// A monomial is the sorted multiset of its atom keys (rendered s-expressions); a polynomial maps
/// monomials to integer coefficients. Atoms are whatever is not `+`, `-`, `*` or a numeral.
type Mono = Vec<String>;
type Poly = BTreeMap<Mono, i128>;

fn p_const(c: i128) -> Poly {
    let mut p = Poly::new();
    if c != 0 {
        p.insert(vec![], c);
    }
    p
}

fn p_atom(key: String) -> Poly {
    let mut p = Poly::new();
    p.insert(vec![key], 1);
    p
}

fn p_add(a: &Poly, b: &Poly) -> Option<Poly> {
    let mut out = a.clone();
    for (m, c) in b {
        let e = out.entry(m.clone()).or_insert(0);
        *e = e.checked_add(*c)?;
        if *e == 0 {
            out.remove(m);
        }
    }
    (out.len() <= MAX_TERMS).then_some(out)
}

fn p_scale(a: &Poly, k: i128) -> Option<Poly> {
    let mut out = Poly::new();
    for (m, c) in a {
        let v = c.checked_mul(k)?;
        if v != 0 {
            out.insert(m.clone(), v);
        }
    }
    Some(out)
}

fn p_mul(a: &Poly, b: &Poly) -> Option<Poly> {
    let mut out = Poly::new();
    for (ma, ca) in a {
        for (mb, cb) in b {
            let mut m = ma.clone();
            m.extend(mb.iter().cloned());
            m.sort();
            if m.len() > MAX_DEGREE {
                return None;
            }
            let v = ca.checked_mul(*cb)?;
            let e = out.entry(m).or_insert(0);
            *e = e.checked_add(v)?;
        }
    }
    out.retain(|_, c| *c != 0);
    (out.len() <= MAX_TERMS).then_some(out)
}

fn to_poly(s: &S) -> Option<Poly> {
    match s {
        S::Atom(a) => {
            if is_numeral(a) {
                Some(p_const(a.parse::<i128>().ok()?))
            } else {
                Some(p_atom(a.clone()))
            }
        }
        S::List(items) => {
            let Some(S::Atom(h)) = items.first() else { return Some(p_atom(render(s))) };
            match h.as_str() {
                "+" if items.len() >= 2 => {
                    let mut acc = p_const(0);
                    for x in &items[1..] {
                        acc = p_add(&acc, &to_poly(x)?)?;
                    }
                    Some(acc)
                }
                "-" if items.len() == 2 => p_scale(&to_poly(&items[1])?, -1),
                "-" if items.len() >= 3 => {
                    let mut acc = to_poly(&items[1])?;
                    for x in &items[2..] {
                        acc = p_add(&acc, &p_scale(&to_poly(x)?, -1)?)?;
                    }
                    Some(acc)
                }
                "*" if items.len() >= 2 => {
                    let mut acc = p_const(1);
                    for x in &items[1..] {
                        acc = p_mul(&acc, &to_poly(x)?)?;
                    }
                    Some(acc)
                }
                _ => Some(p_atom(render(s))),
            }
        }
    }
}

fn render_poly(p: &Poly) -> String {
    if p.is_empty() {
        return "0".to_string();
    }
    let terms: Vec<String> = p
        .iter()
        .map(|(m, c)| {
            let coef = if *c < 0 { format!("(- {})", -c) } else { c.to_string() };
            match (m.len(), *c) {
                (0, _) => coef,
                (1, 1) => m[0].clone(),
                (_, 1) => format!("(* {})", m.join(" ")),
                _ => format!("(* {coef} {})", m.join(" ")),
            }
        })
        .collect();
    if terms.len() == 1 {
        terms.into_iter().next().unwrap()
    } else {
        format!("(+ {})", terms.join(" "))
    }
}

/// `y = (div y n)·n + (mod y n)` as a polynomial (the modulus itself an atom).
fn decomposition_of(y: &S, n: &S) -> Poly {
    let yk = render(y);
    let nk = render(n);
    let mut p = Poly::new();
    let mut m = vec![format!("(div {yk} {nk})"), nk.clone()];
    m.sort();
    p.insert(m, 1);
    p.insert(vec![format!("(mod {yk} {nk})")], 1);
    p
}

fn is_residue_of(atom: &str, mkey: &str) -> bool {
    // `(mod y m)` for THIS modulus: its own residue — kept as is.
    if let Ok(v) = parse_all(atom) {
        if let Some(S::List(items)) = v.first() {
            return items.len() == 3 && matches!(&items[0], S::Atom(h) if h == "mod") && render(&items[2]) == mkey;
        }
    }
    false
}

const MAX_CHAIN: usize = 12;
const MAX_CHAIN_GRANULAR: usize = 32;

/// The lemma instances for `(mod t m)` / `(div t m)`, as a CHAIN: `t` (as `tp`, optionally with
/// some atoms pre-rewritten by `rewrite`) decomposed against `m` into `P₁ + m·Q₁`, then `P₁`
/// (where a nested residue `(mod X m)` of a compound `X` may remain) into `P₂ + m·Q₂` by lifting
/// ONE occurrence of such a residue through the exact identity `(mod X m) = X − m·(X div m)`, and
/// so on until the residue part mentions only residues of atoms — the canonical residue
/// polynomial both sides of a goal reach. One occurrence per step keeps every instance's premise
/// a low-degree identity the solver verifies from its own `div`/`mod` axioms (measured: lifting
/// everything at once leaves z3 at unknown on a fourth-degree premise; the chain closes).
/// Empty when nothing decomposes or the algebra overflows.
fn instance_chain(t: &S, m: &S, tp: &Poly, rewrite: &BTreeMap<String, Poly>, rules: &[Rule]) -> Vec<Instance> {
    let mkey = render(m);
    let mut out = Vec::new();
    let Some(mut cur) = apply_rewrite(tp, rewrite) else { return out };
    let mut x = render(t);
    let mut first = true;
    // A cross-modulus chain's premises rest on the OTHER modulus being nonzero (its
    // decomposition): they ride with the certificate's facts too.
    let mut witness = !rewrite.is_empty();
    // The witness rung's own chains (a rule step, a foreign lift) continue one substitution per
    // step; the plain and cross-modulus chains keep the measured discipline.
    let mut granular = false;
    let mut steps = 0usize;
    while steps < if granular { MAX_CHAIN_GRANULAR } else { MAX_CHAIN } {
        steps += 1;
        // The witness rung: when the canonical normalization is stuck (nothing to lift), a rule
        // the hypotheses license rewrites the residue polynomial — the premise then rests on the
        // rule's assertion, which the instance records.
        let mut uses: Vec<String> = Vec::new();
        let mut work = cur.clone();
        if !first && !has_liftable(&work, &mkey) {
            if let Some((rewritten, used)) = apply_rules(&work, rules, &mkey) {
                work = rewritten;
                uses = used;
                witness = true;
                granular = true;
            }
        }
        if work.keys().flat_map(|mono| mono.iter()).any(|a| foreign_residue(a, &mkey).is_some()) {
            witness = true;
            granular = true;
        }
        let Some((p, q, changed)) = step(&work, m, &mkey, granular) else { break };
        if !changed && rewrite.is_empty() && uses.is_empty() {
            break;
        }
        let ps = render_poly(&p);
        if ps == x {
            break; // no progress
        }
        // Trivial when the residue part is just `(mod x m)` — `x mod m = (x mod m) mod m`.
        let trivial = p.len() == 1 && p.iter().next().is_some_and(|(mono, c)| *c == 1 && mono.len() == 1 && mono[0] == format!("(mod {x} {mkey})"));
        if !trivial {
            let qs = render_poly(&q);
            let (qw, rw) = if p.is_empty() {
                (qs, "0".to_string())
            } else if q.is_empty() {
                (format!("(div {ps} {mkey})"), format!("(mod {ps} {mkey})"))
            } else {
                (format!("(+ {qs} (div {ps} {mkey}))"), format!("(mod {ps} {mkey})"))
            };
            let premise = format!("(= {x} (+ (* {qw} {mkey}) {rw}))");
            let conclusion = format!("(and (= (div {x} {mkey}) {qw}) (= (mod {x} {mkey}) {rw}))");
            let text = format!("(assert (=> (and (not (= {mkey} 0)) {premise} (>= {rw} 0) (< {rw} (abs {mkey}))) {conclusion}))");
            out.push(Instance { modulus: mkey.clone(), premise, conclusion, text, uses, witness });
        }
        first = false;
        if p.is_empty() || (!has_liftable(&p, &mkey) && apply_rules(&p, rules, &mkey).is_none()) {
            break;
        }
        x = ps;
        cur = p;
    }
    out
}

fn apply_rewrite(tp: &Poly, rewrite: &BTreeMap<String, Poly>) -> Option<Poly> {
    if rewrite.is_empty() {
        return Some(tp.clone());
    }
    let mut acc = p_const(0);
    for (mono, c) in tp {
        let mut term = p_const(*c);
        for atom in mono {
            let factor = rewrite.get(atom).cloned().unwrap_or_else(|| p_atom(atom.clone()));
            term = p_mul(&term, &factor)?;
        }
        acc = p_add(&acc, &term)?;
    }
    Some(acc)
}

/// A residue `(mod X m)` whose `X` is compound (`+`, `-`, `*`) or a numeral: the kinds the chain
/// lifts (a numeral's residue lifts to the numeral — `(1 mod m)² mod m = 1 mod m`).
fn liftable(atom: &str, mkey: &str) -> bool {
    if !is_residue_of(atom, mkey) {
        return foreign_residue(atom, mkey).is_some();
    }
    match parse_all(atom).ok().and_then(|v| v.into_iter().next()) {
        Some(S::List(items)) => match &items[1] {
            S::List(inner) => matches!(inner.first(), Some(S::Atom(h)) if h == "+" || h == "-" || h == "*"),
            S::Atom(a) => is_numeral(a),
        },
        _ => false,
    }
}

/// A residue `(mod X n)` of ANOTHER modulus `n` that is a polynomial multiple of `m` (every
/// monomial of `n` carries `m`: `n = m1·m2` against `m = m1`) — the Chinese-remainder shape,
/// where the solution is a residue modulo the product and the goal reads it modulo each factor.
/// Lifted through its own identity `X − n·(div X n)`, the `n` part is a multiple of `m`.
/// Returns `(X, n, n as a polynomial)`.
fn foreign_residue(atom: &str, mkey: &str) -> Option<(S, S, Poly)> {
    let Some(S::List(items)) = parse_all(atom).ok().and_then(|v| v.into_iter().next()) else { return None };
    if items.len() != 3 || !matches!(&items[0], S::Atom(h) if h == "mod") || is_literal(&items[2]) {
        return None;
    }
    let n = &items[2];
    if render(n) == mkey {
        return None;
    }
    let np = to_poly(n)?;
    if np.is_empty() || !np.keys().all(|mono| mono.iter().any(|a| a == mkey)) {
        return None;
    }
    Some((items[1].clone(), n.clone(), np))
}

fn has_liftable(p: &Poly, mkey: &str) -> bool {
    p.keys().flat_map(|mono| mono.iter()).any(|a| liftable(a, mkey))
}

/// One chain step: every non-residue atom decomposed (`y = (div y m)·m + (mod y m)`), the modulus
/// kept as a factor, residues kept — except ONE occurrence (per monomial) of the first liftable
/// residue, replaced by `X − m·(div X m)` with `X`'s non-residue atoms decomposed. Returns the
/// residue part `P`, the quotient part `Q` (`cur = P + m·Q`), and whether anything changed.
fn step(cur: &Poly, m: &S, mkey: &str, granular: bool) -> Option<(Poly, Poly, bool)> {
    let target: Option<String> = cur.keys().flat_map(|mono| mono.iter()).filter(|a| liftable(a, mkey)).min().cloned();
    // GRANULAR (the witness rung's chains): ONE substitution per step — a lift when there is one
    // (the lifted term's atoms left bare), else one atom decomposed — so every premise is a
    // single identity the solver verifies at once (measured on the CRT chain: a lift and three
    // decompositions in one premise are past 40 s; one at a time, milliseconds).
    let decompose_only: Option<String> = if granular && target.is_none() {
        cur.iter()
            .filter(|(mono, _)| !mono.iter().any(|a| a == mkey))
            .flat_map(|(mono, _)| mono.iter())
            .filter(|a| *a != mkey && !is_residue_of(a, mkey))
            .min()
            .cloned()
    } else {
        None
    };
    let mut full = p_const(0);
    let mut changed = false;
    for (mono, c) in cur {
        let mut term = p_const(*c);
        let mut lifted_once = false;
        // A monomial that already carries the modulus is a multiple of `m` whatever its other
        // atoms are: it goes to `Q` as it stands. Decomposing its atoms too only multiplies the
        // terms (measured on the CRT chain: past the 64-term cap, which ended the chain silently).
        let multiple = mono.iter().any(|a| a == mkey);
        for atom in mono {
            let factor: Poly = if atom == mkey || multiple {
                p_atom(atom.clone())
            } else if target.as_deref() == Some(atom.as_str()) && !lifted_once {
                lifted_once = true;
                changed = true;
                if granular {
                    lift_bare(atom, mkey)?
                } else {
                    lift(atom, m, mkey)?
                }
            } else if is_residue_of(atom, mkey) || (granular && decompose_only.as_deref() != Some(atom.as_str())) {
                p_atom(atom.clone())
            } else {
                changed = true;
                let y = parse_all(atom).ok().and_then(|v| v.into_iter().next()).unwrap_or_else(|| S::Atom(atom.clone()));
                decomposition_of(&y, m)
            };
            term = p_mul(&term, &factor)?;
        }
        full = p_add(&full, &term)?;
    }
    // Split: monomials with a factor m → Q (one m removed); the rest → P.
    let mut p = Poly::new();
    let mut q = Poly::new();
    for (mono, c) in &full {
        if let Some(pos) = mono.iter().position(|a| a == mkey) {
            let mut rest = mono.clone();
            rest.remove(pos);
            *q.entry(rest).or_insert(0) += c;
        } else {
            *p.entry(mono.clone()).or_insert(0) += c;
        }
    }
    p.retain(|_, c| *c != 0);
    q.retain(|_, c| *c != 0);
    Some((p, q, changed))
}

/// `(mod X n)` as `X − n·(div X n)` with `X`'s atoms left as they are (`n` the chain's modulus or
/// a foreign multiple of it, as its polynomial): the granular lift.
fn lift_bare(atom: &str, mkey: &str) -> Option<Poly> {
    let Some(S::List(items)) = parse_all(atom).ok().and_then(|v| v.into_iter().next()) else { return None };
    if items.len() != 3 {
        return None;
    }
    let (x, n) = (&items[1], &items[2]);
    let np = if render(n) == mkey { p_atom(mkey.to_string()) } else { foreign_residue(atom, mkey)?.2 };
    let xp = to_poly(x)?;
    let q = p_atom(format!("(div {} {})", render(x), render(n)));
    p_add(&xp, &p_scale(&p_mul(&np, &q)?, -1)?)
}

/// `(mod X m)` as `X − m·(div X m)`, with `X`'s non-residue atoms decomposed against `m`.
fn lift(atom: &str, m: &S, mkey: &str) -> Option<Poly> {
    let Some(S::List(items)) = parse_all(atom).ok().and_then(|v| v.into_iter().next()) else { return None };
    // A foreign residue `(mod X n)`, `n` a multiple of `m`: `X − n·(div X n)`, `n` as its polynomial.
    if let Some((x, n, np)) = foreign_residue(atom, mkey) {
        let xp = to_poly(&x)?;
        let mut acc = p_const(0);
        for (mono, c) in &xp {
            let mut term = p_const(*c);
            for a in mono {
                let factor = if a == mkey || is_residue_of(a, mkey) {
                    p_atom(a.clone())
                } else {
                    let y = parse_all(a).ok().and_then(|v| v.into_iter().next()).unwrap_or_else(|| S::Atom(a.clone()));
                    decomposition_of(&y, m)
                };
                term = p_mul(&term, &factor)?;
            }
            acc = p_add(&acc, &term)?;
        }
        let q = p_atom(format!("(div {} {})", render(&x), render(&n)));
        let correction = p_scale(&p_mul(&np, &q)?, -1)?;
        return p_add(&acc, &correction);
    }
    let x = &items[1];
    let xp = to_poly(x)?;
    let mut acc = p_const(0);
    for (mono, c) in &xp {
        let mut term = p_const(*c);
        for a in mono {
            let factor = if a == mkey || is_residue_of(a, mkey) {
                p_atom(a.clone())
            } else {
                let y = parse_all(a).ok().and_then(|v| v.into_iter().next()).unwrap_or_else(|| S::Atom(a.clone()));
                decomposition_of(&y, m)
            };
            term = p_mul(&term, &factor)?;
        }
        acc = p_add(&acc, &term)?;
    }
    let mut mono = vec![mkey.to_string(), format!("(div {} {mkey})", render(x))];
    mono.sort();
    let mut correction = Poly::new();
    correction.insert(mono, -1);
    p_add(&acc, &correction)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solver() -> Option<&'static str> {
        for s in ["z3", "cvc5"] {
            if std::process::Command::new(s).arg("--version").output().is_ok() {
                return Some(s);
            }
        }
        None
    }

    #[test]
    fn parses_and_renders() {
        let v = parse_all("(set-logic ALL) ; c\n(assert (= (mod (+ a b) m) 0))").unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!(render(&v[1]), "(assert (= (mod (+ a b) m) 0))");
    }

    #[test]
    fn sum_congruence_instance_has_the_residue_witness() {
        let smt = "(set-logic ALL)\n(declare-const a Int)\n(declare-const b Int)\n(declare-const m Int)\n(assert (not (= (mod (+ a b) m) (mod (+ (mod a m) (mod b m)) m))))\n(check-sat)\n";
        let out = augment(smt).expect("instances");
        // (mod (+ a b) m): P = (mod a m) + (mod b m), Q = (div a m) + (div b m)
        assert!(out.contains("(= (mod (+ a b) m) (mod (+ (mod a m) (mod b m)) m))"), "{out}");
        assert!(out.contains("(div a m)") && out.contains("(div b m)"), "{out}");
        assert!(out.ends_with("(check-sat)\n"));
    }

    #[test]
    fn literal_moduli_and_quantified_bodies_are_left_alone() {
        assert!(augment("(set-logic ALL)\n(declare-const a Int)\n(assert (not (= (mod (+ a 2) 2) (mod a 2))))\n(check-sat)\n").is_none());
        assert!(augment("(set-logic ALL)\n(assert (forall ((a Int) (m Int)) (= (mod a m) (mod a m))))\n(check-sat)\n").is_none());
    }

    #[test]
    fn define_fun_applications_are_unfolded_and_lets_substituted() {
        let smt = "(set-logic ALL)\n(define-fun self ((a Int) (m Int)) Int (mod a m))\n(declare-const x Int)\n(declare-const k Int)\n(assert (let ((y x)) (not (= (self (+ y 1) k) 0))))\n(check-sat)\n";
        let out = augment(smt).expect("instances");
        assert!(out.contains("(mod (+ x 1) k)"), "{out}");
    }

    #[test]
    fn cross_modulus_rewrite_reaches_the_divisibility_step() {
        // a mod g with a also divided by b: the variant rewrites a by its decomposition against b.
        let smt = "(set-logic ALL)\n(declare-const a Int)\n(declare-const b Int)\n(declare-const g Int)\n(assert (= (mod b g) 0))\n(assert (= (mod (mod a b) g) 0))\n(assert (not (= (mod a g) 0)))\n(check-sat)\n";
        let out = augment(smt).expect("instances");
        assert!(out.contains("(div (div a b) g)"), "{out}");
    }

    /// Debug aid: `NL_EUCLID_DUMP=<cert.smt2> cargo test --lib euclid::tests::dump_augmented -- --ignored`
    /// writes the augmented certificate beside the input (`<cert.smt2>.euclid`).
    #[test]
    #[ignore]
    fn dump_augmented() {
        let Ok(p) = std::env::var("NL_EUCLID_DUMP") else { return };
        let smt = std::fs::read_to_string(&p).expect("read");
        let with_variants = std::env::var("NL_EUCLID_VARIANTS").is_ok();
        match instances(&smt, with_variants) {
            Some(insts) => {
                let idx = smt.rfind("(check-sat)").unwrap();
                let mut s = smt[..idx].to_string();
                for i in &insts {
                    s.push_str(&i.text);
                    s.push('\n');
                }
                s.push_str(&smt[idx..]);
                std::fs::write(format!("{p}.euclid"), s).expect("write");
            }
            None => eprintln!("no instances for {p}"),
        }
    }

    #[test]
    fn the_final_query_abstracts_mod_div_and_products_consistently() {
        let s = parse_all("(assert (= (mod (* (self b e m) (mod b m)) m) (+ (* 2 (div e 2)) (* (- 1) x y) (mod e 2))))").unwrap();
        let a = render(&abstract_mod(&s[0]));
        assert_eq!(a, "(assert (= (umod (umul (self b e m) (umod b m)) m) (+ (* 2 (div e 2)) (* (- 1) (umul x y)) (mod e 2))))");
        // Literal moduli and literal coefficients stay real: what is left is linear.
        assert!(a.contains("(div e 2)") && a.contains("(mod e 2)") && !a.contains("umod e"));
        let mut acc = BTreeSet::new();
        collect_umods(&abstract_mod(&s[0]), &mut acc);
        assert_eq!(acc.len(), 2, "{acc:?}");
    }

    /// The modular inverse's contract as `check-refinement` states it: `ext_gcd`'s Bézout post
    /// instantiated at `(a mod m, m)`, the body's `case` as an `ite` over the datatype, the post's
    /// `case` on the result as a tester-guarded `ite`.
    const INVERSE_POST: &str = "(set-logic ALL)\n\
(declare-datatype dt0 ((dt0_Just (dt0_Just_v Int)) (dt0_None)))\n\
(declare-datatype dt1 ((mk_dt1 (dt1_0 Int) (dt1_1 Int))))\n\
(declare-datatype dt2 ((mk_dt2 (dt2_0 Int) (dt2_1 dt1))))\n\
(declare-fun callee_ext (Int Int) dt2)\n\
(declare-const a Int)\n(declare-const m Int)\n\
(assert (=> (and (>= (mod a m) 0) (>= m 0)) (= (dt2_0 (callee_ext (mod a m) m)) (+ (* (mod a m) (dt1_0 (dt2_1 (callee_ext (mod a m) m)))) (* m (dt1_1 (dt2_1 (callee_ext (mod a m) m))))))))\n\
(assert (not (or (not (> m 1)) (let ((g (dt2_0 (callee_ext (mod a m) m))) (x (dt1_0 (dt2_1 (callee_ext (mod a m) m))))) (let ((r (ite (= g 1) (dt0_Just (mod x m)) dt0_None))) (ite ((_ is dt0_Just) r) (= (mod (* a (dt0_Just_v r)) m) (mod 1 m)) true))))))\n\
(check-sat)\n";

    #[test]
    fn the_witness_rung_reads_the_bezout_identity_as_a_rule() {
        // The chain reaches `(a mod m)·(x mod m)` and is stuck; the rule `r·x → g − m·y` (from the
        // contract instance) with `x mod m` un-lifted gives the hypothesis-derived quotient, and the
        // accessor variant reaches `x` through the result's `ite` in the first place.
        let insts = instances(INVERSE_POST, false).expect("instances");
        let rule_inst = insts.iter().find(|i| !i.uses.is_empty()).expect("a witness-rung instance");
        assert!(rule_inst.uses[0].contains("(dt2_0 (callee_ext (mod a m) m))"), "{:?}", rule_inst.uses);
        assert!(rule_inst.premise.contains("(dt1_1 (dt2_1 (callee_ext (mod a m) m)))"), "the quotient mentions y: {}", rule_inst.premise);
        assert!(insts.iter().any(|i| i.premise.starts_with("(= (* a (mod (dt1_0")), "the accessor variant's term: {:?}", insts.iter().map(|i| &i.premise).collect::<Vec<_>>());
    }

    #[test]
    fn the_foreign_residue_lifts_against_a_factor_of_its_modulus() {
        // `(mod X (* m1 m2))` read modulo `m1`: lifted through its own identity, the `m1·m2` part is
        // a multiple of `m1`, and the residue part is `a1 mod m1`.
        let smt = "(set-logic ALL)\n(declare-const a1 Int)\n(declare-const m1 Int)\n(declare-const m2 Int)\n(declare-const t Int)\n(assert (> m1 1))\n(assert (> m2 1))\n(assert (not (= (mod (mod (+ a1 (* m1 t)) (* m1 m2)) m1) (mod a1 m1))))\n(check-sat)\n";
        let insts = instances(smt, false).expect("instances");
        assert!(insts.iter().any(|i| i.conclusion.ends_with("(mod a1 m1)))") && i.premise.contains("(div (+ a1 (* m1 t)) (* m1 m2))") && i.witness), "{:?}", insts.iter().map(|i| &i.conclusion).collect::<Vec<_>>());
        if let Some(s) = solver() {
            assert!(retry(smt, s).is_some(), "the product-modulus residue closes modulo a factor");
        }
    }

    #[test]
    fn accessor_variants_push_through_the_ite() {
        let g = parse_all("(= (mod (* a (dt0_Just_v (ite (= g 1) (dt0_Just (mod x m)) dt0_None))) m) (mod 1 m))").unwrap().remove(0);
        let vs = accessor_variants(&g);
        assert_eq!(vs.len(), 2, "{vs:?}");
        assert_eq!(render(&vs[1]), "(= (mod (* a (mod x m)) m) (mod 1 m))");
        let t = parse_all("(mk_dt2 g (mk_dt1 x y))").unwrap().remove(0);
        assert_eq!(render(&accessor_of_constructor("dt2_1", &t).unwrap()), "(mk_dt1 x y)");
        assert!(accessor_of_constructor("dt0_Just_v", &t).is_none());
    }

    #[test]
    fn the_inverse_contract_closes_through_the_witness_rung() {
        let Some(s) = solver() else { return };
        assert!(retry(INVERSE_POST, s).is_some(), "the modular-inverse post should close with the Bézout rule's instance");
        // Stepwise too: the rule instance's premise verifies beside its hypothesis and the facts.
        let sw = retry_stepwise(INVERSE_POST, s).expect("stepwise");
        assert!(sw.verified >= 2, "{} of {}", sw.verified, sw.total);
        assert!(sw.steps_smt.contains("(assert (> m 1))"), "the precondition rides along as a small fact");
    }

    /// `crt2`'s contract as `check-refinement` states it: `mod_inverse`'s certified contract at
    /// `(m1, m2)` (a residue equation under the `Just` tester), the solution a residue modulo the
    /// PRODUCT, the post reading it modulo each factor.
    const CRT_POST: &str = "(set-logic ALL)\n\
(declare-datatype dt0 ((dt0_Just (dt0_Just_v Int)) (dt0_None)))\n\
(declare-fun callee_inv (Int Int) dt0)\n\
(declare-const a1 Int)\n(declare-const m1 Int)\n(declare-const a2 Int)\n(declare-const m2 Int)\n\
(assert (=> (> m2 1) (ite ((_ is dt0_Just) (callee_inv m1 m2)) (= (mod (* m1 (dt0_Just_v (callee_inv m1 m2))) m2) (mod 1 m2)) true)))\n\
(assert (not (or (not (and (> m1 1) (> m2 1))) (let ((r (ite ((_ is dt0_Just) (callee_inv m1 m2)) (let ((inv (dt0_Just_v (callee_inv m1 m2)))) (let ((t (mod (* (- a2 a1) inv) m2))) (dt0_Just (mod (+ a1 (* m1 t)) (* m1 m2))))) dt0_None))) (ite ((_ is dt0_Just) r) (and (= (mod (dt0_Just_v r) m1) (mod a1 m1)) (= (mod (dt0_Just_v r) m2) (mod a2 m2))) true)))))\n\
(check-sat)\n";

    #[test]
    fn the_crt_contract_closes_through_the_foreign_lift_and_the_residue_rule() {
        let insts = instances(CRT_POST, false).expect("instances");
        // The residue rule from mod_inverse's contract: `m1·inv → (1 mod m2) + m2·(div (m1·inv) m2)`.
        assert!(insts.iter().any(|i| !i.uses.is_empty() && i.uses[0].contains("(mod (* m1 (dt0_Just_v (callee_inv m1 m2))) m2)")), "{:?}", insts.iter().map(|i| &i.uses).collect::<Vec<_>>());
        let Some(s) = solver() else { return };
        assert!(retry(CRT_POST, s).is_some() || retry_stepwise(CRT_POST, s).is_some(), "the CRT post should close");
    }

    #[test]
    fn the_lemma_is_discharged_and_the_congruences_close() {
        let Some(s) = solver() else { return };
        assert!(lemma_discharged(s));
        let sum = "(set-logic ALL)\n(declare-const a Int)\n(declare-const b Int)\n(declare-const m Int)\n(assert (not (or (= m 0) (= (mod (+ a b) m) (mod (+ (mod a m) (mod b m)) m)))))\n(check-sat)\n";
        assert!(retry(sum, s).is_some(), "sum congruence should close with the instances");
        let product = "(set-logic ALL)\n(declare-const a Int)\n(declare-const b Int)\n(declare-const m Int)\n(assert (not (or (= m 0) (= (mod (* a b) m) (mod (* (mod a m) (mod b m)) m)))))\n(check-sat)\n";
        assert!(retry(product, s).is_some(), "product congruence should close with the instances");
        // A FALSE congruence stays open: instances are true facts, they refute nothing.
        let wrong = "(set-logic ALL)\n(declare-const a Int)\n(declare-const b Int)\n(declare-const m Int)\n(assert (not (or (= m 0) (= (mod (+ a b) m) (mod (+ (mod a m) (mod b m) 1) m)))))\n(check-sat)\n";
        assert!(retry(wrong, s).is_none());
    }
}
