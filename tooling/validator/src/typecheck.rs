//! Type checker for Nova Lingua function bodies (spec/type-expression.schema.json +
//! body-expression.schema.json). The second pillar of "verified by default" (principle 3): until now
//! a record could *declare* a type and nothing checked the body actually has it. This infers the
//! body's type and unifies it with the declared signature.
//!
//! A compact Hindley-Milner core: lexical environment, fresh unification variables, union-find
//! substitution with the occurs check, builtins as polymorphic schemes (instantiated fresh per use),
//! and let kept monomorphic (honest simplification). The declared signature's `forall` variables are
//! **skolemized** to rigid constants, so the body must be genuinely polymorphic — a body that only
//! works at one instance is correctly rejected.
//!
//! Scope / honesty: `nat` is normalized to `int` (a `nat` is a non-negative `int`; v0.1 has no
//! refinement-aware checking here). Sum/`variant` types and `ref` (named-type-by-address) are treated
//! opaquely — `case` arms over them are checked structurally but their payload types are inferred as
//! fresh variables rather than resolved. Effects/refinements are out of scope (separate concerns).

use anyhow::{anyhow, bail, Result};
use serde_json::Value as J;
use std::collections::BTreeMap;
use std::collections::{HashMap, HashSet};

/// An inference type. `Con` covers scalars (`int`/`bool`/…), `List` (one arg), skolems (`$a`), and
/// opaque sums/refs; `Fun` is an uncurried arrow matching the `fn` type AST.
#[derive(Clone, Debug)]
enum Ty {
    Var(u32),
    Con(String, Vec<Ty>),
    Fun(Vec<Ty>, Box<Ty>),
    Tup(Vec<Ty>),
    Rec(BTreeMap<String, Ty>),
}

fn con(name: &str) -> Ty {
    Ty::Con(name.to_string(), vec![])
}

struct Infer {
    subst: HashMap<u32, Ty>,
    next: u32,
    /// Variables constrained to be *numeric* — they may unify only with `int`/`float` (or another
    /// variable, which then also becomes numeric). Backs the arithmetic operators (int OR float).
    numeric: HashSet<u32>,
}

impl Infer {
    fn new() -> Self {
        Infer { subst: HashMap::new(), next: 0, numeric: HashSet::new() }
    }

    fn fresh(&mut self) -> Ty {
        let v = self.next;
        self.next += 1;
        Ty::Var(v)
    }

    /// A fresh *numeric* variable: it may unify only with `int`, `float`, or another (then-also-numeric)
    /// variable — so an arithmetic operator works over either numeric type but not over a non-number.
    fn fresh_numeric(&mut self) -> Ty {
        let v = self.next;
        self.next += 1;
        self.numeric.insert(v);
        Ty::Var(v)
    }

    /// Follow variable bindings one level (shallow).
    fn resolve(&self, t: &Ty) -> Ty {
        match t {
            Ty::Var(v) => match self.subst.get(v) {
                Some(u) => self.resolve(u),
                None => t.clone(),
            },
            _ => t.clone(),
        }
    }

    /// Fully resolve a type for display / final inspection.
    fn zonk(&self, t: &Ty) -> Ty {
        match self.resolve(t) {
            Ty::Con(n, args) => Ty::Con(n, args.iter().map(|a| self.zonk(a)).collect()),
            Ty::Fun(ps, r) => Ty::Fun(ps.iter().map(|p| self.zonk(p)).collect(), Box::new(self.zonk(&r))),
            Ty::Tup(xs) => Ty::Tup(xs.iter().map(|x| self.zonk(x)).collect()),
            Ty::Rec(m) => Ty::Rec(m.iter().map(|(k, v)| (k.clone(), self.zonk(v))).collect()),
            other => other,
        }
    }

