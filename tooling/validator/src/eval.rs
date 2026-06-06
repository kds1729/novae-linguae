//! Property evaluator — checks a function record's algebraic `properties[]` against its worked
//! `examples[]`.
//!
//! Each example binds the reserved predicate variables `result` (the example's result) and
//! `arg0..argN` (its positional args), both decoded from their value-expression ASTs. A property's
//! predicate AST is then evaluated three-valued per example:
//!   * **CONTRADICTED** — the predicate is `false` on at least one example (the record is internally
//!     inconsistent); `check-properties` fails.
//!   * **UNVERIFIABLE** — every example hits something undecidable from static examples alone: an
//!     unbound variable, a quantifier, or an op that needs re-applying an unknown function
//!     (`map` / `filter` / `foldl` / `foldr` / `compose`). Reported, not a failure — these need a
//!     runtime / property-testing engine, which is out of scope here.
//!   * **CONSISTENT** — `true` on ≥1 example and `false` on none. Not a proof; just not contradicted.
//!
//! Evaluable ops (decidable from examples): the boolean connectives, comparisons, arithmetic, and
//! the total list ops `length` / `head` / `tail` / `nil` / `cons` / `id`. This is exactly the
//! boundary at which postcondition-style laws like `eq(length(result), length(arg0))` are decided
//! while functor/composition laws are honestly deferred.

use anyhow::{anyhow, Result};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq)]
enum Term {
    Bool(bool),
    Int(i128),
    Float(f64),
    Str(String),
    Bytes(String),
    Unit,
    List(Vec<Term>),
    Tuple(Vec<Term>),
    Record(BTreeMap<String, Term>),
    Variant(String, Option<Box<Term>>),
    FnRef(String),
}

/// Decode a value-expression AST (as in `examples.args[i]` / `examples.result`) to a Term.
fn value_to_term(v: &Value) -> Option<Term> {
    let obj = v.as_object()?;
    match obj.get("kind")?.as_str()? {
        "bool" => Some(Term::Bool(obj.get("value")?.as_bool()?)),
        "int" | "nat" => int_term(obj.get("value")?),
        "float" => Some(Term::Float(obj.get("value")?.as_f64()?)),
        "string" => Some(Term::Str(obj.get("value")?.as_str()?.to_string())),
        "bytes" => Some(Term::Bytes(obj.get("value")?.as_str()?.to_string())),
        "unit" => Some(Term::Unit),
        "list" => {
            let mut xs = Vec::new();
            for e in obj.get("elems")?.as_array()? {
                xs.push(value_to_term(e)?);
            }
            Some(Term::List(xs))
        }
        "tuple" => {
            let mut xs = Vec::new();
            for e in obj.get("elems")?.as_array()? {
                xs.push(value_to_term(e)?);
            }
            Some(Term::Tuple(xs))
        }
        "record" => {
            let mut m = BTreeMap::new();
            for f in obj.get("fields")?.as_array()? {
                let fo = f.as_object()?;
                m.insert(fo.get("name")?.as_str()?.to_string(), value_to_term(fo.get("value")?)?);
            }
            Some(Term::Record(m))
        }
        "variant" => {
            let tag = obj.get("tag")?.as_str()?.to_string();
            let payload = match obj.get("payload") {
                Some(p) => Some(Box::new(value_to_term(p)?)),
                None => None,
            };
            Some(Term::Variant(tag, payload))
        }
        "fn_ref" => Some(Term::FnRef(obj.get("target")?.as_str()?.to_string())),
        _ => None,
    }
}

fn int_term(v: &Value) -> Option<Term> {
    if let Some(i) = v.as_i64() {
        Some(Term::Int(i as i128))
    } else {
        v.as_str()?.parse::<i128>().ok().map(Term::Int)
    }
}

/// Decode a predicate `lit.value` (a raw JSON scalar) to a Term.
fn lit_to_term(v: &Value) -> Option<Term> {
    match v {
        Value::Bool(b) => Some(Term::Bool(*b)),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Some(Term::Int(i as i128))
            } else {
                n.as_f64().map(Term::Float)
            }
        }
        Value::String(s) => Some(Term::Str(s.clone())),
        Value::Null => Some(Term::Unit),
        _ => None,
    }
}

fn as_f64(t: &Term) -> Option<f64> {
    match t {
        Term::Int(i) => Some(*i as f64),
        Term::Float(f) => Some(*f),
        _ => None,
    }
}

