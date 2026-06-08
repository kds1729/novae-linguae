//! Tree-walking evaluator for the Nova Lingua body-expression AST
//! (spec/body-expression.schema.json). This is the language's missing semantic core: it **executes**
//! a body. Given a function record's body and an example's arguments it computes a result, so the
//! worked `examples[]` become runnable tests rather than unchecked assertions, and `properties[]`
//! that reference `map`/`filter`/`fold`/`compose` can be verified by *running* rather than deferred.
//!
//! Values are the value-expression AST (spec/value-expression.schema.json). The evaluator is a
//! call-by-value lambda calculus with: lexical closures, currying / partial application, `let`,
//! `case` over the four pattern kinds, record field projection, and a small total builtin library
//! (arithmetic, comparison, booleans, lists, and the higher-order `map`/`filter`/`foldl`/`foldr`/
//! `compose`/`apply`). `if` is absent by design — it is `case` on a `bool` (principle 8).
//!
//! Scope: this is a reference evaluator for clarity, matching the v0.1 body schema. Integers are
//! i128 (the big-int string form is accepted but must fit); `int` and `nat` share the `Int`
//! representation (a `nat` is a non-negative `int`), so example checking compares values, not kind
//! tags. `field`/record and `tuple` are supported. Effects are not modelled — bodies that would
//! perform I/O are out of scope for this pure evaluator.

use anyhow::{anyhow, bail, Result};
use base64::Engine;
use serde_json::{json, Value as J};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

type Env = BTreeMap<String, Val>;

// Composition / linking: a scoped address → body-AST map. When set, applying a `fn_ref` value resolves
// the target (a function content-address, or a body's own `expr_` address) to its body and runs it, so
// records compose end-to-end — e.g. running map's example `map(<fn_ref double>, [1,2,3])` executes the
// referenced `double` (commons/linking, principle 4: assemble from existing records). Thread-local so
// the evaluator's signatures stay unchanged; set for the duration of a linked run, then cleared.
thread_local! {
    static RESOLVER: RefCell<HashMap<String, J>> = RefCell::new(HashMap::new());
}

/// Install the link map (address → body AST) for `fn_ref` resolution during evaluation.
pub fn set_resolver(map: HashMap<String, J>) {
    RESOLVER.with(|r| *r.borrow_mut() = map);
}

/// Clear the link map.
pub fn clear_resolver() {
    RESOLVER.with(|r| r.borrow_mut().clear());
}

fn resolve_fn_ref(addr: &str) -> Option<J> {
    RESOLVER.with(|r| r.borrow().get(addr).cloned())
}

// Effect enforcement: a scoped capability sandbox. Effectful builtins (`print` → io.console, `rand`
// → random) gate on a *granted* effect set and append to a structured trace — so a body may only
// perform effects its grant permits (e.g. a function record's declared `signature.effects`), and the
// trace is a replayable record of what it did (principles 5 + 9). Pure bodies touch none of this.
thread_local! {
    static EFFECTS: RefCell<EffectState> = RefCell::new(EffectState::new());
}

struct EffectState {
    granted: std::collections::HashSet<String>,
    trace: Vec<J>,
    rng: u64,
}

impl EffectState {
    fn new() -> Self {
        EffectState { granted: std::collections::HashSet::new(), trace: Vec::new(), rng: 0x1234_5678_9abc_def0 }
    }
}

/// Install the granted effect set for the next evaluation; resets the trace and the effect PRNG so a
/// run is reproducible. An effectful builtin not in this set is rejected at eval time.
pub fn set_effect_grants<I: IntoIterator<Item = String>>(granted: I) {
    EFFECTS.with(|e| {
        let mut e = e.borrow_mut();
        e.granted = granted.into_iter().collect();
        e.trace.clear();
        e.rng = 0x1234_5678_9abc_def0;
    });
}

/// Reset the effect sandbox (empty grant, empty trace).
pub fn clear_effects() {
    EFFECTS.with(|e| *e.borrow_mut() = EffectState::new());
}

/// Drain the structured effect trace collected during evaluation (principle 9: AI-ingestible trace).
pub fn take_effect_trace() -> Vec<J> {
    EFFECTS.with(|e| std::mem::take(&mut e.borrow_mut().trace))
}

/// Gate and record an effect. Errors if `effect` is not in the granted set — this is the enforcement:
/// declared effects become a capability the runtime checks, not just metadata.
fn perform_effect(effect: &str, detail: J) -> Result<()> {
    EFFECTS.with(|e| {
        let mut e = e.borrow_mut();
        if !e.granted.contains(effect) {
            bail!("ungranted effect `{effect}`: the body performed it, but it is not in the granted capability set (declare it in signature.effects or pass --grant {effect})");
        }
        e.trace.push(json!({ "effect": effect, "detail": detail }));
        Ok(())
    })
}

/// Deterministic per-evaluation PRNG draw in `[0, bound)` for the `rand` effect.
fn effect_rand(bound: i128) -> Result<i128> {
    if bound <= 0 {
        bail!("rand bound must be positive, got {bound}");
    }
    let r = EFFECTS.with(|e| {
        let mut e = e.borrow_mut();
        let mut x = e.rng | 1;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        e.rng = x;
        x.wrapping_mul(0x2545f4914f6cdd1d)
    });
    Ok((r % bound as u64) as i128)
}

/// A runtime value. Mirrors the value-expression kinds, plus the two callable forms (`Closure`,
/// `Builtin`) that only exist at runtime.
#[derive(Clone, Debug)]
pub enum Val {
    Bool(bool),
    Int(i128),
    Float(f64),
    Str(String),
    Bytes(Vec<u8>),
    Unit,
    List(Vec<Val>),
    Tuple(Vec<Val>),
    Record(BTreeMap<String, Val>),
    Variant(String, Option<Box<Val>>),
    FnRef(String),
    Closure { params: Vec<String>, body: Rc<J>, env: Env },
    Builtin { name: String, arity: usize, applied: Vec<Val> },
}

