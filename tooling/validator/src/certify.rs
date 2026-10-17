//! Certification — run every "verified by default" check against a function record and its body, and bundle
//! the verdicts into one result. This is the library core behind `nl-validator certify` (the CLI adds
//! table/JSON output and optional signing) and is also called from the agent loop
//! ([`crate::orchestrate`]), so a discovered function is *certified before it is applied* — principle 3
//! (verified by default) made operational in "assemble, don't write".
//!
//! The checks are `schema` (the record validates against the pinned v0.2 function-record schema —
//! the ZEROTH check, evolution/gcp-sdk-poc finding 12: without it a record could certify and still
//! be refused by the commons gate, because the producer's checker and the admitting authority
//! disagreed about what a valid record even is), `typecheck` (type), `check-effects` (effects ⊆
//! declared), `check-refinement` (the type-implied `nat` + declared `pre`/`post`),
//! `check-termination` (a declared `terminates: always`), and `check-complexity` (a declared
//! `complexity` and the structured `cost`). A record is **certified** unless a check *actively
//! fails its declaration* — a SCHEMA-INVALID record, an ILL-TYPED body, an UNDER-DECLARED effect,
//! or a VIOLATED refinement (`hard_fail`); the conservative UNVERIFIABLE verdicts (a
//! bound/termination the structural analysis can't confirm) are recorded but do not revoke
//! certification, since none of those checks can *disprove* a claim, only fail to establish it.

use serde_json::Value as J;
use std::collections::HashMap;

use crate::{
    analyze_output_size, check_refinements, infer_effects,
    parse_class, parse_output_size, typecheck_record, ComplexityOutcome, OutputSize, RefinementOutcome,
    TerminationOutcome,
};

/// One check's verdict inside a [`Certification`].
#[derive(Debug, Clone)]
pub struct CertCheck {
    pub check: String,
    pub verdict: &'static str,
    pub detail: String,
    /// True only when the check *actively failed* its declaration (ILL-TYPED / UNDER-DECLARED / VIOLATED) —
    /// these revoke certification. A conservative UNVERIFIABLE is not a hard failure.
    pub hard_fail: bool,
}

impl CertCheck {
    fn new(check: impl Into<String>, verdict: &'static str, detail: impl Into<String>, hard_fail: bool) -> Self {
        CertCheck { check: check.into(), verdict, detail: detail.into(), hard_fail }
    }
}

/// The bundled result of certifying a record.
#[derive(Debug, Clone)]
pub struct Certification {
    /// Content-address of the certified record (`fn_…`).
    pub subject: String,
    /// Content-address of its body expression (`expr_…`).
    pub body_hash: String,
    pub checks: Vec<CertCheck>,
    /// True unless a check actively failed its declaration.
    pub certified: bool,
}

/// The result type of a record's `signature.type` (unwrapping a `forall`), for output-size analysis.
fn unwrap_result_type(ty: &J) -> Option<J> {
    let t = if ty.get("kind").and_then(|k| k.as_str()) == Some("forall") { ty.get("body")? } else { ty };
    t.get("result").cloned()
}

/// The complexity/cost verdict token + detail for a declared `O(…)` class vs an inferred sound bound.
fn time_verdict_parts(declared: &str, inferred: &ComplexityOutcome) -> (&'static str, String) {
    match inferred {
        ComplexityOutcome::Opaque(why) => ("UNVERIFIABLE", format!("declared `{declared}`, not established: {why}")),
        ComplexityOutcome::Bound(b) => {
            let inf = b.display();
            match parse_class(declared) {
                None => ("UNVERIFIABLE", format!("declared `{declared}` is not a recognized class (inferred {inf})")),
                Some(dc) if *b == dc => ("SOUND", format!("within declared `{declared}` (inferred {inf})")),
                Some(dc) if *b < dc => ("VERIFIED", format!("provably {inf}, tighter than declared `{declared}`")),
                Some(_) => ("UNVERIFIABLE", format!("declared `{declared}`, but sound bound is {inf} (worse)")),
            }
        }
    }
}

/// The pinned v0.2 function-record schema, compiled into the binary (it is self-contained —
/// inlined `$defs`, no cross-file refs — so the check runs wherever the binary does, with no
/// spec tree on disk). Single source: the file in `spec/` at build time.
const FN_RECORD_V02_SCHEMA: &str = include_str!("../../../spec/function-record.v0.2.schema.json");

