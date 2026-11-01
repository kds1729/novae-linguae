//! The GROUND rung: a quantifier-free certificate over uninterpreted callees and tuple datatypes,
//! rewritten to pure `QF_NIA` so the solver reaches its nonlinear engine.
//!
//! Measured on the Gaussian tier's Euclidean property (`N(z − q·w) < N(w)`, 2026-10-30): the
//! instances-only certificate — gi_div's four rounding bounds, gi_mul's exact posts, gi_norm's
//! definition, all stated at the body's applications — is a degree-4 polynomial problem z3
//! decides in under a second as plain integer arithmetic, and does not decide in 90 s with the
//! callees as uninterpreted functions under `(set-logic ALL)`: the uninterpreted symbols and the
//! datatype keep the query in the combination solver, whose nonlinear reasoning is the weaker one.
//!
//! The rewrite is Ackermann's reduction without the congruence constraints, and sound as a
//! PROOF rung for that reason: every `let` is expanded, every `define-fun` application
//! beta-reduced, every projection of a constructor reduced, every equality between datatype terms
//! split into its projections, and then every remaining callee application of Int or Bool sort —
//! and every projection of a datatype-valued one — becomes a fresh constant, one per distinct
//! term. Any model of the original formula gives a model of the rewritten one (evaluate each
//! term), so the rewritten formula's UNSAT is the original's UNSAT; the converse does not hold,
//! which is why a SAT here is never reported as anything. Asserts containing a quantifier are
//! dropped (weaker again). Anything outside Int/Bool/tuples — strings, lists, sums with testers,
//! datatype-valued constants — makes the rung decline (`None`), honestly.

use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq)]
enum S {
    A(String),
    L(Vec<S>),
}

fn tokenize(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let cs: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < cs.len() {
        let c = cs[i];
        if c == '(' || c == ')' {
            out.push(c.to_string());
            i += 1;
        } else if c.is_whitespace() {
            i += 1;
        } else if c == ';' {
            while i < cs.len() && cs[i] != '\n' {
                i += 1;
            }
        } else if c == '|' {
            let mut j = i + 1;
            while j < cs.len() && cs[j] != '|' {
                j += 1;
            }
            out.push(cs[i..=j.min(cs.len() - 1)].iter().collect());
            i = j + 1;
        } else if c == '"' {
            let mut j = i + 1;
            while j < cs.len() && cs[j] != '"' {
                j += 1;
            }
            out.push(cs[i..=j.min(cs.len() - 1)].iter().collect());
            i = j + 1;
        } else {
            let mut j = i;
            while j < cs.len() && !cs[j].is_whitespace() && cs[j] != '(' && cs[j] != ')' {
                j += 1;
            }
            out.push(cs[i..j].iter().collect());
            i = j;
        }
    }
    out
}

fn parse(tokens: &[String]) -> Option<Vec<S>> {
    let mut stack: Vec<Vec<S>> = Vec::new();
    let mut cur: Vec<S> = Vec::new();
    for t in tokens {
        if t == "(" {
            stack.push(std::mem::take(&mut cur));
        } else if t == ")" {
            let done = std::mem::take(&mut cur);
            cur = stack.pop()?;
            cur.push(S::L(done));
        } else {
            cur.push(S::A(t.clone()));
        }
    }
    if stack.is_empty() { Some(cur) } else { None }
}

fn show(e: &S) -> String {
    match e {
        S::A(a) => a.clone(),
        S::L(xs) => format!("({})", xs.iter().map(show).collect::<Vec<_>>().join(" ")),
    }
}

fn head(e: &S) -> Option<&str> {
    match e {
        S::L(xs) => xs.first().and_then(|h| if let S::A(a) = h { Some(a.as_str()) } else { None }),
        _ => None,
    }
}

struct Ground {
    /// constructor → (datatype, field sorts)
    ctors: BTreeMap<String, (String, Vec<String>)>,
    /// selector → (datatype, index)
    projs: BTreeMap<String, (String, usize)>,
    /// uninterpreted function → (arity, result sort)
    funs: BTreeMap<String, (usize, String)>,
    /// define-fun → (params, body)
    defs: BTreeMap<String, (Vec<String>, S)>,
    /// Ackermann constants: canonical term → (name, sort), in creation order
    consts: BTreeMap<String, (String, String)>,
    order: Vec<String>,
}