// ---------------------------------------------------------------------------
// Value (de)serialization: value-expression AST <-> Val.
// ---------------------------------------------------------------------------

fn parse_int(v: &J) -> Result<i128> {
    if let Some(i) = v.as_i64() {
        return Ok(i as i128);
    }
    if let Some(u) = v.as_u64() {
        return Ok(u as i128);
    }
    if let Some(s) = v.as_str() {
        return s.parse::<i128>().map_err(|e| anyhow!("integer literal {s:?}: {e}"));
    }
    bail!("not an integer literal: {v}")
}

/// Decode a value-expression AST node into a runtime `Val`.
pub fn decode_value(v: &J) -> Result<Val> {
    let kind = v.get("kind").and_then(|k| k.as_str()).ok_or_else(|| anyhow!("value missing kind: {v}"))?;
    Ok(match kind {
        "bool" => Val::Bool(v["value"].as_bool().ok_or_else(|| anyhow!("bool value"))?),
        "int" | "nat" => Val::Int(parse_int(&v["value"])?),
        "float" => Val::Float(v["value"].as_f64().ok_or_else(|| anyhow!("float value"))?),
        "string" => Val::Str(v["value"].as_str().ok_or_else(|| anyhow!("string value"))?.to_string()),
        "bytes" => {
            let s = v["value"].as_str().ok_or_else(|| anyhow!("bytes value"))?;
            Val::Bytes(base64::engine::general_purpose::STANDARD.decode(s).map_err(|e| anyhow!("base64: {e}"))?)
        }
        "unit" => Val::Unit,
        "list" => Val::List(decode_seq(&v["elems"])?),
        "tuple" => Val::Tuple(decode_seq(&v["elems"])?),
        "record" => {
            let mut m = BTreeMap::new();
            for f in v["fields"].as_array().ok_or_else(|| anyhow!("record fields"))? {
                let name = f["name"].as_str().ok_or_else(|| anyhow!("field name"))?.to_string();
                m.insert(name, decode_value(&f["value"])?);
            }
            Val::Record(m)
        }
        "variant" => {
            let tag = v["tag"].as_str().ok_or_else(|| anyhow!("variant tag"))?.to_string();
            let payload = match v.get("payload") {
                Some(p) => Some(Box::new(decode_value(p)?)),
                None => None,
            };
            Val::Variant(tag, payload)
        }
        "fn_ref" => Val::FnRef(v["target"].as_str().ok_or_else(|| anyhow!("fn_ref target"))?.to_string()),
        other => bail!("unknown value kind: {other}"),
    })
}

fn decode_seq(v: &J) -> Result<Vec<Val>> {
    v.as_array().ok_or_else(|| anyhow!("expected an array of values"))?.iter().map(decode_value).collect()
}

/// Encode a runtime `Val` back into a value-expression AST node (for `eval`'s output). Integers are
/// emitted as `int`; callables and `fn_ref` are emitted in an informational form.
pub fn encode_value(v: &Val) -> J {
    match v {
        Val::Bool(b) => json!({ "kind": "bool", "value": b }),
        Val::Int(i) => {
            // JSON numbers are exact only below 2^53 (spec/canonical-serialization.md); stringify above.
            if i.unsigned_abs() < (1u128 << 53) {
                json!({ "kind": "int", "value": *i as i64 })
            } else {
                json!({ "kind": "int", "value": i.to_string() })
            }
        }
        Val::Float(f) => json!({ "kind": "float", "value": f }),
        Val::Str(s) => json!({ "kind": "string", "value": s }),
        Val::Bytes(b) => json!({ "kind": "bytes", "value": base64::engine::general_purpose::STANDARD.encode(b) }),
        Val::Unit => json!({ "kind": "unit" }),
        Val::List(xs) => json!({ "kind": "list", "elems": xs.iter().map(encode_value).collect::<Vec<_>>() }),
        Val::Tuple(xs) => json!({ "kind": "tuple", "elems": xs.iter().map(encode_value).collect::<Vec<_>>() }),
        Val::Record(m) => json!({
            "kind": "record",
            "fields": m.iter().map(|(k, v)| json!({ "name": k, "value": encode_value(v) })).collect::<Vec<_>>()
        }),
        Val::Variant(tag, payload) => match payload {
            Some(p) => json!({ "kind": "variant", "tag": tag, "payload": encode_value(p) }),
            None => json!({ "kind": "variant", "tag": tag }),
        },
        Val::FnRef(t) => json!({ "kind": "fn_ref", "target": t }),
        Val::Closure { params, .. } => json!({ "kind": "function", "params": params.len() }),
        Val::Builtin { name, arity, applied } => {
            json!({ "kind": "function", "builtin": name, "remaining": arity - applied.len() })
        }
    }
}

/// Structural equality (the semantics of the `eq` builtin and `lit` patterns).
pub fn val_eq(a: &Val, b: &Val) -> bool {
    match (a, b) {
        (Val::Bool(x), Val::Bool(y)) => x == y,
        (Val::Int(x), Val::Int(y)) => x == y,
        (Val::Float(x), Val::Float(y)) => x == y,
        (Val::Str(x), Val::Str(y)) => x == y,
        (Val::Bytes(x), Val::Bytes(y)) => x == y,
        (Val::Unit, Val::Unit) => true,
        (Val::List(x), Val::List(y)) | (Val::Tuple(x), Val::Tuple(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| val_eq(p, q))
        }
        (Val::Record(x), Val::Record(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| val_eq(v, w)))
        }
        (Val::Variant(t1, p1), Val::Variant(t2, p2)) => {
            t1 == t2
                && match (p1, p2) {
                    (None, None) => true,
                    (Some(a), Some(b)) => val_eq(a, b),
                    _ => false,
                }
        }
        (Val::FnRef(x), Val::FnRef(y)) => x == y,
        _ => false, // closures/builtins are not comparable
    }
}

