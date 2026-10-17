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
use std::collections::{BTreeMap, BTreeSet};

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
    /// The obligation applied a linked callee under its certified contract. The callee is then an
    /// UNINTERPRETED function constrained only by that contract, so a satisfiable negation may be a
    /// spurious model: `run_solver` reports it UNKNOWN, never REFUTED.
    pub uses_callees: bool,
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
    // A body that refers to `self` is RECURSIVE: inlining it as a `define-fun` would be ill-formed
    // (the first-order fragment has no recursion). Say so — the historical failure surfaced from the
    // inner lowering as "`self` used but no body supplied", which misread a supplied body.
    if references_self(inner) {
        bail!(
            "body is recursive (refers to `self`): the first-order prover inlines non-recursive bodies only — \
             a law about a recursive function needs the inductive backend (structural list recursion) or a \
             domain-qualified equivalence claim (numeric induction)"
        );
    }
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

/// Resolve the head operator of an application node: either an explicit `op` string (predicate form),
/// a `fn` that is a `var` (body form), or a `fn` that is a `fn_ref` LITERAL (composition by content
/// address — the op is then the target's `fn_…` address, resolved against the callee-contract registry).
fn head_op(node: &J) -> Option<String> {
    if let Some(op) = node.get("op").and_then(|o| o.as_str()) {
        return Some(op.to_string());
    }
    if node.pointer("/fn/kind").and_then(|k| k.as_str()) == Some("var") {
        return node.pointer("/fn/name").and_then(|n| n.as_str()).map(String::from);
    }
    if node.pointer("/fn/kind").and_then(|k| k.as_str()) == Some("lit")
        && node.pointer("/fn/value/kind").and_then(|k| k.as_str()) == Some("fn_ref")
    {
        return node.pointer("/fn/value/target").and_then(|t| t.as_str()).map(String::from);
    }
    None
}

// --- callee contracts (modular verification) --------------------------------------------------------
//
// A body that applies another record by `fn_ref` is outside the first-order fragment (the callee's
// body is not inlined — it may be recursive, or large, or simply another artifact). Over a LINKED
// view, the callee becomes an UNINTERPRETED function whose CERTIFIED contract is an axiom: for every
// postcondition the callee's own `check-refinement` proves (re-derived on the same view, never read
// from its declaration), `∀ params. (pre ∧ nat-typed params ≥ 0) ⇒ post[result := callee(params)]`.
// That is modular verification through content addresses: `lcm`'s `result ≥ 0` follows from `gcd`'s
// proved `result ≥ 1`, without re-proving `gcd`. Contracts are installed per proof by the caller
// (`set_callee_contracts`), thread-locally, and the certificate records which were used.

/// A linked callee's certified contract, as the prover uses it.
#[derive(Debug, Clone)]
pub struct CalleeContract {
    /// The SMT symbol standing for the callee (`callee_<address prefix>`).
    pub symbol: String,
    /// The callee's parameter names (the contract predicates range over them).
    pub params: Vec<String>,
    pub param_sorts: Vec<Sort>,
    pub ret: Sort,
    /// Declared preconditions (assumed in the axiom's antecedent).
    pub pres: Vec<J>,
    /// PROVED postconditions (the axiom's consequent, with `result` the callee's application).
    pub posts: Vec<J>,
    /// PROVED laws (`forall` predicates about `self`, as the callee's record states them): each becomes
    /// an axiom with `self` read as the callee — `lcm`'s commutativity follows from `gcd`'s.
    pub laws: Vec<J>,
    /// Parameter positions typed `nat` (≥ 0 in the antecedent).
    pub nat_positions: BTreeSet<usize>,
}

/// Rewrite a law's `self` applications (either spelling) into applications of `target`, so the law
/// lowers against the callee's uninterpreted symbol.
fn law_about(node: &J, target: &str) -> J {
    if let Some((head, args)) = flatten_call(node) {
        if head == "self" {
            let args: Vec<J> = args.iter().map(|a| law_about(a, target)).collect();
            return serde_json::json!({ "kind": "app", "op": target, "args": args });
        }
    }
    match node {
        J::Object(m) => J::Object(m.iter().map(|(k, v)| (k.clone(), law_about(v, target))).collect()),
        J::Array(xs) => J::Array(xs.iter().map(|v| law_about(v, target)).collect()),
        other => other.clone(),
    }
}

thread_local! {
    static CALLEE_CONTRACTS: std::cell::RefCell<BTreeMap<String, CalleeContract>> = const { std::cell::RefCell::new(BTreeMap::new()) };
    static CALLEES_USED: std::cell::RefCell<BTreeSet<String>> = const { std::cell::RefCell::new(BTreeSet::new()) };
}

/// Install the callee contracts the next proofs may use (replacing any), returning the previous set so
/// a nested use (proving a callee's own contract while computing it) can restore them.
pub fn set_callee_contracts(contracts: BTreeMap<String, CalleeContract>) -> BTreeMap<String, CalleeContract> {
    CALLEE_CONTRACTS.with(|c| std::mem::replace(&mut *c.borrow_mut(), contracts))
}

fn callee_contract(target: &str) -> Option<CalleeContract> {
    CALLEE_CONTRACTS.with(|c| c.borrow().get(target).cloned())
}

fn mark_callee_used(target: &str) {
    CALLEES_USED.with(|u| {
        u.borrow_mut().insert(target.to_string());
    });
}

fn take_callees_used() -> BTreeSet<String> {
    CALLEES_USED.with(|u| std::mem::take(&mut *u.borrow_mut()))
}