impl Ground {
    fn is_dt(&self, sort: &str) -> bool {
        self.ctors.values().any(|(dt, _)| dt == sort)
    }

    /// Expand `let`s and `define-fun` applications (beta), substituting `env`.
    fn expand(&self, e: &S, env: &BTreeMap<String, S>, depth: usize) -> Option<S> {
        if depth > 64 {
            return None;
        }
        match e {
            S::A(a) => Some(env.get(a).cloned().unwrap_or_else(|| e.clone())),
            S::L(xs) => {
                let h = head(e)?;
                if h == "let" {
                    let S::L(binds) = xs.get(1)? else { return None };
                    let mut inner = env.clone();
                    let mut fresh = Vec::new();
                    for b in binds {
                        let S::L(pair) = b else { return None };
                        let S::A(name) = pair.first()? else { return None };
                        fresh.push((name.clone(), self.expand(pair.get(1)?, env, depth)?));
                    }
                    inner.extend(fresh);
                    return self.expand(xs.get(2)?, &inner, depth);
                }
                if h == "forall" || h == "exists" {
                    return Some(e.clone()); // dropped by the caller
                }
                if let Some((params, body)) = self.defs.get(h) {
                    if params.len() != xs.len() - 1 {
                        return None;
                    }
                    let args: Vec<S> = xs[1..].iter().map(|a| self.expand(a, env, depth)).collect::<Option<_>>()?;
                    let bound: BTreeMap<String, S> = params.iter().cloned().zip(args).collect();
                    return self.expand(body, &bound, depth + 1);
                }
                Some(S::L(xs.iter().map(|x| self.expand(x, env, depth)).collect::<Option<_>>()?))
            }
        }
    }

    fn contains_quantifier(e: &S) -> bool {
        match e {
            S::A(_) => false,
            S::L(xs) => matches!(head(e), Some("forall") | Some("exists")) || xs.iter().any(Self::contains_quantifier),
        }
    }

    /// The datatype a term produces, if any.
    fn dt_of(&self, e: &S) -> Option<String> {
        let h = head(e)?;
        if let Some((dt, _)) = self.ctors.get(h) {
            return Some(dt.clone());
        }
        if let Some((_, ret)) = self.funs.get(h) {
            if self.is_dt(ret) {
                return Some(ret.clone());
            }
        }
        if h == "ite" {
            if let S::L(xs) = e {
                return self.dt_of(xs.get(2)?);
            }
        }
        // A projection whose field is itself a datatype (`(dt1_1 (ext_gcd a b))` is a pair).
        if let Some((dt2, j)) = self.projs.get(h) {
            let s = self.field_sort_any(dt2, *j)?;
            if self.is_dt(&s) {
                return Some(s);
            }
        }
        None
    }

    fn constant(&mut self, key: String, sort: &str) -> S {
        if let Some((n, _)) = self.consts.get(&key) {
            return S::A(n.clone());
        }
        let n = format!("ground_{}", self.consts.len());
        self.consts.insert(key.clone(), (n.clone(), sort.to_string()));
        self.order.push(key);
        S::A(n)
    }

