//! The EXACT-DIVISION rung for the Gaussian integers: divisibility through a certified
//! Euclidean remainder, stepwise.
//!
//! The Gaussian tier (`evolution/gaussian-tier-poc`) states "g divides z" as `gi_rem(z, g) = 0`,
//! and `gi_gcd`'s "the result divides both inputs" is Euclid's step: g divides w and the remainder
//! of z by w, hence z. In ℤ the Euclidean rung (`euclid`) proves such steps through instances of
//! the division-uniqueness lemma over the solver's own `mod`/`div`. Here the remainder is a
//! RECORD — `gi_rem`, with `gi_div` its quotient and `gi_norm` the norm — known to the solver only
//! through its certified contract: the DIVISION IDENTITY `z = r + q·w` (Gaussian product,
//! componentwise) and the DESCENT `N(r) < N(w)`. Measured 2026-11-09 on `gi_gcd`'s step: z3
//! decides neither the exact-division lemma (`z = k·g ⇒ gi_rem(z, g) = 0`) in one query nor the
//! step with the lemma's conclusions supplied as implications — the premise `z = k·g` is a
//! degree-3 polynomial identity through three contract instances, and the solver does not find
//! the substitutions. It decides each of these at once:
//!
//!  1. the lemmas, over integers: `x = k·g ∧ x = r + q·g ⇒ r = (k − q)·g` (an identity);
//!     `g ≠ 0 ∧ r = u·g ∧ N(r) < N(g) ⇒ r = 0` (norm multiplicativity and the bound, which z3
//!     proves given `r = u·g` and not without it); and the closure `w = kw·g ∧ y = ky·g ∧
//!     x = m·w + y ⇒ x = (m·kw + ky)·g` (distributivity) — discharged by the solver once per
//!     solver and cached, never assumed;
//!  2. an instance's PREMISE as LINEAR facts: for the goal's `gi_rem(z, g)`, with `z`'s
//!     decomposition against another divisor `w` in the certificate (`gi_rem(z, w)`, `gi_div(z, w)`
//!     — the body's own call), the premise is `w = gi_div(w, g)·g ∧ y = gi_div(y, g)·g ∧ z =
//!     gi_div(z, w)·w + y` for `y = gi_rem(z, w)` — every conjunct a linear consequence of a
//!     contract instance and a zero-remainder hypothesis (the induction hypothesis), no
//!     distribution asked of the solver; checked alone, under a branch of the body's `case` when
//!     it does not hold outright (in `gi_gcd`'s base case the divisor is `z` itself: the premise
//!     is `z = 1·g`; in the step it is the chain);
//!  3. the FINAL query: the certificate with each verified premise's conclusion
//!     `g ≠ 0 ∧ branch ⇒ gi_rem(z, g) = 0` as a fact — milliseconds.
//!
//! Honesty: an instance's conclusion follows from its premise by the three lemmas and the
//! remainder's contract instances the certificate already carries, so asserting it after the
//! solver verified the premise adds nothing a model of the certificate could falsify; a premise
//! the solver does not verify leaves its instance out. The certificate records the premise
//! queries (`steps`) beside the final query and names the lemma scripts. The rung detects the
//! contract SHAPE — a tuple-valued callee of four integers whose posts are the Gaussian division
//! identity against another such callee and a norm descent, with the norm's post `a² + b²` —
//! from the certificate's `;; certified contract` axioms, by polynomial normalization, so no
//! record is named. It is a retry on UNKNOWN only.

use std::collections::{BTreeMap, BTreeSet};

use crate::euclid::{definitions, discharged_get, discharged_set, equations_in, expand, is_literal, p_add, p_atom, p_mul, p_scale, parse_all, render, to_poly, Poly, S};
use crate::prove::{run_smt_rlimit, SatAnswer};

/// `x = k·g ∧ x = r + q·g ⇒ r = (k − q)·g`, componentwise over the Gaussian product.
pub const LEMMA_IDENTITY_SMT: &str = "(set-logic QF_NIA)\n\
;; exact division, the identity: x = k·g ∧ x = r + q·g ⇒ r = (k − q)·g  (Gaussian products, componentwise)\n\
(declare-const g1 Int) (declare-const g2 Int) (declare-const x1 Int) (declare-const x2 Int)\n\
(declare-const k1 Int) (declare-const k2 Int) (declare-const q1 Int) (declare-const q2 Int)\n\
(declare-const r1 Int) (declare-const r2 Int)\n\
(assert (= x1 (- (* k1 g1) (* k2 g2))))\n(assert (= x2 (+ (* k1 g2) (* k2 g1))))\n\
(assert (= x1 (+ r1 (- (* q1 g1) (* q2 g2)))))\n(assert (= x2 (+ r2 (+ (* q1 g2) (* q2 g1)))))\n\
(assert (not (and (= r1 (- (* (- k1 q1) g1) (* (- k2 q2) g2))) (= r2 (+ (* (- k1 q1) g2) (* (- k2 q2) g1))))))\n\
(check-sat)\n";

/// `g ≠ 0 ∧ r = u·g ∧ N(r) < N(g) ⇒ r = 0`: a nonzero multiple of g has norm at least N(g).
pub const LEMMA_ZERO_SMT: &str = "(set-logic QF_NIA)\n\
;; exact division, the bound: g ≠ 0 ∧ r = u·g ∧ N(r) < N(g) ⇒ r = 0  (N(u·g) = N(u)·N(g) ≥ N(g) unless u = 0)\n\
(declare-const g1 Int) (declare-const g2 Int) (declare-const u1 Int) (declare-const u2 Int)\n\
(declare-const r1 Int) (declare-const r2 Int)\n\
(assert (or (not (= g1 0)) (not (= g2 0))))\n\
(assert (= r1 (- (* u1 g1) (* u2 g2))))\n(assert (= r2 (+ (* u1 g2) (* u2 g1))))\n\
(assert (< (+ (* r1 r1) (* r2 r2)) (+ (* g1 g1) (* g2 g2))))\n\
(assert (not (and (= r1 0) (= r2 0))))\n\
(check-sat)\n";