/// The SMT preamble for every callee the lowering just used: its declaration and its contract axioms.
fn callee_preamble() -> Result<(String, bool)> {
    let used = take_callees_used();
    let any = !used.is_empty();
    let mut out = String::new();
    for target in used {
        let Some(c) = callee_contract(&target) else { continue };
        out.push_str(&format!(
            "(declare-fun {} ({}) {})\n",
            c.symbol,
            c.param_sorts.iter().map(|s| s.smt()).collect::<Vec<_>>().join(" "),
            c.ret.smt()
        ));
        if c.posts.is_empty() {
            continue;
        }
        let mut env: BTreeMap<String, Sort> = c.params.iter().cloned().zip(c.param_sorts.iter().copied()).collect();
        let app = serde_json::json!({ "kind": "app", "op": target, "args": c.params.iter().map(|p| serde_json::json!({ "kind": "var", "name": p })).collect::<Vec<_>>() });
        let subst: BTreeMap<String, J> = [("result".to_string(), app)].into_iter().collect();
        let mut ante: Vec<String> = Vec::new();
        for (i, p) in c.params.iter().enumerate() {
            if c.nat_positions.contains(&i) {
                ante.push(format!("(>= {p} 0)"));
            }
        }
        for pre in &c.pres {
            ante.push(lower(pre, &mut env.clone(), None)?);
        }
        let binders = c.params.iter().zip(&c.param_sorts).map(|(p, s)| format!("({p} {})", s.smt())).collect::<Vec<_>>().join(" ");
        for post in &c.posts {
            let body = lower(&crate::equiv::subst_many(post, &subst), &mut env, None)?;
            let ante_smt = match ante.len() {
                0 => "true".to_string(),
                1 => ante[0].clone(),
                _ => format!("(and {})", ante.join(" ")),
            };
            if c.params.is_empty() {
                out.push_str(&format!("(assert (=> {ante_smt} {body}))\n"));
            } else {
                out.push_str(&format!(";; certified contract of {target}\n(assert (forall ({binders}) (=> {ante_smt} {body})))\n"));
            }
        }
        // Proved laws: `forall vars. P` with `self` read as the callee, guarded to the callee's nat
        // domain on the variables it feeds there (a law proved by numeric induction assumed it).
        for law in &c.laws {
            let law = uncurry(law);
            let Some(vars_arr) = law.get("vars").and_then(|v| v.as_array()) else { continue };
            let vars: Vec<String> = vars_arr.iter().filter_map(|x| x.as_str().map(String::from)).collect();
            let Some(pred) = law.get("body") else { continue };
            let pred = law_about(pred, &target);
            let Ok(var_sorts) = infer_var_sorts(&pred, &vars, Some(&c.param_sorts)) else { continue };
            let mut lenv: BTreeMap<String, Sort> = var_sorts.clone();
            let Ok(body) = lower(&pred, &mut lenv, None) else { continue };
            let mut guards: Vec<String> = Vec::new();
            for call in collect_self_applications_of(&pred, &target) {
                for (i, a) in call.iter().enumerate() {
                    if c.nat_positions.contains(&i) {
                        if let Some(v) = var_name(a) {
                            if vars.iter().any(|x| x == v) {
                                let g = format!("(>= {v} 0)");
                                if !guards.contains(&g) {
                                    guards.push(g);
                                }
                            }
                        }
                    }
                }
            }
            let lbinders = vars.iter().map(|v| format!("({v} {})", var_sorts[v].smt())).collect::<Vec<_>>().join(" ");
            let guarded = match guards.len() {
                0 => body,
                1 => format!("(=> {} {body})", guards[0]),
                _ => format!("(=> (and {}) {body})", guards.join(" ")),
            };
            out.push_str(&format!(";; certified law of {target}\n(assert (forall ({lbinders}) {guarded}))\n"));
        }
    }
    // Lowering the axioms marked the callee used again; the preamble is complete, so clear.
    take_callees_used();
    Ok((out, any))
}

