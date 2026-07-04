//! SMT proof backend — the rung above *bounded* property checking. `check-properties` decides a
//! `forall` law by ranging its variables over the worked examples (example-bound), or by sampling /
//! bounded-exhaustive enumeration (`--generate`, proptest.rs). All three are finite: they can refute a
//! law (a counterexample) but can only ever say "no counterexample *in the range I tried*". This module
//! discharges the obligation **over the unbounded domain**: it translates a property and the function
//! body into SMT-LIB 2, asserts the *negation* of the universally-quantified law, and asks a solver
//! whether that is satisfiable. `unsat` means no counterexample exists *anywhere* — a real proof; `sat`
//! yields a concrete counterexample; `unknown` is an honest "the solver gave up".
//!
//! The emitted SMT-LIB script **is the machine-checkable certificate** (spec/evaluation.md): any
//! SMT solver re-checks it independently, so a receiver verifies by re-checking the certificate rather
//! than trusting this tool — verification is re-execution, lifted to proof (principles 3, 5).
//!
//! Decidable fragment (honest scope). Sorts are `Int`, `Bool`, and `String` (the solver's native
//! string theory — spec/expressiveness.md phase 1 gets the same proof reach as arithmetic); the
//! function under test (`self`) is inlined as an SMT `define-fun` whose parameter sorts are inferred
//! from body usage (default `Int`). Supported: arithmetic (`add`/`sub`/`mul`/`neg`/`abs`/`min`/`max`/
//! `mod`/`div`), comparisons, boolean connectives, the string operations `str_concat` (`str.++`),
//! `str_length` (`str.len`), and `str_contains` (`str.contains` — note the needle/haystack argument
//! swap), string literals and string equality, `let`, boolean `case` (→ `ite`), literals, and `self`
//! applied to arguments. Anything outside it — lists, `str_split`/`str_join` (sequences-of-strings,
//! not in the solver's string theory), `to_string`/`parse_int` (solver `str.from_int`/`str.to_int`
//! semantics differ from ours on negatives — mapping them would be UNSOUND, so they stay out),
//! maps/JSON, higher-order arguments, recursion, opaque callees — is reported UNSUPPORTED rather than
//! silently "proved". This is the same boundary proptest draws (it reports those UNGENERATABLE).

use anyhow::{anyhow, bail, Result};
use serde_json::Value as J;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sort {
    Int,
    Bool,
    Str,
}

impl Sort {
    fn smt(self) -> &'static str {
        match self {
            Sort::Int => "Int",
            Sort::Bool => "Bool",
            Sort::Str => "String",
        }
    }
}

/// The result of attempting to prove one property.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProofOutcome {
    /// The solver returned `unsat` for the negation: the law holds for all inputs in the domain.
    Proved,
    /// The solver returned `sat`: a counterexample exists (the solver's model, lightly cleaned).
    Refuted(String),
    /// The solver returned `unknown`.
    Unknown,
    /// No solver binary was found; the certificate was still produced (re-checkable elsewhere).
    NoSolver,
    /// The property or body is outside the decidable fragment — not attempted (reason given).
    Unsupported(String),
}

/// A proof obligation: the SMT-LIB script (the certificate) plus a note on what it encodes.
pub struct Certificate {
    pub smt: String,
    pub quantified: Vec<(String, Sort)>,
    pub uses_self: bool,
}

// --- self definition ------------------------------------------------------------------------------

/// The function under test, lowered to an SMT `define-fun`. Parameter sorts are inferred from how
/// the body uses each parameter (string ops → `String`, boolean connectives → `Bool`), defaulting
/// to `Int` — the sorts then also constrain the law's `self`-application arguments.
struct SelfDef {
    params: Vec<String>,
    param_sorts: Vec<Sort>,
    ret: Sort,
    body_smt: String,
}

fn lower_self(body: &J) -> Result<SelfDef> {
    // A body is `\p1 .. pn -> expr` (or a bare 0-ary expr).
    let (params, inner): (Vec<String>, &J) = if body.get("kind").and_then(|k| k.as_str()) == Some("lambda") {
        let ps = body
            .get("params")
            .and_then(|p| p.as_array())
            .ok_or_else(|| anyhow!("lambda has no params"))?
            .iter()
            .map(|p| p.get("name").and_then(|n| n.as_str()).map(String::from).ok_or_else(|| anyhow!("param has no name")))
            .collect::<Result<Vec<_>>>()?;
        (ps, body.get("body").ok_or_else(|| anyhow!("lambda has no body"))?)
    } else {
        (vec![], body)
    };
    // Infer parameter sorts from body usage (the same walk the law-side inference uses — it reads
    // both the op and fn/var application spellings), defaulting to Int.
    let mut inferred: BTreeMap<String, Option<Sort>> = params.iter().map(|p| (p.clone(), None)).collect();
    visit_for_sorts(inner, &params, &mut inferred, None)?;
    let param_sorts: Vec<Sort> = params.iter().map(|p| inferred[p].unwrap_or(Sort::Int)).collect();
    let mut env: BTreeMap<String, Sort> = params.iter().cloned().zip(param_sorts.iter().copied()).collect();
    let ret = sort_of(inner, &env, None)?;
    let body_smt = lower(inner, &mut env, None)?;
    Ok(SelfDef { params, param_sorts, ret, body_smt })
}

// --- translation ----------------------------------------------------------------------------------

