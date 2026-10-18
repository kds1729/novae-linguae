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
        collect(goal, &mut candidates, &mut decompositions, &mut mod_keys);
        goal_count = candidates.len();
    }
    for e in expanded.iter().rev().skip(1) {
        collect(e, &mut candidates, &mut decompositions, &mut mod_keys);
    }
    if candidates.is_empty() {
        return None;
    }
    let mut out: Vec<String> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for (i, (t, m)) in candidates.iter().enumerate() {
        if out.len() >= MAX_INSTANCES {
            break;
        }
        let Some(tp) = to_poly(t) else { continue };
        for inst in instance_chain(t, m, &tp, &BTreeMap::new()) {
            if seen.insert(inst.clone()) {
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
                    if seen.insert(inst.clone()) {
                        out.push(inst);
                    }
                }
                if out.len() >= MAX_INSTANCES {
                    break;
                }
            }
        }
    }
    if out.is_empty() {
        return None;
    }
    let idx = smt.rfind("(check-sat)")?;
    let mut s = String::with_capacity(smt.len() + out.iter().map(|x| x.len() + 1).sum::<usize>() + 128);
    s.push_str(&smt[..idx]);
    s.push_str(&format!(";; the Euclidean rung: instances of `{LEMMA_NAME}` (discharged separately) at the certificate's mod/div terms\n"));
    for i in &out {
        s.push_str(i);
        s.push('\n');
    }
    s.push_str(&smt[idx..]);
    Some(s)
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
fn instance_chain(t: &S, m: &S, tp: &Poly, rewrite: &BTreeMap<String, Poly>) -> Vec<String> {
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
            out.push(format!(
                "(assert (=> (and (not (= {mkey} 0)) (= {x} (+ (* {qw} {mkey}) {rw})) (>= {rw} 0) (< {rw} (abs {mkey}))) (and (= (div {x} {mkey}) {qw}) (= (mod {x} {mkey}) {rw}))))"
            ));
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

/// A residue `(mod X m)` whose `X` is compound (`+`, `-`, `*`): the one kind the chain lifts.
fn liftable(atom: &str, mkey: &str) -> bool {
    if !is_residue_of(atom, mkey) {
        return false;
    }
    matches!(parse_all(atom).ok().and_then(|v| v.into_iter().next()), Some(S::List(items)) if matches!(&items[1], S::List(inner) if matches!(inner.first(), Some(S::Atom(h)) if h == "+" || h == "-" || h == "*")))
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
        match augment(&smt) {
            Some(a) => std::fs::write(format!("{p}.euclid"), a).expect("write"),
            None => eprintln!("no instances for {p}"),
        }
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
