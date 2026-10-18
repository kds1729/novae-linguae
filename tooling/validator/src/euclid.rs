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
    // (1) The premises, each alone.
    let mut steps = String::new();
    steps.push_str(";; the Euclidean rung, stepwise: each instance's premise (a ring identity over the solver's div/mod axioms)\n;; checked in isolation; every (check-sat) below must answer unsat for the instance to enter the final query.\n");
    steps.push_str(&header);
    steps.push('\n');
    let mut facts: Vec<(String, String)> = Vec::new();
    // A budget: a premise the solver cannot verify costs its whole timeout, and a law that is
    // not going to close must not pay hundreds of them. Past the deadline the remaining
    // instances are left out (stated in the steps script) and the final query runs on what
    // verified.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(STEPWISE_BUDGET_SECS);
    let mut skipped = 0usize;
    for i in &insts {
        if std::time::Instant::now() >= deadline {
            skipped += 1;
            continue;
        }
        let q = format!("{header}\n(assert (not (= {} 0)))\n(assert (not {}))\n(check-sat)\n", i.modulus, i.premise);
        let ok = matches!(run_smt_secs(&q, solver, STEP_SECS), Ok(SatAnswer::Unsat));
        steps.push_str(&format!(
            "(push 1)\n(assert (not (= {} 0)))\n(assert (not {}))\n(check-sat){}\n(pop 1)\n",
            i.modulus,
            i.premise,
            if ok { "" } else { " ; NOT discharged by the proving solver — the instance was left out of the final query" }
        ));
        if ok {
            facts.push((i.modulus.clone(), i.conclusion.clone()));
        }
    }
    if skipped > 0 {
        steps.push_str(&format!(";; {skipped} instance(s) not attempted: the {STEPWISE_BUDGET_SECS} s budget for premises ran out\n"));
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
    let augmented = augment(smt)?;
    if !lemma_discharged(solver) {
        return None;
    }
    match run_smt_secs(&augmented, solver, 5) {
        Ok(SatAnswer::Unsat) => Some(augmented),
        _ => None,
    }
}