/// `w = kw·g ∧ y = ky·g ∧ x = m·w + y ⇒ x = (m·kw + ky)·g`: multiples of g are closed under
/// Gaussian-linear combination.
pub const LEMMA_CLOSURE_SMT: &str = "(set-logic QF_NIA)\n\
;; exact division, closure: w = kw·g ∧ y = ky·g ∧ x = m·w + y ⇒ x = (m·kw + ky)·g\n\
(declare-const g1 Int) (declare-const g2 Int) (declare-const w1 Int) (declare-const w2 Int)\n\
(declare-const kw1 Int) (declare-const kw2 Int) (declare-const y1 Int) (declare-const y2 Int)\n\
(declare-const ky1 Int) (declare-const ky2 Int) (declare-const m1 Int) (declare-const m2 Int)\n\
(declare-const x1 Int) (declare-const x2 Int)\n\
(assert (= w1 (- (* kw1 g1) (* kw2 g2))))\n(assert (= w2 (+ (* kw1 g2) (* kw2 g1))))\n\
(assert (= y1 (- (* ky1 g1) (* ky2 g2))))\n(assert (= y2 (+ (* ky1 g2) (* ky2 g1))))\n\
(assert (= x1 (+ (- (* m1 w1) (* m2 w2)) y1)))\n(assert (= x2 (+ (+ (* m1 w2) (* m2 w1)) y2)))\n\
(assert (not (and (= x1 (- (* (+ (- (* m1 kw1) (* m2 kw2)) ky1) g1) (* (+ (+ (* m1 kw2) (* m2 kw1)) ky2) g2)))\n\
                  (= x2 (+ (* (+ (- (* m1 kw1) (* m2 kw2)) ky1) g2) (* (+ (+ (* m1 kw2) (* m2 kw1)) ky2) g1))))))\n\
(check-sat)\n";

/// `x = k·g ∧ G = u·g` for a unit `u` ⇒ `x = (ū·k)·G`: a divisor's associates divide the same
/// numbers (the four units, each a ring identity).
pub const LEMMA_ASSOCIATE_SMT: &str = "(set-logic QF_NIA)\n\
;; associates: x = k·g ∧ G = u·g (u a unit) ⇒ x = (conj(u)·k)·G, for each of the four units\n\
(declare-const g1 Int) (declare-const g2 Int) (declare-const k1 Int) (declare-const k2 Int)\n\
(declare-const x1 Int) (declare-const x2 Int) (declare-const G1 Int) (declare-const G2 Int)\n\
(assert (= x1 (- (* k1 g1) (* k2 g2))))\n(assert (= x2 (+ (* k1 g2) (* k2 g1))))\n\
(assert (not (and\n\
  (=> (and (= G1 g1) (= G2 g2)) (and (= x1 (- (* k1 G1) (* k2 G2))) (= x2 (+ (* k1 G2) (* k2 G1)))))\n\
  (=> (and (= G1 (- g2)) (= G2 g1)) (and (= x1 (- (* k2 G1) (* (- k1) G2))) (= x2 (+ (* k2 G2) (* (- k1) G1)))))\n\
  (=> (and (= G1 (- g1)) (= G2 (- g2))) (and (= x1 (- (* (- k1) G1) (* (- k2) G2))) (= x2 (+ (* (- k1) G2) (* (- k2) G1)))))\n\
  (=> (and (= G1 g2) (= G2 (- g1))) (and (= x1 (- (* (- k2) G1) (* k1 G2))) (= x2 (+ (* (- k2) G2) (* k1 G1))))))))\n\
(check-sat)\n";

/// The marker the rung's final query carries (the CLI writes the lemma scripts beside a
/// certificate that carries it).
pub const MARKER: &str = ";; the exact-division rung";

/// A premise query's budget — z3's RESOURCE limit for that many seconds (`run_smt_rlimit`), so a
/// verified premise set does not depend on the machine's load: with the contract instances
/// stated, a premise that holds is unsat in milliseconds, one that does not costs this.
const PREMISE_SECS: u64 = 1;
const FINAL_SECS: u64 = 10;
const BUDGET_SECS: u64 = 60;
const MAX_GOAL_TERMS: usize = 8;
const MAX_INSTANCES: usize = 24;
const MAX_CONDITIONS: usize = 2;

/// Has `solver` proved the three lemmas? Once per solver per thread (cached beside the Euclidean
/// rung's verdicts, so a worker thread inherits them).
pub fn lemmas_discharged(solver: &str) -> bool {
    let key = format!("gauss:{solver}");
    if let Some(v) = discharged_get(&key) {
        return v;
    }
    // Under a RESOURCE limit, not a wall clock: a verdict cached for the thread must not depend on
    // how loaded the machine was when it was asked (the full test suite's eight threads beside a
    // module build once timed the associate lemma out at 5 s wall and left the rung unavailable to
    // that thread for good, 2026-11-09).
    let unsat = |smt: &str| matches!(run_smt_rlimit(smt, solver, 5), Ok(SatAnswer::Unsat));
    let ok = unsat(LEMMA_IDENTITY_SMT) && unsat(LEMMA_ZERO_SMT) && unsat(LEMMA_CLOSURE_SMT) && unsat(LEMMA_ASSOCIATE_SMT);
    // A negative under an expired enclosing budget (`prove::past_deadline`: the solver was not
    // asked, or was cut short) says nothing about the solver and must not be remembered for the
    // thread — the suite's threads share these verdicts across tests, and one test's spent law
    // budget made the rung unavailable to the next (intermittent, 2026-11-09).
    if ok || !crate::prove::past_deadline() {
        discharged_set(&key, ok);
    }
    ok
}

/// A certified Euclidean pair the certificate's contract axioms describe: the remainder callee,
/// its quotient callee, the norm callee, and the pair datatype's two selectors.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pair {
    pub rem: String,
    pub div: String,
    pub norm: String,
    pub sel0: String,
    pub sel1: String,
}

/// A stepwise proof by this rung: the final query (the certificate with the verified instances'
/// conclusions as facts), the premise queries as one re-checkable script, and the counts.
pub struct Exact {
    pub final_smt: String,
    pub steps_smt: String,
    pub verified: usize,
    pub total: usize,
}

fn is_form(f: &S, h: &str) -> bool {
    matches!(f, S::List(items) if matches!(items.first(), Some(S::Atom(x)) if x == h))
}

fn atom(s: &S) -> Option<&str> {
    match s {
        S::Atom(a) => Some(a.as_str()),
        _ => None,
    }
}