/// Rewrite curried direct-application spines — the surface parser's juxtaposition form,
/// `((f a) b)` — into flat applications `f(a, b)`, so the translator sees one head with all its
/// arguments (the same normalization the termination/complexity analyzers apply). Purely
/// structural; `apply`-spines and everything else pass through untouched. Children are rewritten
/// first, so one flattening step per node suffices (the loop is defensive).
fn uncurry(node: &J) -> J {
    match node {
        J::Object(m) => {
            let mut out = serde_json::Map::new();
            for (k, v) in m {
                out.insert(k.clone(), uncurry(v));
            }
            let mut node = J::Object(out);
            loop {
                let is_curried = node.get("kind").and_then(|k| k.as_str()) == Some("app")
                    && node.get("op").is_none()
                    && node.pointer("/fn/kind").and_then(|k| k.as_str()) == Some("app")
                    && node.pointer("/fn/op").is_none();
                if !is_curried {
                    break;
                }
                let inner_fn = node.pointer("/fn/fn").cloned().unwrap_or(J::Null);
                let mut args = node.pointer("/fn/args").and_then(|a| a.as_array()).cloned().unwrap_or_default();
                args.extend(node.get("args").and_then(|a| a.as_array()).cloned().unwrap_or_default());
                node = serde_json::json!({ "kind": "app", "fn": inner_fn, "args": args });
            }
            node
        }
        J::Array(items) => J::Array(items.iter().map(uncurry).collect()),
        other => other.clone(),
    }
}

/// Resolve the head operator of an application node: either an explicit `op` string (predicate form)
/// or a `fn` that is a `var` (body form).
fn head_op(node: &J) -> Option<String> {
    if let Some(op) = node.get("op").and_then(|o| o.as_str()) {
        return Some(op.to_string());
    }
    if node.pointer("/fn/kind").and_then(|k| k.as_str()) == Some("var") {
        return node.pointer("/fn/name").and_then(|n| n.as_str()).map(String::from);
    }
    None
}

/// Sort returned by `op` given the sorts of its operands are well-formed.
fn op_result_sort(op: &str) -> Option<Sort> {
    Some(match op {
        "add" | "sub" | "mul" | "neg" | "abs" | "min" | "max" | "mod" | "div" | "str_length" => Sort::Int,
        "eq" | "neq" | "lt" | "le" | "gt" | "ge" | "and" | "or" | "xor" | "not" | "str_contains" => Sort::Bool,
        "str_concat" => Sort::Str,
        _ => return None,
    })
}

/// Flatten an `apply` spine `apply(apply(self, a), b)` into `(self, [a, b])`, returning the head var
/// name and the argument nodes in source order. Also accepts a direct `self(args)` call.
fn flatten_call<'a>(node: &'a J) -> Option<(String, Vec<&'a J>)> {
    let op = head_op(node)?;
    if op != "apply" {
        // Direct call form: `self(args)` written with op/fn = self.
        let args: Vec<&J> = node.get("args").and_then(|a| a.as_array()).map(|a| a.iter().collect()).unwrap_or_default();
        return Some((op, args));
    }
    // `apply` form: args = [f, x]; recurse into f.
    let args = node.get("args").and_then(|a| a.as_array())?;
    if args.len() != 2 {
        return None;
    }
    let (head, mut collected) = flatten_call(&args[0]).or_else(|| {
        // base: f is a bare var
        if args[0].get("kind").and_then(|k| k.as_str()) == Some("var") {
            args[0].get("name").and_then(|n| n.as_str()).map(|n| (n.to_string(), vec![]))
        } else {
            None
        }
    })?;
    collected.push(&args[1]);
    Some((head, collected))
}

fn sort_of(node: &J, env: &BTreeMap<String, Sort>, self_def: Option<&SelfDef>) -> Result<Sort> {
    let kind = node.get("kind").and_then(|k| k.as_str()).unwrap_or_default();
    match kind {
        "lit" => match node.pointer("/value/kind").and_then(|k| k.as_str()) {
            Some("int") | Some("nat") => Ok(Sort::Int),
            Some("bool") => Ok(Sort::Bool),
            Some("string") => Ok(Sort::Str),
            other => bail!("unsupported literal kind: {other:?}"),
        },
        "var" => {
            let name = node.get("name").and_then(|n| n.as_str()).unwrap_or_default();
            env.get(name).copied().ok_or_else(|| anyhow!("free variable `{name}` (out of fragment)"))
        }
        "let" => {
            let mut e2 = env.clone();
            let name = node.get("name").and_then(|n| n.as_str()).ok_or_else(|| anyhow!("let has no name"))?;
            let vsort = sort_of(node.get("value").ok_or_else(|| anyhow!("let has no value"))?, env, self_def)?;
            e2.insert(name.to_string(), vsort);
            sort_of(node.get("body").ok_or_else(|| anyhow!("let has no body"))?, &e2, self_def)
        }
        "case" => {
            // Sort of the first arm's body (all arms share a sort if well-formed).
            let arms = node.get("arms").and_then(|a| a.as_array()).ok_or_else(|| anyhow!("case has no arms"))?;
            let arm = arms.first().ok_or_else(|| anyhow!("case has no arms"))?;
            sort_of(arm.get("body").ok_or_else(|| anyhow!("arm has no body"))?, env, self_def)
        }
        "app" => {
            let op = head_op(node).ok_or_else(|| anyhow!("application with no resolvable head"))?;
            if op == "id" {
                return sort_of(&node.get("args").and_then(|a| a.as_array()).and_then(|a| a.first()).cloned()
                    .ok_or_else(|| anyhow!("id needs an arg"))?, env, self_def);
            }
            if let Some(s) = op_result_sort(&op) {
                return Ok(s);
            }
            // self / apply-of-self
            if let Some((head, _)) = flatten_call(node) {
                if head == "self" {
                    return self_def.map(|s| s.ret).ok_or_else(|| anyhow!("`self` used but no body supplied"));
                }
            }
            bail!("unsupported operator `{op}` (out of fragment)")
        }
        other => bail!("unsupported expression kind `{other}` (out of fragment)"),
    }
}

/// Lower an integer/boolean/string literal value-AST to an SMT term.
fn lower_lit(value: &J) -> Result<String> {
    match value.get("kind").and_then(|k| k.as_str()) {
        Some("bool") => Ok(value.get("value").and_then(|v| v.as_bool()).unwrap_or(false).to_string()),
        Some("int") | Some("nat") => {
            let v = value.get("value").ok_or_else(|| anyhow!("literal has no value"))?;
            let n: i128 = if let Some(i) = v.as_i64() {
                i as i128
            } else if let Some(s) = v.as_str() {
                s.parse().map_err(|_| anyhow!("bad integer literal {s:?}"))?
            } else {
                bail!("unsupported integer literal {v}")
            };
            Ok(if n < 0 { format!("(- {})", -n) } else { n.to_string() })
        }
        Some("string") => {
            let s = value.get("value").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("string literal has no value"))?;
            Ok(smt_string_lit(s))
        }
        other => bail!("unsupported literal kind: {other:?}"),
    }
}

