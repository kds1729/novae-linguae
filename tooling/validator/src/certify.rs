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
/// The DERIVED-CONTRACT CACHE: where a callee's re-derived contract is kept between validator
/// processes, when the operator opts in with `NL_DERIVED_CACHE` (`1` = the default directory,
/// `$XDG_CACHE_HOME/nl-validator/derived` or `~/.cache/nl-validator/derived`; a path = that
/// directory; unset, `0` or `off` = no cache). What it holds is THIS machine's own earlier
/// derivation — every post and law in it was PROVED here, by this validator version, from the
/// record at that content address (the key: `<fn_…>.v<version>.json`; the address covers the body
/// and, through its `fn_ref`s, the callees' records) — so a hit is the same facts, not a declaration
/// read as one. Measured need (2026-10-30, the integer tier): of the 79 s `prove` spent on `lcm`'s
/// one law, 64 s re-derived `gcd`'s four laws — associativity's whole failing ladder included —
/// and every record that applies `gcd` paid it again in its own process. Off by default so a test
/// run or a fresh audit never reads yesterday's proofs; the module builder turns it on. A contract
/// whose sorts name a per-process datatype (`Sort::Data`) is never cached — its indices are not
/// portable.
pub fn derived_cache_dir() -> Option<std::path::PathBuf> {
    let v = std::env::var("NL_DERIVED_CACHE").ok()?;
    match v.trim() {
        "" | "0" => None,
        s if s.eq_ignore_ascii_case("off") || s.eq_ignore_ascii_case("false") => None,
        "1" => {
            let base = std::env::var_os("XDG_CACHE_HOME")
                .map(std::path::PathBuf::from)
                .filter(|p| !p.as_os_str().is_empty())
                .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".cache")))?;
            Some(base.join("nl-validator").join("derived"))
        }
        path => Some(std::path::PathBuf::from(path)),
    }
}

fn derived_cache_path(dir: &std::path::Path, target: &str) -> std::path::PathBuf {
    dir.join(format!("{target}.v{}.json", env!("CARGO_PKG_VERSION")))
}