    fn occurs(&self, v: u32, t: &Ty) -> bool {
        match self.resolve(t) {
            Ty::Var(u) => u == v,
            Ty::Con(_, args) => args.iter().any(|a| self.occurs(v, a)),
            Ty::Fun(ps, r) => ps.iter().any(|p| self.occurs(v, p)) || self.occurs(v, &r),
            Ty::Tup(xs) => xs.iter().any(|x| self.occurs(v, x)),
            Ty::Rec(m) => m.values().any(|x| self.occurs(v, x)),
        }
    }

    fn bind(&mut self, v: u32, t: &Ty) -> Result<()> {
        if let Ty::Var(u) = t {
            if *u == v {
                return Ok(());
            }
        }
        if self.occurs(v, t) {
            bail!("infinite type: variable occurs in {}", show(&self.zonk(t)));
        }
        // A numeric variable accepts only `int`/`float`; binding it to another variable propagates the
        // constraint, and binding it to anything else (bool, a list, a function, …) is a type error.
        if self.numeric.contains(&v) {
            match t {
                Ty::Var(u) => {
                    self.numeric.insert(*u);
                }
                Ty::Con(n, args) if args.is_empty() && (n == "int" || n == "float") => {}
                _ => bail!(
                    "type mismatch: an arithmetic operator needs a numeric type (int or float), got {}",
                    show(&self.zonk(t))
                ),
            }
        }
        self.subst.insert(v, t.clone());
        Ok(())
    }

    fn unify(&mut self, a: &Ty, b: &Ty) -> Result<()> {
        let (a, b) = (self.resolve(a), self.resolve(b));
        match (&a, &b) {
            (Ty::Var(v), _) => self.bind(*v, &b),
            (_, Ty::Var(v)) => self.bind(*v, &a),
            (Ty::Con(n1, a1), Ty::Con(n2, a2)) => {
                if n1 != n2 || a1.len() != a2.len() {
                    bail!("type mismatch: {} vs {}", show(&self.zonk(&a)), show(&self.zonk(&b)));
                }
                for (x, y) in a1.iter().zip(a2) {
                    self.unify(x, y)?;
                }
                Ok(())
            }
            (Ty::Fun(p1, r1), Ty::Fun(p2, r2)) => {
                if p1.len() != p2.len() {
                    bail!("function arity mismatch: {} vs {}", show(&self.zonk(&a)), show(&self.zonk(&b)));
                }
                for (x, y) in p1.iter().zip(p2) {
                    self.unify(x, y)?;
                }
                self.unify(r1, r2)
            }
            (Ty::Tup(x), Ty::Tup(y)) => {
                if x.len() != y.len() {
                    bail!("tuple width mismatch: {} vs {}", show(&self.zonk(&a)), show(&self.zonk(&b)));
                }
                for (p, q) in x.iter().zip(y) {
                    self.unify(p, q)?;
                }
                Ok(())
            }
            (Ty::Rec(x), Ty::Rec(y)) => {
                if x.len() != y.len() || x.keys().ne(y.keys()) {
                    bail!("record field mismatch: {} vs {}", show(&self.zonk(&a)), show(&self.zonk(&b)));
                }
                for (k, v) in x {
                    self.unify(v, &y[k])?;
                }
                Ok(())
            }
            _ => bail!("type mismatch: {} vs {}", show(&self.zonk(&a)), show(&self.zonk(&b))),
        }
    }
}

fn show(t: &Ty) -> String {
    match t {
        Ty::Var(v) => format!("?{v}"),
        Ty::Con(n, args) if args.is_empty() => n.clone(),
        Ty::Con(n, args) => format!("{n}({})", args.iter().map(show).collect::<Vec<_>>().join(", ")),
        Ty::Fun(ps, r) => format!("({}) -> {}", ps.iter().map(show).collect::<Vec<_>>().join(", "), show(r)),
        Ty::Tup(xs) => format!("({})", xs.iter().map(show).collect::<Vec<_>>().join(", ")),
        Ty::Rec(m) => format!(
            "{{{}}}",
            m.iter().map(|(k, v)| format!("{k}: {}", show(v))).collect::<Vec<_>>().join(", ")
        ),
    }
}