// ---------------------------------------------------------------------------
// Evaluation.
// ---------------------------------------------------------------------------

fn as_int(v: &Val) -> Result<i128> {
    match v {
        Val::Int(i) => Ok(*i),
        _ => bail!("expected an integer, got {}", encode_value(v)),
    }
}

/// Coerce an `Int` or `Float` to `f64` (for mixed-numeric arithmetic / comparison).
fn as_f64n(v: &Val) -> Result<f64> {
    match v {
        Val::Int(i) => Ok(*i as f64),
        Val::Float(f) => Ok(*f),
        _ => bail!("expected a number, got {}", encode_value(v)),
    }
}

fn as_bool(v: &Val) -> Result<bool> {
    match v {
        Val::Bool(b) => Ok(*b),
        _ => bail!("expected a bool, got {}", encode_value(v)),
    }
}

fn as_list(v: &Val) -> Result<Vec<Val>> {
    match v {
        Val::List(xs) => Ok(xs.clone()),
        _ => bail!("expected a list, got {}", encode_value(v)),
    }
}

/// Builtin arity, or `None` if `name` is not a builtin. `nil` is handled separately (a nullary value).
fn builtin_arity(name: &str) -> Option<usize> {
    Some(match name {
        "neg" | "abs" | "not" | "id" | "head" | "tail" | "length" | "null" | "reverse" | "fst"
        | "snd" | "print" | "rand" | "now" | "panic" => 1,
        "add" | "sub" | "mul" | "div" | "mod" | "eq" | "neq" | "lt" | "le" | "gt" | "ge" | "and"
        | "or" | "xor" | "cons" | "append" | "concat" | "map" | "filter" | "min" | "max"
        | "apply" => 2,
        "foldl" | "foldr" | "compose" => 3,
        _ => return None,
    })
}

/// The effect an effectful builtin performs, or `None` if pure. Mirrors the gating in `run_builtin`.
pub fn builtin_effect(name: &str) -> Option<&'static str> {
    match name {
        "print" => Some("io.console"),
        "rand" => Some("random"),
        "now" => Some("time"),
        "panic" => Some("panic"),
        _ => None,
    }
}

/// Whether `name` is a known builtin (incl. `nil`) — i.e. its meaning and effects are known statically.
pub fn is_builtin(name: &str) -> bool {
    name == "nil" || builtin_arity(name).is_some()
}

/// Resolve a `var` name: lexical environment first, then the builtin library, then the `nil` constant.
fn resolve_var(name: &str, env: &Env) -> Result<Val> {
    if let Some(v) = env.get(name) {
        return Ok(v.clone());
    }
    if name == "nil" {
        return Ok(Val::List(vec![]));
    }
    if let Some(arity) = builtin_arity(name) {
        return Ok(Val::Builtin { name: name.to_string(), arity, applied: vec![] });
    }
    bail!("unbound variable: {name}")
}

/// Evaluate a body-expression AST node in an environment.
pub fn eval(expr: &J, env: &Env) -> Result<Val> {
    let kind = expr.get("kind").and_then(|k| k.as_str()).ok_or_else(|| anyhow!("expr missing kind: {expr}"))?;
    match kind {
        "var" => resolve_var(expr["name"].as_str().ok_or_else(|| anyhow!("var name"))?, env),
        "lit" => decode_value(&expr["value"]),
        "lambda" => {
            let params = expr["params"]
                .as_array()
                .ok_or_else(|| anyhow!("lambda params"))?
                .iter()
                .map(|p| p["name"].as_str().map(String::from).ok_or_else(|| anyhow!("param name")))
                .collect::<Result<Vec<_>>>()?;
            Ok(Val::Closure { params, body: Rc::new(expr["body"].clone()), env: env.clone() })
        }
        "app" => {
            let f = eval(&expr["fn"], env)?;
            let args = expr["args"]
                .as_array()
                .ok_or_else(|| anyhow!("app args"))?
                .iter()
                .map(|a| eval(a, env))
                .collect::<Result<Vec<_>>>()?;
            apply(f, args)
        }
        "let" => {
            let name = expr["name"].as_str().ok_or_else(|| anyhow!("let name"))?.to_string();
            let bound = eval(&expr["value"], env)?;
            let mut env2 = env.clone();
            env2.insert(name, bound);
            eval(&expr["body"], &env2)
        }
        "case" => {
            let scrutinee = eval(&expr["scrutinee"], env)?;
            for arm in expr["arms"].as_array().ok_or_else(|| anyhow!("case arms"))? {
                if let Some(binds) = match_pattern(&arm["pattern"], &scrutinee)? {
                    let mut env2 = env.clone();
                    env2.extend(binds);
                    return eval(&arm["body"], &env2);
                }
            }
            bail!("non-exhaustive case: no arm matched {}", encode_value(&scrutinee))
        }
        "field" => {
            let record = eval(&expr["record"], env)?;
            let name = expr["name"].as_str().ok_or_else(|| anyhow!("field name"))?;
            match record {
                Val::Record(m) => m.get(name).cloned().ok_or_else(|| anyhow!("no field {name} on record")),
                other => bail!("field projection on a non-record: {}", encode_value(&other)),
            }
        }
        other => bail!("unknown expression kind: {other}"),
    }
}