fn v02_validator() -> Option<&'static jsonschema::Validator> {
    static V: std::sync::OnceLock<Option<jsonschema::Validator>> = std::sync::OnceLock::new();
    V.get_or_init(|| {
        let schema: J = serde_json::from_str(FN_RECORD_V02_SCHEMA).ok()?;
        jsonschema::options().with_draft(jsonschema::Draft::Draft202012).build(&schema).ok()
    })
    .as_ref()
}

/// Run every "verified by default" check against `record` + `body`, resolving `fn_ref` effect callees
/// against `records`. See the module docs for the certification rule.
pub fn certify_record(record: &J, body: &J, records: &HashMap<String, J>, solver: &str) -> Certification {
    certify_record_linked(record, body, records, None, solver)
}

/// A LINKED view over a records directory for folding `fn_ref` callees into the termination and
/// complexity checks: `records` maps `fn_…` → function record, `bodies` maps `fn_…`/`expr_…` → body
/// (what [`crate::build_link_map`] builds). A callee's verdict is RE-DERIVED from its own body (its
/// declaration is never read as a fact), transitively, memoized, with a cycle guard (a cycle is
/// unresolvable — mutual recursion through addresses has no single measure here).
pub struct LinkedView<'a> {
    records: &'a HashMap<String, J>,
    bodies: &'a HashMap<String, J>,
    solver: String,
    term: std::cell::RefCell<HashMap<String, Option<bool>>>,
    cx: std::cell::RefCell<HashMap<String, Option<crate::Class>>>,
    bounds: std::cell::RefCell<HashMap<String, Option<Vec<usize>>>>,
    contracts: std::cell::RefCell<HashMap<String, Option<crate::CalleeContract>>>,
    in_progress: std::cell::RefCell<std::collections::HashSet<String>>,
}

impl<'a> LinkedView<'a> {
    pub fn new(records: &'a HashMap<String, J>, bodies: &'a HashMap<String, J>, solver: &str) -> Self {
        LinkedView {
            records,
            bodies,
            solver: solver.to_string(),
            term: Default::default(),
            cx: Default::default(),
            bounds: Default::default(),
            contracts: Default::default(),
            in_progress: Default::default(),
        }
    }

    /// The callee's parameter positions whose values bound its RESULT, from certified facts only: a
    /// declared postcondition `result < p` / `result <= p` (alone or inside a conjunction) that
    /// `check-refinement` PROVES here, or a list result the output-size analysis bounds linearly in
    /// the input (then every parameter). `None` when nothing certified bounds it.
    pub fn result_bounded_by(&self, target: &str) -> Option<Vec<usize>> {
        if let Some(v) = self.bounds.borrow().get(target) {
            return v.clone();
        }
        let key = format!("bounds:{target}");
        if !self.in_progress.borrow_mut().insert(key.clone()) {
            return None;
        }
        let out = self.callee(target).and_then(|(rec, body)| {
            let params: Vec<String> = body
                .get("params")
                .and_then(|p| p.as_array())
                .map(|ps| ps.iter().filter_map(|p| p.get("name").and_then(|n| n.as_str()).map(String::from)).collect())
                .unwrap_or_default();
            let sig_type = rec.pointer("/signature/type")?;
            let refinements: Vec<J> = rec.pointer("/signature/refinements").and_then(|r| r.as_array()).cloned().unwrap_or_default();
            // A list result at most linear in the input bounds itself by the whole input.
            if let Some(rt) = unwrap_result_type(sig_type) {
                if crate::complexity::is_list_type(&rt) {
                    return match analyze_output_size(&rt, body) {
                        crate::OutputSize::Const => Some(vec![]),
                        crate::OutputSize::Linear => Some((0..params.len()).collect()),
                        _ => None,
                    };
                }
            }
            // A proved `result < p` / `result <= p`.
            let posts: Vec<&J> = refinements
                .iter()
                .filter(|r| r.get("kind").and_then(|k| k.as_str()) == Some("post"))
                .filter_map(|r| r.get("expr"))
                .collect();
            if posts.is_empty() {
                return None;
            }
            let reports = check_refinements(sig_type, &refinements, body, &self.solver);
            let mut idxs: Vec<usize> = Vec::new();
            for (i, post) in posts.iter().enumerate() {
                let sound = reports
                    .iter()
                    .any(|r| r.label == format!("post[{i}]") && matches!(r.outcome, RefinementOutcome::Sound));
                if !sound {
                    continue;
                }
                for p in upper_bound_params(post) {
                    if let Some(j) = params.iter().position(|q| *q == p) {
                        if !idxs.contains(&j) {
                            idxs.push(j);
                        }
                    }
                }
            }
            (!idxs.is_empty()).then_some(idxs)
        });
        self.in_progress.borrow_mut().remove(&key);
        self.bounds.borrow_mut().insert(target.to_string(), out.clone());
        out
    }