/// Convert a type-expression AST to an inference type. Free / forall-bound type variables become
/// rigid skolem constants (`$name`), so a declared polymorphic type is checked, not instantiated.
fn ast_to_ty(t: &J) -> Result<Ty> {
    let kind = t.get("kind").and_then(|k| k.as_str()).ok_or_else(|| anyhow!("type missing kind: {t}"))?;
    Ok(match kind {
        "var" => Ty::Con(format!("${}", t["name"].as_str().ok_or_else(|| anyhow!("type var name"))?), vec![]),
        "builtin" => {
            let n = t["name"].as_str().ok_or_else(|| anyhow!("builtin name"))?;
            con(if n == "nat" { "int" } else { n }) // nat ≡ int in this checker
        }
        "ref" => Ty::Con(format!("ref:{}", t["target"].as_str().unwrap_or("?")), vec![]),
        "forall" => ast_to_ty(&t["body"])?, // vars become rigid via the `var` rule above
        "fn" => {
            let params = t["params"].as_array().ok_or_else(|| anyhow!("fn params"))?.iter().map(ast_to_ty).collect::<Result<Vec<_>>>()?;
            Ty::Fun(params, Box::new(ast_to_ty(&t["result"])?))
        }
        "apply" => {
            let ctor = t["ctor"].as_str_name().unwrap_or_else(|| "App".to_string());
            let args = t["args"].as_array().ok_or_else(|| anyhow!("apply args"))?.iter().map(ast_to_ty).collect::<Result<Vec<_>>>()?;
            Ty::Con(ctor, args)
        }
        "tuple" => Ty::Tup(t["elems"].as_array().ok_or_else(|| anyhow!("tuple elems"))?.iter().map(ast_to_ty).collect::<Result<Vec<_>>>()?),
        "record" => {
            let mut m = BTreeMap::new();
            for f in t["fields"].as_array().ok_or_else(|| anyhow!("record fields"))? {
                m.insert(f["name"].as_str().ok_or_else(|| anyhow!("field name"))?.to_string(), ast_to_ty(&f["type"])?);
            }
            Ty::Rec(m)
        }
        "sum" => con("Sum"), // opaque: variant payloads not resolved here
        other => bail!("unknown type kind: {other}"),
    })
}

// Tiny helper: the `apply.ctor` is itself a type node (usually a builtin like List).
trait CtorName {
    fn as_str_name(&self) -> Option<String>;
}
impl CtorName for J {
    fn as_str_name(&self) -> Option<String> {
        match self.get("kind")?.as_str()? {
            "builtin" => Some(self.get("name")?.as_str()?.to_string()),
            "var" => Some(format!("${}", self.get("name")?.as_str()?)),
            "ref" => Some(format!("ref:{}", self.get("target")?.as_str()?)),
            _ => None,
        }
    }
}

/// Type of a literal value-expression. List element types are unified.
fn lit_ty(v: &J, inf: &mut Infer) -> Result<Ty> {
    let kind = v.get("kind").and_then(|k| k.as_str()).ok_or_else(|| anyhow!("value kind"))?;
    Ok(match kind {
        "bool" => con("bool"),
        "int" | "nat" => con("int"),
        "float" => con("float"),
        "string" => con("string"),
        "bytes" => con("bytes"),
        "unit" => con("unit"),
        "list" => {
            let e = inf.fresh();
            for el in v["elems"].as_array().ok_or_else(|| anyhow!("list elems"))? {
                let et = lit_ty(el, inf)?;
                inf.unify(&e, &et)?;
            }
            Ty::Con("List".into(), vec![e])
        }
        "tuple" => Ty::Tup(v["elems"].as_array().ok_or_else(|| anyhow!("tuple elems"))?.iter().map(|e| lit_ty(e, inf)).collect::<Result<Vec<_>>>()?),
        "record" => {
            let mut m = BTreeMap::new();
            for f in v["fields"].as_array().ok_or_else(|| anyhow!("record fields"))? {
                m.insert(f["name"].as_str().ok_or_else(|| anyhow!("field name"))?.to_string(), lit_ty(&f["value"], inf)?);
            }
            Ty::Rec(m)
        }
        "variant" => con("Sum"),       // opaque
        "fn_ref" => inf.fresh(),         // target type not resolved here
        other => bail!("unknown value kind in literal: {other}"),
    })
}

