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

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Sort {
    Int,
    Bool,
    Str,
    /// A declared SMT datatype — a tuple or a sum (`Maybe`, `Result`, …) — by its index in the
    /// per-thread datatype registry (`sort_of_type` registers; `datatype_preamble` declares).
    Data(u16),
    /// A list of integers — the structural backend's `Lst = nil | cons(Int, Lst)` datatype, declared
    /// here whenever a certificate mentions the sort. Lists are LOWERED only inside the induction rung
    /// (`lists_in_fragment`): the first-order prover keeps refusing them, so a list law still reaches
    /// the structural backend; a RECURSIVE function's law or contract that mentions a list goes
    /// through numeric/structural induction with `length`/`cons`/`head`/`tail`/`null` as the solver's
    /// own definitions. A callee's contract may carry the sort either way.
    Lst,
}

impl Sort {
    fn smt(self) -> String {
        match self {
            Sort::Int => "Int".into(),
            Sort::Bool => "Bool".into(),
            Sort::Str => "String".into(),
            Sort::Data(i) => format!("dt{i}"),
            Sort::Lst => {
                LST_USED.with(|u| u.set(true));
                "Lst".into()
            }
        }
    }
}

thread_local! {
    /// Whether list operations lower in the current certificate (set by the induction rung only).
    static LISTS_IN_FRAGMENT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Whether the `Lst` sort was emitted into the certificate being built (declare it, then).
    static LST_USED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The recursive list definitions the certificate's lowering used (`length`, `append`, `reverse`).
    static LIST_DEFS_USED: std::cell::RefCell<BTreeSet<&'static str>> = const { std::cell::RefCell::new(BTreeSet::new()) };
}

fn lists_in_fragment() -> bool {
    LISTS_IN_FRAGMENT.with(|f| f.get())
}

/// Admit list operations for the rest of the current certificate; restores the previous setting on
/// drop (so an early `?` return leaves the first-order prover's refusal in force).
struct ListsGuard(bool);
impl Drop for ListsGuard {
    fn drop(&mut self) {
        LISTS_IN_FRAGMENT.with(|f| f.set(self.0));
    }
}
fn admit_lists() -> ListsGuard {
    ListsGuard(LISTS_IN_FRAGMENT.with(|f| f.replace(true)))
}

fn mark_list_def(name: &'static str) {
    LIST_DEFS_USED.with(|d| {
        d.borrow_mut().insert(name);
    });
}

/// Forget the list-emission marks of a previous certificate (called where a certificate starts).
fn reset_list_emission() {
    LST_USED.with(|u| u.set(false));
    LIST_DEFS_USED.with(|d| d.borrow_mut().clear());
}

/// The `Lst` datatype and the recursive list definitions a certificate used, in dependency order
/// (`reverse` is defined through `append`). Empty when the certificate never mentioned a list.
fn list_preamble() -> String {
    if !LST_USED.with(|u| u.get()) && LIST_DEFS_USED.with(|d| d.borrow().is_empty()) {
        return String::new();
    }
    let used = LIST_DEFS_USED.with(|d| d.borrow().clone());
    let mut out = String::from("(declare-datatypes ((Lst 0)) (((nil) (cons (hd Int) (tl Lst)))))\n");
    if used.contains("length") {
        out.push_str("(define-fun-rec length ((xs Lst)) Int (ite ((_ is nil) xs) 0 (+ 1 (length (tl xs)))))\n");
    }
    if used.contains("append") || used.contains("reverse") {
        out.push_str("(define-fun-rec append ((xs Lst) (ys Lst)) Lst (ite ((_ is nil) xs) ys (cons (hd xs) (append (tl xs) ys))))\n");
    }
    if used.contains("reverse") {
        out.push_str("(define-fun-rec reverse ((xs Lst)) Lst (ite ((_ is nil) xs) nil (append (reverse (tl xs)) (cons (hd xs) nil))))\n");
    }
    if used.contains("all_nat") {
        out.push_str("(define-fun-rec all_nat ((xs Lst)) Bool (ite ((_ is nil) xs) true (and (>= (hd xs) 0) (all_nat (tl xs)))))\n");
    }
    out
}

/// The parameter positions of the function under proof typed `List nat`: their elements are
/// bounded below by type (the list analogue of `nat_positions_of_type`), stated as `all_nat` on the
/// law variables fed to them — `from_digits ≥ 0` is false over Int-element lists and true over its
/// declared domain.
fn nat_list_param_positions() -> BTreeSet<usize> {
    let Some(ty) = SELF_TYPE.with(|t| t.borrow().clone()) else { return BTreeSet::new() };
    let fn_ty = if ty.get("kind").and_then(|k| k.as_str()) == Some("forall") { ty.get("body").cloned().unwrap_or(J::Null) } else { ty };
    fn_ty
        .get("params")
        .and_then(|p| p.as_array())
        .map(|ps| {
            ps.iter()
                .enumerate()
                .filter(|(_, p)| {
                    let p = crate::fold_canonical_type_refs(p);
                    p.get("kind").and_then(|k| k.as_str()) == Some("apply")
                        && p.get("ctor").and_then(crate::builtin_type_name) == Some("List")
                        && p.get("args").and_then(|a| a.as_array()).is_some_and(|a| a.len() == 1 && crate::builtin_type_name(&a[0]) == Some("nat"))
                })
                .map(|(i, _)| i)
                .collect()
        })
        .unwrap_or_default()
}

// --- datatypes (tuples and sums as SMT datatypes) ----------------------------------------------------
//
// The fragment's values were Int/Bool/String. A record whose result is a TUPLE (`ext_gcd`'s
// `(g, (x, y))`) or a SUM (`mod_inverse`'s `Maybe int`) could neither state a contract about it nor
// be lowered. Both now map to SMT datatypes, registered per thread from the DECLARED types (the
// function's own signature, a callee's contract) — tuple construction and destructuring patterns,
// variant construction and matching, and the `fst`/`snd` projections lower to constructors,
// accessors and testers; every certificate declares the datatypes it uses.

#[derive(Debug, Clone, PartialEq, Eq)]
enum Datatype {
    Tuple(Vec<Sort>),
    /// Variants in declaration order, each with its optional payload sort.
    Sum(Vec<(String, Option<Sort>)>),
}

thread_local! {
    static DATATYPES: std::cell::RefCell<Vec<Datatype>> = const { std::cell::RefCell::new(Vec::new()) };
    /// The declared type of the function under proof (`signature.type`), when the caller supplies it:
    /// parameter and result sorts come from it rather than from usage inference.
    static SELF_TYPE: std::cell::RefCell<Option<J>> = const { std::cell::RefCell::new(None) };
}

fn register_datatype(d: Datatype) -> Sort {
    DATATYPES.with(|r| {
        let mut r = r.borrow_mut();
        if let Some(i) = r.iter().position(|x| *x == d) {
            return Sort::Data(i as u16);
        }
        r.push(d);
        Sort::Data((r.len() - 1) as u16)
    })
}

fn datatype(i: u16) -> Option<Datatype> {
    DATATYPES.with(|r| r.borrow().get(i as usize).cloned())
}