    fn callee(&self, target: &str) -> Option<(&'a J, &'a J)> {
        let rec = self.records.get(target)?;
        let body = self.bodies.get(target).or_else(|| rec.get("body_hash").and_then(|h| h.as_str()).and_then(|h| self.bodies.get(h)))?;
        Some((rec, body))
    }

    /// Does the callee at `target` provably terminate (its own analysis, over this same view)?
    pub fn terminates(&self, target: &str) -> Option<bool> {
        if let Some(v) = self.term.borrow().get(target) {
            return *v;
        }
        if !self.in_progress.borrow_mut().insert(target.to_string()) {
            return None; // a cycle: unresolvable
        }
        let (rec, body) = match self.callee(target) {
            Some(x) => x,
            None => {
                self.in_progress.borrow_mut().remove(target);
                return None;
            }
        };
        let nat = crate::terminate::nat_param_positions(rec);
        let lookup = |t: &str| self.terminates(t);
        let out = matches!(
            crate::terminate::analyze_termination_linked(body, &nat, Some(&lookup)),
            crate::terminate::TerminationOutcome::Always
        );
        self.in_progress.borrow_mut().remove(target);
        self.term.borrow_mut().insert(target.to_string(), Some(out));
        Some(out)
    }

    /// The callee's own inferred complexity class (over this same view), if it has one.
    pub fn complexity(&self, target: &str) -> Option<crate::Class> {
        if let Some(v) = self.cx.borrow().get(target) {
            return *v;
        }
        let key = format!("cx:{target}");
        if !self.in_progress.borrow_mut().insert(key.clone()) {
            return None;
        }
        let out = match self.callee(target) {
            Some((_, body)) => match crate::complexity::analyze_complexity_linked(body, Some(self)) {
                crate::complexity::ComplexityOutcome::Bound(c) => Some(c),
                _ => None,
            },
            None => None,
        };
        self.in_progress.borrow_mut().remove(&key);
        self.cx.borrow_mut().insert(target.to_string(), out);
        out
    }
}