/// Polymorphic type scheme of a builtin, instantiated with fresh variables. `None` if not a builtin.
fn builtin_scheme(name: &str, inf: &mut Infer) -> Option<Ty> {
    let list = |t: Ty| Ty::Con("List".into(), vec![t]);
    Some(match name {
        // Numeric over int OR float (a fresh numeric variable threads input and output).
        "add" | "sub" | "mul" | "min" | "max" => {
            let a = inf.fresh_numeric();
            Ty::Fun(vec![a.clone(), a.clone()], Box::new(a))
        }
        "neg" | "abs" => {
            let a = inf.fresh_numeric();
            Ty::Fun(vec![a.clone()], Box::new(a))
        }
        // Integer-only: division and modulo keep `int` semantics.
        "div" | "mod" => Ty::Fun(vec![con("int"), con("int")], Box::new(con("int"))),
        // Effectful builtins (effects tracked at eval, not in this checker): `print : forall a. a ->
        // unit`, `rand : int -> int`.
        "print" => {
            let a = inf.fresh();
            Ty::Fun(vec![a], Box::new(con("unit")))
        }
        "rand" => Ty::Fun(vec![con("int")], Box::new(con("int"))),
        "now" => {
            let a = inf.fresh();
            Ty::Fun(vec![a], Box::new(con("int")))
        }
        "panic" => {
            let (a, b) = (inf.fresh(), inf.fresh());
            Ty::Fun(vec![a], Box::new(b)) // diverges: a -> b
        }
        "read_file" => Ty::Fun(vec![con("string")], Box::new(con("string"))),
        "write_file" => Ty::Fun(vec![con("string"), con("string")], Box::new(con("unit"))),
        "http_get" => Ty::Fun(vec![con("string")], Box::new(con("string"))),
        "http_post" => Ty::Fun(vec![con("string"), con("string")], Box::new(con("string"))),
        "spawn" => Ty::Fun(vec![con("string"), list(con("string"))], Box::new(con("string"))),
        // `replicate : forall a. int -> a -> List a` — the heap-allocating builtin (effect `alloc`).
        "replicate" => {
            let a = inf.fresh();
            Ty::Fun(vec![con("int"), a.clone()], Box::new(list(a)))
        }
        "lt" | "le" | "gt" | "ge" => {
            let a = inf.fresh_numeric();
            Ty::Fun(vec![a.clone(), a], Box::new(con("bool")))
        }
        "and" | "or" | "xor" => Ty::Fun(vec![con("bool"), con("bool")], Box::new(con("bool"))),
        "not" => Ty::Fun(vec![con("bool")], Box::new(con("bool"))),
        "eq" | "neq" => {
            let a = inf.fresh();
            Ty::Fun(vec![a.clone(), a], Box::new(con("bool")))
        }
        "id" => {
            let a = inf.fresh();
            Ty::Fun(vec![a.clone()], Box::new(a))
        }
        "length" => {
            let a = inf.fresh();
            Ty::Fun(vec![list(a)], Box::new(con("int")))
        }
        "null" => {
            let a = inf.fresh();
            Ty::Fun(vec![list(a)], Box::new(con("bool")))
        }
        "head" => {
            let a = inf.fresh();
            Ty::Fun(vec![list(a.clone())], Box::new(a))
        }
        "tail" | "reverse" => {
            let a = inf.fresh();
            Ty::Fun(vec![list(a.clone())], Box::new(list(a)))
        }
        "cons" => {
            let a = inf.fresh();
            Ty::Fun(vec![a.clone(), list(a.clone())], Box::new(list(a)))
        }
        "append" | "concat" => {
            let a = inf.fresh();
            Ty::Fun(vec![list(a.clone()), list(a.clone())], Box::new(list(a)))
        }
        "map" => {
            let (a, b) = (inf.fresh(), inf.fresh());
            Ty::Fun(vec![Ty::Fun(vec![a.clone()], Box::new(b.clone())), list(a)], Box::new(list(b)))
        }
        "filter" => {
            let a = inf.fresh();
            Ty::Fun(vec![Ty::Fun(vec![a.clone()], Box::new(con("bool"))), list(a.clone())], Box::new(list(a)))
        }
        "foldl" => {
            let (a, b) = (inf.fresh(), inf.fresh());
            Ty::Fun(vec![Ty::Fun(vec![b.clone(), a.clone()], Box::new(b.clone())), b.clone(), list(a)], Box::new(b))
        }
        "foldr" => {
            let (a, b) = (inf.fresh(), inf.fresh());
            Ty::Fun(vec![Ty::Fun(vec![a.clone(), b.clone()], Box::new(b.clone())), b.clone(), list(a)], Box::new(b))
        }
        "compose" => {
            let (a, b, c) = (inf.fresh(), inf.fresh(), inf.fresh());
            Ty::Fun(vec![Ty::Fun(vec![b.clone()], Box::new(c.clone())), Ty::Fun(vec![a.clone()], Box::new(b)), a], Box::new(c))
        }
        "apply" => {
            let (a, b) = (inf.fresh(), inf.fresh());
            Ty::Fun(vec![Ty::Fun(vec![a.clone()], Box::new(b.clone())), a], Box::new(b))
        }
        "fst" => {
            let (a, b) = (inf.fresh(), inf.fresh());
            Ty::Fun(vec![Ty::Tup(vec![a.clone(), b])], Box::new(a))
        }
        "snd" => {
            let (a, b) = (inf.fresh(), inf.fresh());
            Ty::Fun(vec![Ty::Tup(vec![a, b.clone()])], Box::new(b))
        }
        "nil" => {
            let a = inf.fresh();
            list(a)
        }
        _ => return None,
    })
}