/// The SMT sort of a TYPE expression: builtins to the base sorts, tuples and sums to registered
/// datatypes (nested types register inner-first). `None` for anything else (lists, maps, type
/// variables, functions) — out of the fragment.
pub fn sort_of_type(ty: &J) -> Option<Sort> {
    let ty = crate::fold_canonical_type_refs(ty);
    match ty.get("kind").and_then(|k| k.as_str()) {
        Some("builtin") => match crate::builtin_type_name(&ty) {
            Some("int") | Some("nat") => Some(Sort::Int),
            Some("bool") => Some(Sort::Bool),
            Some("string") => Some(Sort::Str),
            _ => None,
        },
        Some("ref") => match crate::builtin_type_name(&ty) {
            Some("int") | Some("nat") => Some(Sort::Int),
            Some("bool") => Some(Sort::Bool),
            Some("string") => Some(Sort::Str),
            _ => None,
        },
        Some("tuple") => {
            let elems: Vec<Sort> = ty.get("elems")?.as_array()?.iter().map(sort_of_type).collect::<Option<_>>()?;
            Some(register_datatype(Datatype::Tuple(elems)))
        }
        Some("sum") => {
            let mut variants: Vec<(String, Option<Sort>)> = Vec::new();
            for v in ty.get("variants")?.as_array()? {
                let tag = v.get("tag")?.as_str()?.to_string();
                let payload = match v.get("type") {
                    Some(t) => Some(sort_of_type(t)?),
                    None => None,
                };
                variants.push((tag, payload));
            }
            Some(register_datatype(Datatype::Sum(variants)))
        }
        // `List int` / `List nat`: the structural backend's `Lst` (Int elements only — the one
        // element sort the list definitions are stated over).
        Some("apply") if crate::builtin_type_name(ty.get("ctor")?) == Some("List") => {
            let args = ty.get("args")?.as_array()?;
            if args.len() == 1 && sort_of_type(&args[0]) == Some(Sort::Int) {
                Some(Sort::Lst)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Install (or clear) the declared type of the function under proof; returns the previous value.
pub fn set_self_type(ty: Option<J>) -> Option<J> {
    SELF_TYPE.with(|t| std::mem::replace(&mut *t.borrow_mut(), ty))
}

/// The declared parameter and result sorts of the function under proof, when a type was supplied
/// and it is a function type of the given arity (each sort `None` where out of the fragment).
/// The declared type's datatypes registered before an obligation is lowered. A refinement
/// obligation INLINES a non-recursive body (no `self` definition is lowered, so nothing else
/// registers the declared sorts), yet a variant the body constructs — `Just(mod x m)` — or the
/// post's `case` on the result needs the declared sum in scope.
fn register_declared_self_type() {
    let Some(ty) = SELF_TYPE.with(|t| t.borrow().clone()) else { return };
    let fn_ty = if ty.get("kind").and_then(|k| k.as_str()) == Some("forall") { ty.get("body").cloned().unwrap_or(ty) } else { ty };
    if let Some(ps) = fn_ty.get("params").and_then(|p| p.as_array()) {
        for p in ps {
            let _ = sort_of_type(p);
        }
    }
    if let Some(r) = fn_ty.get("result") {
        let _ = sort_of_type(r);
    }
}

fn declared_self_sorts(arity: usize) -> Option<(Vec<Option<Sort>>, Option<Sort>)> {
    let ty = SELF_TYPE.with(|t| t.borrow().clone())?;
    let fn_ty = if ty.get("kind").and_then(|k| k.as_str()) == Some("forall") { ty.get("body")?.clone() } else { ty };
    if fn_ty.get("kind").and_then(|k| k.as_str()) != Some("fn") {
        return None;
    }
    let params = fn_ty.get("params")?.as_array()?;
    if params.len() != arity {
        return None;
    }
    Some((params.iter().map(sort_of_type).collect(), fn_ty.get("result").and_then(sort_of_type)))
}

/// The registered SUM datatype carrying `tag` (with a payload of this sort, when given) — the sort a
/// bare variant construction `Just(e)` / `None` takes. Ambiguity (two registered sums share the tag)
/// refuses: a constructor alone does not name its type.
fn sum_sort_with_tag(tag: &str, payload: Option<Sort>) -> Result<Sort> {
    let hits: Vec<u16> = DATATYPES.with(|r| {
        r.borrow()
            .iter()
            .enumerate()
            .filter(|(_, d)| match d {
                Datatype::Sum(vs) => vs.iter().any(|(t, p)| t == tag && (payload.is_none() || *p == payload)),
                _ => false,
            })
            .map(|(i, _)| i as u16)
            .collect()
    });
    match hits.as_slice() {
        [i] => Ok(Sort::Data(*i)),
        [] => bail!("variant `{tag}` belongs to no declared sum type in scope (declare the function's type, or the callee's)"),
        _ => bail!("variant `{tag}` is ambiguous between several declared sum types"),
    }
}

/// The datatype declarations every certificate carries (all registered — a declaration an obligation
/// does not use is harmless, and nested tuples are registered inner-first, so the order is valid).
fn datatype_preamble() -> String {
    DATATYPES.with(|r| {
        let mut out = String::new();
        for (i, d) in r.borrow().iter().enumerate() {
            match d {
                Datatype::Tuple(elems) => {
                    let fields = elems.iter().enumerate().map(|(j, s)| format!("(dt{i}_{j} {})", s.smt())).collect::<Vec<_>>().join(" ");
                    out.push_str(&format!("(declare-datatype dt{i} ((mk_dt{i} {fields})))\n"));
                }
                Datatype::Sum(vs) => {
                    let ctors = vs
                        .iter()
                        .map(|(t, p)| match p {
                            Some(s) => format!("(dt{i}_{t} (dt{i}_{t}_v {}))", s.smt()),
                            None => format!("(dt{i}_{t})"),
                        })
                        .collect::<Vec<_>>()
                        .join(" ");
                    out.push_str(&format!("(declare-datatype dt{i} ({ctors}))\n"));
                }
            }
        }
        out
    })
}

/// Prepend the datatype declarations to a finished certificate (after `set-logic`): the list
/// datatype and definitions first (a tuple may carry a list), then the tuples and sums.
fn with_datatypes(smt: String) -> String {
    let dts = datatype_preamble(); // may mention `Lst` (marks it used) — so the list preamble is read after
    let dts = format!("{}{dts}", list_preamble());
    if dts.is_empty() {
        smt
    } else {
        smt.replacen("(set-logic ALL)\n", &format!("(set-logic ALL)\n{dts}"), 1)
    }
}

/// Bind a destructuring PATTERN against a scrutinee term of a known sort: the names it binds, each
/// with the accessor term that selects it and its sort. `bind` names the whole term; a `tuple`
/// pattern selects components; `wildcard` binds nothing.
fn bind_pattern(pat: &J, term: &str, sort: Sort, out: &mut Vec<(String, String, Sort)>) -> Result<()> {
    match pat.get("kind").and_then(|k| k.as_str()) {
        Some("wildcard") => Ok(()),
        Some("bind") => {
            let name = pat.get("name").and_then(|n| n.as_str()).ok_or_else(|| anyhow!("bind has no name"))?;
            out.push((name.to_string(), term.to_string(), sort));
            Ok(())
        }
        Some("tuple") => {
            let Sort::Data(i) = sort else { bail!("tuple pattern against a non-tuple sort") };
            let Some(Datatype::Tuple(elems)) = datatype(i) else { bail!("tuple pattern against a non-tuple sort") };
            let pats = pat.get("elems").and_then(|e| e.as_array()).ok_or_else(|| anyhow!("tuple pattern has no elems"))?;
            if pats.len() != elems.len() {
                bail!("tuple pattern arity {} against a {}-tuple", pats.len(), elems.len());
            }
            for (j, (sub, es)) in pats.iter().zip(elems.iter()).enumerate() {
                bind_pattern(sub, &format!("(dt{i}_{j} {term})"), *es, out)?;
            }
            Ok(())
        }
        other => bail!("unsupported pattern `{other:?}` inside a destructuring pattern (out of fragment)"),
    }
}

fn wrap_lets(bindings: &[(String, String, Sort)], body: String) -> String {
    bindings.iter().rev().fold(body, |acc, (n, t, _)| format!("(let (({n} {t})) {acc})"))
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
    /// The certificate carries instances of the Euclidean-division uniqueness lemma
    /// (`euclid::LEMMA_SMT`, discharged by the solver first): the residue normal forms of its
    /// `mod`/`div` terms. Re-checking it end to end means re-checking the lemma beside it.
    pub euclid: bool,
    /// The STEPWISE Euclidean checker closed it (`euclid::retry_stepwise`): `smt` is then the final
    /// query over uninterpreted `mod`/`div`, `steps` the premise queries (one script, every
    /// `(check-sat)` unsat), and `stepwise` = (premises verified, instances generated).
    pub steps: Option<String>,
    pub stepwise: Option<(usize, usize)>,
    /// The STAGED rung closed it (`staged::retry_staged`): `smt` is then the final queries (one block
    /// per branch of the negated goal, each unsat), `steps` the disjunct refutations, and `staged` =
    /// (branches, conclusions derived).
    pub staged: Option<(usize, usize)>,
}

/// The Euclidean rung over a certificate that stayed UNKNOWN: one query with the lemma's
/// instances, then the stepwise checker. Updates the certificate in place; true when it closed.
fn euclid_ladder(cert: &mut Certificate, solver: &str) -> bool {
    if let Some(augmented) = crate::euclid::retry(&cert.smt, solver) {
        cert.smt = augmented;
        cert.euclid = true;
        return true;
    }
    if let Some(sw) = crate::euclid::retry_stepwise(&cert.smt, solver) {
        cert.smt = sw.final_smt;
        cert.steps = Some(sw.steps_smt);
        cert.stepwise = Some((sw.verified, sw.total));
        cert.euclid = true;
        return true;
    }
    false
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
    // Parameter sorts: the DECLARED type when supplied (tuples and sums have no usage signature), else
    // inferred from body usage (the same walk the law-side inference uses — it reads both the op and
    // fn/var application spellings), defaulting to Int.
    let declared = declared_self_sorts(params.len());
    let mut inferred: BTreeMap<String, Option<Sort>> = params.iter().map(|p| (p.clone(), None)).collect();
    visit_for_sorts(inner, &params, &mut inferred, None)?;
    let param_sorts: Vec<Sort> = params
        .iter()
        .enumerate()
        .map(|(i, p)| declared.as_ref().and_then(|d| d.0[i]).or(inferred[p]).unwrap_or(Sort::Int))
        .collect();
    let mut env: BTreeMap<String, Sort> = params.iter().cloned().zip(param_sorts.iter().copied()).collect();
    let ret = match declared.as_ref().and_then(|d| d.1) {
        Some(r) => r,
        None => sort_of(inner, &env, None)?,
    };
    let body_smt = lower_body(inner, &mut env)?;
    Ok(SelfDef { params, param_sorts, ret, body_smt })
}

// --- translation ----------------------------------------------------------------------------------

/// Rewrite curried direct-application spines — the surface parser's juxtaposition form,
/// `((f a) b)` — into flat applications `f(a, b)`, so the translator sees one head with all its
/// arguments (the same normalization the termination/complexity analyzers apply). Purely
/// structural; `apply`-spines and everything else pass through untouched. Children are rewritten
/// first, so one flattening step per node suffices (the loop is defensive). Shared with the
/// structural-induction backend, whose entry points normalize the same way (a surface-parsed
/// cons-recursive body read UNSUPPORTED there — "application with no resolvable head" — until it did).
pub(crate) fn uncurry(node: &J) -> J {
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
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
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
    /// Why a linked callee has NO usable contract (unresolvable, sorts outside the fragment, a
    /// cycle) — so the refusal names the real reason instead of "pass --records" when the records
    /// WERE passed (the integer-tier module's `digits_roundtrip` read that false instruction).
    static CALLEE_REFUSALS: std::cell::RefCell<BTreeMap<String, String>> = const { std::cell::RefCell::new(BTreeMap::new()) };
    static CALLEES_USED: std::cell::RefCell<BTreeSet<String>> = const { std::cell::RefCell::new(BTreeSet::new()) };
    /// Every application of a linked callee the current lowering emitted (target → lowered argument
    /// terms): its contract is instantiated at each (see `callee_preamble`) beside the quantified
    /// axioms — nonlinear consequences (`isqrt(n·n) = n` from `r² ≤ x < (r+1)²`) close on the
    /// instances where the quantifiers leave z3 at unknown (measured 2026-10-17).
    static CALLEE_APPS: std::cell::RefCell<BTreeMap<String, Vec<Vec<String>>>> = const { std::cell::RefCell::new(BTreeMap::new()) };
}

thread_local! {
    /// Applications are recorded only while the OBLIGATION's predicate is lowered — its argument
    /// terms are over the declared constants. A body's applications (lowered in parameter scope,
    /// then instantiated by `let`) and a contract's own (in a quantifier's scope) are not.
    static CALLEE_APPS_ENABLED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Whether contracts are also stated as QUANTIFIED axioms (the default). The second rung of a
    /// ladder turns them off: with nonlinear consequences z3 answers unknown on the quantified
    /// form even when the instances alone are unsat in 10 ms (`isqrt(n·n) = n`; measured). The
    /// instances are consequences of the axioms, so the instances-only certificate is as sound.
    static CONTRACT_QUANTIFIERS: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
}

/// Run `f` with the contracts' quantified axioms suppressed (instances only).
fn without_contract_quantifiers<T>(f: impl FnOnce() -> T) -> T {
    let prev = CONTRACT_QUANTIFIERS.with(|q| q.replace(false));
    let out = f();
    CONTRACT_QUANTIFIERS.with(|q| q.set(prev));
    out
}

/// Lower an obligation's predicate with callee-application recording on.
fn lower_goal(node: &J, env: &mut BTreeMap<String, Sort>, self_def: Option<&SelfDef>) -> Result<String> {
    let prev = CALLEE_APPS_ENABLED.with(|e| e.replace(true));
    let out = lower(node, env, self_def);
    CALLEE_APPS_ENABLED.with(|e| e.set(prev));
    out
}

thread_local! {
    /// The callee applications inside the function's own (inlined) body, in PARAMETER scope —
    /// recorded while `lower_self` lowers it, then instantiated at every `self` application the
    /// goal makes (`let` over the parameters), so a checker record's `case ext_gcd(a, b) of …`
    /// sees `ext_gcd`'s Bézout post at exactly `(a, b)`.
    static BODY_APPS: std::cell::RefCell<Vec<(String, Vec<String>)>> = const { std::cell::RefCell::new(Vec::new()) };
    static BODY_APPS_ENABLED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Lower the function's own body with its callee applications recorded (parameter scope).
fn lower_body(node: &J, env: &mut BTreeMap<String, Sort>) -> Result<String> {
    BODY_APPS.with(|b| b.borrow_mut().clear());
    let prev = BODY_APPS_ENABLED.with(|e| e.replace(true));
    let out = lower(node, env, None);
    BODY_APPS_ENABLED.with(|e| e.set(prev));
    out
}

fn take_body_apps() -> Vec<(String, Vec<String>)> {
    BODY_APPS.with(|b| std::mem::take(&mut *b.borrow_mut()))
}

thread_local! {
    /// The `let`- and pattern-bound names in scope while an expression is lowered, each with the
    /// SMT term that defines it (innermost last). A callee application recorded for a contract
    /// INSTANCE is stated outside those scopes, so its arguments are resolved through this stack
    /// first: `case normalize(n, d) of (p, q) => gcd(|p|, q)` records gcd's application at
    /// `(abs (dt_0 (normalize n d)))`, `(dt_1 (normalize n d))` — in the goal's scope — where
    /// before it recorded `(abs p)`, `q` and the certificate failed with "unknown constant p"
    /// (the rational tier's lowest-terms checker, 2026-10-30).
    static LOCAL_BINDINGS: std::cell::RefCell<Vec<(String, String)>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Lower under a local binding `name = def` (pushed for the duration of `f`).
fn with_locals<T>(binds: &[(String, String)], f: impl FnOnce() -> T) -> T {
    LOCAL_BINDINGS.with(|l| l.borrow_mut().extend(binds.iter().cloned()));
    let out = f();
    LOCAL_BINDINGS.with(|l| {
        let mut l = l.borrow_mut();
        let n = l.len() - binds.len();
        l.truncate(n);
    });
    out
}

/// Replace whole-token occurrences of `name` in an SMT term by `def`.
fn replace_token(s: &str, name: &str, def: &str) -> String {
    let is_ident = |c: char| c.is_alphanumeric() || c == '_' || c == '\'' || c == '.' || c == '!' || c == '?';
    let mut out = String::with_capacity(s.len());
    let bytes: Vec<char> = s.chars().collect();
    let pat: Vec<char> = name.chars().collect();
    let mut i = 0;
    while i < bytes.len() {
        let at = i + pat.len() <= bytes.len() && bytes[i..i + pat.len()] == pat[..];
        let before_ok = i == 0 || !is_ident(bytes[i - 1]);
        let after_ok = i + pat.len() == bytes.len() || !is_ident(bytes[i + pat.len()]);
        if at && before_ok && after_ok {
            out.push_str(def);
            i += pat.len();
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    out
}

/// An application's argument terms with every local binding in scope substituted by its
/// definition (innermost first), so the terms read in the enclosing scope.
fn resolve_locals(args: &[String]) -> Vec<String> {
    LOCAL_BINDINGS.with(|l| {
        let l = l.borrow();
        if l.is_empty() {
            return args.to_vec();
        }
        args.iter()
            .map(|a| {
                let mut t = a.clone();
                for (name, def) in l.iter().rev() {
                    if t.contains(name.as_str()) {
                        t = replace_token(&t, name, def);
                    }
                }
                t
            })
            .collect()
    })
}

fn record_callee_app(target: &str, args: &[String]) {
    let args = resolve_locals(args);
    if BODY_APPS_ENABLED.with(|e| e.get()) {
        BODY_APPS.with(|b| b.borrow_mut().push((target.to_string(), args.to_vec())));
        return;
    }
    if !CALLEE_APPS_ENABLED.with(|e| e.get()) {
        return;
    }
    push_callee_app(target, &args);
}

/// Record a callee application in goal scope unconditionally.
fn push_callee_app(target: &str, args: &[String]) {
    CALLEE_APPS.with(|a| {
        let mut a = a.borrow_mut();
        let apps = a.entry(target.to_string()).or_default();
        if !apps.iter().any(|x| x == args) {
            apps.push(args.to_vec());
        }
    });
}

fn take_callee_apps() -> BTreeMap<String, Vec<Vec<String>>> {
    CALLEE_APPS.with(|a| std::mem::take(&mut *a.borrow_mut()))
}

/// A law about `self` decided from `self`'s OWN certified contract — its proved postconditions and
/// laws (re-derived by the linked view, the way a callee's are), with `self` an uninterpreted
/// function and the contract instantiated at the law's applications. The rung for a law outside
/// every induction principle whose truth is a consequence of facts already proved about the
/// function: `isqrt(n·n) = n` from the floor bounds. Sound as a callee contract is — only proved
/// facts enter, and a satisfiable negation is UNKNOWN (the function is uninterpreted). The law's
/// variables fed to `nat` parameters are domain-qualified, as in `prove_property_in_domain`.
pub fn prove_from_self_contract(
    prop_expr: &J,
    target: &str,
    contract: CalleeContract,
    nat_positions: &BTreeSet<usize>,
    solver: &str,
) -> (ProofOutcome, Option<Certificate>) {
    let law = uncurry(prop_expr);
    let vars: Vec<String> = law.get("vars").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
    let Some(pred) = law.get("body") else { return (ProofOutcome::Unsupported("`forall` has no body".into()), None) };
    let mut guards: Vec<J> = Vec::new();
    for call in collect_self_applications(pred) {
        for (i, a) in call.iter().enumerate() {
            if nat_positions.contains(&i) {
                if let Some(v) = var_name(a) {
                    if vars.iter().any(|x| x == v) && !guards.iter().any(|g| g.pointer("/args/0/name").and_then(|n| n.as_str()) == Some(v)) {
                        guards.push(serde_json::json!({ "kind": "app", "op": "ge", "args": [{ "kind": "var", "name": v }, { "kind": "lit", "value": 0 }] }));
                    }
                }
            }
        }
    }
    let pred = law_about(pred, target);
    let body = if guards.is_empty() {
        pred
    } else {
        let conj = if guards.len() == 1 { guards.remove(0) } else { serde_json::json!({ "kind": "app", "op": "and", "args": guards }) };
        serde_json::json!({ "kind": "app", "op": "or", "args": [{ "kind": "app", "op": "not", "args": [conj] }, pred] })
    };
    let law = serde_json::json!({ "kind": "forall", "vars": vars, "body": body });
    let mut contracts = CALLEE_CONTRACTS.with(|c| c.borrow().clone());
    contracts.insert(target.to_string(), contract);
    let prev = set_callee_contracts(contracts);
    let out = prove_property_in_domain(&law, None, &BTreeSet::new(), solver);
    set_callee_contracts(prev);
    out
}

/// Install the callee contracts the next proofs may use (replacing any), returning the previous set so
/// a nested use (proving a callee's own contract while computing it) can restore them.
pub fn set_callee_contracts(contracts: BTreeMap<String, CalleeContract>) -> BTreeMap<String, CalleeContract> {
    CALLEE_CONTRACTS.with(|c| std::mem::replace(&mut *c.borrow_mut(), contracts))
}

/// Install the stated reasons for the linked callees that yielded no contract (see
/// `LinkedView::refusals_for`), returning the previous map. Installed beside the contracts.
pub fn set_callee_refusals(refusals: BTreeMap<String, String>) -> BTreeMap<String, String> {
    CALLEE_REFUSALS.with(|c| std::mem::replace(&mut *c.borrow_mut(), refusals))
}

fn callee_contract(target: &str) -> Option<CalleeContract> {
    CALLEE_CONTRACTS.with(|c| c.borrow().get(target).cloned())
}

/// The refusal for a callee without a contract: the linked view's stated reason when records were
/// supplied, else the instruction to supply them.
fn no_contract_reason(target: &str) -> String {
    match CALLEE_REFUSALS.with(|c| c.borrow().get(target).cloned()) {
        Some(why) => format!("applies the fn_ref `{target}` without a usable contract: {why}"),
        None => format!("applies the fn_ref `{target}` (its certified contract is not available here — pass --records to use it)"),
    }
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
/// Returns the declarations + quantified axioms (emitted before the obligation's constants), the
/// contract INSTANCES at the obligation's applications (emitted after them — they mention the
/// constants), and whether any callee was used.
fn callee_preamble() -> Result<(String, String, bool)> {
    let mut used = take_callees_used();
    let mut apps = take_callee_apps();
    let any = !used.is_empty();
    let retry = DIVISIBILITY_CALLEES.with(|d| d.get());
    // The witness rung's additions (see `WITNESS_CALLEES`): rounds, witness callees, transitive
    // instances, pairs, Int-argument candidates — a further retry, never the divisibility one.
    let witness = WITNESS_CALLEES.with(|w| w.get());
    // Declarations FIRST, for every callee the obligation applies — and, after the axioms are
    // lowered, for every callee a CONTRACT applies that the obligation did not (a post such as
    // `gcd(|fst(result)|, snd(result)) = 1` on `rat_normalize` names `gcd`; emitted after
    // `normalize`'s axioms, or not at all, the solver answered "unknown constant" — the rational
    // tier's lowest-terms checker, 2026-10-30).
    let declare = |c: &CalleeContract| format!("(declare-fun {} ({}) {})\n", c.symbol, c.param_sorts.iter().map(|s| s.smt()).collect::<Vec<_>>().join(" "), c.ret.smt());
    let mut decls = String::new();
    let mut declared: BTreeSet<String> = BTreeSet::new();
    let mut out = String::new();
    let mut inst = String::new();
    // Instances are stated once (the rounds below revisit a callee with more applications).
    let mut seen_inst: BTreeSet<String> = BTreeSet::new();
    let mut push_inst = |inst: &mut String, text: String| {
        if seen_inst.insert(text.clone()) {
            inst.push_str(&text);
        }
    };
    let mut axioms_done: BTreeSet<String> = BTreeSet::new();
    // ROUNDS (the retry only goes past the first): an instance stated at an application may itself
    // APPLY a callee — `rat_normalize`'s post `gcd(|fst(result)|, snd(result)) = 1` at `(a, b)`
    // applies `gcd` at `(|p|, q)` — and that application deserves ITS contract's instances too
    // (transitive instances); and a used callee may have a WITNESS in the contract set
    // (`witness_callees`: `ext_gcd`'s post names `gcd`, so `ext_gcd` is applied wherever `gcd`
    // is). Each adds applications for the next round; three rounds bound it.
    for round in 0..3 {
        if witness {
            witness_callees(&mut used, &mut apps);
        }
        let targets: Vec<String> = used.iter().cloned().collect();
        let mut new_apps: BTreeMap<String, Vec<Vec<String>>> = BTreeMap::new();
        for target in &targets {
            let Some(c) = callee_contract(target) else { continue };
            if declared.insert(target.clone()) {
                decls.push_str(&declare(&c));
            }
            // The applications of this callee the obligation makes: each contract fact is also stated at
            // them explicitly (below), so the solver need not instantiate a quantifier to use it.
            let at: Vec<Vec<String>> = apps.get(target).cloned().unwrap_or_default().into_iter().filter(|a| a.len() == c.params.len()).collect();
            if c.posts.is_empty() {
                continue;
            }
            let first = axioms_done.insert(target.clone());
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
                // The post lowered in the callee's parameter scope, its own callee applications
                // recorded (in that scope) for the transitive instances below.
                let prev_en = CALLEE_APPS_ENABLED.with(|e| e.replace(true));
                let lowered = lower(&crate::equiv::subst_many(post, &subst), &mut env, None);
                CALLEE_APPS_ENABLED.with(|e| e.set(prev_en));
                let inner = take_callee_apps();
                let body = lowered?;
                let ante_smt = match ante.len() {
                    0 => "true".to_string(),
                    1 => ante[0].clone(),
                    _ => format!("(and {})", ante.join(" ")),
                };
                if c.params.is_empty() {
                    if first {
                        out.push_str(&format!("(assert (=> {ante_smt} {body}))\n"));
                    }
                } else {
                    if first && CONTRACT_QUANTIFIERS.with(|q| q.get()) {
                        out.push_str(&format!(";; certified contract of {target}\n(assert (forall ({binders}) (=> {ante_smt} {body})))\n"));
                    }
                    for args in &at {
                        push_inst(&mut inst, format!("(assert {})\n", let_params(&c.params, args, &format!("(=> {ante_smt} {body})"))));
                        if witness {
                            // Transitive instances: the applications inside this post, at this
                            // instance, are applications in goal scope — of OTHER callees (the
                            // post's `result` is this callee's own application at its parameters,
                            // which the instance already is).
                            for (t2, a2) in &inner {
                                if t2 == target {
                                    continue;
                                }
                                for a in a2 {
                                    let mapped: Vec<String> = a.iter().map(|s| substitute_params(&c.params, args, s)).collect();
                                    let known = apps.get(t2).is_some_and(|v| v.contains(&mapped)) || new_apps.get(t2).is_some_and(|v| v.contains(&mapped));
                                    if !known && new_apps.values().map(|v| v.len()).sum::<usize>() < MAX_TRANSITIVE_APPS {
                                        new_apps.entry(t2.clone()).or_default().push(mapped);
                                        used.insert(t2.clone());
                                    }
                                }
                            }
                        }
                    }
                }
            }
            // Proved laws: `forall vars. P` with `self` read as the callee, guarded to the callee's nat
            // domain on the variables it feeds there (a law proved by numeric induction assumed it).
            for law in &c.laws {
                let law = uncurry(law);
                let Some(vars_arr) = law.get("vars").and_then(|v| v.as_array()) else { continue };
                let vars: Vec<String> = vars_arr.iter().filter_map(|x| x.as_str().map(String::from)).collect();
                let Some(pred) = law.get("body") else { continue };
                let pred = law_about(pred, target);
                let Ok(var_sorts) = infer_var_sorts(&pred, &vars, Some(&c.param_sorts)) else { continue };
                let mut lenv: BTreeMap<String, Sort> = var_sorts.clone();
                let Ok(body) = lower(&pred, &mut lenv, None) else { continue };
                let mut guards: Vec<String> = Vec::new();
                let calls = collect_self_applications_of(&pred, target);
                for call in &calls {
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
                if first && CONTRACT_QUANTIFIERS.with(|q| q.get()) {
                    out.push_str(&format!(";; certified law of {target}\n(assert (forall ({lbinders}) {guarded}))\n"));
                }
                // The law's instance at each application the obligation makes, when one of the law's
                // own applications of the callee fixes every law variable (distinct variables covering
                // them): the variables take the obligation's argument terms, positionally.
                let distinct_names = |call: &Vec<J>| -> Option<Vec<String>> {
                    let names: Vec<&str> = call.iter().filter_map(|a| var_name(a)).collect();
                    (names.len() == call.len() && names.iter().collect::<BTreeSet<_>>().len() == names.len()).then(|| names.into_iter().map(String::from).collect())
                };
                let fixing = calls.iter().find(|call| distinct_names(call).is_some_and(|names| vars.iter().all(|v| names.contains(v))));
                if let Some(call) = fixing {
                    let names: Vec<String> = call.iter().filter_map(|a| var_name(a).map(String::from)).collect();
                    for args in &at {
                        push_inst(&mut inst, format!("(assert {})\n", let_params(&names, args, &guarded)));
                    }
                } else if retry {
                    // The callee-side divisibility rung: a law one of whose variables no application
                    // fixes (`d` in gcd's greatest-divisor law, `a` in associativity) ranges over the
                    // obligation's applications of this callee and their pairwise PRODUCTS — the witness
                    // `e·g` that lowest terms after normalization needs (measured by hand first: with it,
                    // unsat; without, sat). EVERY such law, not only the ones that speak of `mod`:
                    // restricting to divisibility laws lost the proof (measured — z3's path uses the
                    // commutativity/associativity instances at those terms too), so the rung's query
                    // has its own budget instead (`DIVISIBILITY_BUDGET_SECS`).
                    let partial = calls.iter().find_map(|call| {
                        let names = distinct_names(call)?;
                        let free: Vec<&String> = vars.iter().filter(|v| !names.contains(v)).collect();
                        (free.len() == 1 && var_sorts.get(free[0].as_str()) == Some(&Sort::Int)).then(|| (names, free[0].clone()))
                    });
                    if let Some((mut names, free)) = partial {
                        names.push(free.clone());
                        let candidates = if witness { free_var_candidates_for(&c, &at) } else { free_var_candidates(&c.symbol, &at, true) };
                        for t in candidates {
                            for args in &at {
                                let mut with = args.clone();
                                with.push(t.clone());
                                push_inst(&mut inst, format!(";; certified law of {target} at ({} {}) with {free} = {t}\n(assert {})\n", c.symbol, args.join(" "), let_params(&names, &with, &guarded)));
                            }
                        }
                    }
                    // A law TWO applications fix jointly (`rat_normalize`'s `cross`: `self(n, d)` and
                    // `self(m, e)`, every variable in one of them): instantiated over ordered PAIRS of
                    // the obligation's applications — the two normal forms the goal compares (the
                    // witness rung).
                    let pair = if !witness { None } else { calls.iter().enumerate().find_map(|(i, c1)| {
                        let n1 = distinct_names(c1)?;
                        calls.iter().skip(i + 1).find_map(|c2| {
                            let n2 = distinct_names(c2)?;
                            let all: BTreeSet<&String> = n1.iter().chain(n2.iter()).collect();
                            (all.len() == n1.len() + n2.len() && vars.iter().all(|v| all.contains(v))).then(|| (n1.clone(), n2))
                        })
                    }) };
                    if let Some((n1, n2)) = pair {
                        let names: Vec<String> = n1.iter().chain(n2.iter()).cloned().collect();
                        let mut count = 0usize;
                        for (i, a1) in at.iter().enumerate() {
                            for (j, a2) in at.iter().enumerate() {
                                if i == j || count >= MAX_PAIR_INSTANCES {
                                    continue;
                                }
                                let with: Vec<String> = a1.iter().chain(a2.iter()).cloned().collect();
                                push_inst(&mut inst, format!(";; certified law of {target} at the pair ({} {}) / ({} {})\n(assert {})\n", c.symbol, a1.join(" "), c.symbol, a2.join(" "), let_params(&names, &with, &guarded)));
                                count += 1;
                            }
                        }
                    }
                }
            }
            // The divisibility lemma instances at this callee's applications, when its laws include a
            // divisibility fact and the solver has discharged the lemmas (the rung is a retry).
            if retry
                && c.ret == Sort::Int
                && c.laws.iter().any(|l| serde_json::to_string(l).unwrap_or_default().contains("\"mod\""))
                && DIVISIBILITY_SOLVER.with(|s| s.borrow().as_deref().map(crate::euclid::divisibility_discharged).unwrap_or(false))
            {
                push_inst(&mut inst, divisibility_instances(&c.symbol, &at));
            }
        }
        // Lowering the axioms marked callees used again — the ones the obligation applies, and any a
        // contract applies on its own: those need a declaration too (their axioms are not needed; a
        // contract's mention of them is a term, not a use under contract).
        for target in take_callees_used() {
            if !declared.contains(&target) {
                if let Some(c) = callee_contract(&target) {
                    decls.push_str(&declare(&c));
                    declared.insert(target.clone());
                }
            }
            if witness {
                used.insert(target);
            }
        }
        if !witness || new_apps.is_empty() || round == 2 {
            break;
        }
        for (t, more) in new_apps {
            apps.entry(t).or_default().extend(more);
        }
    }
    take_callee_apps();
    Ok((format!("{decls}{out}"), inst, any))
}

/// The most applications the transitive-instance rounds add (the retry only).
const MAX_TRANSITIVE_APPS: usize = 24;

/// A term over the callee's parameters read at an instance: the parameters SUBSTITUTED by the
/// instance's argument terms, simultaneously (a `let` would do, but as an application key it must
/// match the terms the goal spells — `a`, not `(let ((n a) (d b)) n)`).
fn substitute_params(params: &[String], args: &[String], term: &str) -> String {
    let mut t = term.to_string();
    for (i, p) in params.iter().enumerate() {
        t = replace_token(&t, p, &format!("\u{1}{i}\u{1}"));
    }
    for (i, a) in args.iter().enumerate() {
        t = t.replace(&format!("\u{1}{i}\u{1}"), a);
    }
    t
}
/// The most ordered pairs a two-application law is instantiated at (the retry only).
const MAX_PAIR_INSTANCES: usize = 40;

/// WITNESS CALLEES (the retry only, 2026-10-31): a contract E in the installed set with a post
/// `fst(result) = G(p…)` or `result = G(p…)` — E COMPUTES G, with more (`ext_gcd` returns the gcd
/// beside its Bézout coefficients) — is applied at every application of G the certificate has, so
/// its contract (the Bézout identity, Euclid's lemma) speaks about exactly the gcd terms the goal
/// mentions. A record that applies nothing of E reaches E's facts this way; the view's contract
/// closure brings E in (`LinkedView::contracts_closure`, the witness scan). Bounded per G.
fn witness_callees(used: &mut BTreeSet<String>, apps: &mut BTreeMap<String, Vec<Vec<String>>>) {
    let contracts: BTreeMap<String, CalleeContract> = CALLEE_CONTRACTS.with(|c| c.borrow().clone());
    let g_targets: Vec<String> = used.iter().cloned().collect();
    for g in g_targets {
        let Some(g_apps) = apps.get(&g).cloned() else { continue };
        if g_apps.is_empty() {
            continue;
        }
        for (e_target, e) in &contracts {
            if *e_target == g {
                continue;
            }
            // A post equating the (first component of the) result with an application of G to E's
            // parameters, each a distinct parameter.
            let positions: Option<Vec<usize>> = e.posts.iter().find_map(|post| {
                if post.get("op").and_then(|o| o.as_str()) != Some("eq") {
                    return None;
                }
                let args = post.get("args").and_then(|a| a.as_array())?;
                if args.len() != 2 {
                    return None;
                }
                let is_result_side = |n: &J| {
                    var_name(n) == Some("result")
                        || (n.get("op").and_then(|o| o.as_str()) == Some("fst") && n.get("args").and_then(|a| a.as_array()).is_some_and(|a| a.len() == 1 && var_name(&a[0]) == Some("result")))
                };
                let g_side = if is_result_side(&args[0]) { &args[1] } else if is_result_side(&args[1]) { &args[0] } else { return None };
                if g_side.get("op").and_then(|o| o.as_str()) != Some(g.as_str()) {
                    return None;
                }
                let g_args = g_side.get("args").and_then(|a| a.as_array())?;
                // E's parameter at each of G's argument positions.
                let idx: Option<Vec<usize>> = g_args.iter().map(|a| var_name(a).and_then(|n| e.params.iter().position(|p| p == n))).collect();
                let idx = idx?;
                if idx.iter().collect::<BTreeSet<_>>().len() != idx.len() || idx.len() != e.params.len() {
                    return None;
                }
                Some(idx)
            });
            let Some(idx) = positions else { continue };
            let mut added = 0usize;
            for ga in &g_apps {
                if ga.len() != idx.len() {
                    continue;
                }
                // E's arguments in E's parameter order: parameter k is G's argument at the position
                // where the post puts parameter k.
                let mut e_args: Vec<String> = vec![String::new(); e.params.len()];
                for (pos, k) in idx.iter().enumerate() {
                    e_args[*k] = ga[pos].clone();
                }
                let slot = apps.entry(e_target.clone()).or_default();
                if !slot.contains(&e_args) && added < MAX_WITNESS_APPS {
                    slot.push(e_args);
                    added += 1;
                    used.insert(e_target.clone());
                }
            }
        }
    }
}

/// The most applications a witness callee gains per witnessed callee.
const MAX_WITNESS_APPS: usize = 8;

/// The candidate terms for a callee law's one free Int variable: the callee's own application
/// terms and their pairwise products when the callee returns an Int (the divisor `e·g` of the
/// lowest-terms proof); when it returns a tuple (`ext_gcd`), the Int-sorted ARGUMENTS of its
/// applications instead — the `s` of Euclid's lemma is a denominator the goal holds, never the
/// application itself.
fn free_var_candidates_for(c: &CalleeContract, at: &[Vec<String>]) -> Vec<String> {
    if c.ret == Sort::Int {
        return free_var_candidates(&c.symbol, at, true);
    }
    let mut terms: Vec<String> = Vec::new();
    for args in at {
        for (i, a) in args.iter().enumerate() {
            if c.param_sorts.get(i) == Some(&Sort::Int) && !terms.contains(a) {
                terms.push(a.clone());
            }
        }
    }
    terms.truncate(MAX_FREE_TERMS);
    terms
}

/// Sort returned by `op` given the sorts of its operands are well-formed.
fn op_result_sort(op: &str) -> Option<Sort> {
    Some(match op {
        "add" | "sub" | "mul" | "neg" | "abs" | "min" | "max" | "mod" | "div" | "str_length" => Sort::Int,
        "eq" | "neq" | "lt" | "le" | "gt" | "ge" | "and" | "or" | "xor" | "not" | "str_contains"
        | "str_lt" => Sort::Bool,
        "str_concat" => Sort::Str,
        "length" | "head" if lists_in_fragment() => Sort::Int,
        "null" if lists_in_fragment() => Sort::Bool,
        "cons" | "tail" | "append" | "reverse" if lists_in_fragment() => Sort::Lst,
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
            Some("list") if lists_in_fragment() => Ok(Sort::Lst),
            other => bail!("unsupported literal kind: {other:?}"),
        },
        "var" => {
            let name = node.get("name").and_then(|n| n.as_str()).unwrap_or_default();
            match env.get(name).copied() {
                Some(s) => Ok(s),
                None if name == "nil" && lists_in_fragment() => Ok(Sort::Lst),
                None => bail!("free variable `{name}` (out of fragment)"),
            }
        }
        "let" => {
            let mut e2 = env.clone();
            let name = node.get("name").and_then(|n| n.as_str()).ok_or_else(|| anyhow!("let has no name"))?;
            let vsort = sort_of(node.get("value").ok_or_else(|| anyhow!("let has no value"))?, env, self_def)?;
            e2.insert(name.to_string(), vsort);
            sort_of(node.get("body").ok_or_else(|| anyhow!("let has no body"))?, &e2, self_def)
        }
        "case" => {
            // Sort of the first arm's body (all arms share a sort if well-formed), under the names its
            // pattern binds (a tuple destructure, a variant payload, a bind of the scrutinee).
            let arms = node.get("arms").and_then(|a| a.as_array()).ok_or_else(|| anyhow!("case has no arms"))?;
            let arm = arms.first().ok_or_else(|| anyhow!("case has no arms"))?;
            let scrut = node.get("scrutinee").ok_or_else(|| anyhow!("case has no scrutinee"))?;
            let mut e2 = env.clone();
            if let Some(pat) = arm.get("pattern") {
                let ssort = sort_of(scrut, env, self_def)?;
                let mut binds = Vec::new();
                match pat.get("kind").and_then(|k| k.as_str()) {
                    Some("variant") => {
                        if let (Some(sub), Sort::Data(i)) = (pat.get("pattern").or(pat.get("payload")), ssort) {
                            let tag = pat.get("tag").and_then(|t| t.as_str()).unwrap_or_default();
                            if let Some(Datatype::Sum(vs)) = datatype(i) {
                                if let Some((_, Some(ps))) = vs.iter().find(|(t, _)| t == tag) {
                                    bind_pattern(sub, "_", *ps, &mut binds)?;
                                }
                            }
                        }
                    }
                    Some("lit") => {}
                    _ => bind_pattern(pat, "_", ssort, &mut binds)?,
                }
                for (n, _, s) in binds {
                    e2.insert(n, s);
                }
            }
            sort_of(arm.get("body").ok_or_else(|| anyhow!("arm has no body"))?, &e2, self_def)
        }
        "tuple" => {
            let elems = node.get("elems").and_then(|e| e.as_array()).ok_or_else(|| anyhow!("tuple has no elems"))?;
            let sorts: Vec<Sort> = elems.iter().map(|e| sort_of(e, env, self_def)).collect::<Result<_>>()?;
            Ok(register_datatype(Datatype::Tuple(sorts)))
        }
        "variant" => {
            let tag = node.get("tag").and_then(|t| t.as_str()).ok_or_else(|| anyhow!("variant has no tag"))?;
            let payload = match node.get("payload") {
                Some(pl) => Some(sort_of(pl, env, self_def)?),
                None => None,
            };
            sum_sort_with_tag(tag, payload)
        }
        "app" => {
            let op = head_op(node).ok_or_else(|| anyhow!("application with no resolvable head"))?;
            if op == "fst" || op == "snd" {
                let arg = node.get("args").and_then(|a| a.as_array()).and_then(|a| a.first()).ok_or_else(|| anyhow!("{op} needs an arg"))?;
                let Sort::Data(i) = sort_of(arg, env, self_def)? else { bail!("{op} applied to a non-tuple (out of fragment)") };
                let Some(Datatype::Tuple(elems)) = datatype(i) else { bail!("{op} applied to a non-tuple (out of fragment)") };
                let j = if op == "fst" { 0 } else { 1 };
                return elems.get(j).copied().ok_or_else(|| anyhow!("{op} on a tuple with fewer than {} components", j + 1));
            }
            if op.starts_with("fn_") {
                return callee_contract(&op).map(|c| c.ret).ok_or_else(|| anyhow!("{}", no_contract_reason(&op)));
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
            Some("list") => Some("list"),
            _ => None,
        },
        J::Bool(_) => Some("bool"),
        J::Number(n) => Some(if n.is_i64() || n.is_u64() { "int" } else { "float" }),
        J::String(_) => Some("string"),
        _ => None,
    }
}

/// The payload of a literal in either spelling (the structured form's `value`, or the bare literal itself).
pub(crate) fn lit_payload(value: &J) -> &J {
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
        // A list literal (the structured value form) is a cons spine ending in nil.
        Some("list") if lists_in_fragment() => {
            let elems = raw.get("elems").and_then(|e| e.as_array()).cloned().unwrap_or_default();
            let mut acc = "nil".to_string();
            for e in elems.iter().rev() {
                acc = format!("(cons {} {acc})", lower_lit(e)?);
            }
            Ok(acc)
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
            } else if name == "nil" && lists_in_fragment() {
                Ok("nil".to_string())
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
            let body_node = node.get("body").ok_or_else(|| anyhow!("let has no body"))?;
            let body = with_locals(&[(name.clone(), vsmt.clone())], || lower(body_node, &mut e2, self_def))?;
            Ok(format!("(let (({name} {vsmt})) {body})"))
        }
        "case" => lower_case(node, env, self_def),
        "app" => lower_app(node, env, self_def),
        "tuple" => {
            let Sort::Data(i) = sort_of(node, env, self_def)? else { bail!("tuple without a tuple sort") };
            let elems = node.get("elems").and_then(|e| e.as_array()).ok_or_else(|| anyhow!("tuple has no elems"))?;
            let lowered: Vec<String> = elems.iter().map(|e| lower(e, env, self_def)).collect::<Result<_>>()?;
            Ok(format!("(mk_dt{i} {})", lowered.join(" ")))
        }
        "variant" => {
            let Sort::Data(i) = sort_of(node, env, self_def)? else { bail!("variant without a sum sort") };
            let tag = node.get("tag").and_then(|t| t.as_str()).ok_or_else(|| anyhow!("variant has no tag"))?;
            match node.get("payload") {
                Some(pl) => Ok(format!("(dt{i}_{tag} {})", lower(pl, env, self_def)?)),
                None => Ok(format!("dt{i}_{tag}")),
            }
        }
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
    let mut variant_arms: Vec<(String, String, String)> = Vec::new(); // (tester, tag, lowered body)
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
                let b = with_locals(&[(name.to_string(), scrut_smt.clone())], || lower(body, &mut e2, self_def))?;
                default = Some(format!("(let (({name} {scrut_smt})) {b})"));
            }
            Some("lit") => {
                let litv = lower_lit(pat.get("value").ok_or_else(|| anyhow!("lit pattern has no value"))?)?;
                lit_arms.push((litv, body));
            }
            // A tuple destructure is total: the arm's body under the component bindings is the default.
            Some("tuple") => {
                let mut binds = Vec::new();
                bind_pattern(pat, &scrut_smt, ssort, &mut binds)?;
                let mut e2 = env.clone();
                for (n, _, s) in &binds {
                    e2.insert(n.clone(), *s);
                }
                let locals: Vec<(String, String)> = binds.iter().map(|(n, t, _)| (n.clone(), t.clone())).collect();
                let b = with_locals(&locals, || lower(body, &mut e2, self_def))?;
                default = Some(wrap_lets(&binds, b));
            }
            // Variant arms: tester-guarded, the payload bound by its accessor.
            Some("variant") => {
                let Sort::Data(i) = ssort else { bail!("variant pattern against a non-sum scrutinee") };
                let Some(Datatype::Sum(vs)) = datatype(i) else { bail!("variant pattern against a non-sum scrutinee") };
                let tag = pat.get("tag").and_then(|t| t.as_str()).ok_or_else(|| anyhow!("variant pattern has no tag"))?;
                let Some((_, payload)) = vs.iter().find(|(t, _)| t == tag) else { bail!("variant `{tag}` is not a constructor of the scrutinee's sum") };
                let mut binds = Vec::new();
                if let (Some(sub), Some(ps)) = (pat.get("pattern").or(pat.get("payload")), payload) {
                    bind_pattern(sub, &format!("(dt{i}_{tag}_v {scrut_smt})"), *ps, &mut binds)?;
                }
                let mut e2 = env.clone();
                for (n, _, s) in &binds {
                    e2.insert(n.clone(), *s);
                }
                let locals: Vec<(String, String)> = binds.iter().map(|(n, t, _)| (n.clone(), t.clone())).collect();
                let b = wrap_lets(&binds, with_locals(&locals, || lower(body, &mut e2, self_def))?);
                variant_arms.push((format!("((_ is dt{i}_{tag}) {scrut_smt})"), tag.to_string(), b));
            }
            other => bail!("unsupported case pattern `{other:?}` (out of fragment)"),
        }
    }
    // Variant arms fold like literal arms; with no default, the arms must cover every constructor and
    // the last one is the base.
    if !variant_arms.is_empty() {
        let base = match default.take() {
            Some(d) => d,
            None => {
                let Sort::Data(i) = ssort else { unreachable!() };
                let Some(Datatype::Sum(vs)) = datatype(i) else { unreachable!() };
                if !vs.iter().all(|(t, _)| variant_arms.iter().any(|(_, tag, _)| tag == t)) {
                    bail!("non-exhaustive variant case (out of fragment)");
                }
                variant_arms.pop().map(|(_, _, b)| b).unwrap()
            }
        };
        let mut acc = base;
        for (cond, _, b) in variant_arms.into_iter().rev() {
            acc = format!("(ite {cond} {b} {acc})");
        }
        return Ok(acc);
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
            bail!("{}", no_contract_reason(&op));
        };
        if args.len() != c.params.len() {
            bail!("`{op}` applied to {} args, expects {}", args.len(), c.params.len());
        }
        let lowered = lower_args(env, &args)?;
        mark_callee_used(&op);
        record_callee_app(&op, &lowered);
        return Ok(if lowered.is_empty() { c.symbol.clone() } else { format!("({} {})", c.symbol, lowered.join(" ")) });
    }

    // Tuple projections: accessors of the argument's tuple datatype.
    if op == "fst" || op == "snd" {
        let arg = args.first().ok_or_else(|| anyhow!("{op} needs an arg"))?;
        let Sort::Data(i) = sort_of(arg, env, self_def)? else { bail!("{op} applied to a non-tuple (out of fragment)") };
        let j = if op == "fst" { 0 } else { 1 };
        return Ok(format!("(dt{i}_{j} {})", lower(arg, env, self_def)?));
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
        // Lists (the induction rung only): the datatype's constructors, selectors and tester, and the
        // recursive definitions the structural backend states (`list_preamble`).
        "cons" if lists_in_fragment() => Ok(format!("(cons {} {})", lower(args[0], env, self_def)?, lower(args[1], env, self_def)?)),
        "head" if lists_in_fragment() => Ok(format!("(hd {})", lower(args[0], env, self_def)?)),
        "tail" if lists_in_fragment() => Ok(format!("(tl {})", lower(args[0], env, self_def)?)),
        "null" if lists_in_fragment() => Ok(format!("((_ is nil) {})", lower(args[0], env, self_def)?)),
        "length" if lists_in_fragment() => {
            mark_list_def("length");
            Ok(format!("(length {})", lower(args[0], env, self_def)?))
        }
        "append" if lists_in_fragment() => {
            mark_list_def("append");
            Ok(format!("(append {} {})", lower(args[0], env, self_def)?, lower(args[1], env, self_def)?))
        }
        "reverse" if lists_in_fragment() => {
            mark_list_def("reverse");
            Ok(format!("(reverse {})", lower(args[0], env, self_def)?))
        }
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
            Some("list") if lists_in_fragment() => Some(Sort::Lst),
            _ => None,
        },
        Some("var") if node.get("name").and_then(|n| n.as_str()) == Some("nil") && lists_in_fragment() => Some(Sort::Lst),
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
            "length" | "head" | "tail" | "null" | "reverse" | "append" if lists_in_fragment() => Some(Sort::Lst),
            _ => None, // eq/neq/apply/self/id: don't constrain directly
        };
        if let Some(s) = operand_sort {
            for a in &args {
                if let Some(n) = var_name(a) {
                    constrain(n, s, vars, sorts)?;
                }
            }
        }
        // `cons(h, t)`: an Int head onto a list tail.
        if op == "cons" && lists_in_fragment() {
            if let Some(n) = args.first().and_then(|a| var_name(a)) {
                constrain(n, Sort::Int, vars, sorts)?;
            }
            if let Some(n) = args.get(1).and_then(|a| var_name(a)) {
                constrain(n, Sort::Lst, vars, sorts)?;
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
        let list_core = matches!(op.as_str(), "length" | "head" | "tail" | "reverse" | "cons" | "append" | "null");
        if (list_core && !lists_in_fragment())
            || matches!(op.as_str(), "last" | "init" | "map" | "filter" | "foldl" | "foldr" | "concat"
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
    for key in ["body", "value", "scrutinee", "payload"] {
        if let Some(child) = node.get(key) {
            visit_for_sorts(child, vars, sorts, self_param_sorts)?;
        }
    }
    if let Some(elems) = node.get("elems").and_then(|e| e.as_array()) {
        for e in elems {
            visit_for_sorts(e, vars, sorts, self_param_sorts)?;
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
    take_callee_apps();
    reset_list_emission();
    register_declared_self_type();
    // Normalize away curried application spines first, so surface-parsed properties/bodies
    // (juxtaposition parses curried) analyze identically to AST-authored flat ones.
    let prop_expr = &uncurry(prop_expr);
    let body_flat = body.map(uncurry);
    let body = body_flat.as_ref();
    // A CLOSED predicate — no `forall`, or one with no variables — is an obligation with no
    // quantified variables, not a refusal: the Gaussian tier's `i² = −1` (`fst(self(0, 1, 0, 1))
    // == 0 - 1`) and "the four units" read UNSUPPORTED "not a `forall`" although one solver call
    // decides them (2026-10-30).
    let is_forall = prop_expr.get("kind").and_then(|k| k.as_str()) == Some("forall");
    let vars: Vec<String> = if is_forall {
        prop_expr
            .get("vars")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let pred = if is_forall { prop_expr.get("body").ok_or_else(|| anyhow!("`forall` has no body"))? } else { prop_expr };

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
    let pred_smt = lower_goal(pred, &mut env, self_def.as_ref())?;
    // The body's callee applications, at every `self` application the goal makes: the parameters
    // bound to the goal's argument terms, so the contract instances land where the inlined body
    // applies the callee (the first-order twin of the induction rung's unfolding instances).
    let body_apps = take_body_apps();
    if let (Some(sd), false) = (&self_def, body_apps.is_empty()) {
        for call in collect_self_applications(pred) {
            if call.len() != sd.params.len() {
                continue;
            }
            let Ok(terms) = call.iter().map(|a| lower(a, &mut env.clone(), self_def.as_ref())).collect::<Result<Vec<_>>>() else { continue };
            for (target, args) in &body_apps {
                let scoped: Vec<String> = args.iter().map(|a| let_params(&sd.params, &terms, a)).collect();
                push_callee_app(target, &scoped);
            }
        }
    }

    let mut out = String::new();
    out.push_str("(set-logic ALL)\n");
    let (callees, callee_insts, uses_callees) = callee_preamble()?;
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
    out.push_str(&callee_insts);
    // Assert the negation of the law: unsat ⇒ the law holds for all inputs.
    out.push_str(&format!("(assert (not {pred_smt}))\n"));
    out.push_str("(check-sat)\n");
    out.push_str("(get-model)\n");

    Ok(Certificate { smt: with_datatypes(out), quantified, uses_self, uses_callees, euclid: false, steps: None, stepwise: None, staged: None })
}

pub(crate) fn references_self(node: &J) -> bool {
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
/// z3's virtual-memory cap per query (MB); `NL_SMT_MEMORY_MB` overrides.
const SOLVER_MEMORY_MB: u64 = 6144;

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
        // A memory cap beside the time cap: z3 does not honour its soft timeout while
        // allocating, and one query on this machine reached 13 GB in 30 s (2026-10-29, the
        // integer-tier builder's last records) — past the box, not past the budget. Over the
        // cap z3 answers `(error "out of memory")`, read below as undecided; the heaviest
        // query the project's gauntlets run (`pad_to`'s list induction) peaks at 3.4 GB.
        let mb = std::env::var("NL_SMT_MEMORY_MB").ok().and_then(|v| v.parse::<u64>().ok()).unwrap_or(SOLVER_MEMORY_MB);
        cmd.arg(format!("-memory:{mb}"));
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
        // Over the memory cap (`(error "out of memory")`): undecided, like a timeout.
        _ if stdout.contains("out of memory") || stdout.contains("memory") => Ok(SatAnswer::Unknown),
        _ => Err(anyhow!("unexpected solver output: {}", stdout.trim())),
    }
}

/// Run a solver on a certificate, mapping the SAT answer to a proof outcome.
pub fn run_solver(cert: &Certificate, solver: &str) -> Result<ProofOutcome> {
    run_solver_secs(cert, solver, SOLVER_TIMEOUT_SECS)
}

/// The budget of the callee-side divisibility rung's query: the LAST rung of the first-order
/// ladder, reached only after every cheaper one answered unknown, so a longer budget costs
/// nothing where it is not needed and buys the proofs that sit at the default's edge — the
/// lowest-terms post's closing query takes 3.2 s under z3 4.13 and 10.3 s under z3 4.16 (CI's
/// pin), both past 5 s under a parallel test load. Solver version is part of the measurement.
const DIVISIBILITY_BUDGET_SECS: u64 = 30;

/// [`run_solver`] with an explicit per-check budget.
pub fn run_solver_secs(cert: &Certificate, solver: &str, secs: u64) -> Result<ProofOutcome> {
    Ok(match run_smt_secs(&cert.smt, solver, secs)? {
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
    let prev = DIVISIBILITY_SOLVER.with(|s| s.replace(Some(solver.to_string())));
    let out = prove_property_in_domain_inner(prop_expr, body, self_nat_positions, solver);
    DIVISIBILITY_SOLVER.with(|s| *s.borrow_mut() = prev);
    out
}

fn prove_property_in_domain_inner(
    prop_expr: &J,
    body: Option<&J>,
    self_nat_positions: &BTreeSet<usize>,
    solver: &str,
) -> (ProofOutcome, Option<Certificate>) {
    let cert = match build_certificate_in_domain(prop_expr, body, self_nat_positions) {
        Err(e) => return (ProofOutcome::Unsupported(format!("{e:#}")), None),
        Ok(c) => c,
    };
    match run_solver(&cert, solver) {
        Ok(ProofOutcome::Unknown) => {}
        Ok(outcome) => return (outcome, Some(cert)),
        Err(e) => return (ProofOutcome::Unsupported(format!("solver error: {e:#}")), Some(cert)),
    }
    // The contract ladder: an obligation over linked callees that z3 cannot decide WITH the
    // quantified contract axioms is retried with the contracts stated only at its own
    // applications (their instances — weaker, so as sound), which is where nonlinear
    // consequences close.
    let mut last = cert;
    if last.uses_callees {
        if let Ok(cert2) = without_contract_quantifiers(|| build_certificate_in_domain(prop_expr, body, self_nat_positions)) {
            match run_solver(&cert2, solver) {
                Ok(ProofOutcome::Proved) => return (ProofOutcome::Proved, Some(cert2)),
                _ => last = cert2,
            }
        }
    }
    // The GROUND rung (see `ground`): the instances-only certificate with its callee applications
    // as constants and its tuples reduced, in QF_NIA — where z3's nonlinear engine decides what
    // the combination solver with uninterpreted symbols did not (the Gaussian tier's Euclidean
    // property, degree 4: under a second here, undecided in 90 s before). Weaker than the
    // certificate it rewrites, so its unsat is the certificate's.
    if last.uses_callees {
        if let Some(ground) = crate::ground::ackermannize(&last.smt) {
            if matches!(run_smt_secs(&ground, solver, SOLVER_TIMEOUT_SECS), Ok(SatAnswer::Unsat)) {
                last.smt = ground;
                return (ProofOutcome::Proved, Some(last));
            }
        }
    }
    // The Euclidean rung: the same obligation with the residue normal forms of its `mod`/`div`
    // terms stated as instances of the division-uniqueness lemma (see `euclid`).
    if euclid_ladder(&mut last, solver) {
        return (ProofOutcome::Proved, Some(last));
    }
    // The callee-side divisibility rung (see `DIVISIBILITY_CALLEES`): the instances-only
    // certificate with a callee law's free variable ranging over the callee's applications and
    // their products, and the divisibility lemmas instantiated there — then the Euclidean rung
    // over that. The lowest-terms post of the rational tier closes here (2026-10-30).
    if last.uses_callees {
        if let Ok(mut cert3) = with_divisibility_callees(|| without_contract_quantifiers(|| build_certificate_in_domain(prop_expr, body, self_nat_positions))) {
            if cert3.smt != last.smt {
                match run_solver_secs(&cert3, solver, DIVISIBILITY_BUDGET_SECS) {
                    Ok(ProofOutcome::Proved) => return (ProofOutcome::Proved, Some(cert3)),
                    _ => {
                        // The Euclidean instances over the rung's certificate, at the rung's
                        // budget (the lowest-terms post closes HERE: the product witness needs the
                        // quotients' decompositions), then the ordinary Euclidean ladder.
                        if crate::euclid::lemma_discharged(solver) {
                            if let Some(aug) = crate::euclid::augment(&cert3.smt) {
                                if matches!(run_smt_secs(&aug, solver, DIVISIBILITY_BUDGET_SECS), Ok(SatAnswer::Unsat)) {
                                    cert3.smt = aug;
                                    cert3.euclid = true;
                                    return (ProofOutcome::Proved, Some(cert3));
                                }
                            }
                        }
                        if euclid_ladder(&mut cert3, solver) {
                            return (ProofOutcome::Proved, Some(cert3));
                        }
                        last = cert3;
                    }
                }
            }
        }
    }
    // THE WITNESS RUNG + THE STAGED RUNG (2026-10-31): the instances-only certificate built once more
    // with the witness callees, transitive instances and pair instances (`WITNESS_CALLEES`), then
    // STAGED — each conditional instance's disjuncts refuted one at a time under the facts in force,
    // the conclusions drawn carried into a final query over opaque div/mod. Uniqueness of lowest
    // terms: every step the solver decides alone, the one-shot query it never does (measured). The
    // one-shot form of this certificate is not even tried: it is the staged form that closes.
    if last.uses_callees {
        if let Ok(cert4) = with_witness_callees(|| with_divisibility_callees(|| without_contract_quantifiers(|| build_certificate_in_domain(prop_expr, body, self_nat_positions)))) {
            if let Some(st) = crate::staged::retry_staged(&cert4.smt, solver) {
                let mut cert = cert4;
                cert.smt = st.final_smt;
                cert.steps = Some(st.steps_smt);
                cert.staged = Some((st.branches, st.derived));
                return (ProofOutcome::Proved, Some(cert));
            }
        }
    }
    (ProofOutcome::Unknown, Some(last))
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
    /// A measure that is a CALLEE of several positions (`terminate::MeasuredBy`): every application
    /// puts the same distinct quantified variables there, in order — strong induction on the measure
    /// callee's value at them (`N(c, d)`), well-founded because that value is certified bounded below.
    Vars(Vec<String>),
}

/// The measure positions of a measured (callee) measure, read off the law's applications: all
/// literals → unfolding; the same distinct variables everywhere → [`MeasureSpec::Vars`].
fn measured_spec(pred: &J, positions: &[usize], vars: &[String]) -> Result<MeasureSpec> {
    let mut tuple: Option<Vec<String>> = None;
    let mut any = false;
    for call in collect_self_applications(pred) {
        any = true;
        let args: Vec<&J> = positions
            .iter()
            .map(|p| call.get(*p).ok_or_else(|| anyhow!("a `self` application in the law has no argument at the measure position {p}")))
            .collect::<Result<_>>()?;
        if args.iter().all(|a| a.get("kind").and_then(|k| k.as_str()) == Some("lit")) {
            continue;
        }
        let names: Option<Vec<String>> = args.iter().map(|a| var_name(a).filter(|v| vars.iter().any(|x| x == v)).map(String::from)).collect();
        let Some(names) = names else {
            bail!("the law applies `self` with a compound term at a position of its callee measure — the induction needs a quantified variable at each");
        };
        if names.iter().collect::<BTreeSet<_>>().len() != names.len() {
            bail!("the law applies `self` with one variable at two positions of its callee measure");
        }
        match &tuple {
            None => tuple = Some(names),
            Some(t) if *t == names => {}
            Some(_) => bail!("the law applies `self` with different variables at the positions of its callee measure across applications"),
        }
    }
    if !any {
        bail!("the law does not apply `self`");
    }
    Ok(match tuple {
        None => MeasureSpec::Literal,
        Some(vs) => MeasureSpec::Vars(vs),
    })
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

/// A body with every `let`-bound and tuple-/bind-pattern-bound local replaced by its definition — a
/// `let x = e in …` by `e`, the components of `case s of (p, q) => …` by `fst(s)` / `snd(s)`, a bare
/// bind by the scrutinee — so a term read out of it (a recursive call's arguments) makes sense in
/// the lambda's parameter scope. Variant and literal patterns bind nothing resolvable: their names
/// stay (and shadow). Nested lambdas' parameters shadow too.
fn resolve_body_locals(node: &J) -> J {
    fn go(node: &J, env: &BTreeMap<String, J>) -> J {
        let kind = node.get("kind").and_then(|k| k.as_str()).unwrap_or_default();
        match kind {
            "var" => node.get("name").and_then(|n| n.as_str()).and_then(|n| env.get(n)).cloned().unwrap_or_else(|| node.clone()),
            "let" => {
                let value = node.get("value").map(|v| go(v, env)).unwrap_or(J::Null);
                let mut e2 = env.clone();
                if let Some(n) = node.get("name").and_then(|n| n.as_str()) {
                    e2.insert(n.to_string(), value.clone());
                }
                let body = node.get("body").map(|b| go(b, &e2)).unwrap_or(J::Null);
                let mut m = node.as_object().cloned().unwrap_or_default();
                m.insert("value".into(), value);
                m.insert("body".into(), body);
                J::Object(m)
            }
            "lambda" => {
                let mut e2 = env.clone();
                for p in node.get("params").and_then(|p| p.as_array()).into_iter().flatten() {
                    if let Some(n) = p.get("name").and_then(|n| n.as_str()) {
                        e2.remove(n);
                    }
                }
                let mut m = node.as_object().cloned().unwrap_or_default();
                if let Some(b) = node.get("body") {
                    m.insert("body".into(), go(b, &e2));
                }
                J::Object(m)
            }
            "case" => {
                let scrut = node.get("scrutinee").map(|s| go(s, env)).unwrap_or(J::Null);
                let arms: Vec<J> = node
                    .get("arms")
                    .and_then(|a| a.as_array())
                    .into_iter()
                    .flatten()
                    .map(|arm| {
                        let mut e2 = env.clone();
                        let pat = arm.get("pattern").cloned().unwrap_or(J::Null);
                        let elems: Vec<&J> = pat.get("elems").and_then(|e| e.as_array()).map(|a| a.iter().collect()).unwrap_or_default();
                        match pat.get("kind").and_then(|k| k.as_str()) {
                            Some("bind") => {
                                if let Some(n) = pat.get("name").and_then(|n| n.as_str()) {
                                    e2.insert(n.to_string(), scrut.clone());
                                }
                            }
                            Some("tuple") if elems.len() == 2 => {
                                for (i, e) in elems.iter().enumerate() {
                                    if let (Some("bind"), Some(n)) = (e.get("kind").and_then(|k| k.as_str()), e.get("name").and_then(|n| n.as_str())) {
                                        let proj = if i == 0 { "fst" } else { "snd" };
                                        e2.insert(n.to_string(), serde_json::json!({ "kind": "app", "op": proj, "args": [scrut.clone()] }));
                                    }
                                }
                            }
                            _ => {
                                let mut bound = Vec::new();
                                fn binds(p: &J, out: &mut Vec<String>) {
                                    match p {
                                        J::Object(m) => {
                                            if m.get("kind").and_then(|k| k.as_str()) == Some("bind") {
                                                if let Some(n) = m.get("name").and_then(|n| n.as_str()) {
                                                    out.push(n.to_string());
                                                }
                                            }
                                            m.values().for_each(|v| binds(v, out));
                                        }
                                        J::Array(xs) => xs.iter().for_each(|v| binds(v, out)),
                                        _ => {}
                                    }
                                }
                                binds(&pat, &mut bound);
                                for n in bound {
                                    e2.remove(&n);
                                }
                            }
                        }
                        let mut m = arm.as_object().cloned().unwrap_or_default();
                        if let Some(b) = arm.get("body") {
                            m.insert("body".into(), go(b, &e2));
                        }
                        J::Object(m)
                    })
                    .collect();
                let mut m = node.as_object().cloned().unwrap_or_default();
                m.insert("scrutinee".into(), scrut);
                m.insert("arms".into(), J::Array(arms));
                J::Object(m)
            }
            _ => match node {
                J::Object(m) => J::Object(m.iter().map(|(k, v)| (k.clone(), if k == "pattern" { v.clone() } else { go(v, env) })).collect()),
                J::Array(xs) => J::Array(xs.iter().map(|v| go(v, env)).collect()),
                other => other.clone(),
            },
        }
    }
    go(node, &BTreeMap::new())
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
    let declared = declared_self_sorts(params.len());
    let mut inferred: BTreeMap<String, Option<Sort>> = params.iter().map(|p| (p.clone(), None)).collect();
    visit_for_sorts(inner, &params, &mut inferred, None)?;
    let param_sorts: Vec<Sort> = params
        .iter()
        .enumerate()
        .map(|(i, p)| declared.as_ref().and_then(|d| d.0[i]).or(inferred[p]).unwrap_or(Sort::Int))
        .collect();
    let candidates: Vec<Sort> = match declared.as_ref().and_then(|d| d.1) {
        Some(r) => vec![r],
        None if lists_in_fragment() => vec![Sort::Int, Sort::Bool, Sort::Str, Sort::Lst],
        None => vec![Sort::Int, Sort::Bool, Sort::Str],
    };
    let mut last_err = None;
    for cand in candidates {
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
    // The divisibility rung's lemma instances may enter the self-fact instances below, but only
    // for a solver that has discharged the lemmas — which the certificate builder must know.
    let prev = DIVISIBILITY_SOLVER.with(|s| s.replace(Some(solver.to_string())));
    let out = prove_by_numeric_induction_inner(prop_expr, body, nat_positions, solver);
    DIVISIBILITY_SOLVER.with(|s| *s.borrow_mut() = prev);
    out
}

fn prove_by_numeric_induction_inner(
    prop_expr: &J,
    body: Option<&J>,
    nat_positions: &BTreeSet<usize>,
    solver: &str,
) -> (ProofOutcome, Option<Certificate>, Option<String>) {
    // A ladder: UNFOLDING alone first (the instantiated defining equations, no hypothesis — enough for
    // `gcd(a, 0) = a` and, measured, for `gcd(a, b) = gcd(b, a)`, which the quantified hypothesis only
    // obscures: z3 answers unknown with it and unsat without), then the QUANTIFIED hypothesis, then
    // the hypothesis as INSTANCES at the recursive calls (the digit-sum bound closes on the
    // instances and not on the quantifier — measured, the recurring pattern), then the Euclidean
    // rung over the last certificate.
    let unfolding = match build_induction_certificate(prop_expr, body, nat_positions, IhMode::Off) {
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
    let mut last = unfolding;
    if last.1 != "<unfolding>" {
        for mode in [IhMode::Quantified, IhMode::Instances] {
            let (cert, m) = match build_induction_certificate(prop_expr, body, nat_positions, mode) {
                Err(e) => return (ProofOutcome::Unsupported(format!("{e:#}")), None, None),
                Ok(x) => x,
            };
            if cert.smt == last.0.smt {
                continue; // nothing new to state at this rung (a structural measure has no quantified form)
            }
            match run_smt(&cert.smt, solver) {
                Ok(SatAnswer::Unsat) => return (ProofOutcome::Proved, Some(cert), Some(m)),
                // SAT is not a counterexample: `self` is uninterpreted away from the instantiated points.
                Ok(SatAnswer::Sat(_)) | Ok(SatAnswer::Unknown) => last = (cert, m),
                Ok(SatAnswer::NoSolver) => return (ProofOutcome::NoSolver, Some(cert), Some(m)),
                Err(e) => return (ProofOutcome::Unsupported(format!("solver error: {e:#}")), Some(cert), Some(m)),
            }
        }
    }
    if euclid_ladder(&mut last.0, solver) {
        return (ProofOutcome::Proved, Some(last.0), Some(last.1));
    }
    (ProofOutcome::Unknown, Some(last.0), Some(last.1))
}

/// How the induction hypothesis enters a measure-induction certificate.
#[derive(Clone, Copy, PartialEq, Eq)]
enum IhMode {
    /// Not at all: the instantiated defining equations alone (unfolding).
    Off,
    /// As the universally quantified statement over smaller measures.
    Quantified,
    /// As explicit instances at every recursive call the unfoldings reach, each guarded by the
    /// measure decrease and the domain facts — the form the solver uses without having to
    /// instantiate a quantifier over nonlinear terms.
    Instances,
}

thread_local! {
    /// The function's OWN certified facts (its proved postconditions and laws, as a
    /// `CalleeContract` the linked view re-derived) to state at every `self` application a
    /// measure-induction certificate makes. Installed by the caller for one retry only — never
    /// while the linked view is computing that contract (the facts must not assume themselves).
    static SELF_FACTS: std::cell::RefCell<Option<CalleeContract>> = const { std::cell::RefCell::new(None) };
    /// The solver the running numeric induction will use (set by `prove_by_numeric_induction`):
    /// the divisibility rung states its lemma instances only for a solver that has discharged
    /// the lemmas (`euclid::divisibility_discharged`). `None` = no rung.
    static DIVISIBILITY_SOLVER: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
    /// The measure of the body under proof as the LINKED termination analysis found it — installed by
    /// a caller that holds the view (`set_self_measure`), since the measured descent (a callee of
    /// several positions decreasing by another callee's certified post, `terminate::MeasuredBy`) is
    /// not visible to the view-less `induction_measure` this rung falls back to. `None` = fall back.
    static SELF_MEASURE: std::cell::RefCell<Option<crate::terminate::Measure>> = const { std::cell::RefCell::new(None) };
    /// The divisibility rung for CALLEE laws (a retry in `prove_property_in_domain`): a callee law
    /// with a free variable instantiated over the obligation's applications of that callee &&
    /// their pairwise products — the product witness `e·g` the lowest-terms proof needs — plus
    /// the divisibility lemma instances at those applications. Off in the base certificate: the
    /// instances cost the solver time where they are not needed (lcm's plain certificate sits at
    /// the 5 s budget's edge), so they are a further rung, not the first.
    static DIVISIBILITY_CALLEES: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Build with the callee-side divisibility rung on.
fn with_divisibility_callees<T>(f: impl FnOnce() -> T) -> T {
    let prev = DIVISIBILITY_CALLEES.with(|c| c.replace(true));
    let out = f();
    DIVISIBILITY_CALLEES.with(|c| c.set(prev));
    out
}

thread_local! {
    /// THE WITNESS RUNG for callees (2026-10-31; a retry after the divisibility one): the
    /// preamble's rounds — witness callees applied at the applications of the callee they compute
    /// (`ext_gcd` at `gcd`'s), the applications inside instantiated posts as applications
    /// (transitive instances), a law two applications fix jointly instantiated over pairs, and a
    /// tuple-returning callee's free law variable ranging over its Int arguments. Off in every
    /// earlier rung: measured inside the divisibility retry, the added instances cost
    /// `rat_normalize`'s lowest-terms post its 30 s budget. The certificate it builds is the one
    /// the staged rung stages.
    static WITNESS_CALLEES: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn with_witness_callees<T>(f: impl FnOnce() -> T) -> T {
    let prev = WITNESS_CALLEES.with(|c| c.replace(true));
    let out = f();
    WITNESS_CALLEES.with(|c| c.set(prev));
    out
}

/// The candidate terms a law's free variable ranges over: the applications' terms and, when
/// `products` is set, their pairwise products (the divisor `e·g` of the lowest-terms proof).
fn free_var_candidates(symbol: &str, at: &[Vec<String>], products: bool) -> Vec<String> {
    let mut terms: Vec<String> = at.iter().map(|a| format!("({symbol} {})", a.join(" "))).collect();
    terms.sort();
    terms.dedup();
    terms.truncate(MAX_FREE_TERMS);
    if products {
        let base = terms.clone();
        for (i, a) in base.iter().enumerate() {
            for b in &base[i + 1..] {
                terms.push(format!("(* {a} {b})"));
            }
        }
        terms.truncate(MAX_FREE_TERMS * 2);
    }
    terms
}

/// The most self-application terms a law's free variable ranges over (the divisibility rung).
const MAX_FREE_TERMS: usize = 12;

/// THE DIVISIBILITY RUNG (2026-10-30). `gcd` associativity was "a fact no contract states" for ten
/// follow-ups; it follows from two facts the contract CAN state — `gcd` divides both arguments,
/// and every common divisor divides `gcd` (`∀ a b d. d | a ∧ d | b ⇒ d | gcd(a, b)`, PROVED by the
/// numeric induction with the Euclidean rung) — once (1) the second law is INSTANTIATED although no
/// application in it fixes `d` (`self_fact_instances` lets one free Int variable range over the
/// certificate's own self-application terms: the only candidates such a law needs are the gcds
/// the goal mentions), and (2) divisibility's transitivity and antisymmetry are stated at those
/// terms, here, as instances of two lemmas the solver discharges first. Measured by hand before
/// building: with both, z3 closes associativity in 10 ms; without transitivity, unknown; without
/// the greatest-divisor law, satisfiable. Emitted only when the self facts include a `mod … = 0`
/// law (a divisibility fact) and the solver has discharged the lemmas.
fn divisibility_instances(symbol: &str, at: &[Vec<String>]) -> String {
    let mut terms: Vec<String> = at.iter().map(|a| format!("({symbol} {})", a.join(" "))).collect();
    terms.sort();
    terms.dedup();
    if terms.len() < 2 {
        return String::new();
    }
    let mut out = String::from(";; the divisibility rung: transitivity, antisymmetry and scaling of `mod … = 0` at the certificate's applications (lemmas `div_trans`, `div_antisym`, `div_scale`, `div_bound`, discharged separately)\n");
    let mut seen: BTreeSet<String> = BTreeSet::new();
    // z | self(x, …) ∧ self(x, …) | x  ⇒  z | x: a divisor of an application divides its arguments
    // (through the application's own divides-both fact), for every other application z.
    for args in at {
        let m = format!("({symbol} {})", args.join(" "));
        for x in args {
            for z in &terms {
                if *z == m {
                    continue;
                }
                let inst = format!("(assert (=> (and (not (= {m} 0)) (not (= {z} 0)) (= (mod {x} {m}) 0) (= (mod {m} {z}) 0)) (= (mod {x} {z}) 0)))\n");
                if seen.insert(inst.clone()) {
                    out.push_str(&inst);
                }
            }
        }
    }
    // Two applications that divide each other are equal (both positive).
    for (i, g) in terms.iter().enumerate() {
        for h in &terms[i + 1..] {
            out.push_str(&format!("(assert (=> (and (>= {g} 1) (>= {h} 1) (= (mod {g} {h}) 0) (= (mod {h} {g}) 0)) (= {g} {h})))\n"));
        }
    }
    // Scaling: e | x ⇒ e·g | g·x, for an application e, each of its arguments x, and every
    // other application g — the step that carries a divisor of the quotient up to the product.
    for args in at {
        let e = format!("({symbol} {})", args.join(" "));
        for x in args {
            for g in &terms {
                if *g == e {
                    continue;
                }
                let inst = format!("(assert (=> (and (not (= {e} 0)) (not (= {g} 0)) (= (mod {x} {e}) 0)) (= (mod (* {g} {x}) (* {e} {g})) 0)))\n");
                if seen.insert(inst.clone()) {
                    out.push_str(&inst);
                }
            }
        }
    }
    out
}

/// Install the body-under-proof's measure from a LINKED termination analysis (see `SELF_MEASURE`);
/// returns the previous value for restoring.
pub fn set_self_measure(measure: Option<crate::terminate::Measure>) -> Option<crate::terminate::Measure> {
    SELF_MEASURE.with(|m| m.replace(measure))
}

/// Do the boolean `facts` entail `goal`, over the Int/Bool/String fragment with every variable
/// free? `facts ∧ ¬goal` UNSAT ⇒ yes; anything else (sat, unknown, out of fragment, no solver) ⇒
/// `false` — never a false entailment. Used by the linked termination analysis to discharge a
/// callee's precondition at a call site from the branch conditions in force. Leaves the prover's
/// callee bookkeeping as it found it (a callee the facts apply is a term here, not a use).
pub fn entails(facts: &[J], goal: &J, solver: &str) -> bool {
    let saved_used = take_callees_used();
    let saved_apps = take_callee_apps();
    let out = entails_inner(facts, goal, solver);
    take_callees_used();
    take_callee_apps();
    for t in saved_used {
        mark_callee_used(&t);
    }
    for (t, apps) in saved_apps {
        for a in apps {
            push_callee_app(&t, &a);
        }
    }
    out
}

fn entails_inner(facts: &[J], goal: &J, solver: &str) -> bool {
    let not_goal = serde_json::json!({ "kind": "app", "op": "not", "args": [goal.clone()] });
    let mut conj = not_goal;
    for f in facts {
        conj = serde_json::json!({ "kind": "app", "op": "and", "args": [f.clone(), conj] });
    }
    let conj = uncurry(&conj);
    let mut vars: BTreeSet<String> = BTreeSet::new();
    fn collect_vars(node: &J, out: &mut BTreeSet<String>) {
        if let Some(v) = var_name(node) {
            out.insert(v.to_string());
        }
        match node {
            J::Object(m) => m.values().for_each(|v| collect_vars(v, out)),
            J::Array(xs) => xs.iter().for_each(|v| collect_vars(v, out)),
            _ => {}
        }
    }
    collect_vars(&conj, &mut vars);
    let vars: Vec<String> = vars.into_iter().filter(|v| v != "nil").collect();
    let Ok(sorts) = infer_var_sorts(&conj, &vars, None) else { return false };
    let mut env: BTreeMap<String, Sort> = sorts.clone();
    if sort_of(&conj, &env, None).ok() != Some(Sort::Bool) {
        return false;
    }
    let Ok(smt) = lower(&conj, &mut env, None) else { return false };
    let Ok((callees, _, _)) = callee_preamble() else { return false };
    let mut out = String::from("(set-logic ALL)\n");
    out.push_str(&callees);
    for v in &vars {
        out.push_str(&format!("(declare-const {v} {})\n", sorts[v].smt()));
    }
    out.push_str(&format!("(assert {smt})\n(check-sat)\n"));
    matches!(run_smt_secs(&with_datatypes(out), solver, 5), Ok(SatAnswer::Unsat))
}

/// Install (or clear) the function's own certified facts for the measure-induction rung,
/// returning the previous setting. See `SELF_FACTS`.
pub fn set_self_facts(facts: Option<CalleeContract>) -> Option<CalleeContract> {
    SELF_FACTS.with(|s| std::mem::replace(&mut *s.borrow_mut(), facts))
}

/// The function's own contract instantiated at the given `self` applications (argument terms in
/// goal scope): each proved postcondition under its antecedent, and each proved law at the
/// application that fixes its variables. The same rendering as a callee's contract instances.
fn self_fact_instances(c: &CalleeContract, at: &[Vec<String>], goal: &[Vec<String>], sd: &SelfDef) -> Result<String> {
    let mut inst = String::new();
    if c.params.len() != sd.params.len() || at.is_empty() {
        return Ok(inst);
    }
    let mut env: BTreeMap<String, Sort> = c.params.iter().cloned().zip(c.param_sorts.iter().copied()).collect();
    let app = serde_json::json!({ "kind": "app", "op": "self", "args": c.params.iter().map(|p| serde_json::json!({ "kind": "var", "name": p })).collect::<Vec<_>>() });
    let subst: BTreeMap<String, J> = [("result".to_string(), app)].into_iter().collect();
    let mut ante: Vec<String> = Vec::new();
    for (i, p) in c.params.iter().enumerate() {
        if c.nat_positions.contains(&i) {
            ante.push(format!("(>= {p} 0)"));
        }
    }
    for pre in &c.pres {
        ante.push(lower(pre, &mut env.clone(), Some(sd))?);
    }
    let ante_smt = match ante.len() {
        0 => "true".to_string(),
        1 => ante[0].clone(),
        _ => format!("(and {})", ante.join(" ")),
    };
    for post in &c.posts {
        let body = lower(&crate::equiv::subst_many(post, &subst), &mut env, Some(sd))?;
        for args in at {
            inst.push_str(&format!(
                ";; self's certified postcondition at (self {})\n(assert {})\n",
                args.join(" "),
                let_params(&c.params, args, &format!("(=> {ante_smt} {body})"))
            ));
        }
    }
    for law in &c.laws {
        let law = uncurry(law);
        let Some(vars_arr) = law.get("vars").and_then(|v| v.as_array()) else { continue };
        let vars: Vec<String> = vars_arr.iter().filter_map(|x| x.as_str().map(String::from)).collect();
        let Some(pred) = law.get("body") else { continue };
        let Ok(var_sorts) = infer_var_sorts(pred, &vars, Some(&c.param_sorts)) else { continue };
        let mut lenv: BTreeMap<String, Sort> = var_sorts.clone();
        let Ok(body) = lower(pred, &mut lenv, Some(sd)) else { continue };
        let mut guards: Vec<String> = Vec::new();
        for call in collect_self_applications(pred) {
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
        let guarded = match guards.len() {
            0 => body,
            1 => format!("(=> {} {body})", guards[0]),
            _ => format!("(=> (and {}) {body})", guards.join(" ")),
        };
        let calls = collect_self_applications(pred);
        let distinct_vars = |call: &Vec<J>| -> Option<Vec<String>> {
            let names: Vec<&str> = call.iter().filter_map(|a| var_name(a)).collect();
            (names.len() == call.len() && names.iter().collect::<BTreeSet<_>>().len() == names.len())
                .then(|| names.into_iter().map(String::from).collect())
        };
        let fixing = calls.iter().find(|call| distinct_vars(call).is_some_and(|names| vars.iter().all(|v| names.contains(v))));
        if let Some(call) = fixing {
            let names: Vec<String> = call.iter().filter_map(|a| var_name(a).map(String::from)).collect();
            for args in at {
                inst.push_str(&format!(";; self's certified law at (self {})\n(assert {})\n", args.join(" "), let_params(&names, args, &guarded)));
            }
        } else if let Some((mut names, free)) = calls.iter().find_map(|call| {
            // An application fixing all but ONE Int variable: that variable ranges over the
            // certificate's self-application terms (the divisibility rung — `d` in the
            // greatest-divisor law can only usefully be another gcd the goal mentions).
            let names = distinct_vars(call)?;
            let free: Vec<&String> = vars.iter().filter(|v| !names.contains(v)).collect();
            (free.len() == 1 && var_sorts.get(free[0].as_str()) == Some(&Sort::Int)).then(|| (names, free[0].clone()))
        }) {
            let mut terms: Vec<String> = goal.iter().map(|a| format!("(self {})", a.join(" "))).collect();
            terms.sort();
            terms.dedup();
            terms.truncate(MAX_FREE_TERMS);
            names.push(free.clone());
            for args in goal {
                for t in &terms {
                    let mut with = args.clone();
                    with.push(t.clone());
                    inst.push_str(&format!(";; self's certified law at (self {}) with {free} = {t}\n(assert {})\n", args.join(" "), let_params(&names, &with, &guarded)));
                }
            }
        }
    }
    // The divisibility rung, when the facts include a divisibility law and the solver has
    // discharged the lemmas (see `divisibility_instances`).
    let divisibility_law = c.laws.iter().any(|l| serde_json::to_string(l).unwrap_or_default().contains("\"mod\""));
    if divisibility_law && DIVISIBILITY_SOLVER.with(|s| s.borrow().as_deref().map(crate::euclid::divisibility_discharged).unwrap_or(false)) {
        inst.push_str(&divisibility_instances("self", goal));
    }
    Ok(inst)
}

fn build_induction_certificate(prop_expr: &J, body: Option<&J>, nat_positions: &BTreeSet<usize>, ih: IhMode) -> Result<(Certificate, String)> {
    let with_ih = ih != IhMode::Off;
    take_callees_used();
    take_callee_apps();
    reset_list_emission();
    // Lists lower in THIS rung (the first-order prover keeps refusing them, so a plain list law
    // still reaches the structural backend): a recursion over the integers that builds or reads a
    // list (`pad_to`), or a structural recursion whose contract is numeric (`from_digits ≥ 0`),
    // inducts over its measure with the list definitions as the solver's own.
    let _lists = admit_lists();
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
    // The measure: the linked analysis's when a caller installed it (it alone sees the measured
    // descent — a callee of several positions decreasing by another callee's certified post), else
    // the view-less one.
    let measure = SELF_MEASURE
        .with(|m| m.borrow().clone())
        .or_else(|| crate::terminate::induction_measure(&body, nat_positions))
        .ok_or_else(|| anyhow!("the body's recursion has no provable numeric measure (structural list recursion, or termination not established)"))?;
    let sd = lower_self_uninterpreted(&body)?;
    let positions: Vec<usize> = measure.by_callee.as_ref().map(|b| b.positions.clone()).unwrap_or_else(|| vec![measure.position]);
    if positions.iter().any(|p| *p >= sd.params.len()) {
        bail!("measure position out of range");
    }
    // The measure callee's symbol, for the hypothesis's order `M(v') < M(v)`: its contract must be
    // installed (the view's closure installs it — a post that applies it names it).
    let measure_symbol: Option<String> = match &measure.by_callee {
        Some(by) => {
            let Some(c) = callee_contract(&by.target) else {
                bail!("the measure callee `{}` has no contract here — pass --records so its certified bound is available", by.target);
            };
            Some(c.symbol.clone())
        }
        None => None,
    };
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
    // … and one fed to a `List nat` parameter ranges over lists of nats (`all_nat`).
    let nat_list_positions = nat_list_param_positions();
    let mut nat_list_vars: BTreeSet<String> = BTreeSet::new();
    for call in collect_self_applications(pred) {
        for (i, a) in call.iter().enumerate() {
            if nat_list_positions.contains(&i) {
                if let Some(v) = var_name(a) {
                    if vars.iter().any(|x| x == v) {
                        nat_list_vars.insert(v.to_string());
                    }
                }
            }
        }
    }
    let spec = match &measure.by_callee {
        Some(_) => measured_spec(pred, &positions, &vars)?,
        None => measure_spec(pred, measure.position, &vars, &nat_vars)?,
    };
    let m = match (&spec, &measure.by_callee) {
        (MeasureSpec::Literal, _) => "<unfolding>".to_string(),
        (MeasureSpec::Vars(vs), Some(by)) => format!("{}…({})", &by.target[..by.target.len().min(11)], vs.join(", ")),
        (MeasureSpec::Var(v), _) => v.clone(),
        (MeasureSpec::Sum(vs), _) => vs.join("+"),
        (MeasureSpec::Vars(vs), None) => vs.join(", "),
    };
    if measure_symbol.is_some() && !matches!(spec, MeasureSpec::Literal) {
        // The order's symbol must be declared: a use, for the preamble below.
        if let Some(by) = &measure.by_callee {
            mark_callee_used(&by.target);
        }
    }

    let var_sorts = infer_var_sorts(pred, &vars, Some(&sd.param_sorts))?;
    let mut env: BTreeMap<String, Sort> = var_sorts.clone();
    if sort_of(pred, &env, Some(&sd))? != Sort::Bool {
        bail!("property body is not a boolean predicate");
    }
    let pred_smt = lower_goal(pred, &mut env, Some(&sd))?;

    // The induction hypothesis: the law over primed variables, at smaller measures.
    let prime = |v: &str| format!("{v}__ih");
    let renaming: BTreeMap<String, J> = vars.iter().map(|v| (v.clone(), serde_json::json!({ "kind": "var", "name": prime(v) }))).collect();
    let pred_ih = crate::equiv::subst_many(pred, &renaming);
    let mut env_ih: BTreeMap<String, Sort> = vars.iter().map(|v| (prime(v), var_sorts[v])).collect();
    let pred_ih_smt = lower(&pred_ih, &mut env_ih, Some(&sd))?;

    let mut out = String::new();
    out.push_str("(set-logic ALL)\n");
    if let Some(by) = &measure.by_callee {
        out.push_str(&format!(
            ";; induction on the measure `{m}` — the callee {} applied to positions {:?}, which every recursive call\n;; makes smaller by a callee's certified post (the termination checker's measured descent); the measure's\n;; value is certified bounded below, so the order is well-founded. self is uninterpreted, its defining\n;; equation instantiated at every application the law makes and at the recursive calls inside them.\n",
            by.target, by.positions
        ));
    } else if measure.structural {
        out.push_str(&format!(
            ";; structural induction on `{m}` (measure position {}: the list every recursive call tails): self is\n;; uninterpreted, its defining equation instantiated at every application the law makes and at the\n;; recursive calls inside them; the hypothesis is instantiated at each tail-descending call.\n",
            measure.position
        ));
    } else {
        out.push_str(&format!(
            ";; numeric induction on `{m}` (measure position {}, floor {}): self is uninterpreted, its defining\n;; equation instantiated at every application the law makes and at the recursive calls inside them.\n",
            measure.position, measure.floor
        ));
    }
    out.push_str(&format!(
        "(declare-fun self ({}) {})\n",
        sd.param_sorts.iter().map(|s| s.smt()).collect::<Vec<_>>().join(" "),
        sd.ret.smt()
    ));
    let (callees, callee_insts, uses_callees) = callee_preamble()?;
    out.push_str(&callees);
    for v in &vars {
        out.push_str(&format!("(declare-const {v} {})\n", var_sorts[v].smt()));
    }
    for v in &nat_vars {
        out.push_str(&format!("(assert (>= {v} 0))\n"));
    }
    for v in &nat_list_vars {
        mark_list_def("all_nat");
        out.push_str(&format!("(assert (all_nat {v}))\n"));
    }
    out.push_str(&callee_insts);
    // IH — only when there is a measure to induct on (a law with a literal at the measure position is
    // decided by the unfolding below alone). One variable: `floor ≤ m' < m`. A sum of nat variables:
    // `Σ v' < Σ v` (each v' ≥ 0 — well-founded over the nats).
    if measure.structural && matches!(spec, MeasureSpec::Sum(_)) {
        bail!("the law applies `self` at several measures of a structural (list) recursion — a sum of lengths is not a supported measure");
    }
    let ih_order: Option<Vec<String>> = match &spec {
        // A structural measure gets NO quantified hypothesis: a guard `length(m') < length(m)` over
        // the recursive `length` definition is exactly what z3 cannot discharge (measured: the
        // `from_digits ≥ 0` obligation closes in 10 ms without that guard and times out with it).
        // The hypothesis is stated instead as explicit INSTANCES at the body's tail-descending
        // recursive calls, guarded by the descended list being a cons — the form in which the
        // termination checker proved the descent (below, with the defining-equation instances).
        MeasureSpec::Var(_) if measure.structural => None,
        // A callee measure: the order is the measure callee's value, `M(v'…) < M(v…)`.
        MeasureSpec::Var(v) if measure_symbol.is_some() => {
            let sym = measure_symbol.as_deref().unwrap_or_default();
            Some(vec![format!("(< ({sym} {}) ({sym} {v}))", prime(v))])
        }
        MeasureSpec::Vars(vs) => {
            let Some(sym) = measure_symbol.as_deref() else { bail!("a tuple of measure variables without a measure callee") };
            Some(vec![format!(
                "(< ({sym} {}) ({sym} {}))",
                vs.iter().map(|v| prime(v)).collect::<Vec<_>>().join(" "),
                vs.join(" ")
            )])
        }
        MeasureSpec::Var(m) => Some(vec![format!("(>= {} {})", prime(m), smt_int_lit(measure.floor)), format!("(< {} {m})", prime(m))]),
        MeasureSpec::Sum(vs) => {
            let sum = |f: &dyn Fn(&str) -> String| if vs.len() == 1 { f(&vs[0]) } else { format!("(+ {})", vs.iter().map(|v| f(v)).collect::<Vec<_>>().join(" ")) };
            Some(vec![format!("(< {} {})", sum(&|v| prime(v)), sum(&|v| v.to_string()))])
        }
        MeasureSpec::Literal => None,
    };
    if let (IhMode::Quantified, Some(mut ih_guards)) = (ih, ih_order.clone()) {
        let ih_binders = vars.iter().map(|v| format!("({} {})", prime(v), var_sorts[v].smt())).collect::<Vec<_>>().join(" ");
        for v in &nat_vars {
            ih_guards.push(format!("(>= {} 0)", prime(v)));
        }
        for v in &nat_list_vars {
            ih_guards.push(format!("(all_nat {})", prime(v)));
        }
        out.push_str(&format!(
            "(assert (forall ({ih_binders}) (=> (and {}) {pred_ih_smt})))\n",
            ih_guards.join(" ")
        ));
    } else {
        let _ = pred_ih_smt;
    }
    // Defining-equation instantiations: at the law's applications, and at the recursive calls inside them.
    // The recursive calls' arguments are read in PARAMETER scope: a `let`- or pattern-bound local they
    // mention (`case rem(a, b, c, d) of (r1, r2) => self(c, d, r1, r2)`) is resolved to its definition
    // (`fst(rem(a, b, c, d))`), so the call lowers and the hypothesis can be stated at it.
    let recursive_calls = collect_self_applications(&resolve_body_locals(body.get("body").unwrap_or(&J::Null)));
    let mut seen: BTreeSet<String> = BTreeSet::new();
    // Every `self` application the certificate states, by its argument terms in goal scope — where
    // the function's own certified facts are instantiated (`SELF_FACTS`).
    let mut self_apps: Vec<Vec<String>> = Vec::new();
    // The GOAL's own applications (the law's terms, before the unfoldings' inner calls): the
    // divisibility rung instantiates over these only — the recursive calls' applications
    // multiply the instances past what the solver decides in budget (measured: 4 goal terms
    // close associativity in 10 ms; all 8 applications, 234 assertions, time out).
    let mut goal_apps: Vec<Vec<String>> = Vec::new();
    for call in collect_self_applications(pred) {
        if call.len() != sd.params.len() {
            bail!("self applied to {} args, expects {}", call.len(), sd.params.len());
        }
        let terms: Vec<String> = call.iter().map(|a| lower(a, &mut env.clone(), Some(&sd))).collect::<Result<_>>()?;
        let app = format!("(self {})", terms.join(" "));
        if seen.insert(app.clone()) {
            out.push_str(&format!("(assert (= {app} {}))\n", let_params(&sd.params, &terms, &sd.body_smt)));
            self_apps.push(terms.clone());
            goal_apps.push(terms.clone());
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
            // The recursive call's arguments in GOAL scope: the parameters substituted by this
            // application's arguments BEFORE lowering — no `let` over the parameter names, so a law
            // whose variables permute them cannot be shadowed. (A call reaching a body-local
            // binding does not lower here and contributes nothing.)
            let p2a: BTreeMap<String, J> = sd.params.iter().cloned().zip(call.iter().cloned()).collect();
            let r_goal: Option<Vec<String>> = rc.iter().map(|x| lower(&crate::equiv::subst_many(x, &p2a), &mut env.clone(), Some(&sd)).ok()).collect();
            if let Some(r) = &r_goal {
                if !self_apps.contains(r) {
                    self_apps.push(r.clone());
                }
            }
            // The hypothesis as an INSTANCE at this recursive call: the law over the primed variables,
            // each bound to the call's argument at its position (the application must put distinct
            // law variables at every position; variables it leaves out keep their own value), under
            // exactly the quantified form's guards — the measure decrease and the domain facts.
            if let (IhMode::Instances, false, Some(guards), Some(r)) = (ih, measure.structural, &ih_order, &r_goal) {
                let law_vars: Option<Vec<&str>> = call.iter().map(|a| var_name(a)).collect();
                if let Some(law_vars) = law_vars {
                    let distinct = law_vars.iter().collect::<BTreeSet<_>>().len() == law_vars.len();
                    if distinct && law_vars.iter().all(|v| vars.iter().any(|x| x == v)) {
                        let mut all_guards: Vec<String> = guards.clone();
                        for v in &nat_vars {
                            all_guards.push(format!("(>= {} 0)", prime(v)));
                        }
                        for v in &nat_list_vars {
                            all_guards.push(format!("(all_nat {})", prime(v)));
                        }
                        let binds = law_vars.iter().zip(r).map(|(v, t)| format!("({} {t})", prime(v))).collect::<Vec<_>>().join(" ");
                        let inst = format!("(let ({binds}) (=> (and {}) {pred_ih_smt}))", all_guards.join(" "));
                        let free: Vec<String> = vars.iter().filter(|v| !law_vars.contains(&v.as_str())).map(|v| format!("({} {v})", prime(v))).collect();
                        let inst = if free.is_empty() { inst } else { format!("(let ({}) {inst})", free.join(" ")) };
                        if seen.insert(inst.clone()) {
                            out.push_str(&format!(";; induction hypothesis at the recursive call {inner_app}\n(assert {inst})\n"));
                        }
                    }
                }
            }
            // The structural induction hypothesis, as an instance at this recursive call: when the
            // law applies `self` to distinct law variables and the call tails the measure parameter
            // (`self(b, tail(ds))`), `P[vars := the call's arguments]` holds provided the list it
            // tails is a cons (so the selector names a proper substructure — on `nil`, `tl` is an
            // unconstrained value and the instance would be unsound), under the domain facts at
            // the instance. Exactly the strong structural induction principle, instantiated.
            if measure.structural && with_ih {
                let law_vars: Option<Vec<&str>> = call.iter().map(|a| var_name(a)).collect();
                let Some(law_vars) = law_vars else { continue };
                let distinct = law_vars.iter().collect::<BTreeSet<_>>().len() == law_vars.len();
                if !distinct || !law_vars.iter().all(|v| vars.iter().any(|x| x == v)) {
                    continue;
                }
                let Some((p, k)) = crate::terminate::tail_descent(&rc[measure.position]) else { continue };
                if p != sd.params[measure.position] {
                    continue;
                }
                let measure_term = &terms[measure.position];
                let cons_guards: Vec<String> = (0..k)
                    .map(|j| {
                        let mut t = measure_term.clone();
                        for _ in 0..j {
                            t = format!("(tl {t})");
                        }
                        format!("(not ((_ is nil) {t}))")
                    })
                    .collect();
                let mut domain_guards: Vec<String> = Vec::new();
                for (i, v) in law_vars.iter().enumerate() {
                    if nat_vars.contains(*v) {
                        domain_guards.push(format!("(>= {} 0)", rterms[i]));
                    }
                    if nat_list_vars.contains(*v) {
                        domain_guards.push(format!("(all_nat {})", rterms[i]));
                    }
                }
                let binds = law_vars.iter().zip(&rterms).map(|(v, r)| format!("({v} {r})")).collect::<Vec<_>>().join(" ");
                let inst = format!("(let ({binds}) {pred_smt})");
                let inst = match domain_guards.len() {
                    0 => inst,
                    1 => format!("(=> {} {inst})", domain_guards[0]),
                    _ => format!("(=> (and {}) {inst})", domain_guards.join(" ")),
                };
                let inst = let_params(&sd.params, &terms, &inst);
                let inst = match cons_guards.len() {
                    1 => format!("(=> {} {inst})", cons_guards[0]),
                    _ => format!("(=> (and {}) {inst})", cons_guards.join(" ")),
                };
                if seen.insert(inst.clone()) {
                    out.push_str(&format!(";; induction hypothesis at the recursive call {inner_app}\n(assert {inst})\n"));
                }
            }
        }
    }
    // The function's own certified facts at every application stated above (a retry the caller
    // installs): a step that needs a proved postcondition about `self` — Euclid's divisibility
    // needs `gcd ≥ 1`, else the deepest, unconstrained application can be 0 and `mod` by it says
    // nothing.
    let mut m = m;
    if let Some(c) = SELF_FACTS.with(|s| s.borrow().clone()) {
        let facts = self_fact_instances(&c, &self_apps, &goal_apps, &sd)?;
        if !facts.is_empty() {
            out.push_str(&facts);
            m = format!("{m}, with self's certified contract at the instantiated points");
        }
    }
    if ih == IhMode::Instances && !m.starts_with('<') {
        m = format!("{m} (hypothesis as instances at the recursive calls)");
    }
    out.push_str(&format!("(assert (not {pred_smt}))\n(check-sat)\n"));
    let quantified: Vec<(String, Sort)> = vars.iter().map(|v| (v.clone(), var_sorts[v])).collect();
    Ok((Certificate { smt: with_datatypes(out), quantified, uses_self: true, uses_callees, euclid: false, steps: None, stepwise: None, staged: None }, m))
}

// --- induction through a callee's definition --------------------------------------------------------
//
// A checker record is not recursive — `digits_roundtrip = \b n -> from_digits(b, digits(b, n)) = n`
// — yet its law is an induction: on `n`, through `digits`' own recursion (`digits(b, n) =
// cons(n mod b, digits(b, n div b))`) and `from_digits`' structure. Neither callee's CONTRACT says
// it; their DEFINITIONS do. This rung inlines `self` (non-recursive, as the first-order prover does),
// unfolds each linked callee's defining equation at the applications the goal makes (and one level
// deeper, at the recursive calls inside — the numeric rung's discipline), and inducts on the law
// variable that reaches a recursive callee's measure position: the hypothesis is stated as an
// INSTANCE at that callee's recursive-call argument (`n div b`), guarded by the measure decrease —
// measured: the quantified hypothesis times out, the instance is unsat in 13 ms. Callees stay
// uninterpreted otherwise (under their contracts), so SAT is UNKNOWN.

/// Every linked-callee application in an AST (head a `fn_…` address), as (target, argument ASTs).
fn collect_callee_apps(node: &J) -> Vec<(String, Vec<J>)> {
    let mut out = Vec::new();
    fn go(node: &J, out: &mut Vec<(String, Vec<J>)>) {
        if node.get("kind").and_then(|k| k.as_str()) == Some("app") {
            if let Some(op) = head_op(node) {
                if op.starts_with("fn_") {
                    let args: Vec<J> = node.get("args").and_then(|a| a.as_array()).cloned().unwrap_or_default();
                    if !out.iter().any(|(t, a)| *t == op && *a == args) {
                        out.push((op, args.clone()));
                    }
                }
            }
        }
        match node {
            J::Object(m) => m.values().for_each(|v| go(v, out)),
            J::Array(xs) => xs.iter().for_each(|v| go(v, out)),
            _ => {}
        }
    }
    go(node, &mut out);
    out
}

struct CalleeDef {
    params: Vec<String>,
    body_smt: String,
    /// The callee's own recursive calls, argument ASTs in its (renamed) parameter scope.
    rec_calls: Vec<Vec<J>>,
    measure: Option<crate::terminate::Measure>,
}

fn build_callee_induction(
    prop_expr: &J,
    body: &J,
    nat_positions: &BTreeSet<usize>,
    callee_def: &dyn Fn(&str) -> Option<(J, BTreeSet<usize>)>,
    with_ih: bool,
) -> Result<(Certificate, String)> {
    take_callees_used();
    take_callee_apps();
    reset_list_emission();
    let _lists = admit_lists();
    let law = uncurry(prop_expr);
    let body = uncurry(body);
    if law.get("kind").and_then(|k| k.as_str()) != Some("forall") {
        bail!("not a `forall`");
    }
    let vars: Vec<String> = law.get("vars").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
    let pred = law.get("body").ok_or_else(|| anyhow!("`forall` has no body"))?;
    if !references_self(pred) {
        bail!("the law does not mention `self`");
    }
    let sd = lower_self(&body)?; // non-recursive, inlined; bails (stated) on a recursive body
    let _ = take_body_apps();
    let inner = body.get("body").ok_or_else(|| anyhow!("body is not a lambda"))?;
    let body_apps = collect_callee_apps(inner);
    if body_apps.is_empty() {
        bail!("the body applies no linked callee — nothing to induct through");
    }
    let goal_calls = collect_self_applications(pred);
    if goal_calls.is_empty() {
        bail!("the law applies no `self`");
    }
    let var_sorts = infer_var_sorts(pred, &vars, Some(&sd.param_sorts))?;
    let mut env: BTreeMap<String, Sort> = var_sorts.clone();
    if sort_of(pred, &env, Some(&sd))? != Sort::Bool {
        bail!("property body is not a boolean predicate");
    }
    let pred_smt = lower_goal(pred, &mut env, Some(&sd))?;

    // Each callee's definition, lowered over FRESH parameter names (no clash with the law's).
    let mut defs: BTreeMap<String, CalleeDef> = BTreeMap::new();
    for (t, _) in &body_apps {
        if defs.contains_key(t) {
            continue;
        }
        let Some(c) = callee_contract(t) else { bail!("{}", no_contract_reason(t)) };
        let Some((cbody, cnat)) = callee_def(t) else { bail!("the callee `{t}`'s body is not in the linked view") };
        let cbody = uncurry(&cbody);
        let cparams: Vec<String> = cbody.get("params").and_then(|p| p.as_array()).map(|ps| ps.iter().filter_map(|p| p.get("name").and_then(|n| n.as_str()).map(String::from)).collect()).unwrap_or_default();
        if cparams.len() != c.params.len() {
            bail!("the callee `{t}` has {} params, its contract {}", cparams.len(), c.params.len());
        }
        let cinner = cbody.get("body").ok_or_else(|| anyhow!("the callee `{t}`'s body is not a lambda"))?;
        let fresh: Vec<String> = (0..cparams.len()).map(|i| format!("cp{i}_{}", &t[3..11])).collect();
        let renaming: BTreeMap<String, J> = cparams.iter().zip(&fresh).map(|(p, f)| (p.clone(), serde_json::json!({ "kind": "var", "name": f }))).collect();
        let cinner = crate::equiv::subst_many(cinner, &renaming);
        let rec_calls = collect_self_applications(&cinner);
        let cinner_t = law_about(&cinner, t);
        let mut cenv: BTreeMap<String, Sort> = fresh.iter().cloned().zip(c.param_sorts.iter().copied()).collect();
        let body_smt = lower(&cinner_t, &mut cenv, None)?;
        let measure = crate::terminate::induction_measure(&cbody, &cnat).filter(|m| !m.structural && m.by_callee.is_none());
        defs.insert(t.clone(), CalleeDef { params: fresh, body_smt, rec_calls, measure });
    }

    // The induction variable: a law variable the goal feeds to a `self` parameter that the body
    // passes to a recursive callee at its measure position — and EVERY such callee (strong
    // induction on the one variable covers each callee's descent: `mod_pow` at `e div 2`, the
    // naive power at `e − 1`, in one law about both).
    let mut induct_var: Option<(String, usize)> = None; // (law var, self param index)
    let mut inducts: BTreeMap<String, usize> = BTreeMap::new(); // callee → its measure position
    for (t, args) in &body_apps {
        let Some(m) = defs[t].measure.as_ref() else { continue };
        let Some(p) = args.get(m.position).and_then(|a| var_name(a)) else { continue };
        let Some(i) = sd.params.iter().position(|x| x == p) else { continue };
        match &induct_var {
            None => {
                if let Some(v) = goal_calls.iter().find_map(|call| call.get(i).and_then(|a| var_name(a)).filter(|v| vars.iter().any(|x| x == v))) {
                    induct_var = Some((v.to_string(), i));
                    inducts.insert(t.clone(), m.position);
                }
            }
            Some((_, ii)) if *ii == i => {
                inducts.insert(t.clone(), m.position);
            }
            _ => {}
        }
    }
    let m_label = induct_var
        .as_ref()
        .map(|(v, _)| format!("{v} (through {})", inducts.keys().map(|t| format!("`{}…`", &t[..11])).collect::<Vec<_>>().join(", ")))
        .unwrap_or_else(|| "<unfolding>".into());

    let mut out = String::new();
    out.push_str("(set-logic ALL)\n");
    out.push_str(&format!(";; induction through a callee's definition, on `{m_label}`: self inlined, each linked callee's defining\n;; equation instantiated at the applications the goal makes and at the recursive calls inside them.\n"));
    // The callee preamble (declarations, contract axioms and their instances) is rendered AFTER
    // the unfoldings below, which record more applications for it — the callees' laws at the
    // hypothesis's application (the naive power's doubling law at `e div 2`); it is spliced in here.
    let mut decls = String::new();
    let params = sd.params.iter().zip(&sd.param_sorts).map(|(p, s)| format!("({p} {})", s.smt())).collect::<Vec<_>>().join(" ");
    decls.push_str(&format!("(define-fun self ({params}) {} {})\n", sd.ret.smt(), sd.body_smt));
    for v in &vars {
        decls.push_str(&format!("(declare-const {v} {})\n", var_sorts[v].smt()));
    }
    let mut nat_vars: BTreeSet<String> = BTreeSet::new();
    for call in &goal_calls {
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
    for v in &nat_vars {
        decls.push_str(&format!("(assert (>= {v} 0))\n"));
    }

    // Unfoldings (and hypothesis instances) at every `self` application the goal makes.
    let mut body_out = String::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for call in &goal_calls {
        if call.len() != sd.params.len() {
            continue;
        }
        let terms: Vec<String> = call.iter().map(|a| lower(a, &mut env.clone(), Some(&sd))).collect::<Result<_>>()?;
        let mut penv: BTreeMap<String, Sort> = sd.params.iter().cloned().zip(sd.param_sorts.iter().copied()).collect();
        for (t, args) in &body_apps {
            let c = callee_contract(t).ok_or_else(|| anyhow!("{}", no_contract_reason(t)))?;
            let d = &defs[t];
            // The application's arguments in goal scope: lowered in self's parameter scope, the
            // parameters bound to the goal's terms.
            let a: Vec<String> = args.iter().map(|x| lower(x, &mut penv, Some(&sd)).map(|s| let_params(&sd.params, &terms, &s))).collect::<Result<_>>()?;
            let app = format!("({} {})", c.symbol, a.join(" "));
            let eq = format!("(assert (= {app} {}))\n", let_params(&d.params, &a, &d.body_smt));
            if seen.insert(eq.clone()) {
                body_out.push_str(&eq);
            }
            let mut cenv: BTreeMap<String, Sort> = d.params.iter().cloned().zip(c.param_sorts.iter().copied()).collect();
            for rc in &d.rec_calls {
                if rc.len() != d.params.len() {
                    continue;
                }
                let r: Vec<String> = rc.iter().map(|x| lower(&law_about(x, t), &mut cenv, None)).collect::<Result<_>>()?;
                let inner_app = format!("({} {})", c.symbol, r.join(" "));
                let eq = format!("(assert {})\n", let_params(&d.params, &a, &format!("(= {inner_app} {})", let_params(&d.params, &r, &d.body_smt))));
                if seen.insert(eq.clone()) {
                    body_out.push_str(&eq);
                }
                // The hypothesis at this recursive call: the law with the induction variable replaced
                // by the call's measure argument, guarded by `floor ≤ it < the variable` (the callee's
                // parameter at the measure position is bound to the goal's term for it).
                if let (true, Some((v, i)), Some(p)) = (with_ih, induct_var.as_ref(), inducts.get(t)) {
                    if call.get(*i).and_then(|x| var_name(x)) == Some(v.as_str()) {
                        let m = d.measure.as_ref().expect("chosen callee has a measure");
                        let smaller = &r[*p];
                        let guard = format!("(and (>= {smaller} {}) (< {smaller} {}))", smt_int_lit(m.floor), d.params[*p]);
                        let ih = format!("(=> {guard} (let (({v} {smaller})) {pred_smt}))");
                        let inst = format!(";; induction hypothesis at {inner_app}\n(assert {})\n", let_params(&d.params, &a, &ih));
                        if seen.insert(inst.clone()) {
                            body_out.push_str(&inst);
                            // The body's callee applications at the hypothesis's application — the
                            // law's `self(…)` at `v := smaller` — recorded so the callees' contracts
                            // (their laws above all: the doubling law of the naive power at `e div 2`)
                            // are instantiated there too, by the preamble rendered below.
                            for (t2, args2) in &body_apps {
                                let Ok(a2) = args2.iter().map(|x| lower(x, &mut penv.clone(), Some(&sd)).map(|s| let_params(&sd.params, &terms, &s))).collect::<Result<Vec<String>>>() else { continue };
                                let at_ih: Vec<String> = a2.iter().map(|s| let_params(&d.params, &a, &format!("(let (({v} {smaller})) {s})"))).collect();
                                push_callee_app(t2, &at_ih);
                            }
                        }
                    }
                }
            }
        }
    }
    let (callees, callee_insts, _) = callee_preamble()?;
    out.push_str(&callees);
    out.push_str(&decls);
    out.push_str(&callee_insts);
    out.push_str(&body_out);
    out.push_str(&format!("(assert (not {pred_smt}))\n(check-sat)\n"));
    let quantified: Vec<(String, Sort)> = vars.iter().map(|v| (v.clone(), var_sorts[v])).collect();
    Ok((Certificate { smt: with_datatypes(out), quantified, uses_self: true, uses_callees: true, euclid: false, steps: None, stepwise: None, staged: None }, m_label))
}

/// Prove a law about a NON-recursive function by induction through the definitions of the linked
/// callees it applies (see the module comment above). `callee_def` supplies a callee's body and
/// `nat` positions from the linked view. Unfolding alone first, then with the hypothesis.
pub fn prove_by_callee_induction(
    prop_expr: &J,
    body: &J,
    nat_positions: &BTreeSet<usize>,
    callee_def: &dyn Fn(&str) -> Option<(J, BTreeSet<usize>)>,
    solver: &str,
) -> (ProofOutcome, Option<Certificate>, Option<String>) {
    // The ladder: unfolding alone, then with the hypothesis; each first with the callees' quantified
    // contract axioms, then with their instances only (the contract ladder — measured here too: the
    // round-trip is unsat in 14 ms on the instances and times out under the quantified `≥ 0`).
    let mut last: (ProofOutcome, Option<Certificate>, Option<String>) = (ProofOutcome::Unknown, None, None);
    for with_ih in [false, true] {
        for quantified in [true, false] {
            let built = if quantified {
                build_callee_induction(prop_expr, body, nat_positions, callee_def, with_ih)
            } else {
                without_contract_quantifiers(|| build_callee_induction(prop_expr, body, nat_positions, callee_def, with_ih))
            };
            let (cert, m) = match built {
                Err(e) => return (ProofOutcome::Unsupported(format!("{e:#}")), None, None),
                Ok(x) => x,
            };
            let m = if with_ih { m } else { "<unfolding>".to_string() };
            match run_smt(&cert.smt, solver) {
                Ok(SatAnswer::Unsat) => return (ProofOutcome::Proved, Some(cert), Some(m)),
                Ok(SatAnswer::NoSolver) => return (ProofOutcome::NoSolver, Some(cert), Some(m)),
                Err(e) => return (ProofOutcome::Unsupported(format!("solver error: {e:#}")), Some(cert), Some(m)),
                Ok(SatAnswer::Sat(_)) | Ok(SatAnswer::Unknown) => last = (ProofOutcome::Unknown, Some(cert), Some(m)),
            }
        }
    }
    // The Euclidean rung over the last certificate (see `euclid`).
    if let (ProofOutcome::Unknown, Some(mut cert), m) = last {
        if euclid_ladder(&mut cert, solver) {
            return (ProofOutcome::Proved, Some(cert), m);
        }
        return (ProofOutcome::Unknown, Some(cert), m);
    }
    last
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
    fn a_closed_law_is_a_zero_variable_obligation() {
        let Some(solver) = solver() else { return };
        // `\a b -> add a b` with the CLOSED law `self(2, 3) == 5` — no `forall`, the shape of the
        // Gaussian tier's `i² = −1` — is one solver call, not UNSUPPORTED "not a `forall`".
        let add = json!({ "kind": "lambda", "params": [{ "name": "a" }, { "name": "b" }],
            "body": { "kind": "app", "fn": v("add"), "args": [v("a"), v("b")] } });
        let closed = op("eq", vec![self_app(vec![lit(2), lit(3)]), lit(5)]);
        let (out, _) = prove_property(&closed, Some(&add), solver);
        assert!(matches!(out, ProofOutcome::Proved), "{out:?}");
        let wrong = op("eq", vec![self_app(vec![lit(2), lit(3)]), lit(6)]);
        assert!(matches!(prove_property(&wrong, Some(&add), solver).0, ProofOutcome::Refuted(_)));
        // An explicit `forall` with no variables is the same obligation.
        let empty_forall = law(&[], op("eq", vec![self_app(vec![lit(2), lit(3)]), lit(5)]));
        let (out, _) = prove_property(&empty_forall, Some(&add), solver);
        assert!(matches!(out, ProofOutcome::Proved), "{out:?}");
    }

    #[test]
    fn tuples_and_sums_lower_as_datatypes() {
        let Some(solver) = solver() else { return };
        // pair: \a b -> (a, b) : (int, int) -> (int, int); law fst(self(a, b)) == a.
        let pair = json!({ "kind": "lambda", "params": [{ "name": "a" }, { "name": "b" }],
            "body": { "kind": "tuple", "elems": [v("a"), v("b")] } });
        let int = json!({ "kind": "builtin", "name": "int" });
        let pair_ty = json!({ "kind": "fn", "params": [int, int], "result": { "kind": "tuple", "elems": [int, int] } });
        let prev = set_self_type(Some(pair_ty));
        let law_fst = law(&["a", "b"], op("eq", vec![op("fst", vec![self_app(vec![v("a"), v("b")])]), v("a")]));
        let (out, cert) = prove_property(&law_fst, Some(&pair), solver);
        assert!(matches!(out, ProofOutcome::Proved), "{out:?}");
        assert!(cert.unwrap().smt.contains("(declare-datatype dt"), "the certificate declares the tuple datatype");
        let law_wrong = law(&["a", "b"], op("eq", vec![op("snd", vec![self_app(vec![v("a"), v("b")])]), v("a")]));
        assert!(matches!(prove_property(&law_wrong, Some(&pair), solver).0, ProofOutcome::Refuted(_)));
        // half: \n -> case gt(n, 0) of true -> Just(n) | false -> None : int -> Maybe int, and a caller
        // matching on it: \n -> case self(n)… — expressed as a refinement-style law on a wrapper body
        // `\n -> case half(n) of { Just(x) => x; None => 0 }` whose result equals max(n, 0).
        let maybe_int = json!({ "kind": "sum", "variants": [{ "tag": "Just", "type": int }, { "tag": "None" }] });
        set_self_type(Some(json!({ "kind": "fn", "params": [int], "result": maybe_int })));
        let half = json!({ "kind": "lambda", "params": [{ "name": "n" }],
            "body": { "kind": "case", "scrutinee": { "kind": "app", "fn": { "kind": "var", "name": "gt" }, "args": [v("n"), { "kind": "lit", "value": { "kind": "int", "value": 0 } }] },
                      "arms": [
                          { "pattern": { "kind": "lit", "value": { "kind": "bool", "value": true } }, "body": { "kind": "variant", "tag": "Just", "payload": v("n") } },
                          { "pattern": { "kind": "lit", "value": { "kind": "bool", "value": false } }, "body": { "kind": "variant", "tag": "None" } }] } });
        // Law over the sum-valued function: a variant matched back out through a nested body is out of the
        // predicate grammar, so the checkable law here is equality with itself under the datatype
        // (exercises variant construction + the declared sum sort end to end).
        let law_refl = law(&["n"], op("eq", vec![self_app(vec![v("n")]), self_app(vec![v("n")])]));
        let (out, cert) = prove_property(&law_refl, Some(&half), solver);
        assert!(matches!(out, ProofOutcome::Proved), "{out:?}");
        let smt = cert.unwrap().smt;
        assert!(smt.contains("_Just (dt") && smt.contains("_None)"), "{smt}");
        set_self_type(prev);
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

    /// A callee applied to a LOCALLY bound name — `let p = f(n) in f(p) + 1 > 0` — gets its contract
    /// instance at the binding's DEFINITION (`f(f(n))`), in the goal's scope: before, the instance
    /// was stated at `p` outside the `let` and the solver answered "unknown constant p" (the rational
    /// tier's lowest-terms checker, which binds normalize's result by a tuple pattern, 2026-10-30).
    #[test]
    fn a_callee_instance_at_a_local_binding_is_stated_at_its_definition() {
        let Some(solver) = solver() else { return };
        let target = format!("fn_{}", "cd".repeat(32));
        let contract = CalleeContract {
            symbol: "callee_cdcd".into(),
            params: vec!["x".into()],
            param_sorts: vec![Sort::Int],
            ret: Sort::Int,
            pres: vec![],
            posts: vec![json!({ "kind": "app", "op": "ge", "args": [{ "kind": "var", "name": "result" }, { "kind": "lit", "value": 0 }] })],
            laws: vec![],
            nat_positions: [0].into(),
        };
        let call = |a: J| json!({ "kind": "app", "fn": { "kind": "lit", "value": { "kind": "fn_ref", "target": target } }, "args": [a] });
        let inner = json!({ "kind": "let", "name": "p", "value": call(v("n")),
            "body": op("gt", vec![op("add", vec![call(v("p")), lit(1)]), lit(0)]) });
        let law_ok = law(&["n"], op("or", vec![op("lt", vec![v("n"), lit(0)]), inner]));
        let prev = set_callee_contracts([(target.clone(), contract)].into_iter().collect());
        let (out, cert) = prove_property(&law_ok, None, solver);
        set_callee_contracts(prev);
        assert!(matches!(out, ProofOutcome::Proved), "{out:?}");
        let smt = cert.unwrap().smt;
        // The instance binds the contract's parameter to the DEFINITION of `p`, not to `p`.
        assert!(smt.contains("(let ((x (callee_cdcd n))) (=> (>= x 0) (>= (callee_cdcd x) 0)))"), "the instance at the definition: {smt}");
        assert!(!smt.contains("(let ((x p))"), "never at the local name: {smt}");
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
        assert!(m.as_deref().is_some_and(|m| m.starts_with("a+b")), "{m:?}"); // the last rung tried names the measure
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

    /// A typed integer literal, the form the termination checker reads guards from (a bare `lit`
    /// is fine for the prover, not for the measure).
    /// The Gaussian `gcd`'s law `nonzero` by induction on the MEASURED descent — the measure a callee
    /// (`norm`) of positions 2 and 3, smaller by the remainder's certified post (the Gaussian tier,
    /// 2026-10-31). The measure is the linked termination analysis's, installed here as the view
    /// installs it; without it the view-less analysis sees an opaque callee and the rung refuses.
    #[test]
    fn induction_on_a_callee_measure_proves_the_gaussian_gcd_nonzero() {
        let Some(solver) = solver() else { return };
        let rem = format!("fn_{}", "4e".repeat(32));
        let norm = format!("fn_{}", "83".repeat(32));
        let int = json!({ "kind": "builtin", "name": "int" });
        let pair_ty = json!({ "kind": "tuple", "elems": [int.clone(), int.clone()] });
        let pair_sort = sort_of_type(&pair_ty).expect("a tuple sort");
        let names = |ns: &[&str]| ns.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let r = |acc: &str| op(acc, vec![v("result")]);
        let norm_contract = CalleeContract {
            symbol: "callee_norm".into(), params: names(&["a", "b"]), param_sorts: vec![Sort::Int; 2], ret: Sort::Int, pres: vec![],
            posts: vec![
                op("ge", vec![v("result"), lit(0)]),
                op("eq", vec![v("result"), op("add", vec![op("mul", vec![v("a"), v("a")]), op("mul", vec![v("b"), v("b")])])]),
            ],
            laws: vec![], nat_positions: BTreeSet::new(),
        };
        let rem_contract = CalleeContract {
            symbol: "callee_rem".into(), params: names(&["a", "b", "c", "d"]), param_sorts: vec![Sort::Int; 4], ret: pair_sort,
            pres: vec![op("or", vec![op("neq", vec![v("c"), lit(0)]), op("neq", vec![v("d"), lit(0)])])],
            posts: vec![op("lt", vec![op(&norm, vec![r("fst"), r("snd")]), op(&norm, vec![v("c"), v("d")])])],
            laws: vec![], nat_positions: BTreeSet::new(),
        };
        // \a b c d -> case and(eq(c, 0), eq(d, 0)) of true -> (a, b) | false -> case rem(a, b, c, d) of (r1, r2) -> self(c, d, r1, r2)
        let bapp = |name: &str, args: Vec<J>| json!({ "kind": "app", "fn": { "kind": "var", "name": name }, "args": args });
        let blit = |b: bool| json!({ "kind": "lit", "value": { "kind": "bool", "value": b } });
        let rem_app = json!({ "kind": "app", "fn": { "kind": "lit", "value": { "kind": "fn_ref", "target": rem } }, "args": [v("a"), v("b"), v("c"), v("d")] });
        let body = json!({ "kind": "lambda", "params": [{ "name": "a" }, { "name": "b" }, { "name": "c" }, { "name": "d" }], "body": { "kind": "case",
            "scrutinee": bapp("and", vec![bapp("eq", vec![v("c"), ilit(0)]), bapp("eq", vec![v("d"), ilit(0)])]),
            "arms": [
                { "pattern": blit(true), "body": { "kind": "tuple", "elems": [v("a"), v("b")] } },
                { "pattern": blit(false), "body": { "kind": "case", "scrutinee": rem_app, "arms": [{
                    "pattern": { "kind": "tuple", "elems": [{ "kind": "bind", "name": "r1" }, { "kind": "bind", "name": "r2" }] },
                    "body": self_app(vec![v("c"), v("d"), v("r1"), v("r2")]) }] } } ] } });
        let all_zero = op("and", vec![
            op("and", vec![op("eq", vec![v("a"), lit(0)]), op("eq", vec![v("b"), lit(0)])]),
            op("and", vec![op("eq", vec![v("c"), lit(0)]), op("eq", vec![v("d"), lit(0)])]),
        ]);
        let g = self_app(vec![v("a"), v("b"), v("c"), v("d")]);
        let nonzero = law(&["a", "b", "c", "d"], op("or", vec![all_zero, op("or", vec![op("neq", vec![op("fst", vec![g.clone()]), lit(0)]), op("neq", vec![op("snd", vec![g]), lit(0)])])]));
        let prev_c = set_callee_contracts([(rem.clone(), rem_contract), (norm.clone(), norm_contract)].into_iter().collect());
        let prev_ty = set_self_type(Some(json!({ "kind": "fn", "params": [int.clone(), int.clone(), int.clone(), int], "result": pair_ty })));
        let measure = crate::terminate::Measure {
            position: 2, floor: 0, structural: false,
            by_callee: Some(crate::terminate::MeasuredBy { target: norm.clone(), positions: vec![2, 3] }),
        };
        let prev_m = set_self_measure(Some(measure));
        let (out, cert, m) = prove_by_numeric_induction(&nonzero, Some(&body), &BTreeSet::new(), solver);
        set_self_measure(None);
        // Without the linked measure the callee is opaque to the view-less analysis: no measure, refused.
        let (without, _, _) = prove_by_numeric_induction(&nonzero, Some(&body), &BTreeSet::new(), solver);
        set_self_measure(prev_m);
        set_self_type(prev_ty);
        set_callee_contracts(prev_c);
        assert!(matches!(out, ProofOutcome::Proved), "{out:?} ({m:?})");
        let smt = cert.expect("a certificate").smt;
        assert!(smt.contains("(< (callee_norm c__ih d__ih) (callee_norm c d))"), "the hypothesis is ordered by the measure callee:\n{smt}");
        assert!(matches!(without, ProofOutcome::Unsupported(_)), "{without:?}");
    }

    fn ilit(n: i64) -> J {
        json!({ "kind": "lit", "value": { "kind": "int", "value": n } })
    }

    /// digit_sum: \b n -> case lt(b, 2) of true -> 0 | false -> case lt(n, 1) of true -> 0 | false -> add(mod(n, b), self(b, div(n, b)))
    fn digit_sum_body() -> J {
        let case = |scrutinee: J, t: J, f: J| json!({ "kind": "case", "scrutinee": scrutinee, "arms": [
            { "pattern": { "kind": "lit", "value": { "kind": "bool", "value": true } }, "body": t },
            { "pattern": { "kind": "lit", "value": { "kind": "bool", "value": false } }, "body": f } ] });
        let step = op("add", vec![op("mod", vec![v("n"), v("b")]), self_app(vec![v("b"), op("div", vec![v("n"), v("b")])])]);
        json!({ "kind": "lambda", "params": [{ "name": "b" }, { "name": "n" }],
            "body": case(op("lt", vec![v("b"), ilit(2)]), ilit(0), case(op("lt", vec![v("n"), ilit(1)]), ilit(0), step)) })
    }

    /// gcd: \a b -> case eq(b, 0) of true -> a | false -> self(b, mod(a, b))
    fn gcd_body() -> J {
        json!({ "kind": "lambda", "params": [{ "name": "a" }, { "name": "b" }], "body": { "kind": "case",
            "scrutinee": op("eq", vec![v("b"), ilit(0)]),
            "arms": [
                { "pattern": { "kind": "lit", "value": { "kind": "bool", "value": true } }, "body": v("a") },
                { "pattern": { "kind": "lit", "value": { "kind": "bool", "value": false } },
                  "body": self_app(vec![v("b"), op("mod", vec![v("a"), v("b")])]) } ] } })
    }

    #[test]
    fn hypothesis_as_instances_closes_the_digit_sum_bound() {
        // digit_sum(b, n) <= n over nats: the quantified hypothesis leaves z3 at unknown (a
        // quantifier over `div`/`mod` terms); the same hypothesis INSTANTIATED at the recursive
        // call `self(b, n div b)` closes — the third rung of the ladder, and no Euclidean lemma.
        let Some(solver) = solver() else { return };
        let nats: BTreeSet<usize> = [0, 1].into();
        let bound = law(&["b", "n"], op("or", vec![op("lt", vec![v("n"), lit(0)]), op("le", vec![self_app(vec![v("b"), v("n")]), v("n")])]));
        let (out, cert, m) = prove_by_numeric_induction(&bound, Some(&digit_sum_body()), &nats, solver);
        assert!(matches!(out, ProofOutcome::Proved), "{out:?}");
        let cert = cert.unwrap();
        // The solver version is part of the measurement: z3 4.13 (this box) needs the hypothesis
        // as instances (the third rung); z3 4.16 (the release workflow's pin) closes the step with
        // the quantified hypothesis already. Either is the law proved by numeric induction on `n`.
        let label = m.as_deref().unwrap_or("");
        assert!(label.starts_with('n'), "{m:?}");
        if label.contains("hypothesis as instances") {
            assert!(cert.smt.contains(";; induction hypothesis at the recursive call (self b (div n b))"), "{}", cert.smt);
            assert!(!cert.smt.contains("(forall ((b__ih Int)"), "the instances rung states no quantified hypothesis");
        }
        assert!(!cert.euclid);
        // The false bound `digit_sum(b, n) < n` stays open (never refuted: self is uninterpreted).
        let wrong = law(&["b", "n"], op("or", vec![op("lt", vec![v("n"), lit(0)]), op("lt", vec![self_app(vec![v("b"), v("n")]), v("n")])]));
        let (out, _, _) = prove_by_numeric_induction(&wrong, Some(&digit_sum_body()), &nats, solver);
        assert!(matches!(out, ProofOutcome::Unknown), "{out:?}");
    }

    #[test]
    fn euclid_divisibility_needs_self_facts_and_the_euclidean_rung() {
        // gcd divides both arguments (guarded at (0, 0), where `0 mod 0` is undefined): the step
        // rests on (1) the hypothesis at `self(b, a mod b)`, (2) `a mod g` reduced through `a =
        // (a div b)·b + (a mod b)` — the Euclidean rung's cross-modulus instance — and (3) gcd's
        // own certified `result ≥ 1`, without which the deepest, unconstrained application can be
        // 0 and `mod` by it says nothing. Without (3): UNKNOWN. With it: PROVED, and the
        // certificate carries the lemma instances.
        let Some(solver) = solver() else { return };
        let nats: BTreeSet<usize> = [0, 1].into();
        let g = self_app(vec![v("a"), v("b")]);
        let divides = law(&["a", "b"], op("or", vec![
            op("and", vec![op("eq", vec![v("a"), lit(0)]), op("eq", vec![v("b"), lit(0)])]),
            op("and", vec![op("eq", vec![op("mod", vec![v("a"), g.clone()]), lit(0)]), op("eq", vec![op("mod", vec![v("b"), g.clone()]), lit(0)])])]));
        let (out, _, _) = prove_by_numeric_induction(&divides, Some(&gcd_body()), &nats, solver);
        assert!(matches!(out, ProofOutcome::Unknown), "without self's facts: {out:?}");
        let positive = CalleeContract {
            symbol: "self".into(),
            params: vec!["a".into(), "b".into()],
            param_sorts: vec![Sort::Int, Sort::Int],
            ret: Sort::Int,
            pres: vec![],
            posts: vec![op("or", vec![
                op("and", vec![op("lt", vec![v("a"), lit(1)]), op("lt", vec![v("b"), lit(1)])]),
                op("ge", vec![v("result"), lit(1)])])],
            laws: vec![],
            nat_positions: nats.clone(),
        };
        let prev = set_self_facts(Some(positive));
        let (out, cert, m) = prove_by_numeric_induction(&divides, Some(&gcd_body()), &nats, solver);
        set_self_facts(prev);
        assert!(matches!(out, ProofOutcome::Proved), "{out:?}");
        let cert = cert.unwrap();
        assert!(cert.euclid, "closed through the Euclidean rung");
        assert!(m.as_deref().unwrap_or("").contains("self's certified contract"), "{m:?}");
        assert!(cert.smt.contains(";; self's certified postcondition at (self b (mod a b))"), "{}", cert.smt);
        assert!(cert.smt.contains("instances of `euclid_unique`"), "{}", cert.smt);
        assert!(cert.smt.contains("(div (div a b) (self a b))"), "the cross-modulus witness: {}", cert.smt);
        // The unguarded law is NOT proved: at (0, 0) the modulus is 0.
        let unguarded = law(&["a", "b"], op("and", vec![op("eq", vec![op("mod", vec![v("a"), g.clone()]), lit(0)]), op("eq", vec![op("mod", vec![v("b"), g]), lit(0)])]));
        let prev = set_self_facts(Some(CalleeContract { symbol: "self".into(), params: vec!["a".into(), "b".into()], param_sorts: vec![Sort::Int, Sort::Int], ret: Sort::Int, pres: vec![], posts: vec![op("or", vec![op("and", vec![op("lt", vec![v("a"), lit(1)]), op("lt", vec![v("b"), lit(1)])]), op("ge", vec![v("result"), lit(1)])])], laws: vec![], nat_positions: nats.clone() }));
        let (out, _, _) = prove_by_numeric_induction(&unguarded, Some(&gcd_body()), &nats, solver);
        set_self_facts(prev);
        assert!(matches!(out, ProofOutcome::Unknown), "{out:?}");
    }

    /// gcd associativity — "a fact no contract states" for ten follow-ups — follows from two
    /// facts gcd's contract CAN state (it divides both arguments; every common divisor divides
    /// it) through the DIVISIBILITY RUNG: the greatest-divisor law has a variable no application
    /// fixes (`d`), so it is instantiated with `d` ranging over the certificate's own gcd terms,
    /// and transitivity + antisymmetry of `mod … = 0` are stated at those terms (the lemmas
    /// discharged first). Without the greatest-divisor law the same ladder stays UNKNOWN.
    #[test]
    fn the_divisibility_rung_closes_gcd_associativity() {
        let Some(solver) = solver() else { return };
        let nats: BTreeSet<usize> = [0, 1].into();
        let g = |x: J, y: J| self_app(vec![x, y]);
        let assoc = law(&["a", "b", "c"], op("eq", vec![g(g(v("a"), v("b")), v("c")), g(v("a"), g(v("b"), v("c")))]));
        let divides = law(&["a", "b"], op("or", vec![
            op("and", vec![op("eq", vec![v("a"), lit(0)]), op("eq", vec![v("b"), lit(0)])]),
            op("and", vec![op("eq", vec![op("mod", vec![v("a"), g(v("a"), v("b"))]), lit(0)]), op("eq", vec![op("mod", vec![v("b"), g(v("a"), v("b"))]), lit(0)])])]));
        let greatest = law(&["a", "b", "d"], op("or", vec![op("lt", vec![v("d"), lit(1)]), op("or", vec![
            op("gt", vec![op("mod", vec![v("a"), v("d")]), lit(0)]), op("or", vec![
            op("gt", vec![op("mod", vec![v("b"), v("d")]), lit(0)]),
            op("eq", vec![op("mod", vec![g(v("a"), v("b")), v("d")]), lit(0)])])])]));
        let post = op("or", vec![op("and", vec![op("lt", vec![v("a"), lit(1)]), op("lt", vec![v("b"), lit(1)])]), op("ge", vec![v("result"), lit(1)])]);
        let contract = |laws: Vec<J>| CalleeContract {
            symbol: "self".into(), params: vec!["a".into(), "b".into()], param_sorts: vec![Sort::Int, Sort::Int], ret: Sort::Int,
            pres: vec![], posts: vec![post.clone()], laws, nat_positions: nats.clone(),
        };
        let prev = set_self_facts(Some(contract(vec![divides.clone()])));
        let (out, _, _) = prove_by_numeric_induction(&assoc, Some(&gcd_body()), &nats, solver);
        set_self_facts(prev);
        assert!(matches!(out, ProofOutcome::Unknown), "without the greatest-divisor law: {out:?}");
        let prev = set_self_facts(Some(contract(vec![divides, greatest])));
        let (out, cert, m) = prove_by_numeric_induction(&assoc, Some(&gcd_body()), &nats, solver);
        set_self_facts(prev);
        assert!(matches!(out, ProofOutcome::Proved), "{out:?} {m:?}");
        let smt = cert.unwrap().smt;
        assert!(smt.contains("with d = (self"), "the free variable ranges over the gcd terms: {smt}");
        assert!(smt.contains("the divisibility rung"), "{smt}");
    }

    /// Lowest terms after normalization — the rational tier's headline fact — through the
    /// CALLEE-side divisibility rung: `gcd` is a linked callee under its contract (positivity,
    /// divides-both, the greatest-divisor law), the body divides both components by
    /// `gcd(|n|, d)`, and the post `gcd(|p|, q) = 1` needs the greatest-divisor law at the PRODUCT
    /// `e·g` plus the scaling lemma — hand-measured: with them unsat, without the product
    /// satisfiable. The contract ladder alone leaves it UNKNOWN; the rung closes it.
    #[test]
    fn the_callee_divisibility_rung_proves_lowest_terms_after_normalization() {
        let Some(solver) = solver() else { return };
        let target = format!("fn_{}", "ef".repeat(32));
        let g = |x: J, y: J| json!({ "kind": "app", "fn": { "kind": "lit", "value": { "kind": "fn_ref", "target": target } }, "args": [x, y] });
        let pg = |x: J, y: J| op(&target, vec![x, y]);
        let nats: BTreeSet<usize> = [0, 1].into();
        let post_pos = op("or", vec![op("and", vec![op("lt", vec![v("a"), lit(1)]), op("lt", vec![v("b"), lit(1)])]), op("ge", vec![v("result"), lit(1)])]);
        let divides = law(&["a", "b"], op("or", vec![
            op("and", vec![op("eq", vec![v("a"), lit(0)]), op("eq", vec![v("b"), lit(0)])]),
            op("and", vec![op("eq", vec![op("mod", vec![v("a"), pg(v("a"), v("b"))]), lit(0)]), op("eq", vec![op("mod", vec![v("b"), pg(v("a"), v("b"))]), lit(0)])])]));
        let greatest = law(&["a", "b", "d"], op("or", vec![op("lt", vec![v("d"), lit(1)]), op("or", vec![
            op("gt", vec![op("mod", vec![v("a"), v("d")]), lit(0)]), op("or", vec![
            op("gt", vec![op("mod", vec![v("b"), v("d")]), lit(0)]),
            op("eq", vec![op("mod", vec![pg(v("a"), v("b")), v("d")]), lit(0)])])])]));
        let contract = CalleeContract {
            symbol: "callee_efef".into(), params: vec!["a".into(), "b".into()], param_sorts: vec![Sort::Int, Sort::Int], ret: Sort::Int,
            pres: vec![], posts: vec![post_pos], laws: vec![divides, greatest], nat_positions: nats.clone(),
        };
        // The rational tier's `rat_normalize`: \n d -> case eq n 0 of { true => (0, 1); false => case
        // lt d 0 of { true => (div (neg n) G, div (neg d) G); false => (div n G, div d G) } } with
        // G = gcd(|n|, |d|). (The stripped body without the zero and sign cases is HARDER for z3 —
        // unknown at the budget where this one closes in 3 s; the shape the module ships is the
        // shape under test.)
        let bapp = |name: &str, args: Vec<J>| json!({ "kind": "app", "fn": { "kind": "var", "name": name }, "args": args });
        let gnd = g(bapp("abs", vec![v("n")]), bapp("abs", vec![v("d")]));
        let ilit = |x: i64| json!({ "kind": "lit", "value": { "kind": "int", "value": x } });
        let blit = |b: bool| json!({ "kind": "lit", "value": { "kind": "bool", "value": b } });
        let pair = |a: J, b: J| json!({ "kind": "tuple", "elems": [a, b] });
        let body = json!({ "kind": "lambda", "params": [{ "name": "n" }, { "name": "d" }], "body": { "kind": "case",
            "scrutinee": bapp("eq", vec![v("n"), ilit(0)]),
            "arms": [
                { "pattern": blit(true), "body": pair(ilit(0), ilit(1)) },
                { "pattern": blit(false), "body": { "kind": "case",
                    "scrutinee": bapp("lt", vec![v("d"), ilit(0)]),
                    "arms": [
                        { "pattern": blit(true), "body": pair(bapp("div", vec![bapp("neg", vec![v("n")]), gnd.clone()]), bapp("div", vec![bapp("neg", vec![v("d")]), gnd.clone()])) },
                        { "pattern": blit(false), "body": pair(bapp("div", vec![v("n"), gnd.clone()]), bapp("div", vec![v("d"), gnd.clone()])) } ] } } ] } });
        let ty = json!({ "kind": "fn", "params": [{ "kind": "builtin", "name": "int" }, { "kind": "builtin", "name": "int" }],
            "result": { "kind": "tuple", "elems": [{ "kind": "builtin", "name": "int" }, { "kind": "builtin", "name": "int" }] } });
        // The post as the module states it: pre d ≠ 0, post gcd(|fst(result)|, snd(result)) == 1.
        let r = |acc: &str| op(acc, vec![v("result")]);
        let refinements = vec![
            json!({ "kind": "pre", "expr": op("neq", vec![v("d"), lit(0)]) }),
            json!({ "kind": "post", "expr": op("eq", vec![pg(op("abs", vec![r("fst")]), r("snd")), lit(1)]) }),
        ];
        let prev = set_callee_contracts([(target.clone(), contract)].into_iter().collect());
        let reports = crate::refine::check_refinements(&ty, &refinements, &body, solver);
        set_callee_contracts(prev);
        let post = reports.iter().find(|r| r.label == "post[0]").expect("the post's report");
        assert!(matches!(post.outcome, crate::RefinementOutcome::Sound), "{:?}", post.outcome);
    }

    /// A callee whose CONTRACT applies another callee the obligation does not: the second is
    /// declared before the first's axioms mention it (it was declared after them, or not at all,
    /// and the solver answered "unknown constant" — the rational tier's lowest-terms checker).
    #[test]
    fn a_callee_named_only_inside_a_contract_is_declared_before_the_axioms() {
        let Some(solver) = solver() else { return };
        let a = format!("fn_{}", "0a".repeat(32));
        let b = format!("fn_{}", "0b".repeat(32));
        let contract = |symbol: &str, posts: Vec<J>| CalleeContract {
            symbol: symbol.into(), params: vec!["x".into()], param_sorts: vec![Sort::Int], ret: Sort::Int,
            pres: vec![], posts, laws: vec![], nat_positions: BTreeSet::new(),
        };
        // A's post: B(result) == 1; B's post: result >= 0.
        let a_contract = contract("callee_aaaa", vec![op("eq", vec![op(&b, vec![v("result")]), lit(1)])]);
        let b_contract = contract("callee_bbbb", vec![op("ge", vec![v("result"), lit(0)])]);
        let call_a = json!({ "kind": "app", "fn": { "kind": "lit", "value": { "kind": "fn_ref", "target": a } }, "args": [v("n")] });
        // The law applies A only: `A(n) == A(n)` (trivially true; what matters is the preamble).
        let goal = law(&["n"], op("eq", vec![call_a.clone(), call_a]));
        let prev = set_callee_contracts([(a.clone(), a_contract), (b.clone(), b_contract)].into_iter().collect());
        let (out, cert) = prove_property(&goal, None, solver);
        set_callee_contracts(prev);
        assert!(matches!(out, ProofOutcome::Proved), "{out:?}");
        let smt = cert.unwrap().smt;
        let decl_b = smt.find("(declare-fun callee_bbbb").expect("B declared");
        let use_b = smt.find("(callee_bbbb ").expect("B used in A's axiom");
        assert!(decl_b < use_b, "B must be declared before A's axioms mention it:\n{smt}");
    }

    /// pow_mod_naive: \b e m -> case gt(e, 0) of true -> mod(mul(self(b, sub(e, 1), m), mod(b, m)), m) | false -> mod(1, m)
    fn naive_power_body() -> J {
        let step = op("mod", vec![op("mul", vec![self_app(vec![v("b"), op("sub", vec![v("e"), ilit(1)]), v("m")]), op("mod", vec![v("b"), v("m")])]), v("m")]);
        json!({ "kind": "lambda", "params": [{ "name": "b" }, { "name": "e" }, { "name": "m" }], "body": { "kind": "case",
            "scrutinee": op("gt", vec![v("e"), ilit(0)]),
            "arms": [
                { "pattern": { "kind": "lit", "value": { "kind": "bool", "value": true } }, "body": step },
                { "pattern": { "kind": "lit", "value": { "kind": "bool", "value": false } }, "body": op("mod", vec![ilit(1), v("m")]) } ] } })
    }

    #[test]
    fn the_doubling_law_closes_stepwise() {
        // pow(b, 2e) ≡ pow(b, e)² (mod m): the step is pure modular algebra whose residues the two
        // sides reach only through the unfoldings — one query drowns (measured), the stepwise
        // checker closes it: every instance's premise verified alone, the final query over
        // uninterpreted mod/div/products.
        let Some(solver) = solver() else { return };
        let nats: BTreeSet<usize> = [1].into();
        let doubling = law(&["b", "e", "m"], op("or", vec![
            op("eq", vec![v("m"), lit(0)]),
            op("eq", vec![self_app(vec![v("b"), op("mul", vec![lit(2), v("e")]), v("m")]),
                          op("mod", vec![op("mul", vec![self_app(vec![v("b"), v("e"), v("m")]), self_app(vec![v("b"), v("e"), v("m")])]), v("m")])])]));
        let (out, cert, m) = prove_by_numeric_induction(&doubling, Some(&naive_power_body()), &nats, solver);
        assert!(matches!(out, ProofOutcome::Proved), "{out:?}");
        let cert = cert.unwrap();
        assert!(cert.euclid && cert.stepwise.is_some(), "{m:?}");
        assert!(cert.steps.as_deref().unwrap_or("").contains("(push 1)"));
        assert!(cert.smt.contains("(declare-fun umod (Int Int) Int)") && cert.smt.contains("umul"), "{}", cert.smt);
        // The false doubling `… ≡ pow(b, e)² + 1 (mod m)` stays open.
        let wrong = law(&["b", "e", "m"], op("or", vec![
            op("eq", vec![v("m"), lit(0)]),
            op("eq", vec![self_app(vec![v("b"), op("mul", vec![lit(2), v("e")]), v("m")]),
                          op("mod", vec![op("add", vec![op("mul", vec![self_app(vec![v("b"), v("e"), v("m")]), self_app(vec![v("b"), v("e"), v("m")])]), lit(1)]), v("m")])])]));
        let (out, _, _) = prove_by_numeric_induction(&wrong, Some(&naive_power_body()), &nats, solver);
        assert!(matches!(out, ProofOutcome::Unknown), "{out:?}");
    }

    #[test]
    fn the_sum_congruence_closes_through_the_euclidean_rung() {
        // mod_add's law: (a + b) mod m = ((a mod m) + (b mod m)) mod m, m ≠ 0 — UNKNOWN to the
        // bare first-order prover (nonlinear `mod`), PROVED with the uniqueness-lemma instances.
        let Some(solver) = solver() else { return };
        let body = json!({ "kind": "lambda", "params": [{ "name": "a" }, { "name": "b" }, { "name": "m" }],
            "body": op("mod", vec![op("add", vec![op("mod", vec![v("a"), v("m")]), op("mod", vec![v("b"), v("m")])]), v("m")]) });
        let congruence = law(&["a", "b", "m"], op("or", vec![
            op("eq", vec![v("m"), lit(0)]),
            op("eq", vec![self_app(vec![v("a"), v("b"), v("m")]), op("mod", vec![op("add", vec![v("a"), v("b")]), v("m")])])]));
        let (out, cert) = prove_property(&congruence, Some(&body), solver);
        assert!(matches!(out, ProofOutcome::Proved), "{out:?}");
        assert!(cert.unwrap().euclid);
        // A false congruence stays UNKNOWN — the instances are true facts and refute nothing,
        // and a model found with them is still a model (REFUTED would need one the solver
        // actually produces; with nonlinear `mod` it does not).
        let wrong = law(&["a", "b", "m"], op("or", vec![
            op("eq", vec![v("m"), lit(0)]),
            op("eq", vec![self_app(vec![v("a"), v("b"), v("m")]), op("mod", vec![op("add", vec![v("a"), v("b"), lit(1)]), v("m")])])]));
        let (out, cert) = prove_property(&wrong, Some(&body), solver);
        assert!(!matches!(out, ProofOutcome::Proved), "{out:?}");
        assert!(!cert.unwrap().euclid);
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

    fn ty_list(elem: &str) -> J {
        json!({ "kind": "apply", "ctor": { "kind": "builtin", "name": "List" }, "args": [{ "kind": "builtin", "name": elem }] })
    }
    fn ty_fn(params: Vec<J>, result: J) -> J {
        json!({ "kind": "fn", "params": params, "result": result })
    }
    fn ty(n: &str) -> J {
        json!({ "kind": "builtin", "name": n })
    }
    fn case_bool(scrut: J, on_true: J, on_false: J) -> J {
        json!({ "kind": "case", "scrutinee": scrut, "arms": [
            { "pattern": { "kind": "lit", "value": { "kind": "bool", "value": true } }, "body": on_true },
            { "pattern": { "kind": "lit", "value": { "kind": "bool", "value": false } }, "body": on_false }] })
    }

    /// pad_to: \xs n -> case gt(n, 0) of true -> case null(xs) of true -> cons(0, self(nil, sub(n, 1)))
    /// | false -> cons(head(xs), self(tail(xs), sub(n, 1))) | false -> nil — a NUMERIC recursion that
    /// builds a list (the integer-tier module's record).
    fn pad_to_body() -> J {
        let n1 = op("sub", vec![v("n"), slit(1)]);
        json!({ "kind": "lambda", "params": [{ "name": "xs" }, { "name": "n" }], "body":
            case_bool(op("gt", vec![v("n"), slit(0)]),
                case_bool(op("null", vec![v("xs")]),
                    op("cons", vec![slit(0), self_app(vec![v("nil"), n1.clone()])]),
                    op("cons", vec![op("head", vec![v("xs")]), self_app(vec![op("tail", vec![v("xs")]), n1])])),
                v("nil")) })
    }

    /// from_digits: \b ds -> case null(ds) of true -> 0 | false -> add(head(ds), mul(b, self(b, tail(ds))))
    /// — a STRUCTURAL recursion (on its second parameter) with a numeric contract.
    fn from_digits_body() -> J {
        json!({ "kind": "lambda", "params": [{ "name": "b" }, { "name": "ds" }], "body":
            case_bool(op("null", vec![v("ds")]),
                slit(0),
                op("add", vec![op("head", vec![v("ds")]), op("mul", vec![v("b"), self_app(vec![v("b"), op("tail", vec![v("ds")])])])])) })
    }
    /// A structured (body-form) integer literal — the termination checker reads guards and descents
    /// in this spelling; `lit` above is the predicate form.
    fn slit(n: i64) -> J {
        json!({ "kind": "lit", "value": { "kind": "int", "value": n } })
    }

    #[test]
    fn lists_lower_in_the_induction_rung_only() {
        let Some(solver) = solver() else { return };
        let body = pad_to_body();
        // forall xs n. lt(n, 0) or length(self(xs, n)) == n
        let law_len = |rhs: J| law(&["xs", "n"], op("or", vec![op("lt", vec![v("n"), lit(0)]), op("eq", vec![op("length", vec![self_app(vec![v("xs"), v("n")])]), rhs])]));
        // The first-order prover still refuses list operators (so a list law reaches the structural backend).
        assert!(matches!(prove_property(&law_len(v("n")), Some(&body), solver).0, ProofOutcome::Unsupported(_)));
        let prev = set_self_type(Some(ty_fn(vec![ty_list("int"), ty("nat")], ty_list("int"))));
        let nats: BTreeSet<usize> = [1].into();
        // Induction on `n` with the list in play: PROVED, the certificate declaring the list datatype.
        let (out, cert, m) = prove_by_numeric_induction(&law_len(v("n")), Some(&body), &nats, solver);
        assert!(matches!(out, ProofOutcome::Proved), "{out:?}");
        assert_eq!(m.as_deref(), Some("n"));
        let smt = cert.unwrap().smt;
        assert!(smt.contains("(declare-datatypes ((Lst 0))") && smt.contains("(define-fun-rec length") && smt.contains("(declare-const xs Lst)"), "{smt}");
        // The false law `length(self(xs, n)) == n + 1` is never proved.
        let (out, _, _) = prove_by_numeric_induction(&law_len(op("add", vec![v("n"), lit(1)])), Some(&body), &nats, solver);
        assert!(matches!(out, ProofOutcome::Unknown), "{out:?}");
        set_self_type(prev);
    }

    #[test]
    fn self_contract_rung_decides_consequences_of_proved_facts() {
        let Some(solver) = solver() else { return };
        // isqrt's certified contract: result ≥ 0, result² ≤ n, result ≤ n (posts) and the proved
        // floor-bounds law n < 0 ∨ (self(n)² ≤ n ∧ n < (self(n)+1)²).
        let result = || v("result");
        let floor_bounds = law(&["n"], op("or", vec![op("lt", vec![v("n"), lit(0)]), op("and", vec![
            op("le", vec![op("mul", vec![self_app(vec![v("n")]), self_app(vec![v("n")])]), v("n")]),
            op("lt", vec![v("n"), op("mul", vec![op("add", vec![self_app(vec![v("n")]), lit(1)]), op("add", vec![self_app(vec![v("n")]), lit(1)])])])])]));
        let contract = CalleeContract {
            symbol: "callee_isqrt".into(), params: vec!["n".into()], param_sorts: vec![Sort::Int], ret: Sort::Int, pres: vec![],
            posts: vec![op("ge", vec![result(), lit(0)]), op("le", vec![op("mul", vec![result(), result()]), v("n")]), op("le", vec![result(), v("n")])],
            laws: vec![floor_bounds], nat_positions: [0].into(),
        };
        let target = "fn_isqrt_test";
        let nats: BTreeSet<usize> = [0].into();
        // n < 0 ∨ isqrt(n·n) = n — outside every induction principle, a consequence of the contract.
        let square = |rhs: J| law(&["n"], op("or", vec![op("lt", vec![v("n"), lit(0)]), op("eq", vec![self_app(vec![op("mul", vec![v("n"), v("n")])]), rhs])]));
        let (out, cert) = prove_from_self_contract(&square(v("n")), target, contract.clone(), &nats, solver);
        assert!(matches!(out, ProofOutcome::Proved), "{out:?}");
        let smt = cert.unwrap().smt;
        // The contract is instantiated at the law's application (a `let` binding n to n·n), not only quantified.
        assert!(smt.contains("(let ((n (* n n)))"), "{smt}");
        // The false `isqrt(n·n) = n + 1` is never proved — and never refuted (self is uninterpreted).
        let (out, _) = prove_from_self_contract(&square(op("add", vec![v("n"), lit(1)])), target, contract, &nats, solver);
        assert!(matches!(out, ProofOutcome::Unknown), "{out:?}");
    }

    #[test]
    fn body_callee_applications_get_contract_instances_at_the_goal() {
        let Some(solver) = solver() else { return };
        // A checker record: \a b -> eq(callee(a, b), add(a, b)), where the callee's certified post
        // (over nats) is `result = a + b`. Its law `forall a b. self(a, b) = true` holds on the
        // callee's domain — the instance of the post at the body's application decides it.
        let target = "fn_sum_test";
        let callee_app = json!({ "kind": "app", "op": target, "args": [v("a"), v("b")] });
        let body = json!({ "kind": "lambda", "params": [{ "name": "a" }, { "name": "b" }],
            "body": op("eq", vec![callee_app, op("add", vec![v("a"), v("b")])]) });
        let contract = CalleeContract {
            symbol: "callee_sum".into(), params: vec!["a".into(), "b".into()], param_sorts: vec![Sort::Int, Sort::Int], ret: Sort::Int,
            pres: vec![], posts: vec![op("eq", vec![v("result"), op("add", vec![v("a"), v("b")])])], laws: vec![], nat_positions: [0, 1].into(),
        };
        let prev = set_callee_contracts([(target.to_string(), contract)].into_iter().collect());
        let law = law(&["a", "b"], op("eq", vec![self_app(vec![v("a"), v("b")]), json!({ "kind": "lit", "value": true })]));
        // Over the nat domain (the record's parameters typed nat): PROVED.
        let (out, cert) = prove_property_in_domain(&law, Some(&body), &[0usize, 1].into(), solver);
        assert!(matches!(out, ProofOutcome::Proved), "{out:?}");
        let smt = cert.unwrap().smt;
        assert!(smt.contains("(let ((a (let ((a a) (b b)) a))"), "the body's application is instantiated at the goal's: {smt}");
        // Over all ints the callee's contract says nothing: UNKNOWN, never a false PROVED (nor REFUTED).
        let (out, _) = prove_property_in_domain(&law, Some(&body), &BTreeSet::new(), solver);
        assert!(matches!(out, ProofOutcome::Unknown), "{out:?}");
        set_callee_contracts(prev);
    }

    #[test]
    fn callee_induction_proves_the_digits_round_trip() {
        let Some(solver) = solver() else { return };
        // digits: \b n -> case lt(b, 2) of true -> nil | false -> case lt(n, 1) of true -> nil
        //   | false -> cons(mod(n, b), self(b, div(n, b)))            — numeric recursion on n
        // from_digits: \b ds -> case null(ds) of true -> 0 | false -> add(head(ds), mul(b, self(b, tail(ds))))
        let digits = json!({ "kind": "lambda", "params": [{ "name": "b" }, { "name": "n" }], "body":
            case_bool(op("lt", vec![v("b"), slit(2)]), v("nil"),
                case_bool(op("lt", vec![v("n"), slit(1)]), v("nil"),
                    op("cons", vec![op("mod", vec![v("n"), v("b")]), self_app(vec![v("b"), op("div", vec![v("n"), v("b")])])]))) });
        let from_digits = from_digits_body();
        let (td, tf) = ("fn_digits_test", "fn_from_digits_test");
        let contracts: BTreeMap<String, CalleeContract> = [
            (td.to_string(), CalleeContract { symbol: "callee_digits".into(), params: vec!["b".into(), "n".into()], param_sorts: vec![Sort::Int, Sort::Int], ret: Sort::Lst, pres: vec![], posts: vec![], laws: vec![], nat_positions: [0, 1].into() }),
            (tf.to_string(), CalleeContract { symbol: "callee_from_digits".into(), params: vec!["b".into(), "ds".into()], param_sorts: vec![Sort::Int, Sort::Lst], ret: Sort::Int, pres: vec![], posts: vec![op("ge", vec![v("result"), lit(0)])], laws: vec![], nat_positions: [0].into() }),
        ].into_iter().collect();
        let prev = set_callee_contracts(contracts);
        let defs = move |t: &str| -> Option<(J, BTreeSet<usize>)> {
            if t == td { Some((digits.clone(), [0, 1].into())) } else if t == tf { Some((from_digits.clone(), [0].into())) } else { None }
        };
        // digits_roundtrip: \b n -> case lt(b, 2) of true -> true | false -> case lt(n, 0) of true -> true
        //   | false -> eq(from_digits(b, digits(b, n)), RHS)
        let checker = |rhs: J| json!({ "kind": "lambda", "params": [{ "name": "b" }, { "name": "n" }], "body":
            case_bool(op("lt", vec![v("b"), slit(2)]), json!({ "kind": "lit", "value": { "kind": "bool", "value": true } }),
                case_bool(op("lt", vec![v("n"), slit(0)]), json!({ "kind": "lit", "value": { "kind": "bool", "value": true } }),
                    op("eq", vec![op(tf, vec![v("b"), op(td, vec![v("b"), v("n")])]), rhs]))) });
        let law = law(&["b", "n"], op("eq", vec![self_app(vec![v("b"), v("n")]), json!({ "kind": "lit", "value": true })]));
        let nats: BTreeSet<usize> = [0, 1].into();
        let (out, cert, m) = prove_by_callee_induction(&law, &checker(v("n")), &nats, &defs, solver);
        assert!(matches!(out, ProofOutcome::Proved), "{out:?}");
        assert_eq!(m.as_deref(), Some("n (through `fn_digits_t…`)"));
        let smt = cert.unwrap().smt;
        assert!(smt.contains(";; induction hypothesis at (callee_digits") && smt.contains("(= (callee_digits "), "{smt}");
        // The false round-trip `… = n + 1` is never proved.
        let (out, _, _) = prove_by_callee_induction(&law, &checker(op("add", vec![v("n"), lit(1)])), &nats, &defs, solver);
        assert!(matches!(out, ProofOutcome::Unknown), "{out:?}");
        set_callee_contracts(prev);
    }

    #[test]
    fn structural_measure_induction_states_the_declared_element_domain() {
        let Some(solver) = solver() else { return };
        let body = from_digits_body();
        let nats: BTreeSet<usize> = [0].into();
        let nonneg = law(&["b", "ds"], op("ge", vec![self_app(vec![v("b"), v("ds")]), lit(0)]));
        // Over `List nat`: PROVED by induction on the structure of `ds` — the hypothesis instantiated at
        // the tail-descending call, the element domain stated as `all_nat`, no quantified hypothesis.
        let prev = set_self_type(Some(ty_fn(vec![ty("nat"), ty_list("nat")], ty("nat"))));
        let (out, cert, m) = prove_by_numeric_induction(&nonneg, Some(&body), &nats, solver);
        assert!(matches!(out, ProofOutcome::Proved), "{out:?}");
        assert_eq!(m.as_deref(), Some("ds"));
        let smt = cert.unwrap().smt;
        assert!(smt.contains("(define-fun-rec all_nat") && smt.contains("(assert (all_nat ds))") && !smt.contains("(forall ((b__ih"), "{smt}");
        assert!(smt.contains("(=> (not ((_ is nil) ds))"), "{smt}");
        // The false strengthening `self(b, ds) >= 1` is never proved.
        let (out, _, _) = prove_by_numeric_induction(&law(&["b", "ds"], op("ge", vec![self_app(vec![v("b"), v("ds")]), lit(1)])), Some(&body), &nats, solver);
        assert!(matches!(out, ProofOutcome::Unknown), "{out:?}");
        // Over `List int` the same law is FALSE (ds = [-1]): the element domain is not stated, UNKNOWN.
        set_self_type(Some(ty_fn(vec![ty("nat"), ty_list("int")], ty("int"))));
        let (out, cert, _) = prove_by_numeric_induction(&nonneg, Some(&body), &nats, solver);
        assert!(matches!(out, ProofOutcome::Unknown), "{out:?}");
        assert!(!cert.unwrap().smt.contains("all_nat"));
        set_self_type(prev);
    }
}