impl<'a> LinkedView<'a> {
    /// The callee's CERTIFIED contract for the prover (`prove::CalleeContract`): its declared
    /// preconditions and every postcondition (plus the type-implied `result ≥ 0` of a `nat` result)
    /// that `check-refinement` PROVES on this view — the callee's own callees' contracts installed
    /// first, transitively. `None` when the callee is unresolvable, its sorts are outside the
    /// Int/Bool/String fragment, or it is in progress (a cycle).
    pub fn contract(&self, target: &str) -> Option<crate::CalleeContract> {
        if let Some(v) = self.contracts.borrow().get(target) {
            return v.clone();
        }
        let key = format!("contract:{target}");
        if !self.in_progress.borrow_mut().insert(key.clone()) {
            return None;
        }
        let out = self.callee(target).and_then(|(rec, body)| {
            let params: Vec<String> = body
                .get("params")
                .and_then(|p| p.as_array())
                .map(|ps| ps.iter().filter_map(|p| p.get("name").and_then(|n| n.as_str()).map(String::from)).collect())
                .unwrap_or_default();
            let sig_type = rec.pointer("/signature/type")?;
            let fn_ty = if sig_type.get("kind").and_then(|k| k.as_str()) == Some("forall") { sig_type.get("body")? } else { sig_type };
            let sort_of_type = |t: &J| -> Option<crate::Sort> {
                match crate::builtin_type_name(t) {
                    Some("int") | Some("nat") => Some(crate::Sort::Int),
                    Some("bool") => Some(crate::Sort::Bool),
                    Some("string") => Some(crate::Sort::Str),
                    _ => None,
                }
            };
            let param_tys = fn_ty.get("params").and_then(|p| p.as_array())?;
            if param_tys.len() != params.len() {
                return None;
            }
            let param_sorts: Vec<crate::Sort> = param_tys.iter().map(sort_of_type).collect::<Option<_>>()?;
            let ret = sort_of_type(fn_ty.get("result")?)?;
            let nat_positions = crate::terminate::nat_positions_of_type(sig_type);
            let refinements: Vec<J> = rec.pointer("/signature/refinements").and_then(|r| r.as_array()).cloned().unwrap_or_default();
            let pres: Vec<J> = refinements
                .iter()
                .filter(|r| r.get("kind").and_then(|k| k.as_str()) == Some("pre"))
                .filter_map(|r| r.get("expr").cloned())
                .collect();
            let declared_posts: Vec<J> = refinements
                .iter()
                .filter(|r| r.get("kind").and_then(|k| k.as_str()) == Some("post"))
                .filter_map(|r| r.get("expr").cloned())
                .collect();
            // Prove the callee's contract — posts AND laws — with ITS callees' contracts installed.
            let deps = self.contracts_for(body);
            let prev = crate::set_callee_contracts(deps);
            let reports = check_refinements(sig_type, &refinements, body, &self.solver);
            let laws: Vec<J> = rec
                .get("properties")
                .and_then(|p| p.as_array())
                .cloned()
                .unwrap_or_default()
                .iter()
                .filter_map(|p| p.get("expr").cloned())
                .filter(|law| {
                    matches!(crate::prove_property(law, Some(body), &self.solver).0, crate::ProofOutcome::Proved)
                        || matches!(crate::prove_by_numeric_induction(law, Some(body), &nat_positions, &self.solver).0, crate::ProofOutcome::Proved)
                })
                .collect();
            crate::set_callee_contracts(prev);
            let sound = |label: &str| reports.iter().any(|r| r.label == label && matches!(r.outcome, RefinementOutcome::Sound));
            let mut posts: Vec<J> = declared_posts
                .iter()
                .enumerate()
                .filter(|(i, _)| sound(&format!("post[{i}]")))
                .map(|(_, p)| p.clone())
                .collect();
            if sound("nat-result (≥ 0)") {
                posts.push(serde_json::json!({ "kind": "app", "op": "ge", "args": [
                    { "kind": "var", "name": "result" }, { "kind": "lit", "value": { "kind": "int", "value": 0 } }] }));
            }
            Some(crate::CalleeContract {
                symbol: format!("callee_{}", &target[3..target.len().min(15)]),
                params,
                param_sorts,
                ret,
                pres,
                posts,
                laws,
                nat_positions,
            })
        });
        self.in_progress.borrow_mut().remove(&key);
        self.contracts.borrow_mut().insert(target.to_string(), out.clone());
        out
    }

    /// The contracts of every callee a body applies by `fn_ref` (those that have one).
    pub fn contracts_for(&self, body: &J) -> std::collections::BTreeMap<String, crate::CalleeContract> {
        let mut targets = std::collections::BTreeSet::new();
        fn_ref_targets(body, &mut targets);
        targets.into_iter().filter_map(|t| self.contract(&t).map(|c| (t, c))).collect()
    }
}

/// Every `fn_ref` literal target in an AST.
fn fn_ref_targets(node: &J, out: &mut std::collections::BTreeSet<String>) {
    match node {
        J::Object(m) => {
            if m.get("kind").and_then(|k| k.as_str()) == Some("fn_ref") {
                if let Some(t) = m.get("target").and_then(|t| t.as_str()) {
                    out.insert(t.to_string());
                }
            }
            m.values().for_each(|v| fn_ref_targets(v, out));
        }
        J::Array(xs) => xs.iter().for_each(|v| fn_ref_targets(v, out)),
        _ => {}
    }
}

impl crate::complexity::LinkedCallees for LinkedView<'_> {
    fn class_of(&self, target: &str) -> Option<crate::Class> {
        self.complexity(target)
    }
    fn result_bounded_by(&self, target: &str) -> Option<Vec<usize>> {
        LinkedView::result_bounded_by(self, target)
    }
}