type Env = HashMap<String, Ty>;

fn infer(expr: &J, env: &Env, inf: &mut Infer) -> Result<Ty> {
    let kind = expr.get("kind").and_then(|k| k.as_str()).ok_or_else(|| anyhow!("expr kind: {expr}"))?;
    match kind {
        "var" => {
            let name = expr["name"].as_str().ok_or_else(|| anyhow!("var name"))?;
            if let Some(t) = env.get(name) {
                Ok(t.clone())
            } else if let Some(t) = builtin_scheme(name, inf) {
                Ok(t)
            } else {
                bail!("unbound variable in body: {name}")
            }
        }
        "lit" => lit_ty(&expr["value"], inf),
        "lambda" => {
            let mut env2 = env.clone();
            let mut params = vec![];
            for p in expr["params"].as_array().ok_or_else(|| anyhow!("lambda params"))? {
                let name = p["name"].as_str().ok_or_else(|| anyhow!("param name"))?.to_string();
                let ty = match p.get("type") {
                    Some(ann) => ast_to_ty(ann)?,
                    None => inf.fresh(),
                };
                env2.insert(name, ty.clone());
                params.push(ty);
            }
            let body = infer(&expr["body"], &env2, inf)?;
            Ok(Ty::Fun(params, Box::new(body)))
        }
        "app" => {
            let ft = infer(&expr["fn"], env, inf)?;
            let args = expr["args"].as_array().ok_or_else(|| anyhow!("app args"))?.iter().map(|a| infer(a, env, inf)).collect::<Result<Vec<_>>>()?;
            apply_ty(ft, args, inf)
        }
        "let" => {
            let vt = infer(&expr["value"], env, inf)?;
            let mut env2 = env.clone();
            env2.insert(expr["name"].as_str().ok_or_else(|| anyhow!("let name"))?.to_string(), vt);
            infer(&expr["body"], &env2, inf)
        }
        "case" => {
            let st = infer(&expr["scrutinee"], env, inf)?;
            let result = inf.fresh();
            for arm in expr["arms"].as_array().ok_or_else(|| anyhow!("case arms"))? {
                let mut env2 = env.clone();
                pattern_ty(&arm["pattern"], &st, &mut env2, inf)?;
                let at = infer(&arm["body"], &env2, inf)?;
                inf.unify(&at, &result)?;
            }
            Ok(result)
        }
        "field" => {
            let rt = infer(&expr["record"], env, inf)?;
            let name = expr["name"].as_str().ok_or_else(|| anyhow!("field name"))?;
            match inf.resolve(&rt) {
                Ty::Rec(m) => m.get(name).cloned().ok_or_else(|| anyhow!("record has no field {name}")),
                other => bail!("field projection on a non-record type: {}", show(&inf.zonk(&other))),
            }
        }
        "variant" => {
            // Variant construction. Sum types are opaque (`Sum`), matching how a literal variant value is
            // typed and how the `case` arms / `variant` patterns treat them. The payload expression is still
            // inferred so type errors inside it are caught, but it does not constrain the result.
            if let Some(p) = expr.get("payload") {
                infer(p, env, inf)?;
            }
            Ok(con("Sum"))
        }
        other => bail!("unknown expression kind: {other}"),
    }
}