/// Match a pattern against a value; `Some(bindings)` on success (possibly empty), `None` on mismatch.
fn match_pattern(pat: &J, v: &Val) -> Result<Option<Env>> {
    let kind = pat.get("kind").and_then(|k| k.as_str()).ok_or_else(|| anyhow!("pattern missing kind"))?;
    Ok(match kind {
        "wildcard" => Some(Env::new()),
        "bind" => {
            let name = pat["name"].as_str().ok_or_else(|| anyhow!("bind name"))?.to_string();
            let mut e = Env::new();
            e.insert(name, v.clone());
            Some(e)
        }
        "lit" => {
            if val_eq(&decode_value(&pat["value"])?, v) {
                Some(Env::new())
            } else {
                None
            }
        }
        "variant" => {
            let tag = pat["tag"].as_str().ok_or_else(|| anyhow!("variant tag"))?;
            match v {
                Val::Variant(vtag, payload) if vtag == tag => match (pat.get("payload"), payload) {
                    (None, _) => Some(Env::new()),
                    (Some(pp), Some(pv)) => match_pattern(pp, pv)?,
                    (Some(_), None) => None,
                },
                _ => None,
            }
        }
        other => bail!("unknown pattern kind: {other}"),
    })
}

/// Apply a callable to arguments, supporting currying (too few args → a partial application) and
/// over-application (extra args applied to the result).
pub fn apply(f: Val, mut args: Vec<Val>) -> Result<Val> {
    if args.is_empty() {
        return Ok(f);
    }
    match f {
        Val::Closure { params, body, env } => {
            if args.len() < params.len() {
                let mut env2 = env.clone();
                for (p, a) in params.iter().zip(args.iter()) {
                    env2.insert(p.clone(), a.clone());
                }
                Ok(Val::Closure { params: params[args.len()..].to_vec(), body, env: env2 })
            } else {
                let mut env2 = env;
                for (p, a) in params.iter().zip(args.iter()) {
                    env2.insert(p.clone(), a.clone());
                }
                let result = eval(&body, &env2)?;
                let extra = args.split_off(params.len());
                apply(result, extra)
            }
        }
        Val::Builtin { name, arity, mut applied } => {
            applied.append(&mut args);
            if applied.len() < arity {
                Ok(Val::Builtin { name, arity, applied })
            } else {
                let rest = applied.split_off(arity);
                let result = run_builtin(&name, applied)?;
                apply(result, rest)
            }
        }
        Val::FnRef(addr) => match resolve_fn_ref(&addr) {
            // Composition: resolve the referenced record's body and apply it (see RESOLVER above).
            Some(body) => apply(eval(&body, &Env::new())?, args),
            None => bail!("cannot apply unresolved fn_ref {addr} (run with --records to link it)"),
        },
        other => bail!("cannot apply a non-function value: {}", encode_value(&other)),
    }
}