fn list(s: &S) -> Option<&[S]> {
    match s {
        S::List(items) => Some(items.as_slice()),
        _ => None,
    }
}

fn app(head: &str, args: &[&str]) -> String {
    format!("({head} {})", args.join(" "))
}

/// The pair datatypes (name → its two Int selectors) and the declared functions (name → (param
/// sorts, result sort)) of a certificate.
fn declarations(forms: &[S]) -> (BTreeMap<String, (String, String)>, BTreeMap<String, (Vec<String>, String)>) {
    let mut dts: BTreeMap<String, (String, String)> = BTreeMap::new();
    let mut funs: BTreeMap<String, (Vec<String>, String)> = BTreeMap::new();
    for f in forms {
        let Some(items) = list(f) else { continue };
        match items.first().and_then(atom) {
            // (declare-datatype dt0 ((mk_dt0 (dt0_0 Int) (dt0_1 Int))))
            Some("declare-datatype") if items.len() == 3 => {
                let Some(name) = atom(&items[1]) else { continue };
                let Some(ctors) = list(&items[2]) else { continue };
                if ctors.len() != 1 {
                    continue;
                }
                let Some(ctor) = list(&ctors[0]) else { continue };
                if ctor.len() != 3 {
                    continue;
                }
                let sel = |s: &S| -> Option<String> {
                    let f = list(s)?;
                    (f.len() == 2 && atom(&f[1]) == Some("Int")).then(|| atom(&f[0]).map(String::from)).flatten()
                };
                if let (Some(s0), Some(s1)) = (sel(&ctor[1]), sel(&ctor[2])) {
                    dts.insert(name.to_string(), (s0, s1));
                }
            }
            Some("declare-fun") if items.len() == 4 => {
                let Some(name) = atom(&items[1]) else { continue };
                let Some(params) = list(&items[2]) else { continue };
                let params: Option<Vec<String>> = params.iter().map(|p| atom(p).map(String::from)).collect();
                let Some(ret) = atom(&items[3]) else { continue };
                if let Some(params) = params {
                    funs.insert(name.to_string(), (params, ret.to_string()));
                }
            }
            _ => {}
        }
    }
    (dts, funs)
}

/// A quantified contract axiom `(forall ((v Int)…) BODY)` or `(forall (…) (=> PRE BODY))`: the
/// binder names and the body (the precondition is not needed: the rung guards every conclusion
/// by the divisor being nonzero, which is the remainder's precondition).
fn axiom(f: &S) -> Option<(Vec<String>, S)> {
    let items = list(f)?;
    if atom(items.first()?) != Some("assert") || items.len() != 2 {
        return None;
    }
    let q = list(&items[1])?;
    if atom(q.first()?) != Some("forall") || q.len() != 3 {
        return None;
    }
    let binders: Option<Vec<String>> = list(&q[1])?.iter().map(|b| list(b).and_then(|p| atom(p.first()?)).map(String::from)).collect();
    let body = match list(&q[2]) {
        Some(imp) if atom(imp.first()?) == Some("=>") && imp.len() == 3 => imp[2].clone(),
        _ => q[2].clone(),
    };
    Some((binders?, body))
}

/// The polynomial `a − b` of an equation `(= a b)`, else `None`.
fn equation_poly(body: &S) -> Option<Poly> {
    let items = list(body)?;
    if atom(items.first()?) != Some("=") || items.len() != 3 {
        return None;
    }
    let a = to_poly(&items[1])?;
    let b = to_poly(&items[2])?;
    p_add(&a, &p_scale(&b, -1)?)
}

/// The certified Euclidean pairs a certificate's contract axioms describe (see the module
/// comment): for every tuple-valued 4-ary callee `R`, a 4-ary callee `D` of the same sort whose
/// product `R` adds to — both components of the division identity — a norm callee `N` with the
/// descent post and the post `N(a, b) = a² + b²`.
pub fn pairs(forms: &[S]) -> Vec<Pair> {
    let (dts, funs) = declarations(forms);
    let axioms: Vec<(Vec<String>, S)> = forms.iter().filter_map(axiom).collect();
    let four_ary: Vec<(&String, &String)> = funs
        .iter()
        .filter(|(_, (ps, ret))| ps.len() == 4 && ps.iter().all(|p| p == "Int") && dts.contains_key(ret))
        .map(|(n, (_, ret))| (n, ret))
        .collect();
    let norms: Vec<&String> = funs.iter().filter(|(_, (ps, ret))| ps.len() == 2 && ps.iter().all(|p| p == "Int") && ret == "Int").map(|(n, _)| n).collect();
    // N(a, b) = a² + b² — as an axiom of the certificate, or, when the norm is applied only inside
    // the remainder's posts and the certificate states no axiom of its own for it, as a PROVED post
    // of its installed contract (the linked view's closure; `gi_gcd_canonical`'s certificate
    // declares `gi_norm` without its contract, 2026-11-09).
    let norm_ok = |n: &str| {
        installed_norm_is_square(n) || axioms.iter().any(|(vs, body)| {
            if vs.len() != 2 {
                return false;
            }
            let Some(p) = equation_poly(body) else { return false };
            let (a, b) = (&vs[0], &vs[1]);
            let mut t = p_atom(app(n, &[a, b]));
            t = p_add(&t, &p_scale(&p_mul(&p_atom(a.clone()), &p_atom(a.clone())).unwrap_or_default(), -1).unwrap_or_default()).unwrap_or_default();
            t = p_add(&t, &p_scale(&p_mul(&p_atom(b.clone()), &p_atom(b.clone())).unwrap_or_default(), -1).unwrap_or_default()).unwrap_or_default();
            p == t || p_scale(&p, -1).as_ref() == Some(&t)
        })
    };
    let mut out = Vec::new();
    for (r, dt) in &four_ary {
        let (sel0, sel1) = &dts[*dt];
        for (d, dt2) in &four_ary {
            if d == r || dt2 != dt {
                continue;
            }
            // The two components of z = R(z, w) + D(z, w)·w, as the template polynomials over the
            // axiom's own binder names — compared with each 4-binder equation axiom.
            let matches_identity = |component: usize| {
                axioms.iter().any(|(vs, body)| {
                    if vs.len() != 4 {
                        return false;
                    }
                    let Some(p) = equation_poly(body) else { return false };
                    let args: Vec<&str> = vs.iter().map(|v| v.as_str()).collect();
                    let r_app = app(r, &args);
                    let d_app = app(d, &args);
                    let (z, c, dd) = if component == 0 { (&vs[0], &vs[2], &vs[3]) } else { (&vs[1], &vs[2], &vs[3]) };
                    let r_c = p_atom(app(if component == 0 { sel0 } else { sel1 }, &[&r_app]));
                    let d0 = p_atom(app(sel0, &[&d_app]));
                    let d1 = p_atom(app(sel1, &[&d_app]));
                    // component 0: z − R0 − D0·c + D1·d;  component 1: z − R1 − D0·d − D1·c
                    let Some(prod0) = p_mul(&d0, &p_atom(if component == 0 { c.clone() } else { dd.clone() })) else { return false };
                    let Some(prod1) = p_mul(&d1, &p_atom(if component == 0 { dd.clone() } else { c.clone() })) else { return false };
                    let mut t = p_atom(z.clone());
                    for (term, sign) in [(&r_c, -1), (&prod0, -1), (&prod1, if component == 0 { 1 } else { -1 })] {
                        let Some(s) = p_scale(term, sign) else { return false };
                        let Some(sum) = p_add(&t, &s) else { return false };
                        t = sum;
                    }
                    p == t || p_scale(&p, -1).as_ref() == Some(&t)
                })
            };
            if !matches_identity(0) || !matches_identity(1) {
                continue;
            }
            // The descent: (< (N (sel0 (R v…)) (sel1 (R v…))) (N v2 v3)).
            for n in &norms {
                let descent = axioms.iter().any(|(vs, body)| {
                    if vs.len() != 4 {
                        return false;
                    }
                    let args: Vec<&str> = vs.iter().map(|v| v.as_str()).collect();
                    let r_app = app(r, &args);
                    let want = format!("(< ({n} ({sel0} {r_app}) ({sel1} {r_app})) ({n} {} {}))", vs[2], vs[3]);
                    render(body) == want
                });
                if descent && norm_ok(n) {
                    out.push(Pair { rem: (*r).clone(), div: (*d).clone(), norm: (*n).clone(), sel0: sel0.clone(), sel1: sel1.clone() });
                }
            }
        }
    }
    out
}