    /// `(sel_i e)` reduced: through a constructor, a callee (→ constant), an `ite`, or another
    /// projection (a NESTED datatype field: `(dt0_0 (dt1_1 (ext_gcd a b)))`, the Bézout coefficient
    /// inside the pair inside the triple — one constant per projection path and callee term).
    fn proj(&mut self, dt: &str, i: usize, e: &S) -> Option<S> {
        let S::L(xs) = e else { return None }; // a datatype-valued variable: out of scope
        let h = head(e)?;
        if let Some((dt2, j)) = self.projs.get(h).cloned() {
            let inner = xs.get(1)?;
            let S::L(ixs) = inner else { return None };
            let ih = head(inner)?.to_string();
            if let Some((_, ret)) = self.funs.get(&ih).cloned() {
                if ret != dt2 || self.field_sort_any(&dt2, j).as_deref() != Some(dt) {
                    return None;
                }
                let args: Vec<S> = ixs[1..].iter().map(|a| self.rw(a)).collect::<Option<_>>()?;
                let sort = self.field_sort(dt, i)?;
                let key = format!("(proj{i} (proj{j} {ih} {}))", args.iter().map(show).collect::<Vec<_>>().join(" "));
                return Some(self.constant(key, &sort));
            }
            if self.ctors.contains_key(&ih) {
                // The inner projection of a constructor is its field: project that.
                let field = self.proj(&dt2, j, inner)?;
                return self.proj(dt, i, &field);
            }
            return None;
        }
        if let Some((d, sorts)) = self.ctors.get(h).cloned() {
            if d != dt {
                return None;
            }
            let _ = sorts;
            return self.rw(xs.get(1 + i)?);
        }
        if let Some((_, ret)) = self.funs.get(h).cloned() {
            if ret != dt {
                return None;
            }
            let args: Vec<S> = xs[1..].iter().map(|a| self.rw(a)).collect::<Option<_>>()?;
            let sort = self.field_sort(dt, i)?;
            let key = format!("(proj{i} {h} {})", args.iter().map(show).collect::<Vec<_>>().join(" "));
            return Some(self.constant(key, &sort));
        }
        if h == "ite" {
            let c = self.rw(xs.get(1)?)?;
            let t = self.proj(dt, i, xs.get(2)?)?;
            let f = self.proj(dt, i, xs.get(3)?)?;
            return Some(S::L(vec![S::A("ite".into()), c, t, f]));
        }
        None
    }

    fn field_sort(&self, dt: &str, i: usize) -> Option<String> {
        let s = self.field_sort_any(dt, i)?;
        if s == "Int" || s == "Bool" { Some(s) } else { None }
    }

    /// The field's sort whatever it is (a nested datatype included).
    fn field_sort_any(&self, dt: &str, i: usize) -> Option<String> {
        let (_, sorts) = self.ctors.values().find(|(d, _)| d == dt)?;
        sorts.get(i).cloned()
    }

    fn arity(&self, dt: &str) -> Option<usize> {
        self.ctors.values().find(|(d, _)| d == dt).map(|(_, s)| s.len())
    }

    fn rw(&mut self, e: &S) -> Option<S> {
        let S::L(xs) = e else { return Some(e.clone()) };
        let h = head(e)?.to_string();
        if let Some((dt, i)) = self.projs.get(&h).cloned() {
            return self.proj(&dt, i, xs.get(1)?);
        }
        if h == "=" && xs.len() == 3 {
            if let Some(dt) = self.dt_of(&xs[1]).or_else(|| self.dt_of(&xs[2])) {
                let n = self.arity(&dt)?;
                let mut conj = vec![S::A("and".into())];
                for i in 0..n {
                    let l = self.proj(&dt, i, &xs[1])?;
                    let r = self.proj(&dt, i, &xs[2])?;
                    conj.push(S::L(vec![S::A("=".into()), l, r]));
                }
                return Some(S::L(conj));
            }
        }
        if h == "distinct" && xs.iter().skip(1).any(|x| self.dt_of(x).is_some()) {
            return None;
        }
        if let Some((arity, ret)) = self.funs.get(&h).cloned() {
            if xs.len() - 1 != arity || self.is_dt(&ret) {
                return None; // a bare datatype-valued application outside a projection/equality
            }
            let args: Vec<S> = xs[1..].iter().map(|a| self.rw(a)).collect::<Option<_>>()?;
            let key = format!("({h} {})", args.iter().map(show).collect::<Vec<_>>().join(" "));
            return Some(self.constant(key, &ret));
        }
        if self.ctors.contains_key(&h) {
            return None; // a constructor in a position that is neither projected nor compared
        }
        Some(S::L(xs.iter().map(|x| self.rw(x)).collect::<Option<_>>()?))
    }
}