fn run_builtin(name: &str, a: Vec<Val>) -> Result<Val> {
    // Arithmetic stays exact on two ints; if either operand is a float, promote to f64 (so `number`
    // bodies from TS/JS, which carry floats, run). Comparison always compares numerically.
    let num2 = |fi: fn(i128, i128) -> i128, ff: fn(f64, f64) -> f64| -> Result<Val> {
        Ok(match (&a[0], &a[1]) {
            (Val::Int(x), Val::Int(y)) => Val::Int(fi(*x, *y)),
            _ => Val::Float(ff(as_f64n(&a[0])?, as_f64n(&a[1])?)),
        })
    };
    let numcmp = |f: fn(f64, f64) -> bool| -> Result<Val> { Ok(Val::Bool(f(as_f64n(&a[0])?, as_f64n(&a[1])?))) };
    Ok(match name {
        "add" => num2(|x, y| x + y, |x, y| x + y)?,
        "sub" => num2(|x, y| x - y, |x, y| x - y)?,
        "mul" => num2(|x, y| x * y, |x, y| x * y)?,
        "div" => match (&a[0], &a[1]) {
            (Val::Int(x), Val::Int(y)) => {
                if *y == 0 {
                    bail!("division by zero");
                }
                Val::Int(x.div_euclid(*y))
            }
            _ => Val::Float(as_f64n(&a[0])? / as_f64n(&a[1])?),
        },
        "mod" => match (&a[0], &a[1]) {
            (Val::Int(x), Val::Int(y)) => {
                if *y == 0 {
                    bail!("modulo by zero");
                }
                Val::Int(x.rem_euclid(*y))
            }
            _ => Val::Float(as_f64n(&a[0])? % as_f64n(&a[1])?),
        },
        "neg" => match &a[0] {
            Val::Int(i) => Val::Int(-i),
            v => Val::Float(-as_f64n(v)?),
        },
        "abs" => match &a[0] {
            Val::Int(i) => Val::Int(i.abs()),
            v => Val::Float(as_f64n(v)?.abs()),
        },
        "min" => num2(std::cmp::min, f64::min)?,
        "max" => num2(std::cmp::max, f64::max)?,
        "eq" => Val::Bool(val_eq(&a[0], &a[1])),
        "neq" => Val::Bool(!val_eq(&a[0], &a[1])),
        "lt" => numcmp(|x, y| x < y)?,
        "le" => numcmp(|x, y| x <= y)?,
        "gt" => numcmp(|x, y| x > y)?,
        "ge" => numcmp(|x, y| x >= y)?,
        "and" => Val::Bool(as_bool(&a[0])? && as_bool(&a[1])?),
        "or" => Val::Bool(as_bool(&a[0])? || as_bool(&a[1])?),
        "xor" => Val::Bool(as_bool(&a[0])? ^ as_bool(&a[1])?),
        "not" => Val::Bool(!as_bool(&a[0])?),
        "id" => a.into_iter().next().unwrap(),
        "fst" => match &a[0] {
            Val::Tuple(xs) if !xs.is_empty() => xs[0].clone(),
            other => bail!("fst on a non-tuple: {}", encode_value(other)),
        },
        "snd" => match &a[0] {
            Val::Tuple(xs) if xs.len() >= 2 => xs[1].clone(),
            other => bail!("snd on a non-pair: {}", encode_value(other)),
        },
        "cons" => {
            let mut xs = as_list(&a[1])?;
            xs.insert(0, a[0].clone());
            Val::List(xs)
        }
        "head" => as_list(&a[0])?.into_iter().next().ok_or_else(|| anyhow!("head of empty list"))?,
        "tail" => {
            let xs = as_list(&a[0])?;
            if xs.is_empty() {
                bail!("tail of empty list");
            }
            Val::List(xs[1..].to_vec())
        }
        "length" => Val::Int(as_list(&a[0])?.len() as i128),
        "null" => Val::Bool(as_list(&a[0])?.is_empty()),
        "reverse" => {
            let mut xs = as_list(&a[0])?;
            xs.reverse();
            Val::List(xs)
        }
        "append" | "concat" => {
            let mut xs = as_list(&a[0])?;
            xs.extend(as_list(&a[1])?);
            Val::List(xs)
        }
        "map" => {
            let f = a[0].clone();
            let out = as_list(&a[1])?
                .into_iter()
                .map(|x| apply(f.clone(), vec![x]))
                .collect::<Result<Vec<_>>>()?;
            Val::List(out)
        }
        "filter" => {
            let p = a[0].clone();
            let mut out = vec![];
            for x in as_list(&a[1])? {
                if as_bool(&apply(p.clone(), vec![x.clone()])?)? {
                    out.push(x);
                }
            }
            Val::List(out)
        }
        "foldl" => {
            let f = a[0].clone();
            let mut acc = a[1].clone();
            for x in as_list(&a[2])? {
                acc = apply(f.clone(), vec![acc, x])?;
            }
            acc
        }
        "foldr" => {
            let f = a[0].clone();
            let init = a[1].clone();
            let xs = as_list(&a[2])?;
            let mut acc = init;
            for x in xs.into_iter().rev() {
                acc = apply(f.clone(), vec![x, acc])?;
            }
            acc
        }
        "compose" => {
            // compose(f, g, x) = f (g x)
            let inner = apply(a[1].clone(), vec![a[2].clone()])?;
            apply(a[0].clone(), vec![inner])?
        }
        "apply" => apply(a[0].clone(), vec![a[1].clone()])?,
        // Effectful builtins — gated by the capability sandbox (see EFFECTS above).
        "print" => {
            // io.console: emit the argument to the trace, return unit.
            perform_effect("io.console", encode_value(&a[0]))?;
            Val::Unit
        }
        "rand" => {
            // random: a deterministic draw in [0, n) recorded as an effect.
            let n = as_int(&a[0])?;
            perform_effect("random", json!({ "bound": n.to_string() }))?;
            Val::Int(effect_rand(n)?)
        }
        "now" => {
            // time: a fixed clock reading (deterministic/replayable, principle 5), recorded.
            perform_effect("time", json!({}))?;
            Val::Int(0)
        }
        "panic" => {
            // panic: record the effect, then abort with the message.
            perform_effect("panic", encode_value(&a[0]))?;
            bail!("panic: {}", encode_value(&a[0]));
        }
        other => bail!("unknown builtin: {other}"),
    })
}

// ---------------------------------------------------------------------------
// Top-level entry points used by the CLI (`eval` / `run`).
// ---------------------------------------------------------------------------

/// Evaluate a body AST, then apply it to the given argument values. Returns the resulting value AST.
pub fn eval_body(body: &J, args: &[J]) -> Result<J> {
    let f = eval(body, &Env::new())?;
    let argv = args.iter().map(decode_value).collect::<Result<Vec<_>>>()?;
    Ok(encode_value(&apply(f, argv)?))
}

/// Outcome of running one worked example through the body.
pub struct ExampleRun {
    pub index: usize,
    pub passed: bool,
    pub got: J,
    pub expected: J,
    pub error: Option<String>,
}