/// Sort returned by `op` given the sorts of its operands are well-formed.
fn op_result_sort(op: &str) -> Option<Sort> {
    Some(match op {
        "add" | "sub" | "mul" | "neg" | "abs" | "min" | "max" | "mod" | "div" | "str_length" => Sort::Int,
        "eq" | "neq" | "lt" | "le" | "gt" | "ge" | "and" | "or" | "xor" | "not" | "str_contains"
        | "str_lt" => Sort::Bool,
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
        "lit" => match node.get("value").and_then(lit_kind) {
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
            if op.starts_with("fn_") {
                return callee_contract(&op).map(|c| c.ret).ok_or_else(|| {
                    anyhow!("applies the fn_ref `{op}` (its certified contract is not available here — pass --records to use it)")
                });
            }
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

/// The kind of a predicate literal. `predicate-expression.schema.json` admits BOTH spellings — a
/// structured value-AST (`{"kind":"int","value":0}`) and the bare JSON literal `parse-predicate`
/// emits (`0`, `true`, `"s"`; spec/surface-syntax.md §2) — so the bare form is normalized here
/// (2026-10-17: every surface-authored law hit `unsupported literal kind: None` before this).
pub(crate) fn lit_kind(value: &J) -> Option<&'static str> {
    match value {
        J::Object(_) => match value.get("kind").and_then(|k| k.as_str()) {
            Some("int") => Some("int"),
            Some("nat") => Some("nat"),
            Some("bool") => Some("bool"),
            Some("string") => Some("string"),
            Some("float") => Some("float"),
            _ => None,
        },
        J::Bool(_) => Some("bool"),
        J::Number(n) => Some(if n.is_i64() || n.is_u64() { "int" } else { "float" }),
        J::String(_) => Some("string"),
        _ => None,
    }
}

/// The payload of a literal in either spelling (the structured form's `value`, or the bare literal itself).
fn lit_payload(value: &J) -> &J {
    if value.is_object() {
        value.get("value").unwrap_or(value)
    } else {
        value
    }
}

/// Lower an integer/boolean/string literal value-AST to an SMT term.
fn lower_lit(raw: &J) -> Result<String> {
    let kind = lit_kind(raw);
    let value = serde_json::json!({ "kind": kind, "value": lit_payload(raw) });
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

    // Composition by content address: the callee is an uninterpreted function under its certified
    // contract (axioms emitted by `callee_preamble`), when the linked view supplied one.
    if op.starts_with("fn_") {
        let Some(c) = callee_contract(&op) else {
            bail!("applies the fn_ref `{op}` (its certified contract is not available here — pass --records to use it)");
        };
        if args.len() != c.params.len() {
            bail!("`{op}` applied to {} args, expects {}", args.len(), c.params.len());
        }
        let lowered = lower_args(env, &args)?;
        mark_callee_used(&op);
        return Ok(if lowered.is_empty() { c.symbol.clone() } else { format!("({} {})", c.symbol, lowered.join(" ")) });
    }

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
        // GW4 tier-2: SMT-LIB `str.<` is strict lexicographic order over code points — exactly our
        // `str_lt` semantics (the canonical-map-key order). Argument order agrees (no swap).
        "str_lt" => Ok(format!("(str.< {} {})", lower(args[0], env, self_def)?, lower(args[1], env, self_def)?)),
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
        Some("lit") => match node.get("value").and_then(lit_kind) {
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
        // A callee's parameter sorts constrain the variables fed to it.
        if op.starts_with("fn_") {
            if let Some(c) = callee_contract(&op) {
                for (i, a) in args.iter().enumerate() {
                    if let (Some(n), Some(s)) = (var_name(a), c.param_sorts.get(i)) {
                        constrain(n, *s, vars, sorts)?;
                    }
                }
            }
        }
        // Operand sorts implied by the operator.
        let operand_sort = match op.as_str() {
            "add" | "sub" | "mul" | "neg" | "abs" | "min" | "max" | "mod" | "div" | "lt" | "le" | "gt" | "ge" => Some(Sort::Int),
            "and" | "or" | "xor" | "not" => Some(Sort::Bool),
            "str_concat" | "str_length" | "str_contains" | "str_lt" | "str_lower" | "url_encode" => Some(Sort::Str),
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
            // str_lower stays OUT of the fragment: SMT-LIB has no case-conversion function and
            // encoding the Unicode mapping would be unsound-by-approximation. str_lt is IN (str.<).
            | "str_lower"
            // url_encode stays OUT for the same reason: no SMT percent-encoding counterpart.
            | "url_encode"
            // to_float is OUT: the fragment's arithmetic is Int; a widening into IEEE floats has
            // no sound Int-theory encoding (and the record-level float guard refuses float
            // domains wholesale — see `type_mentions_float`).
            | "to_float"
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
    build_certificate_in_domain(prop_expr, body, &BTreeSet::new())
}

/// [`build_certificate`] over the function's DECLARED domain: a law variable the law feeds to a
/// `nat`-typed parameter of `self` is asserted `≥ 0` (the generative checker already samples it so;
/// a callee's contract axioms are guarded to its nat domain, so without this a law over `lcm : nat
/// nat → nat` would be asked to hold where `gcd`'s contract says nothing — measured: a timeout).
pub fn build_certificate_in_domain(prop_expr: &J, body: Option<&J>, self_nat_positions: &BTreeSet<usize>) -> Result<Certificate> {
    take_callees_used(); // a failed earlier lowering must not leak its callees into this certificate
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
    let (callees, uses_callees) = callee_preamble()?;
    out.push_str(&callees);
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
    // The declared domain: variables fed to `self`'s nat parameters range over nats.
    let mut nat_vars: BTreeSet<String> = BTreeSet::new();
    for call in collect_self_applications(pred) {
        for (i, a) in call.iter().enumerate() {
            if self_nat_positions.contains(&i) {
                if let Some(v) = var_name(a) {
                    if vars.iter().any(|x| x == v) {
                        nat_vars.insert(v.to_string());
                    }
                }
            }
        }
    }
    for v in &nat_vars {
        out.push_str(&format!("(assert (>= {v} 0))\n"));
    }
    // Assert the negation of the law: unsat ⇒ the law holds for all inputs.
    out.push_str(&format!("(assert (not {pred_smt}))\n"));
    out.push_str("(check-sat)\n");
    out.push_str("(get-model)\n");

    Ok(Certificate { smt: out, quantified, uses_self, uses_callees })
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
        // A model over an uninterpreted callee is not a counterexample to the law (the callee is
        // pinned only by its contract): honest UNKNOWN.
        SatAnswer::Sat(_) if cert.uses_callees => ProofOutcome::Unknown,
        SatAnswer::Sat(model) => ProofOutcome::Refuted(model),
        SatAnswer::Unknown => ProofOutcome::Unknown,
        SatAnswer::NoSolver => ProofOutcome::NoSolver,
    })
}

/// Attempt to prove one property: build the certificate, run the solver. Out-of-fragment properties
/// yield `Unsupported` (with the certificate left `None`).
pub fn prove_property(prop_expr: &J, body: Option<&J>, solver: &str) -> (ProofOutcome, Option<Certificate>) {
    prove_property_in_domain(prop_expr, body, &BTreeSet::new(), solver)
}

/// [`prove_property`] over the function's declared domain (see [`build_certificate_in_domain`]).
pub fn prove_property_in_domain(
    prop_expr: &J,
    body: Option<&J>,
    self_nat_positions: &BTreeSet<usize>,
    solver: &str,
) -> (ProofOutcome, Option<Certificate>) {
    match build_certificate_in_domain(prop_expr, body, self_nat_positions) {
        Err(e) => (ProofOutcome::Unsupported(format!("{e:#}")), None),
        Ok(cert) => match run_solver(&cert, solver) {
            Ok(outcome) => (outcome, Some(cert)),
            Err(e) => (ProofOutcome::Unsupported(format!("solver error: {e:#}")), Some(cert)),
        },
    }
}

// --- numeric induction --------------------------------------------------------------------------------
//
// A law about a RECURSIVE function over the integers — `gcd`, `isqrt`, `mod_pow`, a nat-result
// contract on Euclid — is outside the first-order fragment (no `define-fun` can be recursive) and
// outside the structural backend (which inducts over lists). This rung proves it by STRONG
// INDUCTION over the function's own well-founded measure, the one the termination checker already
// identifies (`terminate::induction_measure`: the argument position every self-call shrinks, with
// the floor it is bounded below by):
//
//   * `self` is an UNINTERPRETED function (`declare-fun`) — never a recursive definition z3 would
//     have to unfold on its own (measured on the equivalence prover: it does not).
//   * The induction hypothesis is the law itself, universally quantified over the law's variables,
//     restricted to measures in `[floor, m)`: `∀ v'. floor ≤ m' < m ⇒ P(v')`.
//   * The defining equation is instantiated EXPLICITLY, at every `self`-application the law makes
//     and once more at every recursive call inside those (two levels), as `(self t…) = body[params := t…]`
//     with the body's own `self` calls left uninterpreted — so the step case sees exactly the
//     unfolding it needs, and nothing the solver must invent.
//   * The negated law is checked: UNSAT ⇒ PROVED by induction on the measure. SAT is NOT a
//     refutation (the uninterpreted `self` is under-constrained away from the instantiated points)
//     and is reported UNKNOWN; never a false verdict in either direction.
//
// Scope: every `self`-application in the law must put the SAME law variable at the measure
// position (the variable the induction is on); the law and body must lower to the Int/Bool/String
// fragment (tuples, lists, fn_refs refuse — honestly, as UNSUPPORTED).

/// What the induction ranges over, read off the law's `self` applications at the body's measure position.
enum MeasureSpec {
    /// One quantified variable sits at the measure position of every application: strong induction on it.
    Var(String),
    /// Every application puts a LITERAL there (`self(a, 0)`): no induction variable, and none needed —
    /// the instantiated defining equation alone decides the law (unfolding).
    Literal,
    /// Different variables, or compound terms, at the measure position (`gcd(a, b) = gcd(b, a)`,
    /// `C(n+1, k+1) = C(n, k) + C(n, k+1)`): strong induction on the SUM of the quantified nat variables
    /// those terms mention — well-founded over the nats, and the hypothesis then covers every
    /// application whose measure arguments add up to less.
    Sum(Vec<String>),
}

fn measure_spec(pred: &J, position: usize, vars: &[String], nat_vars: &BTreeSet<String>) -> Result<MeasureSpec> {
    let mut direct: BTreeSet<String> = BTreeSet::new();
    let mut compound = false;
    let mut mentioned: BTreeSet<String> = BTreeSet::new();
    let mut any = false;
    for call in collect_self_applications(pred) {
        any = true;
        let Some(arg) = call.get(position) else {
            bail!("a `self` application in the law has no argument at the measure position {position}");
        };
        if arg.get("kind").and_then(|k| k.as_str()) == Some("lit") {
            continue;
        }
        let in_arg = quantified_vars_in(arg, vars);
        if in_arg.is_empty() {
            bail!("the law applies `self` with a measure argument that mentions no quantified variable");
        }
        mentioned.extend(in_arg.iter().cloned());
        match var_name(arg) {
            Some(v) if vars.iter().any(|x| x == v) => {
                direct.insert(v.to_string());
            }
            _ => compound = true,
        }
    }
    if !any {
        bail!("the law does not apply `self`");
    }
    if mentioned.is_empty() {
        return Ok(MeasureSpec::Literal);
    }
    if !compound && direct.len() == 1 {
        return Ok(MeasureSpec::Var(direct.into_iter().next().unwrap()));
    }
    let not_nat: Vec<&String> = mentioned.iter().filter(|v| !nat_vars.contains(*v)).collect();
    if !not_nat.is_empty() {
        bail!(
            "the law applies `self` at several measures ({}), so the induction needs their SUM, but {} not nat-constrained (fed to a `nat` parameter somewhere in the law) — the sum would not be well-founded",
            mentioned.iter().cloned().collect::<Vec<_>>().join(", "),
            if not_nat.len() == 1 { format!("`{}` is", not_nat[0]) } else { format!("{} are", not_nat.iter().map(|v| format!("`{v}`")).collect::<Vec<_>>().join(", ")) }
        );
    }
    Ok(MeasureSpec::Sum(mentioned.into_iter().collect()))
}

/// The quantified variables a predicate term mentions.
fn quantified_vars_in(node: &J, vars: &[String]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    fn go(node: &J, vars: &[String], out: &mut BTreeSet<String>) {
        if let Some(v) = var_name(node) {
            if vars.iter().any(|x| x == v) {
                out.insert(v.to_string());
            }
        }
        match node {
            J::Object(m) => m.values().for_each(|v| go(v, vars, out)),
            J::Array(xs) => xs.iter().for_each(|v| go(v, vars, out)),
            _ => {}
        }
    }
    go(node, vars, &mut out);
    out
}

/// Every application of `target` (an op-form app) in a predicate AST, as its argument list.
fn collect_self_applications_of(node: &J, target: &str) -> Vec<Vec<J>> {
    let mut out = Vec::new();
    fn go(node: &J, target: &str, out: &mut Vec<Vec<J>>) {
        if node.get("op").and_then(|o| o.as_str()) == Some(target) {
            if let Some(args) = node.get("args").and_then(|a| a.as_array()) {
                out.push(args.clone());
            }
        }
        match node {
            J::Object(m) => m.values().for_each(|v| go(v, target, out)),
            J::Array(xs) => xs.iter().for_each(|v| go(v, target, out)),
            _ => {}
        }
    }
    go(node, target, &mut out);
    out
}

/// Every `self` application in a predicate or body AST (both spellings), as its argument list.
fn collect_self_applications(node: &J) -> Vec<Vec<J>> {
    let mut out = Vec::new();
    fn go(node: &J, out: &mut Vec<Vec<J>>) {
        if let Some((head, args)) = flatten_call(node) {
            if head == "self" {
                out.push(args.iter().map(|a| (*a).clone()).collect());
                for a in args {
                    go(a, out);
                }
                return;
            }
        }
        match node {
            J::Object(m) => m.iter().filter(|(k, _)| *k != "pattern").for_each(|(_, v)| go(v, out)),
            J::Array(xs) => xs.iter().for_each(|v| go(v, out)),
            _ => {}
        }
    }
    go(node, &mut out);
    out
}

/// Lower a RECURSIVE body for the induction rung: `self` stays an uninterpreted symbol inside its own
/// body (the sort of the result is found by trying the candidates against the body's arms).
fn lower_self_uninterpreted(body: &J) -> Result<SelfDef> {
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
        bail!("body is not a lambda");
    };
    let mut inferred: BTreeMap<String, Option<Sort>> = params.iter().map(|p| (p.clone(), None)).collect();
    visit_for_sorts(inner, &params, &mut inferred, None)?;
    let param_sorts: Vec<Sort> = params.iter().map(|p| inferred[p].unwrap_or(Sort::Int)).collect();
    let mut last_err = None;
    for cand in [Sort::Int, Sort::Bool, Sort::Str] {
        let partial = SelfDef { params: params.clone(), param_sorts: param_sorts.clone(), ret: cand, body_smt: String::new() };
        let mut env: BTreeMap<String, Sort> = params.iter().cloned().zip(param_sorts.iter().copied()).collect();
        match sort_of(inner, &env, Some(&partial)) {
            Ok(s) if s == cand => {
                let body_smt = lower(inner, &mut env, Some(&partial))?;
                return Ok(SelfDef { params, param_sorts, ret: cand, body_smt });
            }
            Ok(_) => {}
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow!("could not infer the recursive body's result sort")))
}

/// The SMT-LIB `let` that evaluates `body_smt` with the parameters bound to `terms`.
fn let_params(params: &[String], terms: &[String], body_smt: &str) -> String {
    if params.is_empty() {
        return body_smt.to_string();
    }
    let binds = params.iter().zip(terms).map(|(p, t)| format!("({p} {t})")).collect::<Vec<_>>().join(" ");
    format!("(let ({binds}) {body_smt})")
}

/// Prove `prop_expr` (a `forall` law mentioning `self`) about the recursive `body` by strong induction
/// over the body's numeric measure. Returns the outcome, the certificate (the one SMT script, which
/// re-checks independently), and the measure variable the induction is on.
pub fn prove_by_numeric_induction(
    prop_expr: &J,
    body: Option<&J>,
    nat_positions: &BTreeSet<usize>,
    solver: &str,
) -> (ProofOutcome, Option<Certificate>, Option<String>) {
    // A ladder: UNFOLDING alone first (the instantiated defining equations, no hypothesis — enough for
    // `gcd(a, 0) = a` and, measured, for `gcd(a, b) = gcd(b, a)`, which the quantified hypothesis only
    // obscures: z3 answers unknown with it and unsat without), then the hypothesis added.
    let unfolding = match build_induction_certificate(prop_expr, body, nat_positions, false) {
        Err(e) => return (ProofOutcome::Unsupported(format!("{e:#}")), None, None),
        Ok(x) => x,
    };
    match run_smt(&unfolding.0.smt, solver) {
        Ok(SatAnswer::Unsat) => {
            let label = if unfolding.1 == "<unfolding>" { unfolding.1.clone() } else { format!("{} (by unfolding alone)", unfolding.1) };
            return (ProofOutcome::Proved, Some(unfolding.0), Some(label));
        }
        Ok(SatAnswer::NoSolver) => return (ProofOutcome::NoSolver, Some(unfolding.0), Some(unfolding.1)),
        Err(e) => return (ProofOutcome::Unsupported(format!("solver error: {e:#}")), Some(unfolding.0), Some(unfolding.1)),
        _ => {}
    }
    match build_induction_certificate(prop_expr, body, nat_positions, true) {
        Err(e) => (ProofOutcome::Unsupported(format!("{e:#}")), None, None),
        Ok((cert, m)) => match run_smt(&cert.smt, solver) {
            Ok(SatAnswer::Unsat) => (ProofOutcome::Proved, Some(cert), Some(m)),
            // SAT is not a counterexample: `self` is uninterpreted away from the instantiated points.
            Ok(SatAnswer::Sat(_)) | Ok(SatAnswer::Unknown) => (ProofOutcome::Unknown, Some(cert), Some(m)),
            Ok(SatAnswer::NoSolver) => (ProofOutcome::NoSolver, Some(cert), Some(m)),
            Err(e) => (ProofOutcome::Unsupported(format!("solver error: {e:#}")), Some(cert), Some(m)),
        },
    }
}

fn build_induction_certificate(prop_expr: &J, body: Option<&J>, nat_positions: &BTreeSet<usize>, with_ih: bool) -> Result<(Certificate, String)> {
    take_callees_used();
    let prop_expr = &uncurry(prop_expr);
    let body = body.map(uncurry).ok_or_else(|| anyhow!("numeric induction needs the body"))?;
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
    if !references_self(pred) {
        bail!("the law does not mention `self` — nothing to induct on");
    }
    let measure = crate::terminate::induction_measure(&body, nat_positions)
        .ok_or_else(|| anyhow!("the body's recursion has no provable numeric measure (structural list recursion, or termination not established)"))?;
    let sd = lower_self_uninterpreted(&body)?;
    if measure.position >= sd.params.len() {
        bail!("measure position out of range");
    }
    // Domain facts: a law variable fed to a nat-typed `self` parameter ranges over nats.
    let mut nat_vars: BTreeSet<String> = BTreeSet::new();
    for call in collect_self_applications(pred) {
        for (i, a) in call.iter().enumerate() {
            if nat_positions.contains(&i) {
                if let Some(v) = var_name(a) {
                    if vars.iter().any(|x| x == v) {
                        nat_vars.insert(v.to_string());
                    }
                }
            }
        }
    }
    let spec = measure_spec(pred, measure.position, &vars, &nat_vars)?;
    let m = match &spec {
        MeasureSpec::Var(v) => v.clone(),
        MeasureSpec::Literal => "<unfolding>".to_string(),
        MeasureSpec::Sum(vs) => vs.join("+"),
    };

    let var_sorts = infer_var_sorts(pred, &vars, Some(&sd.param_sorts))?;
    let mut env: BTreeMap<String, Sort> = var_sorts.clone();
    if sort_of(pred, &env, Some(&sd))? != Sort::Bool {
        bail!("property body is not a boolean predicate");
    }
    let pred_smt = lower(pred, &mut env, Some(&sd))?;

    // The induction hypothesis: the law over primed variables, at smaller measures.
    let prime = |v: &str| format!("{v}__ih");
    let renaming: BTreeMap<String, J> = vars.iter().map(|v| (v.clone(), serde_json::json!({ "kind": "var", "name": prime(v) }))).collect();
    let pred_ih = crate::equiv::subst_many(pred, &renaming);
    let mut env_ih: BTreeMap<String, Sort> = vars.iter().map(|v| (prime(v), var_sorts[v])).collect();
    let pred_ih_smt = lower(&pred_ih, &mut env_ih, Some(&sd))?;

    let mut out = String::new();
    out.push_str("(set-logic ALL)\n");
    out.push_str(&format!(
        ";; numeric induction on `{m}` (measure position {}, floor {}): self is uninterpreted, its defining\n;; equation instantiated at every application the law makes and at the recursive calls inside them.\n",
        measure.position, measure.floor
    ));
    out.push_str(&format!(
        "(declare-fun self ({}) {})\n",
        sd.param_sorts.iter().map(|s| s.smt()).collect::<Vec<_>>().join(" "),
        sd.ret.smt()
    ));
    let (callees, uses_callees) = callee_preamble()?;
    out.push_str(&callees);
    for v in &vars {
        out.push_str(&format!("(declare-const {v} {})\n", var_sorts[v].smt()));
    }
    for v in &nat_vars {
        out.push_str(&format!("(assert (>= {v} 0))\n"));
    }
    // IH — only when there is a measure to induct on (a law with a literal at the measure position is
    // decided by the unfolding below alone). One variable: `floor ≤ m' < m`. A sum of nat variables:
    // `Σ v' < Σ v` (each v' ≥ 0 — well-founded over the nats).
    let ih_order: Option<Vec<String>> = match &spec {
        MeasureSpec::Var(m) => Some(vec![format!("(>= {} {})", prime(m), smt_int_lit(measure.floor)), format!("(< {} {m})", prime(m))]),
        MeasureSpec::Sum(vs) => {
            let sum = |f: &dyn Fn(&str) -> String| if vs.len() == 1 { f(&vs[0]) } else { format!("(+ {})", vs.iter().map(|v| f(v)).collect::<Vec<_>>().join(" ")) };
            Some(vec![format!("(< {} {})", sum(&|v| prime(v)), sum(&|v| v.to_string()))])
        }
        MeasureSpec::Literal => None,
    };
    if let (true, Some(mut ih_guards)) = (with_ih, ih_order) {
        let ih_binders = vars.iter().map(|v| format!("({} {})", prime(v), var_sorts[v].smt())).collect::<Vec<_>>().join(" ");
        for v in &nat_vars {
            ih_guards.push(format!("(>= {} 0)", prime(v)));
        }
        out.push_str(&format!(
            "(assert (forall ({ih_binders}) (=> (and {}) {pred_ih_smt})))\n",
            ih_guards.join(" ")
        ));
    } else {
        let _ = pred_ih_smt;
    }
    // Defining-equation instantiations: at the law's applications, and at the recursive calls inside them.
    let recursive_calls = collect_self_applications(body.get("body").unwrap_or(&J::Null));
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for call in collect_self_applications(pred) {
        if call.len() != sd.params.len() {
            bail!("self applied to {} args, expects {}", call.len(), sd.params.len());
        }
        let terms: Vec<String> = call.iter().map(|a| lower(a, &mut env.clone(), Some(&sd))).collect::<Result<_>>()?;
        let app = format!("(self {})", terms.join(" "));
        if seen.insert(app.clone()) {
            out.push_str(&format!("(assert (= {app} {}))\n", let_params(&sd.params, &terms, &sd.body_smt)));
        }
        // One level deeper: the body's own recursive calls, at this instantiation.
        let mut penv: BTreeMap<String, Sort> = sd.params.iter().cloned().zip(sd.param_sorts.iter().copied()).collect();
        for rc in &recursive_calls {
            if rc.len() != sd.params.len() {
                continue;
            }
            let rterms: Vec<String> = rc.iter().map(|a| lower(a, &mut penv, Some(&sd))).collect::<Result<_>>()?;
            let inner_app = format!("(self {})", rterms.join(" "));
            let eq = format!("(= {inner_app} {})", let_params(&sd.params, &rterms, &sd.body_smt));
            let instantiated = let_params(&sd.params, &terms, &eq);
            if seen.insert(instantiated.clone()) {
                out.push_str(&format!("(assert {instantiated})\n"));
            }
        }
    }
    out.push_str(&format!("(assert (not {pred_smt}))\n(check-sat)\n"));
    let quantified: Vec<(String, Sort)> = vars.iter().map(|v| (v.clone(), var_sorts[v])).collect();
    Ok((Certificate { smt: out, quantified, uses_self: true, uses_callees }, m))
}

fn smt_int_lit(n: i64) -> String {
    if n < 0 {
        format!("(- {})", -(n as i128))
    } else {
        n.to_string()
    }
}

/// Does a type-expression AST mention the `float` builtin anywhere (params, result, nested)?
///
/// **The float-domain guard (GW5).** The proof fragment's arithmetic is the solver's *Int*
/// theory; sorts are inferred from body usage with an Int default and the declared type was
/// never consulted — so a float-typed record carrying an arithmetic law (e.g. associativity:
/// true over ℤ, false over IEEE floats) would be "PROVED" over the wrong domain. Callers that
/// hold the record (`prove`, `check-refinement`, `cluster`) check this and report
/// UNSUPPORTED / UNVERIFIABLE / a singleton class instead — honest, never mis-proved.
pub fn type_mentions_float(ty: &J) -> bool {
    // Fold refs-to-canonical-builtin-artifacts first (the v0.2 builtin↔ref interchange): a float
    // spelled as `{"kind":"ref","target":type_<canonical float>}` must trip the guard exactly like
    // the builtin spelling — otherwise a ref-typed record would be mis-proved over Int theory.
    fn walk(ty: &J) -> bool {
        match ty {
            J::Object(map) => {
                if map.get("kind").and_then(|k| k.as_str()) == Some("builtin")
                    && map.get("name").and_then(|n| n.as_str()) == Some("float")
                {
                    return true;
                }
                map.values().any(walk)
            }
            J::Array(items) => items.iter().any(walk),
            _ => false,
        }
    }
    walk(&crate::fold_canonical_type_refs(ty))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// count: \n -> case gt(n, 0) of true -> add(1, self(sub(n, 1))) | false -> 0 — a nat recursion.
    fn count_body() -> J {
        json!({ "kind": "lambda", "params": [{ "name": "n" }],
            "body": { "kind": "case",
                      "scrutinee": { "kind": "app", "fn": { "kind": "var", "name": "gt" },
                                     "args": [{ "kind": "var", "name": "n" }, { "kind": "lit", "value": { "kind": "int", "value": 0 } }] },
                      "arms": [
                          { "pattern": { "kind": "lit", "value": { "kind": "bool", "value": true } },
                            "body": { "kind": "app", "fn": { "kind": "var", "name": "add" },
                                      "args": [{ "kind": "lit", "value": { "kind": "int", "value": 1 } },
                                               { "kind": "app", "fn": { "kind": "var", "name": "self" },
                                                 "args": [{ "kind": "app", "fn": { "kind": "var", "name": "sub" },
                                                           "args": [{ "kind": "var", "name": "n" }, { "kind": "lit", "value": { "kind": "int", "value": 1 } }] }] }] } },
                          { "pattern": { "kind": "lit", "value": { "kind": "bool", "value": false } },
                            "body": { "kind": "lit", "value": { "kind": "int", "value": 0 } } }] } })
    }

    fn law(vars: &[&str], body: J) -> J {
        json!({ "kind": "forall", "vars": vars, "body": body })
    }
    fn self_app(args: Vec<J>) -> J {
        json!({ "kind": "app", "op": "self", "args": args })
    }
    fn v(n: &str) -> J {
        json!({ "kind": "var", "name": n })
    }
    fn lit(n: i64) -> J {
        json!({ "kind": "lit", "value": n })
    }
    fn op(o: &str, args: Vec<J>) -> J {
        json!({ "kind": "app", "op": o, "args": args })
    }

    #[test]
    fn callee_contracts_are_axioms_and_sat_is_never_a_refutation() {
        let Some(solver) = solver() else { return };
        // A callee `fn_abab…` : nat -> int with the proved contract `result >= 0`, applied by a law
        // `fn_abab(n) + 1 > 0` that needs exactly that contract.
        let target = format!("fn_{}", "ab".repeat(32));
        let contract = CalleeContract {
            symbol: "callee_abab".into(),
            params: vec!["x".into()],
            param_sorts: vec![Sort::Int],
            ret: Sort::Int,
            pres: vec![],
            posts: vec![json!({ "kind": "app", "op": "ge", "args": [{ "kind": "var", "name": "result" }, { "kind": "lit", "value": 0 }] })],
            laws: vec![],
            nat_positions: [0].into(),
        };
        let call = |a: J| json!({ "kind": "app", "fn": { "kind": "lit", "value": { "kind": "fn_ref", "target": target } }, "args": [a] });
        // (Guarded to the callee's nat domain: for n < 0 the contract says nothing about fn_abab(n).)
        let law_ok = law(&["n"], op("or", vec![op("lt", vec![v("n"), lit(0)]), op("gt", vec![op("add", vec![call(v("n")), lit(1)]), lit(0)])]));
        let law_too_strong = law(&["n"], op("gt", vec![call(v("n")), lit(5)]));
        // Without the contract installed: UNSUPPORTED, stated.
        let prev = set_callee_contracts(BTreeMap::new());
        let (out, _) = prove_property(&law_ok, None, solver);
        assert!(matches!(out, ProofOutcome::Unsupported(ref why) if why.contains("certified contract")), "{out:?}");
        // With it: the true law proves from the axiom alone …
        set_callee_contracts([(target.clone(), contract)].into_iter().collect());
        let (out, cert) = prove_property(&law_ok, None, solver);
        assert!(matches!(out, ProofOutcome::Proved), "{out:?}");
        let cert = cert.unwrap();
        assert!(cert.uses_callees, "the certificate records the contract use");
        assert!(cert.smt.contains("(declare-fun callee_abab (Int) Int)"), "{}", cert.smt);
        assert!(cert.smt.contains("(>= x 0)"), "{}", cert.smt);
        // … and a law the contract does not imply is UNKNOWN — never REFUTED on a spurious model.
        let (out, _) = prove_property(&law_too_strong, None, solver);
        assert!(matches!(out, ProofOutcome::Unknown), "{out:?}");
        set_callee_contracts(prev);
    }

    #[test]
    fn numeric_induction_proves_true_laws_and_never_false_ones() {
        let Some(solver) = solver() else { return };
        let nats: BTreeSet<usize> = [0].into();
        let body = count_body();
        // count(n) == n over nats: PROVED by strong induction on n.
        let (out, cert, m) = prove_by_numeric_induction(&law(&["n"], op("eq", vec![self_app(vec![v("n")]), v("n")])), Some(&body), &nats, solver);
        assert!(matches!(out, ProofOutcome::Proved), "{out:?}");
        assert_eq!(m.as_deref(), Some("n"));
        let smt = cert.unwrap().smt;
        assert!(smt.contains("(declare-fun self (Int) Int)") && smt.contains("(forall ((n__ih Int))"), "{smt}");
        // A FALSE law is never proved (the uninterpreted self makes SAT uninformative: UNKNOWN).
        let (out, _, _) = prove_by_numeric_induction(&law(&["n"], op("eq", vec![self_app(vec![v("n")]), lit(0)])), Some(&body), &nats, solver);
        assert!(matches!(out, ProofOutcome::Unknown), "{out:?}");
        // A literal at the measure position needs no induction: unfolding alone decides count(0) == 0.
        let (out, _, m) = prove_by_numeric_induction(&law(&["k"], op("eq", vec![self_app(vec![lit(0)]), lit(0)])), Some(&body), &nats, solver);
        assert!(matches!(out, ProofOutcome::Proved), "{out:?}");
        assert_eq!(m.as_deref(), Some("<unfolding>"));
        // Two different measures in one law (commutativity's shape): the SUM measure `a+b` — here the
        // law `count(a) = count(b)` is false, so UNKNOWN (never PROVED, never REFUTED).
        let two = law(&["a", "b"], op("eq", vec![self_app(vec![v("a")]), self_app(vec![v("b")])]));
        let (out, _, m) = prove_by_numeric_induction(&two, Some(&body), &nats, solver);
        assert!(matches!(out, ProofOutcome::Unknown), "{out:?}");
        assert_eq!(m.as_deref(), Some("a+b"));
        // The true two-measure law `count(a) + count(b) = count(b) + count(a)` PROVES (unfolding suffices).
        let sym = law(&["a", "b"], op("eq", vec![
            op("add", vec![self_app(vec![v("a")]), self_app(vec![v("b")])]),
            op("add", vec![self_app(vec![v("b")]), self_app(vec![v("a")])])]));
        let (out, _, _) = prove_by_numeric_induction(&sym, Some(&body), &nats, solver);
        assert!(matches!(out, ProofOutcome::Proved), "{out:?}");
        // A non-recursive body has no measure: UNSUPPORTED (the first-order prover owns it).
        let (out, _, _) = prove_by_numeric_induction(&law(&["n"], op("eq", vec![self_app(vec![v("n")]), v("n")])), Some(&double_body()), &Default::default(), solver);
        assert!(matches!(out, ProofOutcome::Unsupported(_)), "{out:?}");
    }

    fn double_body() -> J {
        json!({ "kind": "lambda", "params": [{ "name": "n" }], "body": {
            "kind": "app", "op": "add", "args": [{ "kind": "var", "name": "n" }, { "kind": "var", "name": "n" }] } })
    }

    #[test]
    fn float_domain_guard_detects_float_types() {
        // The GW5 guard: a float anywhere in the declared type (param, result, nested List) trips it.
        let direct = json!({ "kind": "fn", "params": [{ "kind": "builtin", "name": "float" }],
                             "result": { "kind": "builtin", "name": "float" } });
        assert!(type_mentions_float(&direct));
        let nested = json!({ "kind": "fn",
            "params": [{ "kind": "apply", "ctor": { "kind": "builtin", "name": "List" },
                         "args": [{ "kind": "builtin", "name": "float" }] }],
            "result": { "kind": "builtin", "name": "string" } });
        assert!(type_mentions_float(&nested));
        let intfn = json!({ "kind": "fn", "params": [{ "kind": "builtin", "name": "int" }],
                            "result": { "kind": "builtin", "name": "int" } });
        assert!(!type_mentions_float(&intfn));
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
    fn str_lt_laws_prove_and_refute() {
        // GW4 tier-2: str_lt maps to SMT `str.<` (strict lexicographic over code points, same arg
        // order). Irreflexivity proves; totality-of-< (forall a b. str_lt(a,b)) refutes (a = b).
        let irrefl = json!({ "kind": "forall", "vars": ["s"], "body": {
            "kind": "app", "op": "not", "args": [
                { "kind": "app", "op": "str_lt", "args": [
                    { "kind": "var", "name": "s" }, { "kind": "var", "name": "s" }] }] } });
        let cert = build_certificate(&irrefl, None).unwrap();
        assert!(cert.smt.contains("(str.< s s)"), "\n{}", cert.smt);
        assert_eq!(run_smt(&cert.smt, "z3").unwrap(), SatAnswer::Unsat, "\n{}", cert.smt);

        let all_lt = json!({ "kind": "forall", "vars": ["a", "b"], "body": {
            "kind": "app", "op": "str_lt", "args": [
                { "kind": "var", "name": "a" }, { "kind": "var", "name": "b" }] } });
        let cert = build_certificate(&all_lt, None).unwrap();
        assert!(matches!(run_smt(&cert.smt, "z3").unwrap(), SatAnswer::Sat(_)));
    }

    #[test]
    fn str_lower_is_out_of_fragment() {
        // No SMT case-conversion counterpart — a law over str_lower reads UNSUPPORTED, never mis-typed.
        let prop = json!({ "kind": "forall", "vars": ["s"], "body": {
            "kind": "app", "op": "eq", "args": [
                { "kind": "app", "op": "str_lower", "args": [{ "kind": "var", "name": "s" }] },
                { "kind": "var", "name": "s" }] } });
        assert!(build_certificate(&prop, None).is_err());
    }

    #[test]
    fn url_encode_is_out_of_fragment() {
        // No SMT percent-encoding counterpart — a law over url_encode reads UNSUPPORTED, never mis-typed.
        let prop = json!({ "kind": "forall", "vars": ["s"], "body": {
            "kind": "app", "op": "eq", "args": [
                { "kind": "app", "op": "url_encode", "args": [{ "kind": "var", "name": "s" }] },
                { "kind": "var", "name": "s" }] } });
        assert!(build_certificate(&prop, None).is_err());
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