/// An SMT-LIB 2.6 string literal: `"` doubles, and anything outside printable ASCII is emitted as a
/// `\u{...}` escape so the certificate stays 7-bit clean and unambiguous.
fn smt_string_lit(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\"\""),
            c if (' '..='~').contains(&c) && c != '\\' => out.push(c),
            c => out.push_str(&format!("\\u{{{:x}}}", c as u32)),
        }
    }
    out.push('"');
    out
}

fn lower(node: &J, env: &mut BTreeMap<String, Sort>, self_def: Option<&SelfDef>) -> Result<String> {
    let kind = node.get("kind").and_then(|k| k.as_str()).unwrap_or_default();
    match kind {
        "lit" => lower_lit(node.get("value").ok_or_else(|| anyhow!("lit has no value"))?),
        "var" => {
            let name = node.get("name").and_then(|n| n.as_str()).unwrap_or_default();
            if env.contains_key(name) {
                Ok(name.to_string())
            } else {
                bail!("free variable `{name}` (out of fragment)")
            }
        }
        "let" => {
            let name = node.get("name").and_then(|n| n.as_str()).ok_or_else(|| anyhow!("let has no name"))?.to_string();
            let value = node.get("value").ok_or_else(|| anyhow!("let has no value"))?;
            let vsort = sort_of(value, env, self_def)?;
            let vsmt = lower(value, env, self_def)?;
            let mut e2 = env.clone();
            e2.insert(name.clone(), vsort);
            let body = lower(node.get("body").ok_or_else(|| anyhow!("let has no body"))?, &mut e2, self_def)?;
            Ok(format!("(let (({name} {vsmt})) {body})"))
        }
        "case" => lower_case(node, env, self_def),
        "app" => lower_app(node, env, self_def),
        other => bail!("unsupported expression kind `{other}` (out of fragment)"),
    }
}

/// Lower a `case` to nested `ite`. Supported: a boolean scrutinee with `true`/`false` literal arms, or
/// any scrutinee with literal-pattern arms plus a trailing wildcard/bind default. Otherwise UNSUPPORTED.
fn lower_case(node: &J, env: &mut BTreeMap<String, Sort>, self_def: Option<&SelfDef>) -> Result<String> {
    let scrut = node.get("scrutinee").ok_or_else(|| anyhow!("case has no scrutinee"))?;
    let ssort = sort_of(scrut, env, self_def)?;
    let scrut_smt = lower(scrut, env, self_def)?;
    let arms = node.get("arms").and_then(|a| a.as_array()).ok_or_else(|| anyhow!("case has no arms"))?;

    // Find a default (wildcard or bind) arm; the rest must carry literal patterns.
    let mut default: Option<String> = None;
    let mut lit_arms: Vec<(String, &J)> = Vec::new(); // (lit-smt, body)
    for arm in arms {
        let pat = arm.get("pattern").ok_or_else(|| anyhow!("arm has no pattern"))?;
        let body = arm.get("body").ok_or_else(|| anyhow!("arm has no body"))?;
        match pat.get("kind").and_then(|k| k.as_str()) {
            Some("wildcard") => default = Some(lower(body, env, self_def)?),
            Some("bind") => {
                // Bind the scrutinee to the name within the arm body.
                let name = pat.get("name").and_then(|n| n.as_str()).ok_or_else(|| anyhow!("bind has no name"))?;
                let mut e2 = env.clone();
                e2.insert(name.to_string(), ssort);
                let b = lower(body, &mut e2, self_def)?;
                default = Some(format!("(let (({name} {scrut_smt})) {b})"));
            }
            Some("lit") => {
                let litv = lower_lit(pat.get("value").ok_or_else(|| anyhow!("lit pattern has no value"))?)?;
                lit_arms.push((litv, body));
            }
            other => bail!("unsupported case pattern `{other:?}` (out of fragment)"),
        }
    }

    // Establish the base (else) branch. With no explicit default, a boolean scrutinee whose arms cover
    // both true and false uses the matching `false` arm as the base; otherwise it is non-exhaustive.
    let base = if let Some(d) = default {
        d
    } else if ssort == Sort::Bool {
        // Require both true and false to be present.
        let has_false = lit_arms.iter().any(|(l, _)| l == "false");
        let has_true = lit_arms.iter().any(|(l, _)| l == "true");
        if !(has_false && has_true) {
            bail!("non-exhaustive boolean case (out of fragment)");
        }
        let (_, fbody) = lit_arms.iter().find(|(l, _)| l == "false").unwrap();
        let b = lower(fbody, env, self_def)?;
        lit_arms.retain(|(l, _)| l != "false");
        b
    } else {
        bail!("case without a wildcard/bind default (out of fragment)");
    };

    // Fold the remaining literal arms into nested ite, innermost last. For a boolean scrutinee the
    // condition collapses to the scrutinee itself (or its negation) rather than `(= scrut true)`.
    let mut acc = base;
    for (litv, body) in lit_arms.into_iter().rev() {
        let bsmt = lower(body, env, self_def)?;
        let cond = match (ssort, litv.as_str()) {
            (Sort::Bool, "true") => scrut_smt.clone(),
            (Sort::Bool, "false") => format!("(not {scrut_smt})"),
            _ => format!("(= {scrut_smt} {litv})"),
        };
        acc = format!("(ite {cond} {bsmt} {acc})");
    }
    Ok(acc)
}