/// ONE-STEP UNFOLDINGS (2026-11-01, z3 5.x): for every `define-fun-rec` in `defs` applied in
/// `goal` with `term` as one of its arguments, the definition unfolded once at that application —
/// `(assert (= (F … term …) BODY[params := args]))`, one line each, distinct applications only.
/// Each is an instance of the function's own definition, so asserting it is sound. Measured: the
/// structural step of `filter ∘ reverse = reverse ∘ filter` is unsat in 0.01 s on z3 4.13 and
/// unknown at 30 s on z3 5.3.2/5.3.3, and unsat in 0.02 s on both 5.x with these two lines — 5.x
/// no longer unfolds `reverse (cons h t)` and `filter (cons h t)` on its own there.
pub fn unfoldings_at(defs: &str, goal: &str, term: &str) -> String {
    let (Some(forms), Some(goal), Some(term)) = (parse(&tokenize(defs)), parse(&tokenize(goal)), parse(&tokenize(term))) else {
        return String::new();
    };
    let (Some(goal), Some(term)) = (goal.into_iter().next(), term.into_iter().next()) else { return String::new() };
    // name → (parameter names, body)
    let mut recs: BTreeMap<String, (Vec<String>, S)> = BTreeMap::new();
    for f in &forms {
        let S::L(xs) = f else { continue };
        if xs.len() == 5 && matches!(&xs[0], S::A(h) if h == "define-fun-rec") {
            let (S::A(name), S::L(ps)) = (&xs[1], &xs[2]) else { continue };
            let params: Option<Vec<String>> = ps.iter().map(|p| match p { S::L(pv) => match pv.first() { Some(S::A(n)) => Some(n.clone()), _ => None }, _ => None }).collect();
            if let Some(params) = params {
                recs.insert(name.clone(), (params, xs[4].clone()));
            }
        }
    }
    fn subst(e: &S, m: &BTreeMap<String, S>) -> S {
        match e {
            S::A(a) => m.get(a).cloned().unwrap_or_else(|| e.clone()),
            S::L(xs) => S::L(xs.iter().map(|x| subst(x, m)).collect()),
        }
    }
    fn walk(e: &S, term: &S, recs: &BTreeMap<String, (Vec<String>, S)>, out: &mut Vec<String>) {
        let S::L(xs) = e else { return };
        if let Some(S::A(h)) = xs.first() {
            if let Some((params, body)) = recs.get(h) {
                if params.len() + 1 == xs.len() && xs[1..].iter().any(|a| a == term) {
                    let m: BTreeMap<String, S> = params.iter().cloned().zip(xs[1..].iter().cloned()).collect();
                    let line = format!("(assert (= {} {}))", show(e), show(&subst(body, &m)));
                    if !out.contains(&line) {
                        out.push(line);
                    }
                }
            }
        }
        xs.iter().for_each(|x| walk(x, term, recs, out));
    }
    let mut out = Vec::new();
    walk(&goal, &term, &recs, &mut out);
    out.iter().map(|l| format!("{l}\n")).collect()
}

const PLAIN_SORTS: [&str; 2] = ["Int", "Bool"];