/// The certificate with the uniqueness-lemma instances for its `mod`/`div` terms appended before
/// `(check-sat)`. `None` when there is nothing to add.
pub fn augment(smt: &str) -> Option<String> {
    let out = instances(smt, false)?;
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
/// whole assertion as the one-query rung states it.
#[derive(Clone, Debug)]
pub struct Instance {
    pub modulus: String,
    pub premise: String,
    pub conclusion: String,
    pub text: String,
}

/// The lemma instances for a certificate's `mod`/`div` terms. With `with_variants`, the goal is
/// also read with the certificate's equations (`A = E` from unfoldings, hypothesis conclusions
/// and `ite` branches) substituted into it to a small depth — the terms a goal's two sides meet
/// at when they are formed only through such equations (the fast-exponentiation step). The
/// variants are for the STEPWISE checker only: in one query they drown the solver (measured).
pub fn instances(smt: &str, with_variants: bool) -> Option<Vec<Instance>> {
    let forms = parse_all(smt).ok()?;
    let mut defs: BTreeMap<String, (Vec<String>, S)> = BTreeMap::new();
    let mut asserted: Vec<S> = Vec::new();
    for f in &forms {
        let S::List(items) = f else { continue };
        match items.first() {
            Some(S::Atom(h)) if h == "define-fun" && items.len() == 5 => {
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
            Some(S::Atom(h)) if h == "assert" && items.len() == 2 => asserted.push(items[1].clone()),
            _ => {}
        }
    }
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
    if let Some(goal) = expanded.last() {
        if with_variants {
            let mut equations: Vec<(String, S)> = Vec::new();
            for e in &expanded {
                harvest(e, &mut equations, true);
            }
            for v in variants(goal, &equations) {
                collect(&v, &mut candidates, &mut decompositions, &mut mod_keys);
            }
        } else {
            collect(goal, &mut candidates, &mut decompositions, &mut mod_keys);
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
        for inst in instance_chain(t, m, &tp, &BTreeMap::new()) {
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
                for inst in instance_chain(t, m, &tp, &rewrite) {
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
    Some(out)
}

const MAX_INSTANCES_STEPWISE: usize = 512;
/// Per-premise solver budget (a verifiable premise answers in milliseconds) and the budget for
/// all of them (measured: the agreement of fast exponentiation verifies 250 premises in ~15 s).
const STEP_SECS: u64 = 2;
const STEPWISE_BUDGET_SECS: u64 = 60;
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

/// The lemma instances for `(mod t m)` / `(div t m)`, as a CHAIN: `t` (as `tp`, optionally with
/// some atoms pre-rewritten by `rewrite`) decomposed against `m` into `P₁ + m·Q₁`, then `P₁`
/// (where a nested residue `(mod X m)` of a compound `X` may remain) into `P₂ + m·Q₂` by lifting
/// ONE occurrence of such a residue through the exact identity `(mod X m) = X − m·(X div m)`, and
/// so on until the residue part mentions only residues of atoms — the canonical residue
/// polynomial both sides of a goal reach. One occurrence per step keeps every instance's premise
/// a low-degree identity the solver verifies from its own `div`/`mod` axioms (measured: lifting
/// everything at once leaves z3 at unknown on a fourth-degree premise; the chain closes).
/// Empty when nothing decomposes or the algebra overflows.
fn instance_chain(t: &S, m: &S, tp: &Poly, rewrite: &BTreeMap<String, Poly>) -> Vec<Instance> {
    let mkey = render(m);
    let mut out = Vec::new();
    let Some(mut cur) = apply_rewrite(tp, rewrite) else { return out };
    let mut x = render(t);
    for _ in 0..MAX_CHAIN {
        let Some((p, q, changed)) = step(&cur, m, &mkey) else { break };
        if !changed && rewrite.is_empty() {
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
            out.push(Instance { modulus: mkey.clone(), premise, conclusion, text });
        }
        if p.is_empty() || !has_liftable(&p, &mkey) {
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
        return false;
    }
    match parse_all(atom).ok().and_then(|v| v.into_iter().next()) {
        Some(S::List(items)) => match &items[1] {
            S::List(inner) => matches!(inner.first(), Some(S::Atom(h)) if h == "+" || h == "-" || h == "*"),
            S::Atom(a) => is_numeral(a),
        },
        _ => false,
    }
}

fn has_liftable(p: &Poly, mkey: &str) -> bool {
    p.keys().flat_map(|mono| mono.iter()).any(|a| liftable(a, mkey))
}

/// One chain step: every non-residue atom decomposed (`y = (div y m)·m + (mod y m)`), the modulus
/// kept as a factor, residues kept — except ONE occurrence (per monomial) of the first liftable
/// residue, replaced by `X − m·(div X m)` with `X`'s non-residue atoms decomposed. Returns the
/// residue part `P`, the quotient part `Q` (`cur = P + m·Q`), and whether anything changed.
fn step(cur: &Poly, m: &S, mkey: &str) -> Option<(Poly, Poly, bool)> {
    let target: Option<String> = cur.keys().flat_map(|mono| mono.iter()).filter(|a| liftable(a, mkey)).min().cloned();
    let mut full = p_const(0);
    let mut changed = false;
    for (mono, c) in cur {
        let mut term = p_const(*c);
        let mut lifted_once = false;
        for atom in mono {
            let factor: Poly = if atom == mkey {
                p_atom(atom.clone())
            } else if target.as_deref() == Some(atom.as_str()) && !lifted_once {
                lifted_once = true;
                changed = true;
                lift(atom, m, mkey)?
            } else if is_residue_of(atom, mkey) {
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

/// `(mod X m)` as `X − m·(div X m)`, with `X`'s non-residue atoms decomposed against `m`.
fn lift(atom: &str, m: &S, mkey: &str) -> Option<Poly> {
    let Some(S::List(items)) = parse_all(atom).ok().and_then(|v| v.into_iter().next()) else { return None };
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