fn lower_app(node: &J, env: &mut BTreeMap<String, Sort>, self_def: Option<&SelfDef>) -> Result<String> {
    let op = head_op(node).ok_or_else(|| anyhow!("application with no resolvable head"))?;
    let args: Vec<&J> = node.get("args").and_then(|a| a.as_array()).map(|a| a.iter().collect()).unwrap_or_default();
    let lower_args = |env: &mut BTreeMap<String, Sort>, xs: &[&J]| -> Result<Vec<String>> {
        xs.iter().map(|a| lower(a, env, self_def)).collect()
    };

    // self / apply-of-self.
    if op == "apply" || op == "self" {
        if let Some((head, call_args)) = flatten_call(node) {
            if head == "self" {
                let sd = self_def.ok_or_else(|| anyhow!("`self` used but no body supplied"))?;
                if call_args.len() != sd.params.len() {
                    bail!("self applied to {} args, expects {}", call_args.len(), sd.params.len());
                }
                let lowered: Vec<String> = call_args.iter().map(|a| lower(a, env, self_def)).collect::<Result<_>>()?;
                return Ok(format!("(self {})", lowered.join(" ")));
            }
            bail!("application of non-self function `{head}` (out of fragment)");
        }
    }

    match op.as_str() {
        "id" => lower(args.first().ok_or_else(|| anyhow!("id needs an arg"))?, env, self_def),
        "add" | "sub" | "mul" | "and" | "or" | "xor" => {
            let smt_op = match op.as_str() {
                "add" => "+",
                "sub" => "-",
                "mul" => "*",
                _ => op.as_str(), // and / or / xor are SMT keywords verbatim
            };
            Ok(format!("({} {})", smt_op, lower_args(env, &args)?.join(" ")))
        }
        "neg" => Ok(format!("(- {})", lower(args[0], env, self_def)?)),
        "not" => Ok(format!("(not {})", lower(args[0], env, self_def)?)),
        "abs" => Ok(format!("(abs {})", lower(args[0], env, self_def)?)),
        "mod" => Ok(format!("(mod {} {})", lower(args[0], env, self_def)?, lower(args[1], env, self_def)?)),
        "div" => Ok(format!("(div {} {})", lower(args[0], env, self_def)?, lower(args[1], env, self_def)?)),
        "eq" => Ok(format!("(= {} {})", lower(args[0], env, self_def)?, lower(args[1], env, self_def)?)),
        "neq" => Ok(format!("(not (= {} {}))", lower(args[0], env, self_def)?, lower(args[1], env, self_def)?)),
        "lt" => Ok(format!("(< {} {})", lower(args[0], env, self_def)?, lower(args[1], env, self_def)?)),
        "le" => Ok(format!("(<= {} {})", lower(args[0], env, self_def)?, lower(args[1], env, self_def)?)),
        "gt" => Ok(format!("(> {} {})", lower(args[0], env, self_def)?, lower(args[1], env, self_def)?)),
        "ge" => Ok(format!("(>= {} {})", lower(args[0], env, self_def)?, lower(args[1], env, self_def)?)),
        "min" => {
            let (a, b) = (lower(args[0], env, self_def)?, lower(args[1], env, self_def)?);
            Ok(format!("(ite (<= {a} {b}) {a} {b})"))
        }
        "max" => {
            let (a, b) = (lower(args[0], env, self_def)?, lower(args[1], env, self_def)?);
            Ok(format!("(ite (>= {a} {b}) {a} {b})"))
        }
        // String theory (spec/expressiveness.md phase 1). NB our `str_contains` is needle-first;
        // SMT-LIB's `str.contains` is haystack-first — the arguments swap.
        "str_concat" => Ok(format!("(str.++ {} {})", lower(args[0], env, self_def)?, lower(args[1], env, self_def)?)),
        "str_length" => Ok(format!("(str.len {})", lower(args[0], env, self_def)?)),
        "str_contains" => Ok(format!("(str.contains {} {})", lower(args[1], env, self_def)?, lower(args[0], env, self_def)?)),
        other => bail!("unsupported operator `{other}` (out of fragment)"),
    }
}

// --- quantified-variable sort inference -----------------------------------------------------------

/// Infer each quantified variable's sort (Int, Bool, or String) from how it is used in the predicate.
/// A var used under a boolean connective is Bool; under a string operation it is String; under
/// arithmetic/comparison it is Int; a `self` argument takes the corresponding inferred parameter sort;
/// unresolved defaults to Int. A var used in a list position makes the whole property unsupported.
fn infer_var_sorts(pred: &J, vars: &[String], self_param_sorts: Option<&[Sort]>) -> Result<BTreeMap<String, Sort>> {
    let mut sorts: BTreeMap<String, Option<Sort>> = vars.iter().map(|v| (v.clone(), None)).collect();
    visit_for_sorts(pred, &sorts.keys().cloned().collect::<Vec<_>>(), &mut sorts, self_param_sorts)?;
    Ok(sorts.into_iter().map(|(k, v)| (k, v.unwrap_or(Sort::Int))).collect())
}

/// The sort a node *evidently* produces without any variable context: a literal's kind, or a known
/// operator's result sort. `None` for vars and anything else — used by the `eq`/`neq` cross-side
/// inference (`eq(str_concat(a,b), c)` makes `c` a String).
fn evident_sort(node: &J) -> Option<Sort> {
    match node.get("kind").and_then(|k| k.as_str()) {
        Some("lit") => match node.pointer("/value/kind").and_then(|k| k.as_str()) {
            Some("int") | Some("nat") => Some(Sort::Int),
            Some("bool") => Some(Sort::Bool),
            Some("string") => Some(Sort::Str),
            _ => None,
        },
        Some("app") => op_result_sort(&head_op(node)?),
        _ => None,
    }
}

fn var_name(node: &J) -> Option<&str> {
    if node.get("kind").and_then(|k| k.as_str()) == Some("var") {
        node.get("name").and_then(|n| n.as_str())
    } else {
        None
    }
}

fn constrain(name: &str, sort: Sort, vars: &[String], sorts: &mut BTreeMap<String, Option<Sort>>) -> Result<()> {
    if vars.iter().any(|v| v == name) {
        match sorts.get(name).copied().flatten() {
            Some(existing) if existing != sort => bail!("variable `{name}` used at conflicting sorts"),
            _ => {
                sorts.insert(name.to_string(), Some(sort));
            }
        }
    }
    Ok(())
}