/// Apply a function type to argument types, supporting currying and over-application.
fn apply_ty(ft: Ty, mut args: Vec<Ty>, inf: &mut Infer) -> Result<Ty> {
    if args.is_empty() {
        return Ok(ft);
    }
    match inf.resolve(&ft) {
        Ty::Fun(params, result) => {
            let n = params.len().min(args.len());
            for (p, a) in params.iter().zip(args.iter()).take(n) {
                inf.unify(p, a)?;
            }
            if args.len() < params.len() {
                Ok(Ty::Fun(params[args.len()..].to_vec(), result))
            } else {
                let extra = args.split_off(params.len());
                apply_ty(*result, extra, inf)
            }
        }
        Ty::Var(_) => {
            // Unknown callee: constrain it to a function of the given arguments.
            let result = inf.fresh();
            inf.unify(&ft, &Ty::Fun(args.clone(), Box::new(result.clone())))?;
            Ok(result)
        }
        other => bail!("applying a non-function type: {}", show(&inf.zonk(&other))),
    }
}

/// Check a pattern against the scrutinee type, adding its bindings to `env`.
fn pattern_ty(pat: &J, scrut: &Ty, env: &mut Env, inf: &mut Infer) -> Result<()> {
    let kind = pat.get("kind").and_then(|k| k.as_str()).ok_or_else(|| anyhow!("pattern kind"))?;
    match kind {
        "wildcard" => Ok(()),
        "bind" => {
            env.insert(pat["name"].as_str().ok_or_else(|| anyhow!("bind name"))?.to_string(), scrut.clone());
            Ok(())
        }
        "lit" => {
            let lt = lit_ty(&pat["value"], inf)?;
            inf.unify(&lt, scrut)
        }
        "variant" => {
            // Opaque sum: don't constrain the scrutinee; a payload pattern binds to a fresh type.
            if let Some(p) = pat.get("payload") {
                let fresh = inf.fresh();
                pattern_ty(p, &fresh, env, inf)?;
            }
            Ok(())
        }
        other => bail!("unknown pattern kind: {other}"),
    }
}

/// Verdict text and ok/err for a body checked against a declared type.
pub fn typecheck(declared: &J, body: &J) -> Result<String> {
    let mut inf = Infer::new();
    let dt = ast_to_ty(declared)?;
    // Bind `self` to the declared (skolemized) function type so a self-recursive body type-checks:
    // a recursive call shares the function's own rigid type. Monomorphic recursion only — `self` is a
    // single monotype, not re-generalized — which is exactly what these records need.
    let mut env = Env::new();
    env.insert("self".to_string(), dt.clone());
    let bt = infer(body, &env, &mut inf)?;
    match inf.unify(&bt, &dt) {
        Ok(()) => Ok(format!("WELL-TYPED  {}", show(&inf.zonk(&dt)))),
        Err(e) => Err(anyhow!(
            "ILL-TYPED: body has type {}, declared {} — {e}",
            show(&inf.zonk(&bt)),
            show(&inf.zonk(&dt))
        )),
    }
}