fn num_cmp(op: &str, a: &Term, b: &Term) -> Option<Term> {
    let (x, y) = (as_f64(a)?, as_f64(b)?);
    Some(Term::Bool(match op {
        "lt" => x < y,
        "le" => x <= y,
        "gt" => x > y,
        "ge" => x >= y,
        _ => return None,
    }))
}

fn num_arith(op: &str, a: &Term, b: &Term) -> Option<Term> {
    if let (Term::Int(x), Term::Int(y)) = (a, b) {
        let r = match op {
            "add" => x.checked_add(*y)?,
            "sub" => x.checked_sub(*y)?,
            "mul" => x.checked_mul(*y)?,
            "div" => {
                if *y == 0 {
                    return None;
                }
                x / y
            }
            "mod" => {
                if *y == 0 {
                    return None;
                }
                x % y
            }
            _ => return None,
        };
        return Some(Term::Int(r));
    }
    let (x, y) = (as_f64(a)?, as_f64(b)?);
    Some(Term::Float(match op {
        "add" => x + y,
        "sub" => x - y,
        "mul" => x * y,
        "div" => x / y,
        "mod" => x % y,
        _ => return None,
    }))
}

fn bin_bool(a: &Term, b: &Term, f: fn(bool, bool) -> bool) -> Option<Term> {
    if let (Term::Bool(x), Term::Bool(y)) = (a, b) {
        Some(Term::Bool(f(*x, *y)))
    } else {
        None
    }
}

/// Evaluate a predicate-expression node under `env`. None == UNVERIFIABLE (unbound var, quantifier,
/// or an op not decidable from static examples).
fn eval(node: &Value, env: &BTreeMap<String, Term>) -> Option<Term> {
    let obj = node.as_object()?;
    match obj.get("kind")?.as_str()? {
        "var" => env.get(obj.get("name")?.as_str()?).cloned(),
        "lit" => lit_to_term(obj.get("value")?),
        "app" => {
            let op = obj.get("op")?.as_str()?;
            let args = obj.get("args")?.as_array()?;
            let ev = |i: usize| args.get(i).and_then(|a| eval(a, env));
            match op {
                "eq" => Some(Term::Bool(ev(0)? == ev(1)?)),
                "neq" => Some(Term::Bool(ev(0)? != ev(1)?)),
                "not" => match ev(0)? {
                    Term::Bool(b) => Some(Term::Bool(!b)),
                    _ => None,
                },
                "and" => bin_bool(&ev(0)?, &ev(1)?, |a, b| a && b),
                "or" => bin_bool(&ev(0)?, &ev(1)?, |a, b| a || b),
                "implies" => bin_bool(&ev(0)?, &ev(1)?, |a, b| !a || b),
                "iff" => bin_bool(&ev(0)?, &ev(1)?, |a, b| a == b),
                "lt" | "le" | "gt" | "ge" => num_cmp(op, &ev(0)?, &ev(1)?),
                "add" | "sub" | "mul" | "div" | "mod" => num_arith(op, &ev(0)?, &ev(1)?),
                "neg" => match ev(0)? {
                    Term::Int(i) => Some(Term::Int(i.checked_neg()?)),
                    Term::Float(f) => Some(Term::Float(-f)),
                    _ => None,
                },
                "length" => match ev(0)? {
                    Term::List(xs) => Some(Term::Int(xs.len() as i128)),
                    Term::Str(s) => Some(Term::Int(s.chars().count() as i128)),
                    _ => None,
                },
                "head" => match ev(0)? {
                    Term::List(xs) => xs.first().cloned(),
                    _ => None,
                },
                "tail" => match ev(0)? {
                    Term::List(xs) if !xs.is_empty() => Some(Term::List(xs[1..].to_vec())),
                    _ => None,
                },
                "nil" => Some(Term::List(Vec::new())),
                "cons" => match ev(1)? {
                    Term::List(mut xs) => {
                        let mut v = vec![ev(0)?];
                        v.append(&mut xs);
                        Some(Term::List(v))
                    }
                    _ => None,
                },
                "id" => ev(0),
                // Need re-application of an unknown function — undecidable from static examples.
                "map" | "filter" | "foldl" | "foldr" | "compose" => None,
                _ => None, // content-address ref / scope var
            }
        }
        // A single example provides no domain to range over.
        "forall" | "exists" => None,
        _ => None,
    }
}

fn bindings(example: &Value) -> BTreeMap<String, Term> {
    let mut env = BTreeMap::new();
    if let Some(t) = example.get("result").and_then(value_to_term) {
        env.insert("result".to_string(), t);
    }
    if let Some(args) = example.get("args").and_then(|v| v.as_array()) {
        for (i, a) in args.iter().enumerate() {
            if let Some(t) = value_to_term(a) {
                env.insert(format!("arg{i}"), t);
            }
        }
    }
    env
}