fn visit_for_sorts(node: &J, vars: &[String], sorts: &mut BTreeMap<String, Option<Sort>>, self_param_sorts: Option<&[Sort]>) -> Result<()> {
    let kind = node.get("kind").and_then(|k| k.as_str()).unwrap_or_default();
    if kind == "app" {
        let op = head_op(node).unwrap_or_default();
        let args: Vec<&J> = node.get("args").and_then(|a| a.as_array()).map(|a| a.iter().collect()).unwrap_or_default();
        // Operand sorts implied by the operator.
        let operand_sort = match op.as_str() {
            "add" | "sub" | "mul" | "neg" | "abs" | "min" | "max" | "mod" | "div" | "lt" | "le" | "gt" | "ge" => Some(Sort::Int),
            "and" | "or" | "xor" | "not" => Some(Sort::Bool),
            "str_concat" | "str_length" | "str_contains" => Some(Sort::Str),
            _ => None, // eq/neq/apply/self/id: don't constrain directly
        };
        if let Some(s) = operand_sort {
            for a in &args {
                if let Some(n) = var_name(a) {
                    constrain(n, s, vars, sorts)?;
                }
            }
        }
        // eq/neq don't fix a sort themselves, but if one side's sort is evident (a literal or a
        // known operator), a bare var on the other side takes it: `eq(str_concat(a,b), c)` → c:String.
        if matches!(op.as_str(), "eq" | "neq") && args.len() == 2 {
            for (me, other) in [(0usize, 1usize), (1, 0)] {
                if let (Some(n), Some(s)) = (var_name(args[me]), evident_sort(args[other])) {
                    constrain(n, s, vars, sorts)?;
                }
            }
        }
        // A var in a list/structural position is a hard error (out of fragment) — surfaced here so
        // the property reads UNSUPPORTED, not mis-sorted. str_concat/str_length/str_contains are IN
        // the fragment now (the solver's string theory); split/join and the int<->string conversions
        // stay out (no theory counterpart / unsound mapping).
        if matches!(op.as_str(), "length" | "head" | "tail" | "last" | "init" | "reverse" | "map" | "filter" | "foldl" | "foldr" | "cons" | "append" | "concat" | "null" | "fst" | "snd"
            | "str_split" | "str_join" | "to_string" | "parse_int"
            | "map_put" | "map_get" | "map_del" | "map_size" | "map_keys" | "parse_json" | "render_json") {
            bail!("predicate uses list/structural operator `{op}` (out of fragment)");
        }
        if op == "apply" || op == "self" {
            if let Some((head, call_args)) = flatten_call(node) {
                if head == "self" {
                    for (i, a) in call_args.iter().enumerate() {
                        if let Some(n) = var_name(a) {
                            // A self argument takes the function's inferred parameter sort (Int when
                            // the body isn't available — the historical default).
                            let s = self_param_sorts.and_then(|ps| ps.get(i)).copied().unwrap_or(Sort::Int);
                            constrain(n, s, vars, sorts)?;
                        }
                    }
                }
            }
        }
        for a in &args {
            visit_for_sorts(a, vars, sorts, self_param_sorts)?;
        }
    } else if let Some(arr) = node.get("args").and_then(|a| a.as_array()) {
        for a in arr {
            visit_for_sorts(a, vars, sorts, self_param_sorts)?;
        }
    }
    // Recurse into common subtrees regardless of kind.
    for key in ["body", "value", "scrutinee"] {
        if let Some(child) = node.get(key) {
            visit_for_sorts(child, vars, sorts, self_param_sorts)?;
        }
    }
    if let Some(arms) = node.get("arms").and_then(|a| a.as_array()) {
        for arm in arms {
            if let Some(b) = arm.get("body") {
                visit_for_sorts(b, vars, sorts, self_param_sorts)?;
            }
        }
    }
    Ok(())
}

// --- certificate + proving ------------------------------------------------------------------------

/// Build the SMT-LIB certificate for a single `forall` property. `body` (the function under test) is
/// required only if the property references `self`. Returns Err if the property is outside the
/// decidable fragment (the caller maps that to UNSUPPORTED).
pub fn build_certificate(prop_expr: &J, body: Option<&J>) -> Result<Certificate> {
    // Normalize away curried application spines first, so surface-parsed properties/bodies
    // (juxtaposition parses curried) analyze identically to AST-authored flat ones.
    let prop_expr = &uncurry(prop_expr);
    let body_flat = body.map(uncurry);
    let body = body_flat.as_ref();
    if prop_expr.get("kind").and_then(|k| k.as_str()) != Some("forall") {
        bail!("not a `forall` — no universally-quantified obligation to prove");
    }
    let vars: Vec<String> = prop_expr
        .get("vars")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default();
    if vars.is_empty() {
        bail!("`forall` has no variables");
    }
    let pred = prop_expr.get("body").ok_or_else(|| anyhow!("`forall` has no body"))?;

    // Does the predicate reference `self`? If so, we need the body.
    let uses_self = references_self(pred);
    let self_def = if uses_self {
        let b = body.ok_or_else(|| anyhow!("property references `self` but no body was supplied"))?;
        Some(lower_self(b)?)
    } else {
        None
    };

    let var_sorts = infer_var_sorts(pred, &vars, self_def.as_ref().map(|s| s.param_sorts.as_slice()))?;
    let mut env: BTreeMap<String, Sort> = var_sorts.clone();

    // The predicate must be a Bool.
    let psort = sort_of(pred, &env, self_def.as_ref())?;
    if psort != Sort::Bool {
        bail!("property body is not a boolean predicate");
    }
    let pred_smt = lower(pred, &mut env, self_def.as_ref())?;

    let mut out = String::new();
    out.push_str("(set-logic ALL)\n");
    if let Some(sd) = &self_def {
        let params = sd
            .params
            .iter()
            .zip(sd.param_sorts.iter())
            .map(|(p, s)| format!("({p} {})", s.smt()))
            .collect::<Vec<_>>()
            .join(" ");
        out.push_str(&format!("(define-fun self ({}) {} {})\n", params, sd.ret.smt(), sd.body_smt));
    }
    let quantified: Vec<(String, Sort)> = vars.iter().map(|v| (v.clone(), var_sorts[v])).collect();
    for (v, s) in &quantified {
        out.push_str(&format!("(declare-const {v} {})\n", s.smt()));
    }
    // Assert the negation of the law: unsat ⇒ the law holds for all inputs.
    out.push_str(&format!("(assert (not {pred_smt}))\n"));
    out.push_str("(check-sat)\n");
    out.push_str("(get-model)\n");

    Ok(Certificate { smt: out, quantified, uses_self })
}