/// Run every `examples[]` of a function record through its `body`: bind the example's args, evaluate
/// the body, and compare to the example's claimed `result`. This is what makes the examples executable.
pub fn run_examples(record: &J, body: &J) -> Result<Vec<ExampleRun>> {
    let f = eval(body, &Env::new())?;
    let examples = record.get("examples").and_then(|e| e.as_array()).cloned().unwrap_or_default();
    let mut out = vec![];
    for (index, ex) in examples.iter().enumerate() {
        let args = ex.get("args").and_then(|a| a.as_array()).cloned().unwrap_or_default();
        let expected_j = ex.get("result").cloned().unwrap_or(J::Null);
        let run = (|| -> Result<(bool, J)> {
            let argv = args.iter().map(decode_value).collect::<Result<Vec<_>>>()?;
            let got = apply(f.clone(), argv)?;
            let expected = decode_value(&expected_j)?;
            Ok((val_eq(&got, &expected), encode_value(&got)))
        })();
        match run {
            Ok((passed, got)) => out.push(ExampleRun { index, passed, got, expected: expected_j, error: None }),
            Err(e) => out.push(ExampleRun {
                index,
                passed: false,
                got: J::Null,
                expected: expected_j,
                error: Some(format!("{e:#}")),
            }),
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Run-backed property verification (predicate-expression AST, spec/predicate-expression.schema.json).
//
// The static property checker (eval.rs) honestly marks any law needing to *re-apply a function*
// (`map`/`filter`/`fold`/`compose`/`apply`/the function-under-test `self`) or a quantifier as
// UNVERIFIABLE. With an executable body in hand, those become decidable: `self` is the running
// function, the higher-order ops are the builtins above, and a `forall` ranges over the worked
// examples' arguments (the examples ARE the test inputs). So `forall n. eq(self(n), add(n, n))` is
// now actually checked, per example — CONSISTENT instead of UNVERIFIABLE. Still example-bound, so not
// a proof: a CONSISTENT verdict means "ran true on every example and false on none".
// ---------------------------------------------------------------------------

use crate::Verdict;

fn decode_pred_lit(v: &J) -> Option<Val> {
    match v {
        J::Bool(b) => Some(Val::Bool(*b)),
        J::Number(n) => n.as_i64().map(|i| Val::Int(i as i128)).or_else(|| n.as_f64().map(Val::Float)),
        J::String(s) => Some(Val::Str(s.clone())),
        J::Null => Some(Val::Unit),
        _ => None,
    }
}

/// Evaluate a predicate-expression node. `None` == undecidable (unbound var, unknown op, or an
/// application that errors). `self_fn` is the executable function-under-test, bound to `self`.
fn eval_predicate(node: &J, env: &Env, self_fn: &Option<Val>) -> Option<Val> {
    let kind = node.get("kind")?.as_str()?;
    match kind {
        "var" => {
            let name = node.get("name")?.as_str()?;
            if name == "self" {
                self_fn.clone()
            } else {
                // Bound var first; otherwise a bare builtin/`nil` used as an argument (e.g. `id` in
                // `map(id, xs)`), so functor laws over known builtins become decidable.
                env.get(name).cloned().or_else(|| resolve_var(name, &Env::new()).ok())
            }
        }
        "lit" => {
            // A bare scalar JSON literal, or a structured value-expression payload (the schema
            // allows the latter for compound literals — lists, fn_refs, records, …).
            let v = node.get("value")?;
            decode_pred_lit(v).or_else(|| decode_value(v).ok())
        }
        "forall" | "exists" => {
            // Range the quantifier over THIS example: bind the bound vars positionally to arg0..argN.
            let mut env2 = env.clone();
            if let Some(vars) = node.get("vars").and_then(|v| v.as_array()) {
                for (i, var) in vars.iter().enumerate() {
                    if let (Some(name), Some(arg)) = (var.as_str(), env.get(&format!("arg{i}"))) {
                        env2.insert(name.to_string(), arg.clone());
                    }
                }
            }
            eval_predicate(node.get("body")?, &env2, self_fn)
        }
        "app" => {
            let op = node.get("op")?.as_str()?;
            let arg_nodes = node.get("args")?.as_array()?;
            let args: Option<Vec<Val>> = arg_nodes.iter().map(|a| eval_predicate(a, env, self_fn)).collect();
            let args = args?;
            match op {
                // Boolean connectives not in the builtin library.
                "implies" => match (&args[0], &args[1]) {
                    (Val::Bool(a), Val::Bool(b)) => Some(Val::Bool(!a || *b)),
                    _ => None,
                },
                "iff" => match (&args[0], &args[1]) {
                    (Val::Bool(a), Val::Bool(b)) => Some(Val::Bool(a == b)),
                    _ => None,
                },
                // A content-address op (`fn_…`/`expr_…`) is a commons function referenced by hash —
                // apply it as a `fn_ref` so the thread-local resolver links it (set during claim
                // verification, see `eval_claim`). If no resolver is installed the apply errors →
                // None → undecidable, so this never silently passes.
                _ if op.starts_with("fn_") || op.starts_with("expr_") => {
                    apply(Val::FnRef(op.to_string()), args).ok()
                }
                // Everything else — eq/neq/and/or/not, arithmetic, comparisons, list ops, and the
                // higher-order map/filter/fold/compose/apply — IS a builtin. Run it.
                _ => {
                    let f = resolve_var(op, &Env::new()).ok()?;
                    apply(f, args).ok()
                }
            }
        }
        _ => None,
    }
}

/// Verdict for one property across a record's examples, with the body available to run.
pub fn runtime_verdict(expr: &J, examples: &[J], self_fn: &Option<Val>) -> Verdict {
    let mut any_true = false;
    let mut any_false = false;
    for ex in examples {
        let mut env = Env::new();
        if let Some(r) = ex.get("result").and_then(|r| decode_value(r).ok()) {
            env.insert("result".to_string(), r);
        }
        if let Some(args) = ex.get("args").and_then(|a| a.as_array()) {
            for (i, a) in args.iter().enumerate() {
                if let Ok(v) = decode_value(a) {
                    env.insert(format!("arg{i}"), v);
                }
            }
        }
        match eval_predicate(expr, &env, self_fn) {
            Some(Val::Bool(true)) => any_true = true,
            Some(Val::Bool(false)) => any_false = true,
            _ => {}
        }
    }
    if any_false {
        Verdict::Contradicted
    } else if any_true {
        Verdict::Consistent
    } else {
        Verdict::Unverifiable
    }
}

/// Build the executable function-under-test from a body AST (for `self`), if it evaluates.
pub fn self_fn_from_body(body: &J) -> Option<Val> {
    eval(body, &Env::new()).ok()
}

/// Evaluate a closed predicate-expression — a Nova Locutio `assert` claim — to a runtime value,
/// resolving any content-addressed function ops (`fn_…`/`expr_…`) through the installed resolver
/// (set via [`set_resolver`]). `None` if undecidable. Used by claim verification: the receiver
/// re-runs the claim instead of trusting the asserter (principle 3 — verification is re-execution).
pub fn eval_claim(expr: &J) -> Option<Val> {
    eval_predicate(expr, &Env::new(), &None)
}

/// Evaluate a predicate-expression node under explicit variable bindings, for the generative
/// property-testing engine (`proptest.rs`): `bindings` supplies the quantified variables' sampled
/// values and `self_fn` the function-under-test (`self`). `None` if undecidable on these inputs
/// (e.g. the input is outside the function's domain) — the caller treats that as a skipped case, not
/// a counterexample.
pub fn eval_predicate_env(
    node: &J,
    bindings: &BTreeMap<String, Val>,
    self_fn: &Option<Val>,
) -> Option<Val> {
    eval_predicate(node, bindings, self_fn)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn examples_dir() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spec/examples")
    }

    fn load(name: &str) -> J {
        serde_json::from_str(&std::fs::read_to_string(examples_dir().join(name)).unwrap()).unwrap()
    }

    fn nat(n: i128) -> J {
        json!({ "kind": "nat", "value": n as i64 })
    }

    #[test]
    fn double_runs_on_its_examples() {
        let record = load("double.v0.2.json");
        let body = load("body-double.json");
        let runs = run_examples(&record, &body).unwrap();
        assert_eq!(runs.len(), 3);
        assert!(runs.iter().all(|r| r.passed), "double should match all its worked examples");
        // double(5) == 10
        assert_eq!(eval_body(&body, &[nat(5)]).unwrap(), encode_value(&Val::Int(10)));
    }

    #[test]
    fn is_zero_case_matching() {
        let body = load("body-is-zero.json");
        assert_eq!(eval_body(&body, &[nat(0)]).unwrap(), json!({ "kind": "bool", "value": true }));
        assert_eq!(eval_body(&body, &[nat(7)]).unwrap(), json!({ "kind": "bool", "value": false }));
    }

    #[test]
    fn detects_a_wrong_example() {
        let record = json!({
            "examples": [{ "args": [nat(2)], "result": nat(5) }]  // wrong: double(2) = 4, not 5
        });
        let body = load("body-double.json");
        let runs = run_examples(&record, &body).unwrap();
        assert!(!runs[0].passed);
        assert_eq!(runs[0].got, encode_value(&Val::Int(4)));
    }

    #[test]
    fn higher_order_builtins() {
        // map(double, [1,2,3]) == [2,4,6] using a lambda for double.
        let dbl = json!({
            "kind": "lambda",
            "params": [{ "name": "n" }],
            "body": { "kind": "app", "fn": { "kind": "var", "name": "add" },
                      "args": [{ "kind": "var", "name": "n" }, { "kind": "var", "name": "n" }] }
        });
        let f = eval(&dbl, &Env::new()).unwrap();
        let xs = Val::List(vec![Val::Int(1), Val::Int(2), Val::Int(3)]);
        let mapped = apply(
            Val::Builtin { name: "map".into(), arity: 2, applied: vec![] },
            vec![f.clone(), xs.clone()],
        )
        .unwrap();
        assert!(val_eq(&mapped, &Val::List(vec![Val::Int(2), Val::Int(4), Val::Int(6)])));

        // foldl(add, 0, [1,2,3,4]) == 10
        let add = Val::Builtin { name: "add".into(), arity: 2, applied: vec![] };
        let sum = apply(
            Val::Builtin { name: "foldl".into(), arity: 3, applied: vec![] },
            vec![add, Val::Int(0), Val::List(vec![Val::Int(1), Val::Int(2), Val::Int(3), Val::Int(4)])],
        )
        .unwrap();
        assert!(val_eq(&sum, &Val::Int(10)));
    }

    #[test]
    fn currying_and_compose() {
        // compose(double, double)(3) == 12   (currying: compose applied to 2 of 3 args is a function)
        let dbl = eval(
            &json!({ "kind": "lambda", "params": [{ "name": "n" }],
                     "body": { "kind": "app", "fn": { "kind": "var", "name": "add" },
                               "args": [{ "kind": "var", "name": "n" }, { "kind": "var", "name": "n" }] } }),
            &Env::new(),
        )
        .unwrap();
        let compose = Val::Builtin { name: "compose".into(), arity: 3, applied: vec![] };
        let twice = apply(compose, vec![dbl.clone(), dbl]).unwrap(); // partial: a function
        let out = apply(twice, vec![Val::Int(3)]).unwrap();
        assert!(val_eq(&out, &Val::Int(12)));
    }

    #[test]
    fn composition_resolves_fn_ref_across_records() {
        // Link `double` by its real content-address, then run `\xs -> map(<fn_ref double>, xs)` on
        // [1,2,3]: the fn_ref resolves to double's committed body and runs -> [2,4,6]. Cross-record
        // composition with real data (principle 4: assemble from existing records).
        let double_rec = load("double.v0.2.json");
        let addr = double_rec["hash"].as_str().unwrap().to_string();
        let mut map = HashMap::new();
        map.insert(addr.clone(), load("body-double.json"));
        set_resolver(map);

        let body = json!({ "kind": "lambda", "params": [{ "name": "xs" }],
            "body": { "kind": "app", "fn": { "kind": "var", "name": "map" }, "args": [
                { "kind": "lit", "value": { "kind": "fn_ref", "target": addr } },
                { "kind": "var", "name": "xs" }] } });
        let xs = json!({ "kind": "list", "elems": [nat(1), nat(2), nat(3)] });
        let got = eval_body(&body, &[xs]).unwrap();
        clear_resolver();

        assert_eq!(got, json!({ "kind": "list", "elems": [
            { "kind": "int", "value": 2 }, { "kind": "int", "value": 4 }, { "kind": "int", "value": 6 }] }));

        // An unresolved fn_ref is an honest error (not a silent pass).
        assert!(eval_body(
            &json!({ "kind": "app", "fn": { "kind": "lit", "value": { "kind": "fn_ref", "target": "fn_deadbeef" } },
                     "args": [{ "kind": "lit", "value": nat(1) }] }),
            &[],
        )
        .is_err());
    }

    #[test]
    fn run_backed_property_verification() {
        // double's law `forall n. eq(self(n), add(n, n))` is UNVERIFIABLE statically (self + forall),
        // but with the runnable body it is actually checked over the examples -> CONSISTENT.
        let record = load("double.v0.2.json");
        let body = load("body-double.json");
        let examples: Vec<J> = record["examples"].as_array().unwrap().clone();
        let expr = &record["properties"][0]["expr"];
        let self_fn = self_fn_from_body(&body);
        assert_eq!(runtime_verdict(expr, &examples, &self_fn), Verdict::Consistent);

        // A body that does NOT satisfy the law (triple instead of double) is CONTRADICTED.
        let triple = json!({ "kind": "lambda", "params": [{ "name": "n" }],
            "body": { "kind": "app", "fn": { "kind": "var", "name": "mul" },
                      "args": [{ "kind": "var", "name": "n" }, { "kind": "lit", "value": nat(3) }] } });
        let wrong = self_fn_from_body(&triple);
        assert_eq!(runtime_verdict(expr, &examples, &wrong), Verdict::Contradicted);
    }

    #[test]
    fn let_and_field() {
        // let x = 4 in x ;  and  record field projection
        let e = json!({ "kind": "let", "name": "x", "value": { "kind": "lit", "value": nat(4) },
                        "body": { "kind": "var", "name": "x" } });
        assert!(val_eq(&eval(&e, &Env::new()).unwrap(), &Val::Int(4)));

        let rec = json!({ "kind": "lit", "value": { "kind": "record",
            "fields": [{ "name": "a", "value": nat(1) }, { "name": "b", "value": nat(2) }] } });
        let proj = json!({ "kind": "field", "record": rec, "name": "b" });
        assert!(val_eq(&eval(&proj, &Env::new()).unwrap(), &Val::Int(2)));
    }

    #[test]
    fn effect_enforcement_gates_print() {
        // \msg -> print(msg)
        let body = json!({ "kind": "lambda", "params": [{ "name": "msg" }],
            "body": { "kind": "app", "fn": { "kind": "var", "name": "print" },
                      "args": [{ "kind": "var", "name": "msg" }] } });
        let arg = json!({ "kind": "string", "value": "hi" });

        // Ungranted: the io.console effect is rejected at eval time.
        set_effect_grants(Vec::<String>::new());
        assert!(eval_body(&body, &[arg.clone()]).is_err(), "print must be rejected without io.console");
        clear_effects();

        // Granted: runs, returns unit, and the structured trace records the effect.
        set_effect_grants(vec!["io.console".to_string()]);
        let out = eval_body(&body, &[arg]).unwrap();
        let trace = take_effect_trace();
        clear_effects();
        assert_eq!(out, json!({ "kind": "unit" }));
        assert_eq!(trace.len(), 1);
        assert_eq!(trace[0]["effect"], "io.console");
        assert_eq!(trace[0]["detail"], json!({ "kind": "string", "value": "hi" }));
    }

    #[test]
    fn rand_is_deterministic_and_gated() {
        // \n -> rand(n)
        let body = json!({ "kind": "lambda", "params": [{ "name": "n" }],
            "body": { "kind": "app", "fn": { "kind": "var", "name": "rand" },
                      "args": [{ "kind": "var", "name": "n" }] } });
        let n = json!({ "kind": "nat", "value": 100 });

        set_effect_grants(vec!["random".to_string()]);
        let a = eval_body(&body, &[n.clone()]).unwrap();
        clear_effects();
        set_effect_grants(vec!["random".to_string()]);
        let b = eval_body(&body, &[n.clone()]).unwrap();
        clear_effects();
        assert_eq!(a, b, "rand must be deterministic across runs (same fixed seed)");
        let v = a["value"].as_i64().unwrap();
        assert!((0..100).contains(&v), "rand(100) in [0,100)");

        // Ungranted: random is rejected.
        set_effect_grants(Vec::<String>::new());
        assert!(eval_body(&body, &[n]).is_err(), "rand must be rejected without the random grant");
        clear_effects();
    }

    #[test]
    fn now_and_panic_are_gated_effects() {
        let unary = |op: &str| json!({ "kind": "lambda", "params": [{ "name": "x" }],
            "body": { "kind": "app", "fn": { "kind": "var", "name": op }, "args": [{ "kind": "var", "name": "x" }] } });

        // now → time: ungranted rejected; granted returns a fixed reading + traces `time`.
        set_effect_grants(Vec::<String>::new());
        assert!(eval_body(&unary("now"), &[nat(1)]).is_err());
        clear_effects();
        set_effect_grants(vec!["time".to_string()]);
        assert_eq!(eval_body(&unary("now"), &[nat(1)]).unwrap(), json!({ "kind": "int", "value": 0 }));
        let trace = take_effect_trace();
        clear_effects();
        assert_eq!(trace[0]["effect"], "time");

        // panic → panic: gated, and aborts even when granted.
        set_effect_grants(Vec::<String>::new());
        assert!(eval_body(&unary("panic"), &[nat(1)]).is_err()); // ungranted
        clear_effects();
        set_effect_grants(vec!["panic".to_string()]);
        assert!(eval_body(&unary("panic"), &[nat(1)]).is_err()); // granted but aborts
        clear_effects();
    }
}