/// Verdict for one property across all of a record's examples.
#[derive(Debug, PartialEq)]
pub enum Verdict {
    Contradicted,
    Unverifiable,
    Consistent,
}

/// Evaluate one property predicate against a set of examples.
pub fn evaluate_property(expr: &Value, examples: &[Value]) -> Verdict {
    let mut any_true = false;
    let mut any_false = false;
    for ex in examples {
        match eval(expr, &bindings(ex)) {
            Some(Term::Bool(true)) => any_true = true,
            Some(Term::Bool(false)) => any_false = true,
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

/// Check every `properties[]` entry of a function record against its `examples[]`. Prints a
/// per-property verdict to stdout; returns Err iff any property is CONTRADICTED by an example.
pub fn check_properties(record: &Value) -> Result<()> {
    let props = record
        .get("properties")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let examples = record
        .get("examples")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if props.is_empty() {
        println!("no properties to check");
        return Ok(());
    }
    let mut contradicted = Vec::new();
    for prop in &props {
        let name = prop.get("name").and_then(|v| v.as_str()).unwrap_or("<unnamed>");
        let expr = prop
            .get("expr")
            .ok_or_else(|| anyhow!("property `{name}` missing `expr`"))?;
        let verdict = evaluate_property(expr, &examples);
        let label = match verdict {
            Verdict::Contradicted => "CONTRADICTED",
            Verdict::Unverifiable => "UNVERIFIABLE",
            Verdict::Consistent => "CONSISTENT",
        };
        println!("{name}: {label}");
        if verdict == Verdict::Contradicted {
            contradicted.push(name.to_string());
        }
    }
    if contradicted.is_empty() {
        Ok(())
    } else {
        Err(anyhow!(
            "properties contradicted by worked examples: {}",
            contradicted.join(", ")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn int(n: i64) -> Value {
        json!({ "kind": "int", "value": n })
    }
    fn list(ns: &[i64]) -> Value {
        json!({ "kind": "list", "elems": ns.iter().map(|n| int(*n)).collect::<Vec<_>>() })
    }

    #[test]
    fn length_preservation_consistent_then_contradicted() {
        // eq(length(result), length(arg0))
        let expr = json!({ "kind": "app", "op": "eq", "args": [
            { "kind": "app", "op": "length", "args": [{ "kind": "var", "name": "result" }] },
            { "kind": "app", "op": "length", "args": [{ "kind": "var", "name": "arg0" }] }]});
        let good = vec![json!({ "args": [list(&[1, 2, 3])], "result": list(&[3, 2, 1]) })];
        assert_eq!(evaluate_property(&expr, &good), Verdict::Consistent);

        let bad = vec![json!({ "args": [list(&[1, 2, 3])], "result": list(&[1]) })];
        assert_eq!(evaluate_property(&expr, &bad), Verdict::Contradicted);
    }

    #[test]
    fn identity_law_decided_from_examples() {
        // eq(result, arg0)
        let expr = json!({ "kind": "app", "op": "eq", "args": [
            { "kind": "var", "name": "result" }, { "kind": "var", "name": "arg0" }]});
        let ex = vec![json!({ "args": [int(7)], "result": int(7) })];
        assert_eq!(evaluate_property(&expr, &ex), Verdict::Consistent);
    }

    #[test]
    fn functor_composition_is_unverifiable() {
        // map(...) needs re-applying an unknown function -> UNVERIFIABLE, never silently consistent.
        let expr = json!({ "kind": "app", "op": "eq", "args": [
            { "kind": "app", "op": "map", "args": [{ "kind": "var", "name": "f" }, { "kind": "var", "name": "arg0" }] },
            { "kind": "var", "name": "result" }]});
        let ex = vec![json!({ "args": [list(&[1, 2])], "result": list(&[1, 2]) })];
        assert_eq!(evaluate_property(&expr, &ex), Verdict::Unverifiable);
    }

    #[test]
    fn unbound_variable_is_unverifiable() {
        let expr = json!({ "kind": "app", "op": "eq", "args": [
            { "kind": "var", "name": "result" }, { "kind": "var", "name": "nonesuch" }]});
        let ex = vec![json!({ "args": [int(1)], "result": int(1) })];
        assert_eq!(evaluate_property(&expr, &ex), Verdict::Unverifiable);
    }
}