fn references_self(node: &J) -> bool {
    if var_name(node) == Some("self") {
        return true;
    }
    if node.get("op").and_then(|o| o.as_str()) == Some("self") {
        return true;
    }
    for key in ["body", "value", "scrutinee", "fn"] {
        if let Some(c) = node.get(key) {
            if references_self(c) {
                return true;
            }
        }
    }
    if let Some(arr) = node.get("args").and_then(|a| a.as_array()) {
        if arr.iter().any(references_self) {
            return true;
        }
    }
    if let Some(arms) = node.get("arms").and_then(|a| a.as_array()) {
        if arms.iter().any(|a| a.get("body").map(references_self).unwrap_or(false)) {
            return true;
        }
    }
    false
}

/// A solver's verdict on one `(check-sat)` script.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SatAnswer {
    /// `unsat` — the asserted constraints are unsatisfiable.
    Unsat,
    /// `sat` — satisfiable, with the model text that followed (if any).
    Sat(String),
    /// `unknown`, or the solver hit its time limit (recursive defs + quantifiers can make a query
    /// non-terminating; we bound it so an undecidable query reports `unknown` rather than hanging).
    Unknown,
    /// The solver binary was not found on PATH.
    NoSolver,
}

// Real proofs in our fragment return in well under a second; only a genuinely stuck (undecidable, or
// lemma-needing) query runs to the limit. Lemma discovery issues many speculative queries, so a tight
// budget keeps the whole search responsive while never changing a decidable query's verdict.
const SOLVER_TIMEOUT_SECS: u64 = 5;

/// Run an SMT-LIB 2 script through `solver` (reading from stdin via `-in`), bounded by a wall-clock
/// timeout so an undecidable query becomes `Unknown` instead of hanging. z3's own `-t:` soft limit is
/// passed too (it returns `unknown` cleanly); the process kill is the backstop for any solver.
pub fn run_smt(script: &str, solver: &str) -> Result<SatAnswer> {
    run_smt_secs(script, solver, SOLVER_TIMEOUT_SECS)
}

/// Like [`run_smt`], but with an explicit per-check timeout in seconds. Exploratory search (trying many
/// candidate lemma subsets) uses a short budget — a successful list-law proof closes in well under a
/// second, so a failing subset needn't burn the full default timeout.
pub fn run_smt_secs(script: &str, solver: &str, secs: u64) -> Result<SatAnswer> {
    use std::io::{Read, Write};
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    let mut cmd = Command::new(solver);
    if solver == "z3" || solver.ends_with("/z3") {
        cmd.arg(format!("-t:{}", secs * 1000)); // per-check soft timeout (ms)
    }
    let mut child = match cmd
        .arg("-in")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(SatAnswer::NoSolver),
        Err(e) => return Err(anyhow!("spawning solver `{solver}`: {e}")),
    };
    child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("solver stdin unavailable"))?
        .write_all(script.as_bytes())
        .map_err(|e| anyhow!("writing to solver: {e}"))?; // dropping the handle closes stdin (EOF)

    // Poll for completion, killing the process if it overruns the timeout backstop.
    let deadline = Instant::now() + Duration::from_secs(secs + 5);
    loop {
        match child.try_wait().map_err(|e| anyhow!("waiting on solver: {e}"))? {
            Some(_) => break,
            None => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Ok(SatAnswer::Unknown); // timed out ⇒ undecided
                }
                std::thread::sleep(Duration::from_millis(40));
            }
        }
    }
    let mut stdout = String::new();
    if let Some(mut out) = child.stdout.take() {
        let _ = out.read_to_string(&mut stdout);
    }
    match stdout.split_whitespace().next().unwrap_or("") {
        "unsat" => Ok(SatAnswer::Unsat),
        "sat" => {
            let model = stdout.splitn(2, '\n').nth(1).unwrap_or("").split_whitespace().collect::<Vec<_>>().join(" ");
            Ok(SatAnswer::Sat(model))
        }
        "unknown" | "timeout" | "" => Ok(SatAnswer::Unknown),
        _ => Err(anyhow!("unexpected solver output: {}", stdout.trim())),
    }
}

/// Run a solver on a certificate, mapping the SAT answer to a proof outcome.
pub fn run_solver(cert: &Certificate, solver: &str) -> Result<ProofOutcome> {
    Ok(match run_smt(&cert.smt, solver)? {
        SatAnswer::Unsat => ProofOutcome::Proved,
        SatAnswer::Sat(model) => ProofOutcome::Refuted(model),
        SatAnswer::Unknown => ProofOutcome::Unknown,
        SatAnswer::NoSolver => ProofOutcome::NoSolver,
    })
}