/// The cached contract for `target`, if this validator version derived one here before.
pub fn load_derived(dir: &std::path::Path, target: &str) -> Option<crate::CalleeContract> {
    let text = std::fs::read_to_string(derived_cache_path(dir, target)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Keep a derived contract for the next process (best effort: an unwritable cache is no cache).
pub fn store_derived(dir: &std::path::Path, target: &str, c: &crate::CalleeContract) {
    let portable = !c.param_sorts.iter().chain(std::iter::once(&c.ret)).any(|s| matches!(s, crate::Sort::Data(_)));
    if !portable || std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let Ok(text) = serde_json::to_string(c) else { return };
    let path = derived_cache_path(dir, target);
    let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    if std::fs::write(&tmp, text).is_ok() && std::fs::rename(&tmp, &path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

pub struct LinkedView<'a> {
    records: &'a HashMap<String, J>,
    bodies: &'a HashMap<String, J>,
    solver: String,
    term: std::cell::RefCell<HashMap<String, Option<bool>>>,
    cx: std::cell::RefCell<HashMap<String, Option<crate::Class>>>,
    bounds: std::cell::RefCell<HashMap<String, Option<Vec<usize>>>>,
    contracts: std::cell::RefCell<HashMap<String, Option<crate::CalleeContract>>>,
    shrink: std::cell::RefCell<HashMap<String, Option<u32>>>,
    preserve: std::cell::RefCell<HashMap<String, Option<Vec<usize>>>>,
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
            shrink: Default::default(),
            preserve: Default::default(),
            in_progress: Default::default(),
        }
    }

    /// `complexity::component_preserve` of the callee's body (memoized).
    pub fn preserve(&self, target: &str) -> Option<Vec<usize>> {
        if let Some(v) = self.preserve.borrow().get(target) {
            return v.clone();
        }
        let out = self.callee(target).and_then(|(_, body)| crate::complexity::component_preserve(body));
        self.preserve.borrow_mut().insert(target.to_string(), out.clone());
        out
    }

    /// `complexity::component_shrink` of the callee's body (memoized).
    pub fn shrink(&self, target: &str) -> Option<u32> {
        if let Some(v) = self.shrink.borrow().get(target) {
            return *v;
        }
        let out = self.callee(target).and_then(|(_, body)| crate::complexity::component_shrink(body));
        self.shrink.borrow_mut().insert(target.to_string(), out);
        out
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
                    return match crate::complexity::analyze_output_size_linked(&rt, body, Some(self)) {
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

    /// A callee's body and `nat`-typed parameter positions, for the prover's induction THROUGH
    /// a callee's definition (`prove::prove_by_callee_induction`).
    pub fn callee_def(&self, target: &str) -> Option<(J, std::collections::BTreeSet<usize>)> {
        let (rec, body) = self.callee(target)?;
        let nats = rec.pointer("/signature/type").map(crate::terminate::nat_positions_of_type).unwrap_or_default();
        Some((body.clone(), nats))
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
        let out = matches!(
            crate::terminate::analyze_termination_linked(body, &nat, Some(self)),
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
            Some((rec, body)) => {
                let rt = rec.pointer("/signature/type").and_then(unwrap_result_type);
                match crate::complexity::analyze_complexity_typed(body, rt.as_ref(), Some(self)) {
                    crate::complexity::ComplexityOutcome::Bound(c) => Some(c),
                    _ => None,
                }
            }
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
        // This machine's own earlier derivation of the same record (same content address, same
        // validator), when the operator opted in: see `derived_cache_dir`.
        if self.callee(target).is_some() {
            if let Some(c) = derived_cache_dir().and_then(|d| load_derived(&d, target)) {
                self.in_progress.borrow_mut().remove(&key);
                self.contracts.borrow_mut().insert(target.to_string(), Some(c.clone()));
                return Some(c);
            }
        }
        let out = self.callee(target).and_then(|(rec, body)| {
            let params: Vec<String> = body
                .get("params")
                .and_then(|p| p.as_array())
                .map(|ps| ps.iter().filter_map(|p| p.get("name").and_then(|n| n.as_str()).map(String::from)).collect())
                .unwrap_or_default();
            let sig_type = rec.pointer("/signature/type")?;
            let fn_ty = if sig_type.get("kind").and_then(|k| k.as_str()) == Some("forall") { sig_type.get("body")? } else { sig_type };
            let sort_of_type = crate::prove::sort_of_type;
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
            // Prove the callee's contract — posts AND laws — with ITS callees' contracts installed and
            // its own declared type in force (tuple/sum sorts come from the signature), in the
            // phases of `derive_self_facts` (nothing assumes itself).
            let deps = self.contracts_for_record(rec, body);
            let prev_refusals = crate::prove::set_callee_refusals(self.refusals_for(body));
            let prev = crate::set_callee_contracts(deps);
            let prev_ty = crate::prove::set_self_type(Some(sig_type.clone()));
            let all_laws: Vec<J> = law_exprs(rec);
            let (reports, laws) = derive_self_facts(sig_type, &refinements, &all_laws, body, &nat_positions, &self.solver);
            crate::prove::set_self_type(prev_ty);
            crate::set_callee_contracts(prev);
            crate::prove::set_callee_refusals(prev_refusals);
            let posts = sound_posts(&reports, &declared_posts);
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
        if let (Some(c), Some(dir)) = (&out, derived_cache_dir()) {
            store_derived(&dir, target, c);
        }
        self.contracts.borrow_mut().insert(target.to_string(), out.clone());
        out
    }

    /// The contracts of every callee a body applies by `fn_ref` (those that have one).
    pub fn contracts_for(&self, body: &J) -> std::collections::BTreeMap<String, crate::CalleeContract> {
        let mut targets = std::collections::BTreeSet::new();
        fn_ref_targets(body, &mut targets);
        self.contracts_closure(targets)
    }

    /// [`contracts_for`], seeded also with the functions the RECORD'S OWN pres, posts and laws
    /// apply by content address. `gi_norm`'s law `N(conj(z)) = N(z)` applies `gi_conj`, and
    /// `gi_rem`'s post `N(result) < N(w)` applies `gi_norm` — neither of which the body applies, so
    /// both read UNSUPPORTED "its certified contract is not available here" with the records a
    /// directory entry away (the Gaussian tier's round 1, 2026-10-30): the fourth place that had
    /// assumed a contract applies no callee.
    pub fn contracts_for_record(&self, record: &J, body: &J) -> std::collections::BTreeMap<String, crate::CalleeContract> {
        let mut targets = std::collections::BTreeSet::new();
        fn_ref_targets(body, &mut targets);
        let own = record.get("hash").and_then(|h| h.as_str()).unwrap_or_default();
        for r in record.pointer("/signature/refinements").and_then(|r| r.as_array()).into_iter().flatten() {
            if let Some(e) = r.get("expr") {
                predicate_fn_targets(e, &mut targets);
            }
        }
        for p in record.get("properties").and_then(|p| p.as_array()).into_iter().flatten() {
            if let Some(e) = p.get("expr") {
                predicate_fn_targets(e, &mut targets);
            }
        }
        targets.remove(own);
        self.contracts_closure(targets)
    }

    fn contracts_closure(&self, targets: std::collections::BTreeSet<String>) -> std::collections::BTreeMap<String, crate::CalleeContract> {
        // The CLOSURE under the contracts: a callee's post may itself apply a callee (`rat_normalize`'s
        // `gcd(|fst(result)|, snd(result)) = 1` applies `gcd`), and lowering that post needs the
        // second callee's contract installed too — without it the obligation refused with "its
        // certified contract is not available here" although the records directory held it (the
        // rational tier's round 4, 2026-10-30). Transitive, so a contract's callee's own contract
        // applications are followed as well; each record's contract is derived once (memoized).
        let mut out = std::collections::BTreeMap::new();
        let mut queue: Vec<String> = targets.into_iter().collect();
        while let Some(t) = queue.pop() {
            if out.contains_key(&t) {
                continue;
            }
            let Some(c) = self.contract(&t) else { continue };
            for pred in c.pres.iter().chain(c.posts.iter()).chain(c.laws.iter()) {
                let mut more = std::collections::BTreeSet::new();
                predicate_fn_targets(pred, &mut more);
                queue.extend(more.into_iter().filter(|m| !out.contains_key(m)));
            }
            out.insert(t, c);
        }
        out
    }

    /// For every `fn_ref` callee of `body` that yields NO contract, the stated reason — installed
    /// beside the contracts (`prove::set_callee_refusals`) so a proof that applies such a callee
    /// refuses with it rather than with "pass --records".
    pub fn refusals_for(&self, body: &J) -> std::collections::BTreeMap<String, String> {
        let mut targets = std::collections::BTreeSet::new();
        fn_ref_targets(body, &mut targets);
        targets
            .into_iter()
            .filter(|t| self.contract(t).is_none())
            .map(|t| {
                let why = match self.callee(&t) {
                    None => "it is not among the supplied records (or has no body there)".to_string(),
                    Some((rec, _)) => {
                        let sig = rec.pointer("/signature/type").cloned().unwrap_or(J::Null);
                        let fn_ty = if sig.get("kind").and_then(|k| k.as_str()) == Some("forall") { sig.get("body").cloned().unwrap_or(J::Null) } else { sig.clone() };
                        let param_ok = fn_ty.get("params").and_then(|p| p.as_array()).is_some_and(|ps| ps.iter().all(|p| crate::prove::sort_of_type(p).is_some()));
                        let ret_ok = fn_ty.get("result").is_some_and(|r| crate::prove::sort_of_type(r).is_some());
                        if !param_ok || !ret_ok {
                            "its signature leaves the prover's fragment (a list-, map-, function- or type-variable-typed parameter or result)".to_string()
                        } else {
                            "its contract is being computed (a dependency cycle)".to_string()
                        }
                    }
                };
                (t, why)
            })
            .collect()
    }
}

/// The `expr` of every law in a record's `properties[]`.
pub fn law_exprs(rec: &J) -> Vec<J> {
    rec.get("properties")
        .and_then(|p| p.as_array())
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|p| p.get("expr").cloned())
        .collect()
}

/// The declared postconditions `check-refinement` found SOUND, plus the type-implied `result ≥ 0`
/// of a `nat` result when that is — the facts a contract may state.
pub fn sound_posts(reports: &[crate::RefinementReport], declared_posts: &[J]) -> Vec<J> {
    let sound = |label: &str| reports.iter().any(|r| r.label == label && matches!(r.outcome, RefinementOutcome::Sound));
    let mut posts: Vec<J> = declared_posts.iter().enumerate().filter(|(i, _)| sound(&format!("post[{i}]"))).map(|(_, p)| p.clone()).collect();
    if sound("nat-result (≥ 0)") {
        posts.push(serde_json::json!({ "kind": "app", "op": "ge", "args": [
            { "kind": "var", "name": "result" }, { "kind": "lit", "value": { "kind": "int", "value": 0 } }] }));
    }
    posts
}

/// The shape of a function's OWN facts for the measure-induction rung (`prove::set_self_facts`):
/// `self` with its declared sorts, preconditions and `nat` positions, no posts or laws yet.
/// `None` when the signature leaves the prover's fragment.
pub fn self_shape(sig_type: &J, refinements: &[J], body: &J) -> Option<crate::CalleeContract> {
    let params: Vec<String> = body
        .get("params")
        .and_then(|p| p.as_array())
        .map(|ps| ps.iter().filter_map(|p| p.get("name").and_then(|n| n.as_str()).map(String::from)).collect())
        .unwrap_or_default();
    let fn_ty = if sig_type.get("kind").and_then(|k| k.as_str()) == Some("forall") { sig_type.get("body")? } else { sig_type };
    let param_tys = fn_ty.get("params").and_then(|p| p.as_array())?;
    if param_tys.len() != params.len() {
        return None;
    }
    let param_sorts: Vec<crate::Sort> = param_tys.iter().map(crate::prove::sort_of_type).collect::<Option<_>>()?;
    let ret = crate::prove::sort_of_type(fn_ty.get("result")?)?;
    let pres: Vec<J> = refinements
        .iter()
        .filter(|r| r.get("kind").and_then(|k| k.as_str()) == Some("pre"))
        .filter_map(|r| r.get("expr").cloned())
        .collect();
    Some(crate::CalleeContract {
        symbol: "self".to_string(),
        params,
        param_sorts,
        ret,
        pres,
        posts: vec![],
        laws: vec![],
        nat_positions: crate::terminate::nat_positions_of_type(sig_type),
    })
}

/// A function's own certified facts, derived in PHASES so that nothing assumes itself:
///
/// 1. its laws by the plain rungs — no fact about `self` assumed;
/// 2. its postconditions (what `check-refinement` reports), with the phase-1 laws stated at the
///    instantiated points of each induction — a post whose step needs a law (`binomial ≥ 0` needs
///    `k > n ⇒ C(n, k) = 0`, else the product's second factor is unsigned);
/// 3. the laws phase 1 left open, with the proved posts and the phase-1 laws — a step that needs
///    a proved fact about `self` (Euclid's divisibility needs `gcd ≥ 1`).
///
/// Each phase assumes only what an earlier one proved, with the callee contracts the caller
/// installed. Returns the refinement reports and the proved laws; `check-refinement`, `certify`
/// and the linked view all read the same derivation, so a dependent never sees a fact its
/// callee's own report does not show.
pub fn derive_self_facts(
    sig_type: &J,
    refinements: &[J],
    laws: &[J],
    body: &J,
    nat_positions: &std::collections::BTreeSet<usize>,
    solver: &str,
) -> (Vec<crate::RefinementReport>, Vec<J>) {
    let shape = self_shape(sig_type, refinements, body);
    let facts = |posts: Vec<J>, laws: Vec<J>| {
        shape.clone().map(|mut s| {
            s.posts = posts;
            s.laws = laws;
            s
        })
    };
    // Phase 1.
    let prev = crate::prove::set_self_facts(None);
    let plain: Vec<J> = laws
        .iter()
        .filter(|law| {
            matches!(crate::prove_property(law, Some(body), solver).0, crate::ProofOutcome::Proved)
                || matches!(crate::prove_by_numeric_induction(law, Some(body), nat_positions, solver).0, crate::ProofOutcome::Proved)
                // A list law a bare structural induction closes (`scale_mod` preserves length) is
                // a contract too — the lemma search is skipped here (cost).
                || matches!(crate::prove_by_induction(law, Some(body), solver).0, crate::InductionOutcome::Proved)
        })
        .cloned()
        .collect();
    // Phase 2.
    crate::prove::set_self_facts(if plain.is_empty() { None } else { facts(vec![], plain.clone()) });
    let reports = check_refinements(sig_type, refinements, body, solver);
    let declared_posts: Vec<J> = refinements
        .iter()
        .filter(|r| r.get("kind").and_then(|k| k.as_str()) == Some("post"))
        .filter_map(|r| r.get("expr").cloned())
        .collect();
    let posts = sound_posts(&reports, &declared_posts);
    // Phase 3 (recursive bodies only — the rung that reads the facts is the induction one).
    let mut proved = plain.clone();
    if shape.is_some() && (!posts.is_empty() || !plain.is_empty()) && crate::prove::references_self(body) {
        crate::prove::set_self_facts(facts(posts.clone(), plain.clone()));
        for law in laws {
            if proved.contains(law) {
                continue;
            }
            if matches!(crate::prove_by_numeric_induction(law, Some(body), nat_positions, solver).0, crate::ProofOutcome::Proved) {
                proved.push(law.clone());
                // A law proved here is a fact for the laws after it (in the record's order): gcd
                // associativity rests on the greatest-divisor law, which may itself have needed
                // the posts (2026-10-30, the divisibility rung).
                crate::prove::set_self_facts(facts(posts.clone(), proved.clone()));
            }
        }
    }
    crate::prove::set_self_facts(prev);
    (reports, proved)
}

/// Every `fn_ref` literal target in an AST.
/// Every function a PREDICATE applies by content address (`{"kind":"app","op":"fn_…"}` — the
/// predicate dialect's spelling of a callee application).
fn predicate_fn_targets(node: &J, out: &mut std::collections::BTreeSet<String>) {
    match node {
        J::Object(m) => {
            if m.get("kind").and_then(|k| k.as_str()) == Some("app") {
                if let Some(op) = m.get("op").and_then(|o| o.as_str()) {
                    if op.starts_with("fn_") {
                        out.insert(op.to_string());
                    }
                }
            }
            m.values().for_each(|v| predicate_fn_targets(v, out));
        }
        J::Array(xs) => xs.iter().for_each(|v| predicate_fn_targets(v, out)),
        _ => {}
    }
}

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
    fn components_shrink_by(&self, target: &str) -> Option<u32> {
        self.shrink(target)
    }
    fn components_preserved_by(&self, target: &str) -> Option<Vec<usize>> {
        self.preserve(target)
    }
}

impl crate::terminate::LinkedTermination for LinkedView<'_> {
    fn terminates(&self, target: &str) -> Option<bool> {
        LinkedView::terminates(self, target)
    }
    fn components_shrink_by(&self, target: &str) -> Option<u32> {
        self.shrink(target)
    }
}

/// The variables of a predicate term whose VALUE is linear in them: variables, literals, `add`/`sub`/
/// `min`/`max`/`div`/`mod` over such, `mul` with a literal operand. `None` for anything else (a product
/// of two variables, an application).
fn linear_vars(node: &J) -> Option<Vec<String>> {
    match node.get("kind").and_then(|k| k.as_str()) {
        Some("var") => node.get("name").and_then(|n| n.as_str()).map(|n| vec![n.to_string()]),
        Some("lit") => Some(vec![]),
        Some("app") => {
            let op = node.get("op").and_then(|o| o.as_str())?;
            let args = node.get("args").and_then(|a| a.as_array())?;
            let lit = |n: &J| n.get("kind").and_then(|k| k.as_str()) == Some("lit");
            let ok = match op {
                "add" | "sub" | "min" | "max" | "div" | "mod" | "neg" | "abs" => true,
                "mul" => args.len() == 2 && (lit(&args[0]) || lit(&args[1])),
                _ => false,
            };
            if !ok {
                return None;
            }
            let mut out = Vec::new();
            for a in args {
                out.extend(linear_vars(a)?);
            }
            Some(out)
        }
        _ => None,
    }
}

/// The parameters an upper-bound postcondition names: `result < p`, `result <= p` (or a size-linear
/// expression over parameters), or a conjunction containing such (the reserved `result` on the left).
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
                    // `result <= p`, or `result <= 2 * n + 1`: any size-linear expression over variables.
                    if let Some(vs) = linear_vars(&args[1]) {
                        out.extend(vs);
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
        let prev = view.as_ref().map(|v| crate::set_callee_contracts(v.contracts_for_record(record, body)));
        let prev_refusals = view.as_ref().map(|v| crate::prove::set_callee_refusals(v.refusals_for(body)));
        // The same phased derivation the linked view uses for a callee (its own plain-proved laws
        // may enter a post's induction), so this report and a dependent's view of it agree.
        let nat_positions = crate::terminate::nat_param_positions(record);
        let (reports, _) = derive_self_facts(sig_type, &refinements, &law_exprs(record), body, &nat_positions, solver);
        if let Some(p) = prev {
            crate::set_callee_contracts(p);
        }
        if let Some(p) = prev_refusals {
            crate::prove::set_callee_refusals(p);
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
    let term_cb: Option<crate::terminate::CalleeTerminates<'_>> = view.as_ref().map(|v| v as crate::terminate::CalleeTerminates<'_>);
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
    let result_ty = record.pointer("/signature/type").and_then(unwrap_result_type);
    let complexity = crate::complexity::analyze_complexity_typed(body, result_ty.as_ref(), cx_cb);
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

    fn a_contract(ret: crate::Sort) -> crate::CalleeContract {
        crate::CalleeContract {
            symbol: "callee_test".to_string(),
            params: vec!["a".to_string(), "b".to_string()],
            param_sorts: vec![crate::Sort::Int, crate::Sort::Int],
            ret,
            pres: vec![],
            posts: vec![json!({ "kind": "app", "op": "gte", "args": [ { "kind": "var", "name": "result" }, { "kind": "lit", "value": 1 } ] })],
            laws: vec![],
            nat_positions: [0usize, 1].into_iter().collect(),
        }
    }

    fn fresh_cache_dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("nl-derived-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn a_derived_contract_round_trips_through_the_cache() {
        let dir = fresh_cache_dir("roundtrip");
        let c = a_contract(crate::Sort::Int);
        store_derived(&dir, "fn_test_roundtrip", &c);
        assert_eq!(load_derived(&dir, "fn_test_roundtrip"), Some(c));
        assert_eq!(load_derived(&dir, "fn_test_absent"), None);
        // Keyed by the validator version: another version's derivation is never read.
        let other = dir.join(format!("fn_test_roundtrip.v{}.json", "0.0.0"));
        assert!(!other.exists());
    }

    #[test]
    fn a_contract_over_a_per_process_datatype_is_not_cached() {
        let dir = fresh_cache_dir("data");
        store_derived(&dir, "fn_test_data", &a_contract(crate::Sort::Data(0)));
        assert_eq!(load_derived(&dir, "fn_test_data"), None);
    }

    #[test]
    fn the_cache_is_off_unless_opted_in() {
        // The variable's spellings: unset/0/off = no cache, 1 = the default directory, a path = it.
        // (Read through the resolver only — the test does not set the variable, so a parallel
        // test's view of it is untouched.)
        let resolve = |v: Option<&str>| -> Option<std::path::PathBuf> {
            match v {
                None => None,
                Some(s) => match s.trim() {
                    "" | "0" => None,
                    s if s.eq_ignore_ascii_case("off") => None,
                    "1" => Some(std::path::PathBuf::from("<default>")),
                    p => Some(std::path::PathBuf::from(p)),
                },
            }
        };
        assert_eq!(resolve(None), None);
        assert_eq!(resolve(Some("0")), None);
        assert_eq!(resolve(Some("off")), None);
        assert_eq!(resolve(Some("/tmp/x")), Some(std::path::PathBuf::from("/tmp/x")));
        assert!(resolve(Some("1")).is_some());
    }

    /// The linked view takes a cached contract for a record it can resolve, without deriving: the
    /// stored contract carries a post the record itself never declared, and that is what comes back.
    #[test]
    fn the_linked_view_reads_a_cached_contract_before_deriving() {
        let dir = fresh_cache_dir("view");
        let (rec, body) = record_named("cached_fn");
        let hash = rec["hash"].as_str().unwrap().to_string();
        let mut records = HashMap::new();
        records.insert(hash.clone(), rec.clone());
        let mut bodies = HashMap::new();
        bodies.insert(hash.clone(), body);
        let want = a_contract(crate::Sort::Int);
        store_derived(&dir, &hash, &want);
        std::env::set_var("NL_DERIVED_CACHE", dir.to_str().unwrap());
        let got = LinkedView::new(&records, &bodies, "z3").contract(&hash);
        std::env::remove_var("NL_DERIVED_CACHE");
        assert_eq!(got, Some(want));
        // An unresolvable target is never served from the cache.
        store_derived(&dir, "fn_not_in_view", &a_contract(crate::Sort::Int));
        std::env::set_var("NL_DERIVED_CACHE", dir.to_str().unwrap());
        let none = LinkedView::new(&records, &bodies, "z3").contract("fn_not_in_view");
        std::env::remove_var("NL_DERIVED_CACHE");
        assert_eq!(none, None);
    }

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

    /// A caller's contract set is the CLOSURE under the contracts: `C` applies `A`, whose post
    /// applies `B` — `B`'s contract must be installed for `A`'s post to lower (the rational tier's
    /// `rat_inv`, whose callee `rat_normalize` has a post applying `gcd`, read UNSUPPORTED: "its
    /// certified contract is not available here").
    #[test]
    fn the_contract_set_closes_over_the_callees_a_contract_applies() {
        let Some(solver) = (["z3", "cvc5"].into_iter().find(|s| std::process::Command::new(s).arg("--version").output().map(|o| o.status.success()).unwrap_or(false))) else { return };
        let int = json!({ "kind": "builtin", "name": "int" });
        let mk = |name: &str, body: J, posts: Vec<J>| -> (String, J, J) {
            let bh = crate::hash_artifact_with_kind(&body, crate::ArtifactKind::BodyExpression).unwrap();
            let mut rec = json!({
                "schema_version": "0.2.0", "hash": "fn_".to_string() + &"0".repeat(64), "name_hints": [name],
                "signature": { "type": { "kind": "fn", "params": [int.clone()], "result": int.clone() },
                               "refinements": posts.into_iter().map(|p| json!({ "kind": "post", "expr": p })).collect::<Vec<_>>(),
                               "effects": [], "capabilities": [], "terminates": "always" },
                "examples": [], "intent_tags": [], "derived_from": J::Null, "supersedes": J::Null, "body_hash": bh });
            let h = crate::hash_artifact_with_kind(&rec, crate::ArtifactKind::FunctionRecord).unwrap();
            rec["hash"] = json!(h.clone());
            (h, rec, body)
        };
        let var = |n: &str| json!({ "kind": "var", "name": n });
        let call = |t: &str, a: J| json!({ "kind": "app", "fn": { "kind": "lit", "value": { "kind": "fn_ref", "target": t } }, "args": [a] });
        let lam = |b: J| json!({ "kind": "lambda", "params": [{ "name": "x" }], "body": b });
        // B = \x -> abs(x), post result >= 0.
        let (hb, rb, bb) = mk("b", lam(json!({ "kind": "app", "fn": var("abs"), "args": [var("x")] })),
            vec![json!({ "kind": "app", "op": "ge", "args": [var("result"), { "kind": "lit", "value": 0 }] })]);
        // A = \x -> B(x), post B(result) >= 0 — a post that APPLIES a callee.
        let (ha, ra, ba) = mk("a", lam(call(&hb, var("x"))),
            vec![json!({ "kind": "app", "op": "ge", "args": [{ "kind": "app", "op": hb, "args": [var("result")] }, { "kind": "lit", "value": 0 }] })]);
        // C = \x -> A(x): its contract set must hold A AND B.
        let (_, _, bc) = mk("c", lam(call(&ha, var("x"))), vec![]);
        let mut records = HashMap::new();
        let mut bodies = HashMap::new();
        for (h, r, b) in [(hb.clone(), rb, bb), (ha.clone(), ra, ba)] {
            records.insert(h.clone(), r);
            bodies.insert(h, b);
        }
        let view = LinkedView::new(&records, &bodies, solver);
        let contracts = view.contracts_for(&bc);
        assert!(contracts.contains_key(&ha), "A, applied by the body");
        assert!(contracts.contains_key(&hb), "B, applied only by A's post: {:?}", contracts.keys().collect::<Vec<_>>());
        assert!(!contracts[&ha].posts.is_empty(), "A's post proved (needs B's contract itself)");
    }

    /// The record's OWN pres, posts and laws may apply a callee the body does not (`gi_norm`'s law
    /// `N(conj(z)) = N(z)` applies `gi_conj`; `gi_rem`'s post `N(result) < N(w)` applies `gi_norm`):
    /// `contracts_for_record` seeds the closure with those too (the Gaussian tier's round 1).
    #[test]
    fn the_contract_set_includes_the_callees_the_records_own_predicates_apply() {
        let Some(solver) = (["z3", "cvc5"].into_iter().find(|s| std::process::Command::new(s).arg("--version").output().map(|o| o.status.success()).unwrap_or(false))) else { return };
        let int = json!({ "kind": "builtin", "name": "int" });
        let mk = |name: &str, body: J, posts: Vec<J>| -> (String, J, J) {
            let bh = crate::hash_artifact_with_kind(&body, crate::ArtifactKind::BodyExpression).unwrap();
            let mut rec = json!({
                "schema_version": "0.2.0", "hash": "fn_".to_string() + &"0".repeat(64), "name_hints": [name],
                "signature": { "type": { "kind": "fn", "params": [int.clone()], "result": int.clone() },
                               "refinements": posts.into_iter().map(|p| json!({ "kind": "post", "expr": p })).collect::<Vec<_>>(),
                               "effects": [], "capabilities": [], "terminates": "always" },
                "examples": [], "intent_tags": [], "derived_from": J::Null, "supersedes": J::Null, "body_hash": bh });
            let h = crate::hash_artifact_with_kind(&rec, crate::ArtifactKind::FunctionRecord).unwrap();
            rec["hash"] = json!(h.clone());
            (h, rec, body)
        };
        let var = |n: &str| json!({ "kind": "var", "name": n });
        let lam = |b: J| json!({ "kind": "lambda", "params": [{ "name": "x" }], "body": b });
        // B = \x -> abs(x), post result >= 0.
        let (hb, rb, bb) = mk("b", lam(json!({ "kind": "app", "fn": var("abs"), "args": [var("x")] })),
            vec![json!({ "kind": "app", "op": "ge", "args": [var("result"), { "kind": "lit", "value": 0 }] })]);
        // C = \x -> x applies NOTHING; its post `B(result) >= 0` applies B.
        let (_, rc, bc) = mk("c", lam(var("x")),
            vec![json!({ "kind": "app", "op": "ge", "args": [{ "kind": "app", "op": hb, "args": [var("result")] }, { "kind": "lit", "value": 0 }] })]);
        let records: HashMap<String, J> = [(hb.clone(), rb)].into_iter().collect();
        let bodies: HashMap<String, J> = [(hb.clone(), bb)].into_iter().collect();
        let view = LinkedView::new(&records, &bodies, solver);
        assert!(view.contracts_for(&bc).is_empty(), "the body applies no callee");
        let contracts = view.contracts_for_record(&rc, &bc);
        assert!(contracts.contains_key(&hb), "B, applied only by C's own post: {:?}", contracts.keys().collect::<Vec<_>>());
        assert!(!contracts[&hb].posts.is_empty(), "B's post proved");
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