/// The certificate's script as pure `QF_NIA`, or `None` when the rewrite does not apply (nothing
/// uninterpreted to eliminate, or a construct outside the fragment).
pub fn ackermannize(smt: &str) -> Option<String> {
    let forms = parse(&tokenize(smt))?;
    let mut g = Ground { ctors: BTreeMap::new(), projs: BTreeMap::new(), funs: BTreeMap::new(), defs: BTreeMap::new(), consts: BTreeMap::new(), order: Vec::new() };
    let mut decl_consts: Vec<(String, String)> = Vec::new();
    let mut asserts: Vec<S> = Vec::new();
    for f in &forms {
        let S::L(xs) = f else { return None };
        match head(f)? {
            "set-logic" | "check-sat" | "get-model" | "set-option" | "set-info" => {}
            "declare-datatype" => {
                let S::A(dt) = xs.get(1)? else { return None };
                let S::L(ctors) = xs.get(2)? else { return None };
                for c in ctors {
                    let S::L(parts) = c else { return None };
                    let S::A(cname) = parts.first()? else { return None };
                    let mut sorts = Vec::new();
                    for (i, sel) in parts[1..].iter().enumerate() {
                        let S::L(sp) = sel else { return None };
                        let S::A(sname) = sp.first()? else { return None };
                        let S::A(ssort) = sp.get(1)? else { return None };
                        g.projs.insert(sname.clone(), (dt.clone(), i));
                        sorts.push(ssort.clone());
                    }
                    g.ctors.insert(cname.clone(), (dt.clone(), sorts));
                }
            }
            "declare-fun" => {
                let S::A(name) = xs.get(1)? else { return None };
                let S::L(params) = xs.get(2)? else { return None };
                let S::A(ret) = xs.get(3)? else { return None };
                for p in params {
                    let S::A(ps) = p else { return None };
                    if !PLAIN_SORTS.contains(&ps.as_str()) {
                        return None;
                    }
                }
                if params.is_empty() {
                    if !PLAIN_SORTS.contains(&ret.as_str()) {
                        return None;
                    }
                    decl_consts.push((name.clone(), ret.clone()));
                } else {
                    g.funs.insert(name.clone(), (params.len(), ret.clone()));
                }
            }
            "declare-const" => {
                let S::A(name) = xs.get(1)? else { return None };
                let S::A(sort) = xs.get(2)? else { return None };
                if !PLAIN_SORTS.contains(&sort.as_str()) {
                    return None;
                }
                decl_consts.push((name.clone(), sort.clone()));
            }
            "define-fun" => {
                let S::A(name) = xs.get(1)? else { return None };
                let S::L(params) = xs.get(2)? else { return None };
                let mut names = Vec::new();
                for p in params {
                    let S::L(pp) = p else { return None };
                    let S::A(pn) = pp.first()? else { return None };
                    names.push(pn.clone());
                }
                g.defs.insert(name.clone(), (names, xs.get(4)?.clone()));
            }
            "assert" => asserts.push(xs.get(1)?.clone()),
            _ => return None,
        }
    }
    // Every datatype field must be plain or itself a declared datatype (a pair inside a triple —
    // `ext_gcd`'s Bézout coefficients — reduces through nested projections), and every
    // uninterpreted result plain or a datatype, or the rung declines.
    let dts: Vec<String> = g.ctors.values().map(|(d, _)| d.clone()).collect();
    for (_, sorts) in g.ctors.values() {
        if sorts.iter().any(|s| !PLAIN_SORTS.contains(&s.as_str()) && !dts.contains(s)) {
            return None;
        }
    }
    for (_, ret) in g.funs.values() {
        if !PLAIN_SORTS.contains(&ret.as_str()) && !g.is_dt(ret) {
            return None;
        }
    }
    if g.funs.is_empty() {
        return None; // nothing uninterpreted: the solver already saw plain arithmetic
    }
    let mut body = Vec::new();
    let mut dropped = 0usize;
    for a in &asserts {
        let e = g.expand(a, &BTreeMap::new(), 0)?;
        if Ground::contains_quantifier(&e) {
            dropped += 1;
            continue;
        }
        body.push(g.rw(&e)?);
    }
    if body.is_empty() {
        return None;
    }
    let mut out = String::new();
    out.push_str("; the ground rung: callee applications as constants (Ackermann's reduction without\n");
    out.push_str("; congruence — weaker, so an unsat here is an unsat of the certificate it came from),\n");
    out.push_str(&format!("; {} quantified assert(s) dropped, logic QF_NIA.\n", dropped));
    out.push_str("(set-logic QF_NIA)\n");
    for (n, s) in &decl_consts {
        out.push_str(&format!("(declare-const {n} {s})\n"));
    }
    for key in &g.order {
        let (n, s) = &g.consts[key];
        out.push_str(&format!("(declare-const {n} {s}) ; {key}\n"));
    }
    for r in &body {
        out.push_str(&format!("(assert {})\n", show(r)));
    }
    out.push_str("(check-sat)\n");
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_recursive_definitions_unfold_once_at_the_constructor_term() {
        let defs = "(define-fun-rec append ((xs Lst) (ys Lst)) Lst (ite ((_ is nil) xs) ys (cons (hd xs) (append (tl xs) ys))))\n\
                    (define-fun-rec reverse ((xs Lst)) Lst (ite ((_ is nil) xs) nil (append (reverse (tl xs)) (cons (hd xs) nil))))\n";
        let goal = "(= (reverse (cons h t)) (append (reverse t) (reverse (cons h t))))";
        let u = unfoldings_at(defs, goal, "(cons h t)");
        // One line per DISTINCT application with the term as an argument; `append (reverse t) …` has
        // no such argument and is left alone.
        assert_eq!(u, "(assert (= (reverse (cons h t)) (ite ((_ is nil) (cons h t)) nil (append (reverse (tl (cons h t))) (cons (hd (cons h t)) nil)))))\n");
        assert_eq!(unfoldings_at(defs, "(= (append t t) t)", "(cons h t)"), "");
    }

    fn solver() -> Option<&'static str> {
        ["z3", "cvc5"].into_iter().find(|s| std::process::Command::new(s).arg("--version").output().map(|o| o.status.success()).unwrap_or(false))
    }

    #[test]
    fn callee_applications_become_constants_and_tuples_reduce() {
        let smt = "(set-logic ALL)\n(declare-datatype dt0 ((mk_dt0 (dt0_0 Int) (dt0_1 Int))))\n\
                   (declare-fun callee_f (Int Int) dt0)\n(declare-fun callee_n (Int Int) Int)\n\
                   (define-fun self ((a Int) (b Int)) Int (callee_n (dt0_0 (callee_f a b)) (dt0_1 (callee_f a b))))\n\
                   (declare-const x Int)\n(declare-const y Int)\n\
                   (assert (forall ((p Int) (q Int)) (>= (callee_n p q) 0)))\n\
                   (assert (let ((a x) (b y)) (= (callee_n (dt0_0 (callee_f a b)) (dt0_1 (callee_f a b))) (+ (* (dt0_0 (callee_f a b)) (dt0_0 (callee_f a b))) (* (dt0_1 (callee_f a b)) (dt0_1 (callee_f a b)))))))\n\
                   (assert (= (callee_f x y) (mk_dt0 (- x y) (+ x y))))\n\
                   (assert (not (>= (self x y) 0)))\n(check-sat)\n";
        let out = ackermannize(smt).expect("in the fragment");
        assert!(out.contains("(set-logic QF_NIA)"), "{out}");
        let asserts: String = out.lines().filter(|l| l.starts_with("(assert")).collect::<Vec<_>>().join("\n");
        assert!(!asserts.contains("callee_") && !asserts.contains("dt0_"), "no uninterpreted symbol or selector survives in the asserts:\n{out}");
        assert!(out.contains("1 quantified assert(s) dropped"), "{out}");
        // (proj0 f x y), (proj1 f x y), (n …): three constants, the two projections shared by every use.
        assert_eq!(out.matches("(declare-const ground_").count(), 3, "{out}");
        if let Some(s) = solver() {
            let dir = std::env::temp_dir().join(format!("nl-ground-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let p = dir.join("q.smt2");
            std::fs::write(&p, &out).unwrap();
            let o = std::process::Command::new(s).arg("-T:20").arg(&p).output().unwrap();
            assert!(String::from_utf8_lossy(&o.stdout).starts_with("unsat"), "x² + y² ≥ 0 through the constants: {}", String::from_utf8_lossy(&o.stdout));
        }
    }

    #[test]
    fn the_rung_declines_outside_the_fragment() {
        // Nothing uninterpreted: nothing to gain.
        assert!(ackermannize("(set-logic ALL)\n(declare-const x Int)\n(assert (> (* x x) 0))\n(check-sat)\n").is_none());
        // A string constant.
        assert!(ackermannize("(set-logic ALL)\n(declare-fun callee_f (Int) Int)\n(declare-const s String)\n(assert (> (callee_f 1) 0))\n(check-sat)\n").is_none());
        // A datatype-valued constant (a quantified tuple variable).
        assert!(ackermannize("(set-logic ALL)\n(declare-datatype dt0 ((mk_dt0 (dt0_0 Int))))\n(declare-fun callee_f (Int) Int)\n(declare-const t dt0)\n(assert (> (callee_f (dt0_0 t)) 0))\n(check-sat)\n").is_none());
    }
}