/// Attempt to prove one property: build the certificate, run the solver. Out-of-fragment properties
/// yield `Unsupported` (with the certificate left `None`).
pub fn prove_property(prop_expr: &J, body: Option<&J>, solver: &str) -> (ProofOutcome, Option<Certificate>) {
    match build_certificate(prop_expr, body) {
        Err(e) => (ProofOutcome::Unsupported(format!("{e:#}")), None),
        Ok(cert) => match run_solver(&cert, solver) {
            Ok(outcome) => (outcome, Some(cert)),
            Err(e) => (ProofOutcome::Unsupported(format!("solver error: {e:#}")), Some(cert)),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn double_body() -> J {
        json!({ "kind": "lambda", "params": [{ "name": "n" }], "body": {
            "kind": "app", "op": "add", "args": [{ "kind": "var", "name": "n" }, { "kind": "var", "name": "n" }] } })
    }

    #[test]
    fn certificate_for_doubling_law_is_well_formed() {
        // forall n. eq(self(n), add(n, n))
        let prop = json!({ "kind": "forall", "vars": ["n"], "body": {
            "kind": "app", "op": "eq", "args": [
                { "kind": "app", "op": "apply", "args": [{ "kind": "var", "name": "self" }, { "kind": "var", "name": "n" }] },
                { "kind": "app", "op": "add", "args": [{ "kind": "var", "name": "n" }, { "kind": "var", "name": "n" }] }] } });
        let cert = build_certificate(&prop, Some(&double_body())).unwrap();
        assert!(cert.uses_self);
        assert!(cert.smt.contains("(define-fun self ((n Int)) Int (+ n n))"));
        assert!(cert.smt.contains("(declare-const n Int)"));
        assert!(cert.smt.contains("(assert (not (= (self n) (+ n n))))"));
        assert!(cert.smt.contains("(check-sat)"));
    }

    #[test]
    fn four_int_commutativity_certificate_has_no_self() {
        // forall a b c d. eq(add(add(add(a,b),c),d), add(add(add(d,c),b),a)) — exceeds bounded checks.
        let chain = |w: &str, x: &str, y: &str, z: &str| json!({ "kind": "app", "op": "add", "args": [
            { "kind": "app", "op": "add", "args": [
                { "kind": "app", "op": "add", "args": [{ "kind": "var", "name": w }, { "kind": "var", "name": x }] },
                { "kind": "var", "name": y }] },
            { "kind": "var", "name": z }] });
        let prop = json!({ "kind": "forall", "vars": ["a", "b", "c", "d"], "body": {
            "kind": "app", "op": "eq", "args": [chain("a", "b", "c", "d"), chain("d", "c", "b", "a")] } });
        let cert = build_certificate(&prop, None).unwrap();
        assert!(!cert.uses_self);
        assert_eq!(cert.quantified.len(), 4);
        assert!(cert.quantified.iter().all(|(_, s)| *s == Sort::Int));
    }

    #[test]
    fn boolean_double_negation_infers_bool_sort() {
        // forall b. eq(not(not(b)), b)
        let prop = json!({ "kind": "forall", "vars": ["b"], "body": {
            "kind": "app", "op": "eq", "args": [
                { "kind": "app", "op": "not", "args": [{ "kind": "app", "op": "not", "args": [{ "kind": "var", "name": "b" }] }] },
                { "kind": "var", "name": "b" }] } });
        let cert = build_certificate(&prop, None).unwrap();
        assert_eq!(cert.quantified, vec![("b".to_string(), Sort::Bool)]);
        assert!(cert.smt.contains("(declare-const b Bool)"));
        assert!(cert.smt.contains("(not (not b))"));
    }

    #[test]
    fn string_length_of_concat_law_proves() {
        // forall a b. eq(str_length(str_concat(a, b)), add(str_length(a), str_length(b))) — the
        // distributivity of length over concatenation, PROVED over every string via the solver's
        // string theory (spec/expressiveness.md phase 1 reaching "verified by default" parity).
        let sl = |v: &str| json!({ "kind": "app", "op": "str_length", "args": [{ "kind": "var", "name": v }] });
        let prop = json!({ "kind": "forall", "vars": ["a", "b"], "body": {
            "kind": "app", "op": "eq", "args": [
                { "kind": "app", "op": "str_length", "args": [
                    { "kind": "app", "op": "str_concat", "args": [{ "kind": "var", "name": "a" }, { "kind": "var", "name": "b" }] }] },
                { "kind": "app", "op": "add", "args": [sl("a"), sl("b")] }] } });
        let cert = build_certificate(&prop, None).unwrap();
        assert!(cert.quantified.iter().all(|(_, s)| *s == Sort::Str), "{:?}", cert.quantified);
        assert!(cert.smt.contains("(declare-const a String)"));
        assert!(cert.smt.contains("(str.len (str.++ a b))"));
        assert_eq!(run_smt(&cert.smt, "z3").unwrap(), SatAnswer::Unsat, "\n{}", cert.smt);
    }

    #[test]
    fn string_law_refuted_with_counterexample() {
        // forall s. gt(str_length(s), 0) — false: the empty string. The solver refutes with a model.
        let prop = json!({ "kind": "forall", "vars": ["s"], "body": {
            "kind": "app", "op": "gt", "args": [
                { "kind": "app", "op": "str_length", "args": [{ "kind": "var", "name": "s" }] },
                { "kind": "lit", "value": { "kind": "int", "value": 0 } }] } });
        let cert = build_certificate(&prop, None).unwrap();
        assert!(matches!(run_smt(&cert.smt, "z3").unwrap(), SatAnswer::Sat(_)), "\n{}", cert.smt);
    }

    #[test]
    fn contains_swaps_needle_and_haystack() {
        // Our str_contains is needle-first; SMT's str.contains is haystack-first. The law
        // forall a b. str_contains(a, str_concat(a, b)) — "a prefix is contained" — only proves if
        // the swap is right (with the arguments backwards it is refutable, e.g. a="x", b="y").
        let prop = json!({ "kind": "forall", "vars": ["a", "b"], "body": {
            "kind": "app", "op": "str_contains", "args": [
                { "kind": "var", "name": "a" },
                { "kind": "app", "op": "str_concat", "args": [{ "kind": "var", "name": "a" }, { "kind": "var", "name": "b" }] }] } });
        let cert = build_certificate(&prop, None).unwrap();
        assert!(cert.smt.contains("(str.contains (str.++ a b) a)"), "\n{}", cert.smt);
        assert_eq!(run_smt(&cert.smt, "z3").unwrap(), SatAnswer::Unsat, "\n{}", cert.smt);
    }

    #[test]
    fn string_self_body_infers_string_params() {
        // self = \s -> str_concat(s, "!") ; forall s. eq(str_length(self(s)), add(str_length(s), 1)).
        // The parameter sort is inferred String from body usage, the define-fun and the quantified
        // var follow, and the law PROVES (the appended "!" adds exactly one character).
        let body = json!({ "kind": "lambda", "params": [{ "name": "s" }], "body": {
            "kind": "app", "fn": { "kind": "var", "name": "str_concat" },
            "args": [{ "kind": "var", "name": "s" }, { "kind": "lit", "value": { "kind": "string", "value": "!" } }] } });
        let prop = json!({ "kind": "forall", "vars": ["s"], "body": {
            "kind": "app", "op": "eq", "args": [
                { "kind": "app", "op": "str_length", "args": [
                    { "kind": "app", "op": "apply", "args": [{ "kind": "var", "name": "self" }, { "kind": "var", "name": "s" }] }] },
                { "kind": "app", "op": "add", "args": [
                    { "kind": "app", "op": "str_length", "args": [{ "kind": "var", "name": "s" }] },
                    { "kind": "lit", "value": { "kind": "int", "value": 1 } }] }] } });
        let cert = build_certificate(&prop, Some(&body)).unwrap();
        assert!(cert.smt.contains("(define-fun self ((s String)) String"), "\n{}", cert.smt);
        assert!(cert.smt.contains("(declare-const s String)"), "\n{}", cert.smt);
        assert_eq!(run_smt(&cert.smt, "z3").unwrap(), SatAnswer::Unsat, "\n{}", cert.smt);
    }

    #[test]
    fn smt_string_literals_escape_correctly() {
        assert_eq!(smt_string_lit("plain"), "\"plain\"");
        assert_eq!(smt_string_lit("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(smt_string_lit("a\\b"), "\"a\\u{5c}b\"");
        assert_eq!(smt_string_lit("nl\nend"), "\"nl\\u{a}end\"");
    }

    #[test]
    fn split_and_conversions_stay_unsupported() {
        // str_split has no counterpart in the solver's string theory, and to_string/parse_int
        // deliberately don't map (str.from_int/str.to_int differ on negatives) — all UNSUPPORTED.
        for op in ["str_split", "str_join", "to_string", "parse_int"] {
            let prop = json!({ "kind": "forall", "vars": ["s"], "body": {
                "kind": "app", "op": "eq", "args": [
                    { "kind": "app", "op": op, "args": [{ "kind": "var", "name": "s" }] },
                    { "kind": "var", "name": "s" }] } });
            assert!(build_certificate(&prop, None).is_err(), "{op} must stay out of fragment");
        }
    }

    #[test]
    fn list_property_is_unsupported() {
        // forall xs. eq(reverse(reverse(xs)), xs) — out of the Int/Bool fragment.
        let prop = json!({ "kind": "forall", "vars": ["xs"], "body": {
            "kind": "app", "op": "eq", "args": [
                { "kind": "app", "op": "reverse", "args": [
                    { "kind": "app", "op": "reverse", "args": [{ "kind": "var", "name": "xs" }] }] },
                { "kind": "var", "name": "xs" }] } });
        assert!(build_certificate(&prop, None).is_err(), "list properties must be reported unsupported");
    }

    #[test]
    fn case_lowers_to_ite() {
        // self = \n -> case n>0 of true -> n | false -> neg(n)   (i.e. abs), property forall n. ge(self(n),0)
        let body = json!({ "kind": "lambda", "params": [{ "name": "n" }], "body": {
            "kind": "case",
            "scrutinee": { "kind": "app", "op": "gt", "args": [{ "kind": "var", "name": "n" }, { "kind": "lit", "value": { "kind": "int", "value": 0 } }] },
            "arms": [
                { "pattern": { "kind": "lit", "value": { "kind": "bool", "value": true } }, "body": { "kind": "var", "name": "n" } },
                { "pattern": { "kind": "lit", "value": { "kind": "bool", "value": false } }, "body": { "kind": "app", "op": "neg", "args": [{ "kind": "var", "name": "n" }] } }] } });
        let prop = json!({ "kind": "forall", "vars": ["n"], "body": {
            "kind": "app", "op": "ge", "args": [
                { "kind": "app", "op": "apply", "args": [{ "kind": "var", "name": "self" }, { "kind": "var", "name": "n" }] },
                { "kind": "lit", "value": { "kind": "int", "value": 0 } }] } });
        let cert = build_certificate(&prop, Some(&body)).unwrap();
        assert!(cert.smt.contains("(ite (> n 0) n (- n))"), "got: {}", cert.smt);
    }

    // Solver-backed tests: only run when a solver is on PATH (CI without one still passes).
    fn solver() -> Option<&'static str> {
        for s in ["z3", "cvc5"] {
            if std::process::Command::new(s).arg("--version").output().map(|o| o.status.success()).unwrap_or(false) {
                return Some(s);
            }
        }
        None
    }

    #[test]
    fn solver_proves_and_refutes_when_available() {
        let Some(solver) = solver() else {
            eprintln!("no SMT solver on PATH — skipping solver-backed proof test");
            return;
        };
        // PROVED: forall n. eq(self(n), add(n,n)) for double.
        let proved = json!({ "kind": "forall", "vars": ["n"], "body": {
            "kind": "app", "op": "eq", "args": [
                { "kind": "app", "op": "apply", "args": [{ "kind": "var", "name": "self" }, { "kind": "var", "name": "n" }] },
                { "kind": "app", "op": "add", "args": [{ "kind": "var", "name": "n" }, { "kind": "var", "name": "n" }] }] } });
        let (o, _) = prove_property(&proved, Some(&double_body()), solver);
        assert_eq!(o, ProofOutcome::Proved);

        // REFUTED: forall n. gt(self(n), n) — false at n = 0.
        let refuted = json!({ "kind": "forall", "vars": ["n"], "body": {
            "kind": "app", "op": "gt", "args": [
                { "kind": "app", "op": "apply", "args": [{ "kind": "var", "name": "self" }, { "kind": "var", "name": "n" }] },
                { "kind": "var", "name": "n" }] } });
        let (o2, _) = prove_property(&refuted, Some(&double_body()), solver);
        assert!(matches!(o2, ProofOutcome::Refuted(_)), "expected REFUTED, got {o2:?}");
    }
}