/// A contract predicate over the callee's parameters and `result`, as a polynomial over atoms (the
/// parameter names and `result`): `var`, integer `lit`, `add`/`sub`/`mul`/`neg`; `None` otherwise.
fn pred_poly(node: &serde_json::Value) -> Option<Poly> {
    match node.get("kind").and_then(|k| k.as_str())? {
        "var" => Some(p_atom(node.get("name")?.as_str()?.to_string())),
        "lit" => {
            let v = node.get("value")?;
            if v.get("kind").and_then(|k| k.as_str()) != Some("int") {
                return None;
            }
            let n = match v.get("value")? {
                serde_json::Value::Number(n) => n.as_i64()? as i128,
                serde_json::Value::String(s) => s.parse::<i128>().ok()?,
                _ => return None,
            };
            Some(crate::euclid::p_const(n))
        }
        "app" => {
            let args = node.get("args")?.as_array()?;
            match node.get("op").and_then(|o| o.as_str())? {
                "add" if args.len() == 2 => p_add(&pred_poly(&args[0])?, &pred_poly(&args[1])?),
                "sub" if args.len() == 2 => p_add(&pred_poly(&args[0])?, &p_scale(&pred_poly(&args[1])?, -1)?),
                "mul" if args.len() == 2 => p_mul(&pred_poly(&args[0])?, &pred_poly(&args[1])?),
                "neg" if args.len() == 1 => p_scale(&pred_poly(&args[0])?, -1),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Does the installed contract of the callee with this symbol carry the proved post
/// `result = a² + b²` over its two parameters?
fn installed_norm_is_square(symbol: &str) -> bool {
    let Some(c) = crate::prove::installed_contract_by_symbol(symbol) else { return false };
    if c.params.len() != 2 {
        return false;
    }
    let (a, b) = (&c.params[0], &c.params[1]);
    let Some(sq) = p_mul(&p_atom(a.clone()), &p_atom(a.clone())).and_then(|aa| p_add(&aa, &p_mul(&p_atom(b.clone()), &p_atom(b.clone()))?)) else { return false };
    let Some(want) = p_add(&p_atom("result".into()), &p_scale(&sq, -1).unwrap_or_default()) else { return false };
    c.posts.iter().any(|post| {
        let Some(items) = post.get("args").and_then(|a| a.as_array()) else { return false };
        if post.get("op").and_then(|o| o.as_str()) != Some("eq") || items.len() != 2 {
            return false;
        }
        let (Some(l), Some(r)) = (pred_poly(&items[0]), pred_poly(&items[1])) else { return false };
        let Some(d) = p_add(&l, &p_scale(&r, -1).unwrap_or_default()) else { return false };
        d == want || p_scale(&d, -1).as_ref() == Some(&want)
    })
}

/// Every application `(R x1 x2 g1 g2)` of a remainder callee in a term, as (x, g) pairs of rendered components.
fn rem_apps(s: &S, rems: &BTreeSet<&str>, out: &mut Vec<([String; 2], [String; 2], String)>) {
    let Some(items) = list(s) else { return };
    if let Some(h) = items.first().and_then(atom) {
        if matches!(h, "forall" | "exists" | "lambda") {
            return;
        }
        if rems.contains(h) && items.len() == 5 {
            let x = [render(&items[1]), render(&items[2])];
            let g = [render(&items[3]), render(&items[4])];
            let key = format!("{h} {} {} {} {}", x[0], x[1], g[0], g[1]);
            if !out.iter().any(|(_, _, k)| *k == key) {
                out.push((x, g, key));
            }
        }
    }
    for x in items {
        rem_apps(x, rems, out);
    }
}

/// The `ite` conditions of a certificate's facts (a body's `case` guards, in goal scope): the
/// branches a premise may hold under.
fn conditions(expanded: &[S], out: &mut Vec<String>) {
    fn walk(s: &S, out: &mut Vec<String>) {
        let Some(items) = list(s) else { return };
        if let Some(h) = items.first().and_then(atom) {
            if matches!(h, "forall" | "exists" | "lambda") {
                return;
            }
            if h == "ite" && items.len() == 4 {
                let c = render(&items[1]);
                if !out.contains(&c) && c.len() < 200 {
                    out.push(c);
                }
            }
        }
        for x in items {
            walk(x, out);
        }
    }
    for e in expanded {
        walk(e, out);
    }
}

struct Candidate {
    /// The goal term's x and g (rendered components) and the remainder symbol.
    x: [String; 2],
    g: [String; 2],
    pair: Pair,
    /// The premise, an SMT formula.
    premise: String,
    /// What the premise says, for the steps script.
    note: String,
    /// The remainder applications `(x, g)` the premise rests on: their contract instances are
    /// stated in the premise query (the quantified axioms alone leave z3 to find them — 0.8 s
    /// against milliseconds, measured on `gi_gcd`'s step).
    apps: Vec<([String; 2], [String; 2])>,
    /// A chain through another divisor (its premise holds in a recursive step, where the
    /// induction hypothesis speaks) rather than a trivial witness (a base case's).
    chain: bool,
    /// A fixed condition the premise is checked under and the conclusion guarded by (an
    /// associate relation `G = u·w`), in place of the branch tries.
    cond: Option<String>,
}

/// The units `u` for which the certificate's facts relate the goal's divisor `G` to another divisor
/// `w` as `G = u·w` — read off its equations (through implications, conjunctions, disjunctions and
/// `ite` branches) by polynomial normalization: both components' relations must appear. Returned as
/// the two components of `u·w`.
fn associate_units(all: &[S], g: &[String; 2], w: &[String; 2]) -> Vec<[String; 2]> {
    let mut eqs: Vec<(S, S)> = Vec::new();
    for e in all {
        equations_in(e, true, &mut eqs);
    }
    let polys: Vec<Poly> = eqs
        .iter()
        .filter_map(|(l, r)| {
            let d = p_add(&to_poly(l)?, &p_scale(&to_poly(r)?, -1)?)?;
            (!d.is_empty()).then_some(d)
        })
        .collect();
    let holds = |lhs: &str, rhs_atom: &str, sign: i128| -> bool {
        let Some(t) = p_add(&p_atom(lhs.to_string()), &p_scale(&p_atom(rhs_atom.to_string()), -sign).unwrap_or_default()) else { return false };
        polys.iter().any(|p| *p == t || p_scale(p, -1).as_ref() == Some(&t))
    };
    let neg = |t: &str| format!("(- {t})");
    let mut out = Vec::new();
    // u = 1: G = (w1, w2); u = i: (−w2, w1); u = −1: (−w1, −w2); u = −i: (w2, −w1)
    if holds(&g[0], &w[0], 1) && holds(&g[1], &w[1], 1) {
        out.push([w[0].clone(), w[1].clone()]);
    }
    if holds(&g[0], &w[1], -1) && holds(&g[1], &w[0], 1) {
        out.push([neg(&w[1]), w[0].clone()]);
    }
    if holds(&g[0], &w[0], -1) && holds(&g[1], &w[1], -1) {
        out.push([neg(&w[0]), neg(&w[1])]);
    }
    if holds(&g[0], &w[1], 1) && holds(&g[1], &w[0], -1) {
        out.push([w[1].clone(), neg(&w[0])]);
    }
    out
}

/// The remainder's contract — the division identity's two components and the norm descent —
/// instantiated at the given applications, each under the divisor being nonzero.
fn contract_instances(pair: &Pair, apps: &[([String; 2], [String; 2])]) -> String {
    let mut out = String::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for (x, g) in apps {
        let r = app(&pair.rem, &[&x[0], &x[1], &g[0], &g[1]]);
        if !seen.insert(r.clone()) {
            continue;
        }
        let d = app(&pair.div, &[&x[0], &x[1], &g[0], &g[1]]);
        let pre = format!("(or (not (= {} 0)) (not (= {} 0)))", g[0], g[1]);
        let (r0, r1) = (app(&pair.sel0, &[&r]), app(&pair.sel1, &[&r]));
        let (d0, d1) = (app(&pair.sel0, &[&d]), app(&pair.sel1, &[&d]));
        out.push_str(&format!("(assert (=> {pre} (= {} (+ {r0} (- (* {d0} {}) (* {d1} {}))))))\n", x[0], g[0], g[1]));
        out.push_str(&format!("(assert (=> {pre} (= {} (+ {r1} (+ (* {d0} {}) (* {d1} {}))))))\n", x[1], g[1], g[0]));
        out.push_str(&format!("(assert (=> {pre} (< ({} {r0} {r1}) ({} {} {}))))\n", pair.norm, pair.norm, g[0], g[1]));
    }
    out
}

/// `k·g` componentwise.
fn times(k: &[String; 2], g: &[String; 2]) -> [String; 2] {
    [format!("(- (* {} {}) (* {} {}))", k[0], g[0], k[1], g[1]), format!("(+ (* {} {}) (* {} {}))", k[0], g[1], k[1], g[0])]
}

fn both(eqs: [(String, String); 2]) -> String {
    format!("(and (= {} {}) (= {} {}))", eqs[0].0, eqs[0].1, eqs[1].0, eqs[1].1)
}

/// The instances for the goal's remainder terms (see the module comment): `x = 1·g`, `x = 0·g`,
/// and for every other divisor `w` the certificate takes `x`'s remainder by, the chain through `w`.
fn candidates(goal: &S, all: &[S], pairs: &[Pair]) -> Vec<Candidate> {
    let rems: BTreeSet<&str> = pairs.iter().map(|p| p.rem.as_str()).collect();
    let mut goal_terms = Vec::new();
    rem_apps(goal, &rems, &mut goal_terms);
    let mut every = Vec::new();
    for e in all {
        rem_apps(e, &rems, &mut every);
    }
    let mut out = Vec::new();
    for (x, g, key) in goal_terms.iter().take(MAX_GOAL_TERMS) {
        let sym = key.split(' ').next().unwrap_or_default();
        let Some(pair) = pairs.iter().find(|p| p.rem == sym) else { continue };
        if g.iter().all(|c| is_literal(&S::Atom(c.clone()))) {
            continue; // a literal divisor: the solver's own arithmetic
        }
        let one = times(&["1".into(), "0".into()], g);
        out.push(Candidate {
            x: x.clone(),
            g: g.clone(),
            pair: pair.clone(),
            premise: both([(x[0].clone(), one[0].clone()), (x[1].clone(), one[1].clone())]),
            note: "x = 1·g (the divisor is x itself)".into(),
            apps: vec![(x.clone(), g.clone())],
            chain: false,
            cond: None,
        });
        out.push(Candidate {
            x: x.clone(),
            g: g.clone(),
            pair: pair.clone(),
            premise: both([(x[0].clone(), "0".into()), (x[1].clone(), "0".into())]),
            note: "x = 0·g".into(),
            apps: vec![(x.clone(), g.clone())],
            chain: false,
            cond: None,
        });
        for (x2, w, _) in &every {
            if x2 != x || w == g {
                continue;
            }
            // An ASSOCIATE: the certificate relates g to w as g = u·w for a unit u (`gi_canonical`'s
            // post at `gi_gcd`'s result), and x's remainder by w is taken — then x = D(x, w)·w under
            // `rem(x, w) = 0` and the associate lemma carries the multiple over to g. Checked under
            // the relation itself as the condition (the facts state it as one disjunct of four).
            for ug in associate_units(all, g, w) {
                let xs: Vec<&str> = x.iter().map(|s| s.as_str()).collect();
                let ws: Vec<&str> = w.iter().map(|s| s.as_str()).collect();
                let d_xw = app(&pair.div, &[xs[0], xs[1], ws[0], ws[1]]);
                let m = [app(&pair.sel0, &[&d_xw]), app(&pair.sel1, &[&d_xw])];
                let mw = times(&m, w);
                out.push(Candidate {
                    x: x.clone(),
                    g: g.clone(),
                    pair: pair.clone(),
                    premise: both([(x[0].clone(), mw[0].clone()), (x[1].clone(), mw[1].clone())]),
                    note: format!("an associate: g = u·w for w = ({}, {}), x = D(x, w)·w", w[0], w[1]),
                    apps: vec![(x.clone(), w.clone()), (x.clone(), g.clone())],
                    chain: false,
                    cond: Some(format!("(and (= {} {}) (= {} {}))", g[0], ug[0], g[1], ug[1])),
                });
                if out.len() >= MAX_INSTANCES {
                    return out;
                }
            }
            // Only a divisor the certificate itself reduces (some `R(w, ·)` — the hypothesis
            // `g | w` the chain needs has to be stated somewhere); the induction hypothesis's
            // own result term is a divisor of nothing here, and its chain would only cost its tries.
            if !every.iter().any(|(x3, _, _)| x3 == w) {
                continue;
            }
            let xs: Vec<&str> = x.iter().map(|s| s.as_str()).collect();
            let ws: Vec<&str> = w.iter().map(|s| s.as_str()).collect();
            let gs: Vec<&str> = g.iter().map(|s| s.as_str()).collect();
            let r_xw = app(&pair.rem, &[xs[0], xs[1], ws[0], ws[1]]);
            let d_xw = app(&pair.div, &[xs[0], xs[1], ws[0], ws[1]]);
            let y = [app(&pair.sel0, &[&r_xw]), app(&pair.sel1, &[&r_xw])];
            let m = [app(&pair.sel0, &[&d_xw]), app(&pair.sel1, &[&d_xw])];
            let d_wg = app(&pair.div, &[ws[0], ws[1], gs[0], gs[1]]);
            let d_yg = app(&pair.div, &[&y[0], &y[1], gs[0], gs[1]]);
            let kw = [app(&pair.sel0, &[&d_wg]), app(&pair.sel1, &[&d_wg])];
            let ky = [app(&pair.sel0, &[&d_yg]), app(&pair.sel1, &[&d_yg])];
            let wg = times(&kw, g);
            let yg = times(&ky, g);
            let mw = times(&m, w);
            let premise = format!(
                "(and (= {} {}) (= {} {}) (= {} {}) (= {} {}) (= {} (+ {} {})) (= {} (+ {} {})))",
                w[0], wg[0], w[1], wg[1], y[0], yg[0], y[1], yg[1], x[0], mw[0], y[0], x[1], mw[1], y[1]
            );
            out.push(Candidate {
                x: x.clone(),
                g: g.clone(),
                pair: pair.clone(),
                premise,
                note: format!("the chain through w = ({}, {}): w = D(w, g)·g, y = D(y, g)·g for y = R(x, w), x = D(x, w)·w + y", w[0], w[1]),
                apps: vec![(x.clone(), g.clone()), (x.clone(), w.clone()), (w.clone(), g.clone()), (y.clone(), g.clone())],
                chain: true,
                cond: None,
            });
            if out.len() >= MAX_INSTANCES {
                return out;
            }
        }
    }
    out
}

/// Debug aid (`NL_EUCLID_DEBUG_DIR=<dir>`): why the rung declined a certificate, with the certificate.
fn debug_declined(why: &str, smt: &str) {
    if let Ok(dir) = std::env::var("NL_EUCLID_DEBUG_DIR") {
        let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
        let _ = std::fs::write(format!("{dir}/exact-{n}.declined.txt"), format!("{why}\n\n{smt}"));
    }
}

/// The rung over a certificate that stayed UNKNOWN: `Some` when it closed.
pub fn retry(smt: &str, solver: &str) -> Option<Exact> {
    if crate::prove::past_deadline() {
        return None;
    }
    let forms = parse_all(smt).ok()?;
    let pairs = pairs(&forms);
    if pairs.is_empty() {
        debug_declined("no certified Euclidean pair among the contract axioms", smt);
        return None;
    }
    if !lemmas_discharged(solver) {
        debug_declined("the lemmas did not discharge", smt);
        return None;
    }
    let defs = definitions(&forms);
    let asserted: Vec<&S> = forms.iter().filter(|f| is_form(f, "assert")).collect();
    let (goal_form, hyp_forms) = asserted.split_last()?;
    let expanded: Vec<S> = asserted
        .iter()
        .filter_map(|f| match f {
            S::List(items) if items.len() == 2 => Some(expand(&items[1], &BTreeMap::new(), &defs, 0)),
            _ => None,
        })
        .collect();
    let goal = expanded.last()?;
    let cands = candidates(goal, &expanded, &pairs);
    if cands.is_empty() {
        debug_declined("no remainder term with a non-literal divisor in the goal", smt);
        return None;
    }
    let mut conds: Vec<String> = Vec::new();
    conditions(&expanded[..expanded.len() - 1], &mut conds);
    conds.truncate(MAX_CONDITIONS);
    // The context every premise is checked in: the certificate's hypotheses AND its negated goal —
    // the final query holds both, so a premise need only follow under both (the goal's own
    // literals carry the step's `g ≠ 0`: a post stated as "the result is 0 or divides both" gives
    // the induction hypothesis the same escape, and the chain's zero remainders are reached only
    // past it).
    let header: String = forms
        .iter()
        .filter(|f| !is_form(f, "assert") && !is_form(f, "check-sat") && !is_form(f, "get-model"))
        .map(render)
        .collect::<Vec<_>>()
        .join("\n");
    let hyps: String = hyp_forms.iter().chain(std::iter::once(goal_form)).map(|f| format!("{}\n", render(f))).collect();
    let mut steps = String::new();
    steps.push_str(";; the exact-division rung, stepwise: each instance's premise — linear consequences of the remainder's\n;; contract instances and the certificate's hypotheses — checked alone against the certificate's hypotheses and its\n;; negated goal (both hold in the final query), with the remainder's contract stated at the premise's applications,\n;; outright or under a branch of the body's case; every (check-sat) below\n;; must answer unsat for its conclusion to enter the final query.\n;; Lemmas (discharged by the solver first, written beside this script): the identity, the bound, closure, associates.\n");
    steps.push_str(&header);
    steps.push('\n');
    steps.push_str(&hyps);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(BUDGET_SECS);
    let mut facts: Vec<String> = Vec::new();
    let mut verified = 0usize;
    for c in &cands {
        if crate::prove::past_deadline() {
            return None;
        }
        if std::time::Instant::now() >= deadline {
            steps.push_str(&format!(";; the {BUDGET_SECS} s budget ran out; the remaining instances were not attempted\n"));
            break;
        }
        let nz = format!("(or (not (= {} 0)) (not (= {} 0)))", c.g[0], c.g[1]);
        let r_xg = app(&c.pair.rem, &[&c.x[0], &c.x[1], &c.g[0], &c.g[1]]);
        let concl = format!("(and (= ({} {r_xg}) 0) (= ({} {r_xg}) 0))", c.pair.sel0, c.pair.sel1);
        // The likely branch first — a trivial witness holds in a base case (the `case`'s
        // condition), a chain in the step (its negation) — then outright, then the rest: each
        // failing try costs its budget.
        let mut tries: Vec<Option<String>> = Vec::new();
        if let Some(fixed) = &c.cond {
            tries.push(Some(fixed.clone()));
        } else if let Some(cd) = conds.first() {
            let (likely, other) = (Some(cd.clone()), Some(format!("(not {cd})")));
            let (first, second) = if c.chain { (other, likely) } else { (likely, other) };
            tries.push(first);
            tries.push(None);
            tries.push(second);
        } else {
            tries.push(None);
        }
        if c.cond.is_none() {
            for cd in conds.iter().skip(1) {
                tries.push(Some(cd.clone()));
                tries.push(Some(format!("(not {cd})")));
            }
        }
        let insts = contract_instances(&c.pair, &c.apps);
        let mut closed = false;
        for cond in tries {
            // One verified premise per instance: a premise that holds outright or under a
            // branch is not asked again under another (each failing try costs its timeout).
            if closed || std::time::Instant::now() >= deadline || crate::prove::past_deadline() {
                break;
            }
            let cond_assert = cond.as_ref().map(|cd| format!("(assert {cd})\n")).unwrap_or_default();
            let q = format!("{header}\n{hyps}{insts}{cond_assert}(assert (not {}))\n(check-sat)\n", c.premise);
            let ok = matches!(run_smt_rlimit(&q, solver, PREMISE_SECS), Ok(SatAnswer::Unsat));
            steps.push_str(&format!(
                ";; {} — for ({}, {}) by ({}, {}){}\n(push 1)\n{insts}{cond_assert}(assert (not {}))\n(check-sat){}\n(pop 1)\n",
                c.note,
                c.x[0],
                c.x[1],
                c.g[0],
                c.g[1],
                cond.as_ref().map(|cd| format!(", under {cd}")).unwrap_or_default(),
                c.premise,
                if ok { "" } else { " ; NOT discharged — the instance was left out of the final query" }
            ));
            if ok {
                verified += 1;
                let guard = match &cond {
                    None => nz.clone(),
                    Some(cd) => format!("(and {nz} {cd})"),
                };
                facts.push(format!("(assert (=> {guard} {concl}))\n"));
                closed = true;
            }
        }
    }
    if facts.is_empty() {
        debug_declined(&format!("no premise verified\n\n{steps}"), smt);
        return None;
    }
    // The final query: the certificate with the conclusions before its negated goal.
    let mut fin = String::new();
    for f in &forms {
        if is_form(f, "check-sat") || is_form(f, "get-model") {
            continue;
        }
        if std::ptr::eq(f, *goal_form) {
            fin.push_str(&format!("{MARKER}: the conclusions of the verified instances (steps script), each under the divisor being nonzero and its branch\n"));
            for fact in &facts {
                fin.push_str(fact);
            }
        }
        fin.push_str(&render(f));
        fin.push('\n');
    }
    fin.push_str("(check-sat)\n");
    if let Ok(dir) = std::env::var("NL_EUCLID_DEBUG_DIR") {
        let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
        let _ = std::fs::write(format!("{dir}/exact-{n}.final.smt2"), &fin);
        let _ = std::fs::write(format!("{dir}/exact-{n}.steps.smt2"), &steps);
    }
    match run_smt_rlimit(&fin, solver, FINAL_SECS) {
        Ok(SatAnswer::Unsat) => Some(Exact { final_smt: fin, steps_smt: steps, verified, total: cands.len() }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn z3() -> bool {
        std::process::Command::new("z3").arg("--version").output().map(|o| o.status.success()).unwrap_or(false)
    }

    #[test]
    fn the_three_lemmas_discharge() {
        if !z3() {
            return;
        }
        assert!(lemmas_discharged("z3"));
    }

    /// A certificate in the shape the numeric rung emits for the Gaussian tier: the pair detected
    /// from the contract axioms, by polynomial normalization, with the identity written in another
    /// order than the template's.
    const SHAPE: &str = "(set-logic ALL)\n\
(declare-datatype dt0 ((mk_dt0 (dt0_0 Int) (dt0_1 Int))))\n\
(declare-fun norm (Int Int) Int)\n(declare-fun rem (Int Int Int Int) dt0)\n(declare-fun quo (Int Int Int Int) dt0)\n\
(assert (forall ((a Int) (b Int)) (=> true (= (norm a b) (+ (* b b) (* a a))))))\n\
(assert (forall ((a Int) (b Int) (c Int) (d Int)) (=> (or (not (= c 0)) (not (= d 0))) (< (norm (dt0_0 (rem a b c d)) (dt0_1 (rem a b c d))) (norm c d)))))\n\
(assert (forall ((a Int) (b Int) (c Int) (d Int)) (=> (or (not (= c 0)) (not (= d 0))) (= (+ (dt0_0 (rem a b c d)) (- (* c (dt0_0 (quo a b c d))) (* d (dt0_1 (quo a b c d))))) a))))\n\
(assert (forall ((a Int) (b Int) (c Int) (d Int)) (=> (or (not (= c 0)) (not (= d 0))) (= b (+ (dt0_1 (rem a b c d)) (+ (* (dt0_0 (quo a b c d)) d) (* (dt0_1 (quo a b c d)) c)))))))\n\
(declare-const a Int)\n(declare-const b Int)\n(declare-const g1 Int)\n(declare-const g2 Int)\n";

    #[test]
    fn detects_the_pair_from_contract_shapes() {
        let forms = parse_all(SHAPE).unwrap();
        let ps = pairs(&forms);
        assert_eq!(ps, vec![Pair { rem: "rem".into(), div: "quo".into(), norm: "norm".into(), sel0: "dt0_0".into(), sel1: "dt0_1".into() }]);
    }

    #[test]
    fn a_norm_without_the_square_post_is_not_a_pair() {
        let forms = parse_all(&SHAPE.replace("(+ (* b b) (* a a))", "(+ (* b b) a)")).unwrap();
        assert!(pairs(&forms).is_empty());
    }

    /// `g ≠ 0 ⇒ rem(g, g) = 0` and `rem(0, g) = 0` — the two trivial witnesses, through the rung
    /// (the solver alone answers unknown: the remainder is uninterpreted but for its contract).
    #[test]
    fn divides_itself_and_zero_by_the_rung() {
        if !z3() {
            return;
        }
        let smt = format!(
            "{SHAPE}(assert (or (not (= g1 0)) (not (= g2 0))))\n\
(assert (not (and (= (dt0_0 (rem g1 g2 g1 g2)) 0) (= (dt0_1 (rem g1 g2 g1 g2)) 0) (= (dt0_0 (rem 0 0 g1 g2)) 0) (= (dt0_1 (rem 0 0 g1 g2)) 0))))\n(check-sat)\n"
        );
        let out = retry(&smt, "z3").expect("the rung closes the trivial divisibilities");
        assert!(out.verified >= 2, "both premises verified, got {}", out.verified);
        assert!(out.final_smt.contains(MARKER));
        assert!(out.steps_smt.contains("(check-sat)"));
    }

    /// Euclid's step as the rung sees it in `gi_gcd`: g divides w and the remainder of x by w,
    /// so g divides x — the chain premise, linear, with the two zero remainders as hypotheses.
    #[test]
    fn euclid_step_by_the_chain() {
        if !z3() {
            return;
        }
        let smt = format!(
            "{SHAPE}(declare-const c Int)\n(declare-const d Int)\n\
(assert (or (not (= g1 0)) (not (= g2 0))))\n(assert (or (not (= c 0)) (not (= d 0))))\n\
(assert (and (= (dt0_0 (rem c d g1 g2)) 0) (= (dt0_1 (rem c d g1 g2)) 0)))\n\
(assert (and (= (dt0_0 (rem (dt0_0 (rem a b c d)) (dt0_1 (rem a b c d)) g1 g2)) 0) (= (dt0_1 (rem (dt0_0 (rem a b c d)) (dt0_1 (rem a b c d)) g1 g2)) 0)))\n\
(assert (not (and (= (dt0_0 (rem a b g1 g2)) 0) (= (dt0_1 (rem a b g1 g2)) 0))))\n(check-sat)\n"
        );
        // The solver alone does not close it (a sanity check of what the rung adds).
        assert!(!matches!(crate::prove::run_smt_secs(&smt, "z3", 2), Ok(SatAnswer::Unsat)), "the bare step should not be decided by the solver alone");
        let out = retry(&smt, "z3").expect("the rung closes Euclid's step");
        assert!(out.steps_smt.contains("the chain through w"));
    }

    /// `gi_gcd_canonical`'s shape: G is one of the four associates of g (a disjunction), g divides x
    /// (`rem(x, g) = 0`) — so G divides x.
    #[test]
    fn an_associate_of_a_divisor_divides() {
        if !z3() {
            return;
        }
        let smt = format!(
            "{SHAPE}(declare-const G1 Int)\n(declare-const G2 Int)\n\
(assert (or (not (= g1 0)) (not (= g2 0))))\n\
(assert (or (and (= G1 g1) (= G2 g2)) (and (= G1 g2) (= G2 (- 0 g1))) (and (= G1 (- 0 g1)) (= G2 (- 0 g2))) (and (= G1 (- 0 g2)) (= G2 g1))))\n\
(assert (and (= (dt0_0 (rem a b g1 g2)) 0) (= (dt0_1 (rem a b g1 g2)) 0)))\n\
(assert (not (or (and (= G1 0) (= G2 0)) (and (= (dt0_0 (rem a b G1 G2)) 0) (= (dt0_1 (rem a b G1 G2)) 0)))))\n(check-sat)\n"
        );
        let out = retry(&smt, "z3").expect("the rung carries divisibility over to the associate");
        assert!(out.steps_smt.contains("an associate"));
        assert!(out.verified >= 4, "one premise per unit, got {}", out.verified);
    }

    #[test]
    fn declines_a_certificate_without_a_pair() {
        let smt = "(set-logic ALL)\n(declare-const x Int)\n(assert (> x 0))\n(assert (not (>= x 1)))\n(check-sat)\n";
        assert!(retry(smt, "z3").is_none());
    }
}