/// The parameters an upper-bound postcondition names: `result < p`, `result <= p`, or a conjunction
/// containing such (the reserved `result` on the left, a variable on the right).
fn upper_bound_params(post: &J) -> Vec<String> {
    let mut out = Vec::new();
    fn go(node: &J, out: &mut Vec<String>) {
        let op = node.get("op").and_then(|o| o.as_str()).unwrap_or_default();
        let args = node.get("args").and_then(|a| a.as_array()).cloned().unwrap_or_default();
        match op {
            "and" => args.iter().for_each(|a| go(a, out)),
            "lt" | "le" if args.len() == 2 => {
                let is_result = args[0].get("kind").and_then(|k| k.as_str()) == Some("var")
                    && args[0].get("name").and_then(|n| n.as_str()) == Some("result");
                if is_result {
                    if let Some(p) = (args[1].get("kind").and_then(|k| k.as_str()) == Some("var"))
                        .then(|| args[1].get("name").and_then(|n| n.as_str()))
                        .flatten()
                    {
                        out.push(p.to_string());
                    }
                }
            }
            "gt" | "ge" if args.len() == 2 => {
                // `p > result` / `p >= result`, mirrored.
                let is_result = args[1].get("kind").and_then(|k| k.as_str()) == Some("var")
                    && args[1].get("name").and_then(|n| n.as_str()) == Some("result");
                if is_result {
                    if let Some(p) = (args[0].get("kind").and_then(|k| k.as_str()) == Some("var"))
                        .then(|| args[0].get("name").and_then(|n| n.as_str()))
                        .flatten()
                    {
                        out.push(p.to_string());
                    }
                }
            }
            _ => {}
        }
    }
    go(post, &mut out);
    out
}