/// Check a function record's `signature.type` against its `body`.
pub fn typecheck_record(record: &J, body: &J) -> Result<String> {
    let declared = record
        .get("signature")
        .and_then(|s| s.get("type"))
        .ok_or_else(|| anyhow!("record has no signature.type"))?;
    typecheck(declared, body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::Path;

    fn load(name: &str) -> J {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spec/examples").join(name);
        serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
    }

    #[test]
    fn double_is_well_typed() {
        let record = load("double.v0.2.json");
        let body = load("body-double.json");
        assert!(typecheck_record(&record, &body).is_ok());
    }

    #[test]
    fn self_recursive_record_is_well_typed() {
        // `self` is bound to the declared signature, so a recursive body type-checks: the recursive
        // `self(tail xs)` call shares the function's own type. (Previously failed: unbound variable self.)
        let record = load("length.json");
        let body = load("body-length.json");
        assert!(typecheck_record(&record, &body).is_ok(), "recursive length should type-check");
    }

    #[test]
    fn is_zero_against_nat_to_bool() {
        let body = load("body-is-zero.json");
        let ty = json!({ "kind": "fn", "params": [{ "kind": "builtin", "name": "nat" }],
                         "result": { "kind": "builtin", "name": "bool" } });
        assert!(typecheck(&ty, &body).is_ok(), "is-zero : nat -> bool");
    }

    #[test]
    fn wrong_declared_type_is_rejected() {
        // body-double is nat->nat; claiming nat->bool must fail.
        let body = load("body-double.json");
        let ty = json!({ "kind": "fn", "params": [{ "kind": "builtin", "name": "nat" }],
                         "result": { "kind": "builtin", "name": "bool" } });
        assert!(typecheck(&ty, &body).is_err());
    }

    #[test]
    fn polymorphic_identity_checks_and_monomorphic_rejected() {
        // \x -> x  against  forall a. a -> a   (well-typed)
        let idbody = json!({ "kind": "lambda", "params": [{ "name": "x" }], "body": { "kind": "var", "name": "x" } });
        let poly = json!({ "kind": "forall", "vars": ["a"],
            "body": { "kind": "fn", "params": [{ "kind": "var", "name": "a" }], "result": { "kind": "var", "name": "a" } } });
        assert!(typecheck(&poly, &idbody).is_ok());

        // \x -> add(x, x)  against  forall a. a -> a  must FAIL (the skolem `a` is not numeric).
        let dbl = json!({ "kind": "lambda", "params": [{ "name": "x" }],
            "body": { "kind": "app", "fn": { "kind": "var", "name": "add" },
                      "args": [{ "kind": "var", "name": "x" }, { "kind": "var", "name": "x" }] } });
        assert!(typecheck(&poly, &dbl).is_err());
    }

    #[test]
    fn variant_construction_against_a_sum_type() {
        // \a b -> case b == 0 of { true => None; false => Just(a / b) } : int -> int -> [Just(int) None].
        // Sum types are opaque, so the constructed variant unifies with the declared sum result.
        let body = json!({ "kind": "lambda", "params": [{ "name": "a" }, { "name": "b" }], "body": {
            "kind": "case",
            "scrutinee": { "kind": "app", "fn": { "kind": "var", "name": "eq" },
                "args": [{ "kind": "var", "name": "b" }, { "kind": "lit", "value": { "kind": "int", "value": 0 } }] },
            "arms": [
                { "pattern": { "kind": "lit", "value": { "kind": "bool", "value": true } },
                  "body": { "kind": "variant", "tag": "None" } },
                { "pattern": { "kind": "lit", "value": { "kind": "bool", "value": false } },
                  "body": { "kind": "variant", "tag": "Just",
                    "payload": { "kind": "app", "fn": { "kind": "var", "name": "div" },
                        "args": [{ "kind": "var", "name": "a" }, { "kind": "var", "name": "b" }] } } }] } });
        let sum = json!({ "kind": "sum", "variants": [
            { "tag": "Just", "type": { "kind": "builtin", "name": "int" } }, { "tag": "None" }] });
        let ty = json!({ "kind": "fn", "params": [{ "kind": "builtin", "name": "int" }, { "kind": "builtin", "name": "int" }],
                         "result": sum });
        assert!(typecheck(&ty, &body).is_ok(), "safe-div : int -> int -> Maybe int");
        // A type error *inside* the payload is still caught: `Just(not(a))` needs `a : bool`, clashing with int.
        let bad = json!({ "kind": "lambda", "params": [{ "name": "a" }], "body": {
            "kind": "variant", "tag": "Just",
            "payload": { "kind": "app", "fn": { "kind": "var", "name": "add" },
                "args": [{ "kind": "var", "name": "a" },
                    { "kind": "app", "fn": { "kind": "var", "name": "not" }, "args": [{ "kind": "var", "name": "a" }] }] } } });
        let bad_ty = json!({ "kind": "fn", "params": [{ "kind": "builtin", "name": "int" }], "result": sum });
        assert!(typecheck(&bad_ty, &bad).is_err(), "a payload type error is not hidden by the opaque Sum");
    }

    #[test]
    fn arithmetic_is_numeric_over_int_and_float() {
        // \x -> mul(x, x) checks at BOTH float -> float and int -> int (numeric operators, not int-only).
        let body = json!({ "kind": "lambda", "params": [{ "name": "x" }],
            "body": { "kind": "app", "fn": { "kind": "var", "name": "mul" },
                      "args": [{ "kind": "var", "name": "x" }, { "kind": "var", "name": "x" }] } });
        let ty = |n: &str| json!({ "kind": "fn", "params": [{ "kind": "builtin", "name": n }],
                                   "result": { "kind": "builtin", "name": n } });
        assert!(typecheck(&ty("float"), &body).is_ok(), "mul over float : float -> float");
        assert!(typecheck(&ty("int"), &body).is_ok(), "mul over int : int -> int");
        // Comparisons take numeric args too: \x -> gt(x, x) at float -> bool.
        let cmp = json!({ "kind": "lambda", "params": [{ "name": "x" }],
            "body": { "kind": "app", "fn": { "kind": "var", "name": "gt" },
                      "args": [{ "kind": "var", "name": "x" }, { "kind": "var", "name": "x" }] } });
        let cmp_ty = json!({ "kind": "fn", "params": [{ "kind": "builtin", "name": "float" }],
                             "result": { "kind": "builtin", "name": "bool" } });
        assert!(typecheck(&cmp_ty, &cmp).is_ok(), "gt over float : float -> bool");
    }

    #[test]
    fn arithmetic_operator_rejects_non_number() {
        // \b -> add(b, b)  against  bool -> bool — `add` needs int or float, so this is ill-typed.
        let body = json!({ "kind": "lambda", "params": [{ "name": "b" }],
            "body": { "kind": "app", "fn": { "kind": "var", "name": "add" },
                      "args": [{ "kind": "var", "name": "b" }, { "kind": "var", "name": "b" }] } });
        let bool_ty = json!({ "kind": "fn", "params": [{ "kind": "builtin", "name": "bool" }],
                              "result": { "kind": "builtin", "name": "bool" } });
        assert!(typecheck(&bool_ty, &body).is_err(), "add over bool must be rejected");
    }

    #[test]
    fn map_typechecks_via_higher_order_builtin() {
        // \(f, xs) -> map(f, xs) : forall a b. (a->b) -> List a -> List b
        let body = json!({ "kind": "lambda",
            "params": [{ "name": "f" }, { "name": "xs" }],
            "body": { "kind": "app", "fn": { "kind": "var", "name": "map" },
                      "args": [{ "kind": "var", "name": "f" }, { "kind": "var", "name": "xs" }] } });
        let ty = load("type-map.json");
        assert!(typecheck(&ty, &body).is_ok(), "map body should match its polymorphic type");
    }
}