/// [`certify_record`] with an optional LINKED view (`bodies` = [`crate::build_link_map`] of the records
/// directory): with it, a `fn_ref` callee whose own body provably terminates / has an inferred class
/// folds into the termination and complexity checks instead of reading as an opaque call.
pub fn certify_record_linked(
    record: &J,
    body: &J,
    records: &HashMap<String, J>,
    bodies: Option<&HashMap<String, J>>,
    solver: &str,
) -> Certification {
    let view = bodies.map(|b| LinkedView::new(records, b, solver));
    let term_lookup = |t: &str| view.as_ref().and_then(|v| v.terminates(t));
    let sig = record.pointer("/signature");
    let mut checks: Vec<CertCheck> = Vec::new();

    // 0. Schema (finding 12): "certified" must imply "the commons gate will accept it". A v0.2
    // record is held to the pinned record schema; SCHEMA-INVALID revokes certification. Other
    // schema versions predate the structured schema this binary pins — noted, never failed.
    match record.get("schema_version").and_then(|v| v.as_str()) {
        Some("0.2.0") => match v02_validator() {
            Some(v) => {
                let errors: Vec<String> = v
                    .iter_errors(record)
                    .map(|e| format!("at {}: {e}", e.instance_path))
                    .collect();
                if errors.is_empty() {
                    checks.push(CertCheck::new("schema", "VALID", "v0.2 function-record schema", false));
                } else {
                    checks.push(CertCheck::new("schema", "SCHEMA-INVALID", errors.join("; "), true));
                }
            }
            None => checks.push(CertCheck::new(
                "schema", "UNVERIFIABLE", "embedded v0.2 schema failed to compile", false,
            )),
        },
        v => checks.push(CertCheck::new(
            "schema", "N/A",
            format!("schema_version {} — only v0.2 records are schema-pinned here",
                    v.unwrap_or("(absent)")),
            false,
        )),
    }

    // 1. Type.
    match typecheck_record(record, body) {
        // `typecheck_record` returns a "WELL-TYPED  <type>" line; keep just the type as the detail.
        Ok(ty) => {
            let t = ty.trim().trim_start_matches("WELL-TYPED").trim().to_string();
            checks.push(CertCheck::new("typecheck", "WELL-TYPED", t, false));
        }
        Err(e) => checks.push(CertCheck::new("typecheck", "ILL-TYPED", format!("{e}"), true)),
    }

    // 2. Effects.
    let inf = infer_effects(body, records);
    let declared_eff: std::collections::BTreeSet<String> = sig
        .and_then(|s| s.get("effects"))
        .and_then(|e| e.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let under: Vec<String> = inf.effects.difference(&declared_eff).cloned().collect();
    let eff_show = inf.effects.iter().cloned().collect::<Vec<_>>().join(", ");
    if !under.is_empty() {
        checks.push(CertCheck::new("effects", "UNDER-DECLARED", format!("body performs [{}] not declared", under.join(", ")), true));
    } else if inf.opaque || inf.unresolved {
        let why = if inf.unresolved { "an unresolved fn_ref callee (pass --records)" } else { "an opaque call may perform more" };
        checks.push(CertCheck::new("effects", "UNVERIFIABLE", format!("inferred [{eff_show}] ⊆ declared, but {why}"), false));
    } else {
        checks.push(CertCheck::new("effects", "SOUND", format!("effects [{eff_show}] ⊆ declared"), false));
    }

    // 3. Refinements (type-implied `nat` + declared pre/post) — over the linked view, with every callee's
    //    CERTIFIED contract installed as an axiom (modular verification through content addresses).
    if let Some(sig_type) = record.pointer("/signature/type") {
        let refinements: Vec<J> =
            record.pointer("/signature/refinements").and_then(|r| r.as_array()).cloned().unwrap_or_default();
        let prev = view.as_ref().map(|v| crate::set_callee_contracts(v.contracts_for(body)));
        let reports = check_refinements(sig_type, &refinements, body, solver);
        if let Some(p) = prev {
            crate::set_callee_contracts(p);
        }
        for r in reports {
            let (v, hard): (&'static str, bool) = match &r.outcome {
                RefinementOutcome::Sound => ("SOUND", false),
                RefinementOutcome::Violated(_) => ("VIOLATED", true),
                RefinementOutcome::Unverifiable(_) => ("UNVERIFIABLE", false),
                RefinementOutcome::NotApplicable => ("N/A", false),
                RefinementOutcome::NoSolver => ("NO-SOLVER", false),
            };
            let detail = match &r.outcome {
                RefinementOutcome::Violated(m) => format!("counterexample: {m}"),
                RefinementOutcome::Unverifiable(w) => w.clone(),
                _ => String::new(),
            };
            checks.push(CertCheck::new(format!("refinement:{}", r.label), v, detail, hard));
        }
    }

    // 4. Termination.
    let declared_term = sig.and_then(|s| s.get("terminates")).and_then(|t| t.as_str()).unwrap_or("unknown");
    let term_cb: Option<crate::terminate::CalleeTerminates<'_>> = view.as_ref().map(|_| &term_lookup as crate::terminate::CalleeTerminates<'_>);
    match crate::terminate::analyze_termination_linked(body, &crate::terminate::nat_param_positions(record), term_cb) {
        TerminationOutcome::Always if declared_term == "always" => {
            checks.push(CertCheck::new("termination", "SOUND", "provably always-terminates", false))
        }
        TerminationOutcome::Always => {
            checks.push(CertCheck::new("termination", "VERIFIED", format!("provably always — declared `{declared_term}` could be strengthened"), false))
        }
        TerminationOutcome::Unknown(why) if declared_term == "always" => {
            checks.push(CertCheck::new("termination", "UNVERIFIABLE", format!("declared `always`, not proven: {why}"), false))
        }
        TerminationOutcome::Unknown(_) => {
            checks.push(CertCheck::new("termination", "N/A", format!("declared `{declared_term}` (no `always` to verify)"), false))
        }
    }

    // 5. Complexity + structured cost.
    let cx_cb: Option<crate::complexity::CalleeComplexity<'_>> = view.as_ref().map(|v| v as crate::complexity::CalleeComplexity<'_>);
    let complexity = crate::complexity::analyze_complexity_linked(body, cx_cb);
    match sig.and_then(|s| s.get("complexity")).and_then(|c| c.as_str()) {
        Some(d) => {
            let (v, detail) = time_verdict_parts(d, &complexity);
            checks.push(CertCheck::new("complexity", v, detail, false));
        }
        None => {
            if let ComplexityOutcome::Bound(b) = &complexity {
                checks.push(CertCheck::new("complexity", "N/A", format!("none declared; inferred {}", b.display()), false));
            }
        }
    }
    if let Some(cost) = record.pointer("/signature/cost") {
        if let Some(t) = cost.get("time").and_then(|t| t.as_str()) {
            let (v, detail) = time_verdict_parts(t, &complexity);
            checks.push(CertCheck::new("cost.time", v, detail, false));
        }
        if let Some(os) = cost.get("output_size").and_then(|o| o.as_str()) {
            let inferred = record
                .pointer("/signature/type")
                .and_then(unwrap_result_type)
                .map(|rt| analyze_output_size(&rt, body))
                .unwrap_or(OutputSize::Unknown);
            let dec = parse_output_size(os);
            let (v, detail): (&'static str, String) = match (inferred.degree(), dec.degree()) {
                (None, _) => ("UNVERIFIABLE", format!("declared `{os}`, result-size growth can't be inferred")),
                (_, None) => ("N/A", format!("declared `{os}` (nothing to verify)")),
                (Some(i), Some(d)) if i == d => ("SOUND", format!("result is {os} (inferred {})", inferred.label())),
                (Some(i), Some(d)) if i < d => ("VERIFIED", format!("provably {}, tighter than `{os}`", inferred.label())),
                (Some(_), Some(_)) => ("UNVERIFIABLE", format!("declared `{os}`, but result grows faster ({})", inferred.label())),
            };
            checks.push(CertCheck::new("cost.output_size", v, detail, false));
        }
    }

    let certified = !checks.iter().any(|c| c.hard_fail);
    let subject = record.get("hash").and_then(|h| h.as_str()).unwrap_or("<unknown>").to_string();
    let body_hash = record.get("body_hash").and_then(|h| h.as_str()).unwrap_or("<unknown>").to_string();
    Certification { subject, body_hash, checks, certified }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A minimal well-formed v0.2 record over `\n -> add(n, n)` with the given primary name.
    fn record_named(name: &str) -> (J, J) {
        let body = json!({ "kind": "lambda", "params": [{ "name": "n" }], "body":
            { "kind": "app", "fn": { "kind": "var", "name": "add" },
              "args": [{ "kind": "var", "name": "n" }, { "kind": "var", "name": "n" }] } });
        let bh = crate::hash_artifact_with_kind(&body, crate::ArtifactKind::BodyExpression).unwrap();
        let mut rec = json!({
            "schema_version": "0.2.0", "hash": "fn_".to_string() + &"0".repeat(64),
            "name_hints": [name],
            "signature": { "type": { "kind": "fn", "params": [{ "kind": "builtin", "name": "int" }],
                                     "result": { "kind": "builtin", "name": "int" } },
                           "refinements": [], "effects": [], "capabilities": [],
                           "terminates": "always" },
            "examples": [{ "args": [{ "kind": "int", "value": 2 }],
                           "result": { "kind": "int", "value": 4 } }],
            "intent_tags": [], "derived_from": J::Null, "supersedes": J::Null, "body_hash": bh });
        let h = crate::hash_artifact_with_kind(&rec, crate::ArtifactKind::FunctionRecord).unwrap();
        rec["hash"] = json!(h);
        (rec, body)
    }

    fn schema_check(cert: &Certification) -> &CertCheck {
        cert.checks.iter().find(|c| c.check == "schema").expect("a schema check row")
    }

    #[test]
    fn certified_implies_the_gate_will_accept_it() {
        // Finding 12 (evolution/gcp-sdk-poc): certify's zeroth check is the record schema, so a
        // record the commons gate would refuse can no longer report itself certified. The IAM
        // case — a deeply-nested API's 100-char name_hint — is VALID under the widened cap.
        let (rec, body) = record_named(&"n".repeat(100));
        let cert = certify_record(&rec, &body, &HashMap::new(), "z3");
        assert_eq!(schema_check(&cert).verdict, "VALID");
        assert!(cert.certified, "{:?}", cert.checks);

        // Past the cap (129) the record is SCHEMA-INVALID and certification is revoked — the
        // producer and the admitting authority agree again.
        let (rec, body) = record_named(&"n".repeat(129));
        let cert = certify_record(&rec, &body, &HashMap::new(), "z3");
        assert_eq!(schema_check(&cert).verdict, "SCHEMA-INVALID");
        assert!(!cert.certified);

        // A non-v0.2 record is noted, never failed — it predates the pinned schema.
        let (mut rec, body) = record_named("plain");
        rec["schema_version"] = json!("0.1.0");
        let cert = certify_record(&rec, &body, &HashMap::new(), "z3");
        assert_eq!(schema_check(&cert).verdict, "N/A");
        assert!(!schema_check(&cert).hard_fail);
    }
}
