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

/// An `eprintln!` for a progress line that also resets the heartbeat's silence clock ([`progress_on`]).
macro_rules! progress_line {
    ($($t:tt)*) => {{
        eprintln!($($t)*);
        $crate::certify::mark_progress();
    }};
}
pub(crate) use progress_line;

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
    /// The record's own laws its refinement derivation PROVED (`derive_self_facts`) — and whether
    /// that derivation saw every callee law it could use (`laws_complete`: a full linked view, or no
    /// callee at all). The agent loop reports these instead of deriving them again: certify had
    /// already spent `binomial`'s 270 s on them (2026-11-03).
    pub laws: Vec<J>,
    pub laws_complete: bool,
    /// Every declared law by name with the rung that proved it (`None` = not proved), in the
    /// record's order — what a signed certification records (`laws`, when `laws_complete`).
    pub law_results: Vec<(String, Option<Rung>)>,
    /// That derivation's facts as a contract (the sound posts and the proved laws), for the loop's
    /// law step to read rather than derive the record's own contract again (`binomial`: 40 s, 2026-11-05).
    pub self_contract: Option<crate::CalleeContract>,
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
/// whose sorts name a datatype is stored with the datatypes written out STRUCTURALLY
/// (`PortableContract`, `prove::PortableSort`) and re-registered on load — until 2026-10-31 such a
/// contract was never cached (its registry indices are per process), and every validator process
/// of a module round re-derived `rat_normalize`'s and `ext_gcd`'s, minutes each.
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

/// The derivation's own version: bumped when the RULES that derive a contract change within one
/// validator version (a cached contract is this machine's earlier derivation under those rules —
/// a new rule that proves more must not be served the old result). 2 = the self-contract rung as
/// the derivation's last phase (2026-10-31); 3 = the portable format with structural sorts
/// (`PortableContract`, same day).
// 5 (2026-11-01): the staged rung's quotient abstraction changes what a derivation proves — a
// version-4 entry for the same content address held `unique` open (measured: rational round 8's
// first start read it, so rat_add's contract lacked the law).
// 6 (2026-11-01): contracts derived under a cycle refusal were cached partial (`gi_rem`).
// 7 (2026-11-01): the staged rung is rlimit-bounded and charge-budgeted — deterministic, so a cached
// derivation no longer depends on the load it ran under.
// 8 (2026-11-01): the staged rung at 3 M units/s under the plain `smt` tactic.
// 9 (2026-11-02): a callee contract of laws alone reaches its callers (a derivation that applies
// `list_sum` now proves more), and the refinement checker's first-order rung with lists admitted.
// 10 (2026-11-02): an ambiguous bare variant takes the declared result sort (a body over a callee of
// another `Maybe` lowers where it refused), and a `replace_token` panic past a term's end is fixed.
// 11 (2026-11-03): structural induction over a body that descends on a later parameter
// (`induct::trailing_list_descent` — `alloc_from t ws c w`): its laws prove where they were refused.
// 12 (2026-11-05): phase 5 (a recursive body's law through its own and its callees' definitions),
// the numeric rung's two-level unfolding retry, and the stepwise premises checked in parallel.
// 13 (2026-11-05): phase 5 tries every open law (not only those applying a callee), and phase 6
// inducts on a nat variable over the self-contract (`prove_from_self_contract_by_induction`).
const DERIVATION_VERSION: u32 = 13;

/// The cache file for one VIEW KIND: `""` = the full derivation, `b<secs>` = a budgeted one (each
/// law attempt capped), `p` = posts only. A partial kind holds only facts proved here (fewer axioms
/// can only lose proofs), so it is as sound as the full one — but weaker, and for the budgeted kind
/// load-dependent, so it is read only by a view of the same kind, and a full entry is preferred.
fn derived_cache_path_kind(dir: &std::path::Path, target: &str, kind: &str) -> std::path::PathBuf {
    let kind = if kind.is_empty() { String::new() } else { format!(".{kind}") };
    dir.join(format!("{target}.v{}.d{DERIVATION_VERSION}{kind}.json", env!("CARGO_PKG_VERSION")))
}

/// A derived contract as the cache keeps it: the sorts STRUCTURAL (`prove::PortableSort`), so a
/// tuple- or sum-sorted contract is portable between processes (its datatype registry indices are
/// not); everything else as in `CalleeContract`.
#[derive(serde::Serialize, serde::Deserialize)]
struct PortableContract {
    symbol: String,
    params: Vec<String>,
    param_sorts: Vec<crate::prove::PortableSort>,
    ret: crate::prove::PortableSort,
    pres: Vec<J>,
    posts: Vec<J>,
    laws: Vec<J>,
    nat_positions: std::collections::BTreeSet<usize>,
}

/// The cached contract for `target`, if this validator version derived one here before — its
/// datatypes re-registered in this process.
pub fn load_derived(dir: &std::path::Path, target: &str) -> Option<crate::CalleeContract> {
    load_derived_kind(dir, target, "")
}

fn load_derived_kind(dir: &std::path::Path, target: &str, kind: &str) -> Option<crate::CalleeContract> {
    let text = std::fs::read_to_string(derived_cache_path_kind(dir, target, kind)).ok()?;
    let p: PortableContract = serde_json::from_str(&text).ok()?;
    Some(crate::CalleeContract {
        symbol: p.symbol,
        params: p.params,
        param_sorts: p.param_sorts.iter().map(crate::Sort::from_portable).collect(),
        ret: crate::Sort::from_portable(&p.ret),
        pres: p.pres,
        posts: p.posts,
        laws: p.laws,
        nat_positions: p.nat_positions,
    })
}

/// Keep a derived contract for the next process (best effort: an unwritable cache is no cache).
pub fn store_derived(dir: &std::path::Path, target: &str, c: &crate::CalleeContract) {
    store_derived_kind(dir, target, c, "")
}

fn store_derived_kind(dir: &std::path::Path, target: &str, c: &crate::CalleeContract, kind: &str) {
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let Some(param_sorts) = c.param_sorts.iter().map(|s| s.to_portable()).collect::<Option<Vec<_>>>() else { return };
    let Some(ret) = c.ret.to_portable() else { return };
    let p = PortableContract {
        symbol: c.symbol.clone(),
        params: c.params.clone(),
        param_sorts,
        ret,
        pres: c.pres.clone(),
        posts: c.posts.clone(),
        laws: c.laws.clone(),
        nat_positions: c.nat_positions.clone(),
    };
    let Ok(text) = serde_json::to_string(&p) else { return };
    let path = derived_cache_path_kind(dir, target, kind);
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
    /// The record whose contract set is being assembled at the OUTERMOST [`contracts_for_record`]
    /// (`(primary name, hash)`): every witness scan under it, nested derivations included, skips
    /// that record's OTHER versions. Stranger 22 (2026-11-04): certifying the new `gi_rem`, the
    /// scan inside `gi_div`'s derivation took the OLD `gi_rem` as a witness of `gi_norm` and derived
    /// it in full, whose own scan then took the new one — 2 × 24 s of an 82 s verified run. The
    /// record itself is not excluded, so a module build (one version per name) is unchanged.
    certifying: std::cell::RefCell<Option<(String, String)>>,
    /// How many contract lookups were refused because the target was mid-derivation (a cycle).
    /// A contract computed while this grew may lack facts and is never memoized or cached.
    cycle_refusals: std::cell::Cell<usize>,
    /// Whether a callee's contract carries its LAWS (`true`, the full derivation) or its posts
    /// alone ([`LinkedView::posts_only`] — the cheap first pass of [`certify_record_linked`]).
    callee_laws: bool,
    /// A wall-clock cap on each law attempt in this view's derivations ([`LinkedView::budgeted`]).
    law_budget: Option<std::time::Duration>,
    /// Per-law hints from certifications, by function address ([`LinkedView::with_hints`]); used
    /// only under a law budget (the verified run), so a build never reads one.
    hints: HashMap<String, LawHints>,
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
            cycle_refusals: Default::default(),
            shrink: Default::default(),
            preserve: Default::default(),
            in_progress: Default::default(),
            certifying: Default::default(),
            callee_laws: true,
            law_budget: None,
            hints: HashMap::new(),
        }
    }

    /// This view with per-law hints from certifications (function address → hints). They decide
    /// which law attempts a BUDGETED derivation makes, never what counts as proved.
    pub fn with_hints(self, hints: HashMap<String, LawHints>) -> Self {
        LinkedView { hints, ..self }
    }

    /// The hints for `target`'s laws — only under a law budget.
    fn hints_for(&self, target: &str) -> Option<&LawHints> {
        self.law_budget.and(self.hints.get(target))
    }

    /// A view whose callee contracts carry their pres and posts but NOT their laws. Proving every
    /// law of every callee is most of a linked certify's cost (`mod_inverse`: `gcd`'s six laws and
    /// `ext_gcd`'s one took 159 of 175 s) and most targets never use them; fewer axioms can only
    /// lose proofs, never make a false one, so a SOUND verdict over this view is a proof. Never
    /// written to the derived cache (it is not the full contract); a cached full one is used as is.
    pub fn posts_only(records: &'a HashMap<String, J>, bodies: &'a HashMap<String, J>, solver: &str) -> Self {
        LinkedView { callee_laws: false, ..LinkedView::new(records, bodies, solver) }
    }

    /// A view whose callee contracts carry the laws each provable within `budget` per attempt
    /// (a law attempt's solver queries are cut off at the budget and read `unknown`). The middle
    /// pass of [`certify_record_in_view`]: `rat_add`'s posts need `rat_normalize`'s first law
    /// (10 s), not its other four (100–270 s each, then the staged rung) — the full view took
    /// 21.5 min for one verified run (stranger 19, 2026-11-04). Fewer axioms can only lose proofs,
    /// so a SOUND verdict here is a proof; never cached (its contracts may be partial).
    pub fn budgeted(records: &'a HashMap<String, J>, bodies: &'a HashMap<String, J>, solver: &str, budget: std::time::Duration) -> Self {
        LinkedView { law_budget: Some(budget), ..LinkedView::new(records, bodies, solver) }
    }

    /// Whether this view's derivations try every callee law without a time cap.
    pub fn is_full(&self) -> bool {
        self.callee_laws && self.law_budget.is_none()
    }

    /// This view's derived-cache kind (see `derived_cache_path_kind`).
    fn cache_kind(&self) -> String {
        match (self.callee_laws, self.law_budget) {
            (true, None) => String::new(),
            (true, Some(b)) => format!("b{}", b.as_secs()),
            (false, _) => "p".to_string(),
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
            self.cycle_refusals.set(self.cycle_refusals.get() + 1);
            return None;
        }
        let refusals_before = self.cycle_refusals.get();
        // This machine's own earlier derivation of the same record (same content address, same
        // validator), when the operator opted in: see `derived_cache_dir`.
        if self.callee(target).is_some() {
            // A full derivation serves every view; a partial one (budgeted, posts-only) serves only
            // a view of its own kind — the agent loop's middle pass on its next run (2026-11-04: a
            // second `rat_sub` re-derived `rat_normalize` and `gcd` from scratch, ~4 min each time).
            let kind = self.cache_kind();
            let cached = derived_cache_dir().and_then(|d| {
                load_derived(&d, target).or_else(|| if kind.is_empty() { None } else { load_derived_kind(&d, target, &kind) })
            });
            if let Some(c) = cached {
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
            let prev_measure = crate::prove::set_self_measure(self.self_measure(body, &nat_positions));
            let all_laws: Vec<J> = if self.callee_laws { law_exprs(rec) } else { Vec::new() };
            let callee_name = rec.pointer("/name_hints/0").and_then(|n| n.as_str()).unwrap_or("?");
            let started = std::time::Instant::now();
            if progress() {
                progress_line!("   callee   {callee_name} ({}…): deriving its contract", &target[..target.len().min(11)]);
            }
            let (reports, laws) = with_law_budget(self.law_budget, || {
                let d = derive_self_facts_hinted(sig_type, &refinements, &all_laws, body, &nat_positions, Some(&|t: &str| self.callee_def(t)), self.hints_for(target), &self.solver);
                (d.reports, d.proved)
            });
            crate::prove::set_self_measure(prev_measure);
            crate::prove::set_self_type(prev_ty);
            crate::set_callee_contracts(prev);
            crate::prove::set_callee_refusals(prev_refusals);
            let posts = sound_posts(&reports, &declared_posts);
            if progress() {
                // `posts` counts the type-implied ones (a `nat` result) beside the declared.
                let laws_part = if self.callee_laws {
                    let within = self.law_budget.map(|b| format!(" within {} s each", b.as_secs())).unwrap_or_default();
                    format!("laws {} of {} proved{within}", laws.len(), all_laws.len())
                } else {
                    "laws not tried (posts-only pass)".to_string()
                };
                progress_line!(
                    "   callee   {callee_name}: {} post(s) sound, {laws_part} ({:.1} s)",
                    posts.len(), started.elapsed().as_secs_f64()
                );
            }
            // `NL_DERIVED_DEBUG=1`: what each derivation yielded, on stderr.
            if std::env::var("NL_DERIVED_DEBUG").is_ok_and(|v| v == "1") {
                let names: Vec<String> = rec.get("properties").and_then(|p| p.as_array()).into_iter().flatten()
                    .filter_map(|p| Some((p.get("name")?.as_str()?.to_string(), p.get("expr")?)))
                    .map(|(n, e)| format!("{n}={}", if laws.contains(e) { "PROVED" } else { "open" }))
                    .collect();
                eprintln!("[derived] {} {}: posts {} of {} sound; laws [{}]", rec.get("name_hints").and_then(|h| h.get(0)).and_then(|n| n.as_str()).unwrap_or("?"), &target[..14], posts.len(), declared_posts.len(), names.join(", "));
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
        // TAINTED (2026-11-01): a dependency was refused as mid-derivation while this one ran, so
        // the facts that rest on it may be missing — return it for this use, never memoize or cache
        // it (measured: `gi_rem`, derived inside `gi_div`'s witness scan, lost its norm post and
        // that partial contract was kept, so `gi_gcd`'s measured descent read UNVERIFIABLE).
        if self.cycle_refusals.get() > refusals_before {
            return out;
        }
        if let (Some(c), Some(dir)) = (&out, derived_cache_dir()) {
            store_derived_kind(&dir, target, c, &self.cache_kind());
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
        // The outermost call names the record being certified for every nested witness scan.
        let outermost = self.certifying.borrow().is_none();
        if outermost {
            let name = record.pointer("/name_hints/0").and_then(|n| n.as_str());
            *self.certifying.borrow_mut() = name.map(|n| (n.to_string(), own.to_string()));
        }
        let out = self.contracts_closure(targets);
        if outermost {
            *self.certifying.borrow_mut() = None;
        }
        out
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
        let mut witnesses_scanned = false;
        loop {
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
            // WITNESSES (2026-10-31): a record in the view whose POST applies, by address, a callee
            // already in the set COMPUTES that callee with more — `ext_gcd`'s `fst(result) = gcd(a, b)`
            // beside its Bézout identity — and a proof about `gcd` terms may need those facts although
            // nothing applies `ext_gcd` (the prover's witness callees, `prove::witness_callees`, apply
            // it at `gcd`'s applications). One scan of the view's records' refinements; the witnesses'
            // own contract callees then close as usual.
            if witnesses_scanned {
                break;
            }
            witnesses_scanned = true;
            let mut found: Vec<String> = Vec::new();
            // Another VERSION of a function already in the set is no witness: the agent loop's view
            // holds every candidate's fetched closure, and an older `rat_normalize` (its post applies
            // `gcd`) was derived in full beside the one `rat_add` applies — ~5 of its 11 min
            // (stranger 19, 2026-11-04). A module's records hold one version per name, so builds are
            // unaffected; skipping a witness only withholds axioms, never makes a proof.
            let name_of = |h: &str| self.records.get(h).and_then(|r| r.pointer("/name_hints/0")).and_then(|n| n.as_str()).map(String::from);
            // …including the record mid-derivation that asked (its nested closure scans too — the
            // older `rat_normalize` came in as a witness of `gcd` inside the newer one's derivation).
            let in_progress: Vec<String> =
                self.in_progress.borrow().iter().filter_map(|k| k.strip_prefix("contract:").map(String::from)).collect();
            let in_set: std::collections::BTreeSet<String> =
                out.keys().cloned().chain(in_progress).filter_map(|h| name_of(&h)).collect();
            for (h, rec) in self.records.iter() {
                if out.contains_key(h) {
                    continue;
                }
                if name_of(h).is_some_and(|n| in_set.contains(&n)) {
                    continue;
                }
                if self.certifying.borrow().as_ref().is_some_and(|(name, hash)| h != hash && name_of(h).as_ref() == Some(name)) {
                    continue;
                }
                // Nor, in a BUDGETED view (the agent loop's), the record being certified itself: its
                // contract as its own witness is derived in full beside the derivation that certifies
                // it — `rat_normalize`'s middle pass did the same 120 s twice (2026-11-05). Builders
                // and the full view keep it (their verdicts stay byte-identical).
                if self.law_budget.is_some() && self.certifying.borrow().as_ref().is_some_and(|(_, hash)| h == hash) {
                    continue;
                }
                // Not a record mid-derivation, nor one that APPLIES such a record: deriving it here
                // would see that dependency refused and come out short (`gi_rem` inside `gi_div`).
                let busy = |t: &str| self.in_progress.borrow().contains(&format!("contract:{t}"));
                if busy(h) {
                    continue;
                }
                if let Some((_, body)) = self.callee(h) {
                    let mut applies = std::collections::BTreeSet::new();
                    fn_ref_targets(body, &mut applies);
                    if applies.iter().any(|t| busy(t)) {
                        continue;
                    }
                }
                let mut named = std::collections::BTreeSet::new();
                for r in rec.pointer("/signature/refinements").and_then(|r| r.as_array()).into_iter().flatten() {
                    if r.get("kind").and_then(|k| k.as_str()) == Some("post") {
                        if let Some(e) = r.get("expr") {
                            predicate_fn_targets(e, &mut named);
                        }
                    }
                }
                if named.iter().any(|n| out.contains_key(n)) {
                    found.push(h.clone());
                }
            }
            found.sort();
            if found.is_empty() {
                break;
            }
            queue.extend(found);
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
    callee_def: Option<&dyn Fn(&str) -> Option<(J, std::collections::BTreeSet<usize>)>>,
    solver: &str,
) -> (Vec<crate::RefinementReport>, Vec<J>) {
    let d = derive_self_facts_hinted(sig_type, refinements, laws, body, nat_positions, callee_def, None, solver);
    (d.reports, d.proved)
}

/// The phase of [`derive_self_facts`] that proved a law — what a certification records per law
/// (`spec/certification.schema.json`, `laws[].rung`) and a verified run reads back as a hint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rung {
    /// Phase 1: the plain rungs, no fact about `self` assumed.
    Plain,
    /// Phase 3: numeric induction over the proved posts and earlier laws.
    WithSelfFacts,
    /// Phase 4: from the self-contract, `self` uninterpreted.
    SelfContract,
    /// Phase 5: through the record's own and its callees' definitions.
    SelfDefinition,
    /// Phase 6: strong induction over the self-contract.
    ContractInduction,
}

impl Rung {
    pub fn name(self) -> &'static str {
        match self {
            Rung::Plain => "plain",
            Rung::WithSelfFacts => "with-self-facts",
            Rung::SelfContract => "self-contract",
            Rung::SelfDefinition => "self-definition",
            Rung::ContractInduction => "contract-induction",
        }
    }

    pub fn parse(s: &str) -> Option<Rung> {
        [Rung::Plain, Rung::WithSelfFacts, Rung::SelfContract, Rung::SelfDefinition, Rung::ContractInduction].into_iter().find(|r| r.name() == s)
    }
}

/// What a certification says about one law, as a verified run uses it: which rung proved it, or that
/// a COMPLETE derivation (every callee law available, no time cap) could not. Untrusted scheduling
/// information — it decides which attempts run, never what counts as proved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LawHint {
    ProvedBy(Rung),
    Unproved,
}

/// Hints for one record's laws, keyed by [`law_key`].
pub type LawHints = HashMap<String, LawHint>;

/// The key of a law's predicate in [`LawHints`].
pub fn law_key(law: &J) -> String {
    law.to_string()
}

/// A derivation's result: the post reports, the laws proved (in proof order) and the rung that proved
/// each (same order).
pub struct Derivation {
    pub reports: Vec<crate::RefinementReport>,
    pub proved: Vec<J>,
    pub rungs: Vec<Rung>,
}

/// [`derive_self_facts`] with optional per-law HINTS (from a certification): a law hinted
/// `ProvedBy(r)` is attempted at rung `r` only; one hinted `Unproved` is not attempted. Without
/// hints, exactly [`derive_self_facts`]. Returns the rung of every proved law too.
#[allow(clippy::too_many_arguments)]
pub fn derive_self_facts_hinted(
    sig_type: &J,
    refinements: &[J],
    laws: &[J],
    body: &J,
    nat_positions: &std::collections::BTreeSet<usize>,
    callee_def: Option<&dyn Fn(&str) -> Option<(J, std::collections::BTreeSet<usize>)>>,
    hints: Option<&LawHints>,
    solver: &str,
) -> Derivation {
    // A derivation: the prover's witness + staged retry stays off inside it (see
    // `prove::DERIVATION_DEPTH`) — it is the record under proof's rung, not every callee's.
    crate::prove::in_derivation(|| derive_self_facts_inner(sig_type, refinements, laws, body, nat_positions, callee_def, hints, solver))
}

#[allow(clippy::too_many_arguments)]
fn derive_self_facts_inner(
    sig_type: &J,
    refinements: &[J],
    laws: &[J],
    body: &J,
    nat_positions: &std::collections::BTreeSet<usize>,
    callee_def: Option<&dyn Fn(&str) -> Option<(J, std::collections::BTreeSet<usize>)>>,
    hints: Option<&LawHints>,
    solver: &str,
) -> Derivation {
    // Whether a hint keeps `law` from being attempted at `rung`.
    let skip = |law: &J, rung: Rung| match hints.and_then(|h| h.get(&law_key(law))) {
        Some(LawHint::ProvedBy(r)) => *r != rung,
        Some(LawHint::Unproved) => true,
        None => false,
    };
    let mut rungs: Vec<(J, Rung)> = Vec::new();
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
    let attempt = |law: &J| {
        law_attempt("phase 1", law, || {
            matches!(crate::prove_property(law, Some(body), solver).0, crate::ProofOutcome::Proved)
                || matches!(crate::prove_by_numeric_induction(law, Some(body), nat_positions, solver).0, crate::ProofOutcome::Proved)
                // A list law a bare structural induction closes (`scale_mod` preserves length) is
                // a contract too — the lemma search is skipped here (cost).
                || matches!(crate::prove_by_induction(law, Some(body), solver).0, crate::InductionOutcome::Proved)
        })
    };
    // Phase-1 attempts assume nothing about `self` and nothing from each other, so under a law
    // BUDGET (the agent loop's middle pass) they run in PARALLEL — the proved set is the same, the
    // wall time the slowest attempt rather than the sum (2026-11-04: `rat_normalize`'s five took
    // ~90 s in a row, four of them failing at the 20 s cap). Without a budget (`certify`, the module
    // builders) they stay sequential: the code path, and so every build, is unchanged.
    let budget = LAW_BUDGET.with(|b| b.get());
    let tried: Vec<&J> = laws.iter().filter(|l| !skip(l, Rung::Plain)).collect();
    let plain: Vec<J> = if let Some(b) = budget.filter(|_| tried.len() > 1) {
        let hits = parallel_law_attempts(&tried, b, &attempt);
        tried.iter().zip(hits).filter(|(_, p)| *p).map(|(l, _)| (*l).clone()).collect()
    } else {
        tried.iter().filter(|law| attempt(law)).map(|l| (*l).clone()).collect()
    };
    rungs.extend(plain.iter().map(|l| (l.clone(), Rung::Plain)));
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
    let phase3 = |law: &J| {
        law_attempt("phase 3", law, || matches!(crate::prove_by_numeric_induction(law, Some(body), nat_positions, solver).0, crate::ProofOutcome::Proved))
    };
    let open_after_1 = laws.iter().filter(|l| !proved.contains(l) && !skip(l, Rung::WithSelfFacts)).count();
    if shape.is_some() && (!posts.is_empty() || !plain.is_empty()) && crate::prove::references_self(body) && budget.is_some() && open_after_1 > 1 {
        // Under a law BUDGET (the agent loop), in ROUNDS: every open law at once over the facts so
        // far, then again over the laws that round proved, until a round proves none. Each law
        // sees at least the facts it would in record order (2026-11-05: `binomial`'s two open
        // laws spent 50 s here in a row on a stranger's first verified run).
        loop {
            crate::prove::set_self_facts(facts(posts.clone(), proved.clone()));
            let open: Vec<&J> = laws.iter().filter(|l| !proved.contains(l) && !skip(l, Rung::WithSelfFacts)).collect();
            let hits = parallel_law_attempts(&open, budget.unwrap(), &phase3);
            if !hits.iter().any(|h| *h) {
                break;
            }
            let new: Vec<&J> = open.iter().zip(&hits).filter(|(_, h)| **h).map(|(l, _)| *l).collect();
            rungs.extend(new.iter().map(|l| ((*l).clone(), Rung::WithSelfFacts)));
            // Record order, so the facts (and the SMT text) do not depend on which worker finished first.
            proved = laws.iter().filter(|l| proved.contains(l) || new.contains(l)).cloned().collect();
            if new.len() == open.len() {
                break;
            }
        }
    } else if shape.is_some() && (!posts.is_empty() || !plain.is_empty()) && crate::prove::references_self(body) {
        crate::prove::set_self_facts(facts(posts.clone(), plain.clone()));
        for law in laws {
            if proved.contains(law) || skip(law, Rung::WithSelfFacts) {
                continue;
            }
            if phase3(law) {
                proved.push(law.clone());
                rungs.push((law.clone(), Rung::WithSelfFacts));
                // A law proved here is a fact for the laws after it (in the record's order): gcd
                // associativity rests on the greatest-divisor law, which may itself have needed
                // the posts (2026-10-30, the divisibility rung).
                crate::prove::set_self_facts(facts(posts.clone(), proved.clone()));
            }
        }
    }
    crate::prove::set_self_facts(prev);
    // Phase 4 (2026-10-31): the SELF-CONTRACT rung — a law outside every induction principle that
    // follows from the facts proved above, with `self` uninterpreted under them (`rat_normalize`'s
    // `cross` from `value_preserved` at two applications; `ext_gcd`'s Euclid lemma from the Bézout
    // identity and the link). The CLI's `prove` had this rung and the derivation did not, so a law
    // the builder reported PROVED was missing from the contract every caller received (measured:
    // the rational tier's uniqueness checker saw neither law). Each law proved here is a fact for
    // the laws after it, as in phase 3; never the law under proof itself.
    if let Some(s) = shape.as_ref() {
        if !posts.is_empty() || !proved.is_empty() {
            for law in laws {
                if proved.contains(law) || skip(law, Rung::SelfContract) {
                    continue;
                }
                let mut c = s.clone();
                c.symbol = "self_contract".to_string();
                c.posts = posts.clone();
                c.laws = proved.clone();
                // The staged rung is allowed here (`prove::in_self_contract_phase`): a law it
                // proves on the derived record — uniqueness on `rat_normalize` — is a contract law.
                let proved_here = law_attempt("phase 4", law, || {
                    let outcome = crate::prove::in_self_contract_phase(|| crate::prove::prove_from_self_contract(law, "fn_self_contract", c, nat_positions, solver).0);
                    matches!(outcome, crate::ProofOutcome::Proved)
                });
                if proved_here {
                    proved.push(law.clone());
                    rungs.push((law.clone(), Rung::SelfContract));
                }
            }
        }
    }
    // Phase 5 (2026-11-05): a law of a RECURSIVE body re-read as a checker law over the record's own
    // definition and proved through it and its callees' (`mod_pow`'s value as the naive power —
    // `prove::prove_self_law_through_definitions`). Under the facts proved above; never the law under
    // proof. Not only laws applying a callee: `binomial`'s `pascal` proves here and not by the numeric
    // rung, and a phase-6 induction needs it in the contract.
    if let (Some(s), Some(def)) = (shape.as_ref(), callee_def) {
        if crate::prove::references_self(body) {
            for law in laws {
                if proved.contains(law) || skip(law, Rung::SelfDefinition) {
                    continue;
                }
                let mut c = s.clone();
                c.posts = posts.clone();
                c.laws = proved.clone();
                if law_attempt("phase 5", law, || {
                    matches!(crate::prove_self_law_through_definitions(law, body, c, nat_positions, def, solver).0, crate::ProofOutcome::Proved)
                }) {
                    proved.push(law.clone());
                    rungs.push((law.clone(), Rung::SelfDefinition));
                }
            }
        }
    }
    // Phase 6 (2026-11-05): STRONG INDUCTION on a nat variable over the self-contract — `self`
    // uninterpreted under every fact proved above, the law assumed at smaller values
    // (`prove::prove_from_self_contract_by_induction`): `binomial`'s symmetry from `pascal`,
    // `choose_zero` and `beyond_n`, which the multiplicative body's division keeps from every
    // unfolding rung.
    if let Some(s) = shape.as_ref() {
        if !proved.is_empty() {
            for law in laws {
                if proved.contains(law) || skip(law, Rung::ContractInduction) {
                    continue;
                }
                let mut c = s.clone();
                c.symbol = "self_contract".to_string();
                c.posts = posts.clone();
                c.laws = proved.clone();
                if law_attempt("phase 6", law, || {
                    matches!(crate::prove::prove_from_self_contract_by_induction(law, "fn_self_contract", c, nat_positions, solver).0, crate::ProofOutcome::Proved)
                }) {
                    proved.push(law.clone());
                    rungs.push((law.clone(), Rung::ContractInduction));
                }
            }
        }
    }
    // The rung of each proved law, in proof order (phase 3's rounds reorder `proved` to record order).
    let rungs = proved.iter().map(|l| rungs.iter().find(|(x, _)| x == l).map(|(_, r)| *r).unwrap_or(Rung::Plain)).collect();
    Derivation { reports, proved, rungs }
}

/// `attempt` on each law at once, on ≤ nproc/2 (max 4) worker threads, each with the caller's prover
/// context installed and every attempt capped at `budget`. Whether each law was proved, in order.
fn parallel_law_attempts(laws: &[&J], budget: std::time::Duration, attempt: &(dyn Fn(&J) -> bool + Sync)) -> Vec<bool> {
    let ctx = crate::prove::capture_context();
    let workers = std::thread::available_parallelism().map(|n| n.get() / 2).unwrap_or(1).clamp(1, 4);
    let next = std::sync::atomic::AtomicUsize::new(0);
    let proved: Vec<std::sync::atomic::AtomicBool> = laws.iter().map(|_| std::sync::atomic::AtomicBool::new(false)).collect();
    std::thread::scope(|s| {
        for _ in 0..workers.min(laws.len()) {
            let ctx = ctx.clone();
            let (next, proved) = (&next, &proved);
            s.spawn(move || {
                crate::prove::install_context(ctx);
                with_law_budget(Some(budget), || loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(law) = laws.get(i) else { break };
                    proved[i].store(attempt(law), std::sync::atomic::Ordering::Relaxed);
                });
            });
        }
    });
    proved.iter().map(|p| p.load(std::sync::atomic::Ordering::Relaxed)).collect()
}

/// One attempt at a law. `NL_DERIVED_DEBUG=1` prints the solver queries it spent (measured
/// 2026-11-03: failing attempts on `binomial`'s open laws spend 31–65 queries at ~1 s each, while
/// `rat_normalize`'s `idempotent` PROVES after 286 — no query budget separates the two).
pub fn law_attempt(phase: &str, law: &J, f: impl FnOnce() -> bool) -> bool {
    let before = crate::prove::queries_run();
    let hits_before = crate::prove::query_cache_hits();
    let started = std::time::Instant::now();
    let deadline = LAW_BUDGET.with(|b| b.get()).map(|b| started + b);
    let proved = crate::prove::with_query_deadline(deadline, f);
    let text = || -> String {
        let t = crate::surface::unparse_predicate(law).unwrap_or_else(|_| "?".into());
        // A callee address takes 67 of the 60 characters (`ext_gcd`'s laws all read
        // `forall a b s. b == 0 || fn_29c0f131454ed433bfc54ffbdae2f0afc`): shorten it first.
        short_addresses(&t).chars().take(60).collect()
    };
    if std::env::var("NL_DERIVED_DEBUG").is_ok_and(|v| v == "1") {
        let hits = crate::prove::query_cache_hits() - hits_before;
        eprintln!("[law] {phase} {} queries ({hits} cached), proved={proved}: {}", crate::prove::queries_run() - before, text());
    }
    if progress() {
        // "unproved", not "open": stranger 23 (2026-11-04) read a run full of `law open` lines as
        // alarms; a law the solver gives up on never fails the run (unless `--require-laws`).
        let verdict = if proved { "proved  " } else { "unproved" };
        progress_line!("   law      {verdict} {:>6.1} s  {}", started.elapsed().as_secs_f64(), text());
    }
    proved
}

thread_local! {
    /// The per-attempt cap [`law_attempt`] applies, set by a budgeted view's derivations.
    static LAW_BUDGET: std::cell::Cell<Option<std::time::Duration>> = const { std::cell::Cell::new(None) };
}

/// Run `f` with every [`law_attempt`] in it capped at `budget` (`None` leaves any enclosing cap).
pub fn with_law_budget<T>(budget: Option<std::time::Duration>, f: impl FnOnce() -> T) -> T {
    let Some(b) = budget else { return f() };
    let prev = LAW_BUDGET.with(|c| c.replace(Some(b)));
    let out = f();
    LAW_BUDGET.with(|c| c.set(prev));
    out
}

/// The per-attempt law budget of a verified run: `NL_LAW_BUDGET_SECS` (default 20; `0` = none —
/// every law tried to the end, as before 2026-11-04).
pub fn law_budget() -> Option<std::time::Duration> {
    let secs = std::env::var("NL_LAW_BUDGET_SECS").ok().and_then(|v| v.parse::<u64>().ok()).unwrap_or(20);
    (secs > 0).then(|| std::time::Duration::from_secs(secs))
}

/// Progress lines on stderr while a verified run certifies (`orchestrate --verify` turns it on): one
/// per law attempt and one per callee contract derived, each with its time. Stranger 16 and 17
/// (2026-11-03/04) sat through `binomial`'s and `lcm`'s 3–5 min with one "can take minutes" line
/// and nothing after it; the work is a sequence of solver attempts of seconds to a minute each.
static PROGRESS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_progress(on: bool) {
    PROGRESS.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// Seconds of silence after which the heartbeat prints. Stranger 20 (2026-11-04): `rat_sub`'s 4–5 min
/// run had quiet stretches between the per-law lines (posts, the record's own checks) that "look hung".
const HEARTBEAT_SECS: u64 = 30;

/// Milliseconds since [`progress_clock`]'s start at the last progress line.
static LAST_PROGRESS_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn progress_clock() -> u64 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    START.get_or_init(std::time::Instant::now).elapsed().as_millis() as u64
}

/// Reset the heartbeat's silence clock (every [`progress_line!`] calls it).
pub fn mark_progress() {
    LAST_PROGRESS_MS.store(progress_clock(), std::sync::atomic::Ordering::Relaxed);
}

/// Progress on until the guard drops, with a heartbeat thread: after [`HEARTBEAT_SECS`] without a
/// progress line it prints one saying the solver is still working and for how long.
pub fn progress_on() -> ProgressGuard {
    set_progress(true);
    mark_progress();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = stop.clone();
    let started = std::time::Instant::now();
    let ticker = std::thread::spawn(move || {
        use std::sync::atomic::Ordering::Relaxed;
        while !flag.load(Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(250));
            let silent = progress_clock().saturating_sub(LAST_PROGRESS_MS.load(Relaxed));
            if !flag.load(Relaxed) && silent >= HEARTBEAT_SECS * 1000 {
                progress_line!("   working  still working — {:.0} s so far (not hung)", started.elapsed().as_secs_f64());
            }
        }
    });
    ProgressGuard { stop, ticker: Some(ticker) }
}

pub struct ProgressGuard {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ticker: Option<std::thread::JoinHandle<()>>,
}

impl Drop for ProgressGuard {
    fn drop(&mut self) {
        set_progress(false);
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(t) = self.ticker.take() {
            let _ = t.join();
        }
    }
}

pub fn progress() -> bool {
    PROGRESS.load(std::sync::atomic::Ordering::Relaxed)
}

/// Every `fn_<64 hex>` address in `s` cut to `fn_<8 hex>…` (progress lines only).
fn short_addresses(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("fn_") {
        out.push_str(&rest[..i]);
        let tail = &rest[i + 3..];
        let hex = tail.bytes().take_while(|b| b.is_ascii_hexdigit()).count();
        if hex == 64 {
            out.push_str("fn_");
            out.push_str(&tail[..8]);
            out.push('…');
            rest = &tail[64..];
        } else {
            out.push_str("fn_");
            rest = tail;
        }
    }
    out.push_str(rest);
    out
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
    fn measured_halving(&self, body: &J) -> Option<String> {
        let m = crate::terminate::induction_measure_linked(body, &std::collections::BTreeSet::new(), Some(self))?;
        m.by_callee.filter(|b| b.halving).map(|b| b.target)
    }
    fn cost_positions(&self, target: &str) -> Option<Vec<usize>> {
        // Composed through the callee's own callees (`cost_positions_linked`); a cycle is "unknown".
        let key = format!("costpos:{target}");
        if !self.in_progress.borrow_mut().insert(key.clone()) {
            return None;
        }
        let out = self.callee(target).and_then(|(_, body)| crate::complexity::cost_positions_linked(body, Some(self)));
        self.in_progress.borrow_mut().remove(&key);
        out
    }
    fn result_components_bounded(&self, target: &str) -> Option<Vec<bool>> {
        let key = format!("components:{target}");
        if !self.in_progress.borrow_mut().insert(key.clone()) {
            return None;
        }
        let out = self.callee(target).and_then(|(_, body)| crate::complexity::component_bounds(body, Some(self)));
        self.in_progress.borrow_mut().remove(&key);
        out
    }
    fn result_poly_bounded(&self, target: &str) -> bool {
        // Re-derived from the callee's body over this same view; a cycle is "not known".
        let key = format!("poly:{target}");
        if !self.in_progress.borrow_mut().insert(key.clone()) {
            return false;
        }
        let out = self.callee(target).is_some_and(|(_, body)| crate::complexity::result_poly_bounded(body, Some(self)));
        self.in_progress.borrow_mut().remove(&key);
        out
    }
}

impl crate::terminate::LinkedTermination for LinkedView<'_> {
    fn terminates(&self, target: &str) -> Option<bool> {
        LinkedView::terminates(self, target)
    }
    fn components_shrink_by(&self, target: &str) -> Option<u32> {
        self.shrink(target)
    }
    fn measured_post(&self, target: &str) -> Option<crate::terminate::MeasuredPost> {
        LinkedView::measured_post(self, target)
    }
    fn entails(&self, facts: &[J], goal: &J) -> bool {
        crate::prove::entails(facts, goal, &self.solver)
    }
}

impl LinkedView<'_> {
    /// The callee's certified post that measures its result smaller than a measure of its arguments
    /// — `M(fst(result), snd(result)) < M(p_i, p_j)` or `M(result) < M(p_i)`, `M` a callee applied
    /// by address inside the predicate — when `M` itself terminates and its result is certified
    /// bounded below (a post `result ≥ k` / `result > k`, or a `nat` result). All from re-derived
    /// facts: the post is one `check-refinement` PROVED on this view (the contract), as is `M`'s
    /// bound. The vocabulary of the termination checker's measured descent.
    pub fn measured_post(&self, target: &str) -> Option<crate::terminate::MeasuredPost> {
        let c = self.contract(target)?;
        let var = |n: &J| (n.get("kind").and_then(|k| k.as_str()) == Some("var")).then(|| n.get("name").and_then(|x| x.as_str()).map(String::from)).flatten();
        let proj_of_result = |n: &J, proj: &str| {
            n.get("kind").and_then(|k| k.as_str()) == Some("app")
                && n.get("op").and_then(|o| o.as_str()) == Some(proj)
                && n.get("args").and_then(|a| a.as_array()).is_some_and(|a| a.len() == 1 && var(&a[0]).as_deref() == Some("result"))
        };
        for post in &c.posts {
            if post.get("op").and_then(|o| o.as_str()) != Some("lt") {
                continue;
            }
            let Some(sides) = post.get("args").and_then(|a| a.as_array()).filter(|a| a.len() == 2) else { continue };
            let (lhs, rhs) = (&sides[0], &sides[1]);
            let head = |n: &J| n.get("op").and_then(|o| o.as_str()).filter(|o| o.starts_with("fn_")).map(String::from);
            let (Some(m_l), Some(m_r)) = (head(lhs), head(rhs)) else { continue };
            if m_l != m_r {
                continue;
            }
            let largs = lhs.get("args").and_then(|a| a.as_array()).cloned().unwrap_or_default();
            let rargs = rhs.get("args").and_then(|a| a.as_array()).cloned().unwrap_or_default();
            if largs.is_empty() || largs.len() != rargs.len() {
                continue;
            }
            // The left side: the result's components in order (`fst(result)`, `snd(result)`), or the
            // whole result.
            let components = if largs.len() == 1 && var(&largs[0]).as_deref() == Some("result") {
                1
            } else if largs.len() == 2 && proj_of_result(&largs[0], "fst") && proj_of_result(&largs[1], "snd") {
                2
            } else {
                continue;
            };
            // The right side: distinct parameters of the callee.
            let mut arg_positions = Vec::new();
            for a in &rargs {
                let Some(name) = var(a) else { break };
                let Some(i) = c.params.iter().position(|p| *p == name) else { break };
                if arg_positions.contains(&i) {
                    break;
                }
                arg_positions.push(i);
            }
            if arg_positions.len() != rargs.len() {
                continue;
            }
            // The measure: a terminating callee whose result is certified bounded below.
            if self.terminates(&m_l) != Some(true) {
                continue;
            }
            let Some(mc) = self.contract(&m_l) else { continue };
            let bounded_below = mc.posts.iter().any(|p| {
                let op = p.get("op").and_then(|o| o.as_str());
                let args = p.get("args").and_then(|a| a.as_array()).cloned().unwrap_or_default();
                matches!(op, Some("ge") | Some("gt"))
                    && args.len() == 2
                    && var(&args[0]).as_deref() == Some("result")
                    && args[1].get("kind").and_then(|k| k.as_str()) == Some("lit")
            });
            if !bounded_below {
                continue;
            }
            // HALVING (2026-11-01): another certified post `2·M(lhs) ≤ M(rhs)` — the same measure
            // applications as this one, scaled by the literal 2 (either factor order). The
            // complexity fold reads a descent that halves its measure as a halving recursion.
            let two = |n: &J| n.get("kind").and_then(|k| k.as_str()) == Some("lit") && (n.get("value") == Some(&serde_json::json!(2)) || n.pointer("/value/value") == Some(&serde_json::json!(2)));
            let halving = c.posts.iter().any(|p| {
                let op = p.get("op").and_then(|o| o.as_str());
                let Some(a) = p.get("args").and_then(|a| a.as_array()).filter(|a| a.len() == 2) else { return false };
                if op != Some("le") || &a[1] != rhs {
                    return false;
                }
                let Some(m) = a[0].get("args").and_then(|x| x.as_array()).filter(|x| x.len() == 2) else { return false };
                a[0].get("op").and_then(|o| o.as_str()) == Some("mul") && ((two(&m[0]) && &m[1] == lhs) || (two(&m[1]) && &m[0] == lhs))
            });
            return Some(crate::terminate::MeasuredPost { halving, measure: m_l, arg_positions, components, params: c.params.clone(), pres: c.pres.clone() });
        }
        None
    }

    /// The body's induction measure over THIS view (`terminate::induction_measure_linked`): what the
    /// prover's measure-induction rung inducts on, installed by the callers that derive a record's
    /// facts (`prove::set_self_measure`). `None` for a non-recursive body.
    pub fn self_measure(&self, body: &J, nat_positions: &std::collections::BTreeSet<usize>) -> Option<crate::terminate::Measure> {
        if !crate::prove::references_self(body) {
            return None;
        }
        crate::terminate::induction_measure_linked(body, nat_positions, Some(self))
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
///
/// With a view it runs in two passes. First over [`LinkedView::posts_only`] — callees' pres and posts,
/// not their laws — and if every check that consults the view (refinements, termination,
/// complexity, cost) is decided (SOUND / VERIFIED / N/A), that IS the certification: fewer axioms
/// cannot make a false proof. Anything else — UNVERIFIABLE, or a VIOLATED that may only mean a law
/// was missing — and the whole certification runs again over the full view, exactly as before, so
/// the first pass can only make a verdict cheaper or better, never worse. Measured (2026-11-03):
/// `mod_inverse` 175 s → 16 s; stranger 14 took its verified run's silence for a hang.
pub fn certify_record_linked(
    record: &J,
    body: &J,
    records: &HashMap<String, J>,
    bodies: Option<&HashMap<String, J>>,
    solver: &str,
) -> Certification {
    let Some(b) = bodies else {
        return certify_with_view(record, body, records, None, solver);
    };
    certify_passes(record, body, records, &LinkedView::new(records, b, solver), solver, None)
}

/// The record's per-law results from a COMPLETE derivation (every callee law available, no time
/// cap) — `cert`'s own when it was one, else one more derivation over the full view (its checks
/// discarded: the certification's stay those of `cert`). `None` without linked bodies. What a signed
/// certification records as `laws`: a run may skip a law this says is unproved only because no
/// derivation with more to work from was possible.
pub fn complete_law_results(
    cert: &Certification,
    record: &J,
    body: &J,
    records: &HashMap<String, J>,
    bodies: Option<&HashMap<String, J>>,
    solver: &str,
) -> Option<Vec<(String, Option<Rung>)>> {
    if cert.laws_complete {
        return Some(cert.law_results.clone());
    }
    let full = certify_with_view(record, body, records, Some(&LinkedView::new(records, bodies?, solver)), solver);
    full.laws_complete.then_some(full.law_results)
}

/// [`certify_record_linked`] with the caller's full view for the second pass, so what that pass
/// derives (the record's own contract and laws, its callees') is memoized for the caller: the
/// agent loop proves the same laws next and derived them all a second time (2026-11-03).
///
/// The agent loop's entry: between the two passes, a BUDGETED one ([`LinkedView::budgeted`],
/// `law_budget()`). Only here — `certify` and the module builders keep the two passes, so their
/// signed certificates stay byte-identical.
pub fn certify_record_in_view(record: &J, body: &J, records: &HashMap<String, J>, full: &LinkedView<'_>, solver: &str) -> Certification {
    certify_passes(record, body, records, full, solver, law_budget())
}

fn certify_passes(
    record: &J,
    body: &J,
    records: &HashMap<String, J>,
    full: &LinkedView<'_>,
    solver: &str,
    budget: Option<std::time::Duration>,
) -> Certification {
    let decided = |c: &Certification| c.checks.iter().filter(|c| uses_the_view(&c.check)).all(|c| matches!(c.verdict, "SOUND" | "VERIFIED" | "N/A"));
    // The agent loop's first pass caps the record's OWN laws too: a post rarely needs a slow own
    // law, and the laws are reported, not gating (2026-11-05: stranger 27's first `binomial` spent
    // 115 of 136 s failing `symmetry` and `pascal` with no cap, though its one post needs neither).
    // Undecided ⇒ the middle pass, then the full view — which proves whatever the uncapped
    // posts-only pass would have (a superset of its axioms, no cap), so no verdict is lost.
    let budgeted = budget.filter(|_| full.is_full());
    let first_view = LinkedView { law_budget: budgeted, hints: full.hints.clone(), ..LinkedView::posts_only(records, full.bodies, solver) };
    let first = certify_with_view(record, body, records, Some(&first_view), solver);
    if decided(&first) {
        return first;
    }
    let open = |c: &Certification| -> String {
        c.checks
            .iter()
            .filter(|c| uses_the_view(&c.check) && !matches!(c.verdict, "SOUND" | "VERIFIED" | "N/A"))
            .map(|c| c.check.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    // The middle pass: callee laws, each attempt capped (`law_budget`). Decided ⇒ that is the
    // certification, as for the first pass; else the full view, exactly as before.
    if let Some(budget) = budget.filter(|_| full.is_full()) {
        if progress() {
            progress_line!("   certify  {} undecided on callee posts alone — again with callee laws, {} s per attempt", open(&first), budget.as_secs());
        }
        let middle_view = LinkedView::budgeted(records, full.bodies, solver, budget).with_hints(full.hints.clone());
        let middle = certify_with_view(record, body, records, Some(&middle_view), solver);
        if decided(&middle) {
            return middle;
        }
        if progress() {
            progress_line!("   certify  {} still undecided — again with every callee law, no time cap (slowest)", open(&middle));
        }
    } else if progress() {
        progress_line!("   certify  {} undecided on callee posts alone — again with callee laws (slower)", open(&first));
    }
    certify_with_view(record, body, records, Some(full), solver)
}

/// The certification checks whose verdict can depend on callee contracts from the linked view.
fn uses_the_view(check: &str) -> bool {
    check.starts_with("refinement:") || check.starts_with("cost.") || matches!(check, "termination" | "complexity")
}

fn certify_with_view(
    record: &J,
    body: &J,
    records: &HashMap<String, J>,
    view: Option<&LinkedView<'_>>,
    solver: &str,
) -> Certification {
    let sig = record.pointer("/signature");
    let mut checks: Vec<CertCheck> = Vec::new();
    let (mut laws, mut laws_complete) = (Vec::new(), false);
    let mut law_results: Vec<(String, Option<Rung>)> = Vec::new();
    let mut self_contract = None;

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

    // Parameter docs (optional, testimony): the one structural fact a schema cannot state — one
    // line per parameter of the declared type. A record with no `param_docs` gets no row.
    if let Some(docs) = record.get("param_docs").and_then(|d| d.as_array()) {
        let ty = record.pointer("/signature/type");
        let fn_ty = ty.and_then(|t| if t.get("kind").and_then(|k| k.as_str()) == Some("forall") { t.get("body") } else { Some(t) });
        let arity = fn_ty.and_then(|t| t.get("params")).and_then(|p| p.as_array()).map(|p| p.len());
        match arity {
            Some(n) if n == docs.len() => {
                checks.push(CertCheck::new("param_docs", "SOUND", format!("{n} parameter(s), {n} line(s)"), false))
            }
            Some(n) => checks.push(CertCheck::new(
                "param_docs", "MISMATCH", format!("{} line(s) for {n} parameter(s)", docs.len()), true,
            )),
            None => checks.push(CertCheck::new("param_docs", "UNVERIFIABLE", "the declared type is not a function type", false)),
        }
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
        let refusals_before = view.map(|v| v.cycle_refusals.get());
        let prev = view.map(|v| crate::set_callee_contracts(v.contracts_for_record(record, body)));
        let prev_refusals = view.map(|v| crate::prove::set_callee_refusals(v.refusals_for(body)));
        // The same phased derivation the linked view uses for a callee (its own plain-proved laws
        // may enter a post's induction), so this report and a dependent's view of it agree.
        let nat_positions = crate::terminate::nat_param_positions(record);
        let prev_measure = view.map(|v| crate::prove::set_self_measure(v.self_measure(body, &nat_positions)));
        let started = std::time::Instant::now();
        let def = view.map(|v| move |t: &str| v.callee_def(t));
        let hints = view.zip(record.get("hash").and_then(|h| h.as_str())).and_then(|(v, h)| v.hints_for(h));
        let (reports, proved, rungs) = with_law_budget(view.and_then(|v| v.law_budget), || {
            let d = derive_self_facts_hinted(sig_type, &refinements, &law_exprs(record), body, &nat_positions, def.as_ref().map(|d| d as &dyn Fn(&str) -> _), hints, solver);
            (d.reports, d.proved, d.rungs)
        });
        law_results = record.get("properties").and_then(|p| p.as_array()).into_iter().flatten()
            .filter_map(|p| Some((p.get("name")?.as_str()?.to_string(), p.get("expr")?)))
            .map(|(name, expr)| (name, proved.iter().position(|l| l == expr).map(|i| rungs[i])))
            .collect();
        let declared_posts: Vec<J> = refinements
            .iter()
            .filter(|r| r.get("kind").and_then(|k| k.as_str()) == Some("post"))
            .filter_map(|r| r.get("expr").cloned())
            .collect();
        self_contract = self_shape(sig_type, &refinements, body).map(|mut s| {
            s.posts = sound_posts(&reports, &declared_posts);
            s.laws = proved.clone();
            s
        });
        // A BUDGETED view (the agent loop's) keeps this derivation in the derived cache as the record's
        // contract of that kind, as it does a callee's: the next run that applies the record reads it
        // instead of deriving it (2026-11-05: with the record no longer its own witness, `rat_add`
        // after `rat_normalize` re-derived it, 24 → 150 s). Not when a dependency was refused
        // mid-derivation (tainted, as in `contract`); never for the full or posts-only kinds.
        if let (Some(v), Some(c), Some(target)) = (view, self_contract.as_ref(), record.get("hash").and_then(|h| h.as_str())) {
            let untainted = refusals_before == Some(v.cycle_refusals.get());
            if v.callee_laws && v.law_budget.is_some() && untainted && target.len() > 3 {
                if let Some(dir) = derived_cache_dir() {
                    let mut c = c.clone();
                    c.symbol = format!("callee_{}", &target[3..target.len().min(15)]);
                    store_derived_kind(&dir, target, &c, &v.cache_kind());
                }
            }
        }
        if progress() {
            let sound = reports.iter().filter(|r| matches!(r.outcome, RefinementOutcome::Sound)).count();
            // The lone NOT-APPLICABLE report of a record with nothing to check (a pre only —
            // `is_prime`) is not an unproved post: stranger 18 read "0 of 1 sound" as one.
            let checked = reports.iter().filter(|r| !matches!(r.outcome, RefinementOutcome::NotApplicable)).count();
            let pass = match view {
                Some(v) if !v.callee_laws => match v.law_budget {
                    Some(b) => format!(", laws within {} s each, over callee posts only", b.as_secs()),
                    None => ", over callee posts only".to_string(),
                },
                Some(v) if v.law_budget.is_some() => format!(", laws within {} s each", v.law_budget.unwrap().as_secs()),
                _ => String::new(),
            };
            let posts = if checked == 0 { "no post to prove".to_string() } else { format!("{sound} of {checked} post(s) sound") };
            progress_line!(
                "   contract {posts}, {} of {} law(s) proved ({:.1} s{pass})",
                proved.len(), law_exprs(record).len(), started.elapsed().as_secs_f64()
            );
        }
        laws = proved;
        // Complete = no further attempt in this verified run would add a law: every callee law was
        // available (a full view; a budgeted one — the loop tries laws under the same budget), or
        // there is no callee.
        laws_complete = view.is_some_and(|v| v.callee_laws || v.contracts_for_record(record, body).is_empty());
        if let Some(p) = prev_measure {
            crate::prove::set_self_measure(p);
        }
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
    let term_cb: Option<crate::terminate::CalleeTerminates<'_>> = view.map(|v| v as crate::terminate::CalleeTerminates<'_>);
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
    let cx_cb: Option<crate::complexity::CalleeComplexity<'_>> = view.map(|v| v as crate::complexity::CalleeComplexity<'_>);
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
    Certification { subject, body_hash, checks, certified, laws, laws_complete, law_results, self_contract }
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

    /// `NL_DERIVED_CACHE` is process-wide: the tests that set it hold this, or one's `remove_var`
    /// lands mid-way through another's derivation and its cache write is skipped (CI, v0.1.20).
    static CACHE_ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn cache_env() -> std::sync::MutexGuard<'static, ()> {
        CACHE_ENV.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn fresh_cache_dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("nl-derived-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    /// A partial derivation (budgeted, posts-only) is kept under its own kind and never read as
    /// the full one; the full one is the only entry every kind may read.
    #[test]
    fn partial_derivations_are_cached_under_their_own_kind() {
        let dir = fresh_cache_dir("kinds");
        let c = a_contract(crate::Sort::Int);
        store_derived_kind(&dir, "fn_test_kind", &c, "b20");
        assert_eq!(load_derived_kind(&dir, "fn_test_kind", "b20"), Some(c.clone()));
        assert_eq!(load_derived(&dir, "fn_test_kind"), None, "a budgeted entry is not the full contract");
        assert_eq!(load_derived_kind(&dir, "fn_test_kind", "p"), None, "nor a posts-only one");
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
        let _env = cache_env();
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

    /// The posts-only view (the linked certify's cheap first pass) gives a callee's contract without
    /// its laws and never caches it; the full view proves and caches them. A cached full contract
    /// is the derivation either view would make, so the posts-only view may read it.
    #[test]
    fn a_posts_only_view_skips_callee_laws_and_never_caches_its_partial_contract() {
        let Some(solver) = ["z3", "cvc5"].into_iter().find(|s| std::process::Command::new(s).arg("--version").output().is_ok_and(|o| o.status.success())) else { return };
        let (mut rec, body) = record_named("doubles_with_a_law");
        rec["properties"] = json!([{ "name": "twice", "expr": crate::surface::parse_predicate("forall n. self(n) == n + n").unwrap() }]);
        let hash = crate::hash_artifact_with_kind(&rec, crate::ArtifactKind::FunctionRecord).unwrap();
        rec["hash"] = json!(hash);
        let records: HashMap<String, J> = [(hash.clone(), rec)].into_iter().collect();
        let bodies: HashMap<String, J> = [(hash.clone(), body)].into_iter().collect();
        let _env = cache_env();
        let dir = fresh_cache_dir("posts-only");
        std::env::set_var("NL_DERIVED_CACHE", dir.to_str().unwrap());
        let thin = LinkedView::posts_only(&records, &bodies, solver).contract(&hash).expect("a contract");
        let cached_after_thin = load_derived(&dir, &hash);
        let full = LinkedView::new(&records, &bodies, solver).contract(&hash).expect("a contract");
        let cached_after_full = load_derived(&dir, &hash);
        std::env::remove_var("NL_DERIVED_CACHE");
        assert!(thin.laws.is_empty(), "posts only: {:?}", thin.laws);
        assert_eq!(cached_after_thin, None, "a partial contract is never cached");
        assert_eq!(full.laws.len(), 1, "the full view proves the law");
        assert_eq!(cached_after_full, Some(full));
    }

    /// The budgeted view (the agent loop's middle pass) proves a cheap law within its budget, and
    /// a law attempt whose budget is already spent proves nothing — a cut-off can only lose a
    /// proof (2026-11-04, stranger 19).
    #[test]
    fn a_budgeted_view_proves_within_its_budget_and_a_spent_budget_proves_nothing() {
        let Some(solver) = ["z3", "cvc5"].into_iter().find(|s| std::process::Command::new(s).arg("--version").output().is_ok_and(|o| o.status.success())) else { return };
        let (mut rec, body) = record_named("doubles_with_a_cheap_law");
        rec["properties"] = json!([{ "name": "twice", "expr": crate::surface::parse_predicate("forall n. self(n) == n + n").unwrap() }]);
        let hash = crate::hash_artifact_with_kind(&rec, crate::ArtifactKind::FunctionRecord).unwrap();
        rec["hash"] = json!(hash);
        let records: HashMap<String, J> = [(hash.clone(), rec)].into_iter().collect();
        let bodies: HashMap<String, J> = [(hash.clone(), body)].into_iter().collect();
        let view = LinkedView::budgeted(&records, &bodies, solver, std::time::Duration::from_secs(20));
        assert!(!view.is_full());
        assert_eq!(view.contract(&hash).expect("a contract").laws.len(), 1, "a cheap law proves within budget");
        let spent = LinkedView::budgeted(&records, &bodies, solver, std::time::Duration::ZERO);
        assert!(spent.contract(&hash).expect("a contract").laws.is_empty(), "a spent budget proves nothing");
    }

    /// Under a budget phase 1 runs its law attempts on worker threads (the prover context
    /// installed in each): the proved set must be exactly the sequential one, including a law
    /// that needs a callee contract and one that does not hold.
    #[test]
    fn parallel_phase_one_proves_what_the_sequential_one_does() {
        let _env = CACHE_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let Some(solver) = ["z3", "cvc5"].into_iter().find(|s| std::process::Command::new(s).arg("--version").output().is_ok_and(|o| o.status.success())) else { return };
        let law = |s: &str| crate::surface::parse_predicate(s).unwrap();
        // `doubles`, with the law the parallel attempts need from its contract.
        let (mut callee, callee_body) = record_named("doubles");
        callee["properties"] = json!([{ "name": "twice", "expr": law("forall n. self(n) == n + n") }]);
        let hc = crate::hash_artifact_with_kind(&callee, crate::ArtifactKind::FunctionRecord).unwrap();
        callee["hash"] = json!(hc);
        // \n -> doubles(doubles(n)): its laws rest on the callee's derived contract.
        let callee_ref = json!({ "kind": "lit", "value": { "kind": "fn_ref", "target": hc.clone() } });
        let body = json!({ "kind": "lambda", "params": [{ "name": "n" }], "body":
            { "kind": "app", "fn": callee_ref.clone(), "args": [
              { "kind": "app", "fn": callee_ref, "args": [{ "kind": "var", "name": "n" }] } ] } });
        let (mut rec, _) = record_named("quadruples");
        rec["body_hash"] = json!(crate::hash_artifact_with_kind(&body, crate::ArtifactKind::BodyExpression).unwrap());
        rec["examples"] = json!([{ "args": [{ "kind": "int", "value": 2 }], "result": { "kind": "int", "value": 8 } }]);
        rec["properties"] = json!([
            { "name": "four_times", "expr": law("forall n. self(n) == 4 * n") },
            { "name": "even", "expr": law("forall n. self(n) % 2 == 0") },
            { "name": "false_one", "expr": law("forall n. self(n) == n") },
            { "name": "monotone", "expr": law("forall m n. m > n || self(m) <= self(n)") } ]);
        let hash = crate::hash_artifact_with_kind(&rec, crate::ArtifactKind::FunctionRecord).unwrap();
        rec["hash"] = json!(hash);
        let records: HashMap<String, J> = [(hash.clone(), rec), (hc.clone(), callee)].into_iter().collect();
        let bodies: HashMap<String, J> = [(hash.clone(), body), (hc.clone(), callee_body)].into_iter().collect();
        let prev = std::env::var_os("NL_DERIVED_CACHE");
        std::env::set_var("NL_DERIVED_CACHE", "0");
        let budget = std::time::Duration::from_secs(30);
        let parallel = LinkedView::budgeted(&records, &bodies, solver, budget).contract(&hash).expect("a contract").laws;
        let sequential = LinkedView::new(&records, &bodies, solver).contract(&hash).expect("a contract").laws;
        match prev {
            Some(v) => std::env::set_var("NL_DERIVED_CACHE", v),
            None => std::env::remove_var("NL_DERIVED_CACHE"),
        }
        assert_eq!(parallel, sequential, "the parallel phase 1 proved a different set");
        assert!(parallel.len() >= 2, "the true laws over the callee's contract prove: {parallel:?}");
        assert!(!parallel.contains(&law("forall n. self(n) == n")), "a false law never proves");
    }

    /// Under a budget, phase 3 runs its open laws in parallel ROUNDS; it proves the set the
    /// sequential pass does on a recursive body (two false laws keep two attempts open).
    #[test]
    fn phase_three_rounds_prove_what_the_sequential_pass_does() {
        let Some(solver) = ["z3", "cvc5"].into_iter().find(|s| std::process::Command::new(s).arg("--version").output().is_ok_and(|o| o.status.success())) else { return };
        let law = |s: &str| crate::surface::parse_predicate(s).unwrap();
        let slit = |n: i64| json!({ "kind": "lit", "value": { "kind": "int", "value": n } });
        let n = json!({ "kind": "var", "name": "n" });
        // \n -> case lt(n, 1) of true -> 0 | false -> add(2, self(sub(n, 1)))
        let rec_call = json!({ "kind": "app", "op": "self", "args": [{ "kind": "app", "op": "sub", "args": [n.clone(), slit(1)] }] });
        let body = json!({ "kind": "lambda", "params": [{ "name": "n" }], "body": { "kind": "case",
            "scrutinee": { "kind": "app", "op": "lt", "args": [n, slit(1)] }, "arms": [
            { "pattern": { "kind": "lit", "value": { "kind": "bool", "value": true } }, "body": slit(0) },
            { "pattern": { "kind": "lit", "value": { "kind": "bool", "value": false } }, "body": { "kind": "app", "op": "add", "args": [slit(2), rec_call] } }] } });
        let sig = json!({ "kind": "fn", "params": [{ "kind": "builtin", "name": "int" }], "result": { "kind": "builtin", "name": "int" } });
        let refinements = vec![json!({ "kind": "post", "expr": law("result >= 0") })];
        let laws = vec![
            law("forall n. n < 0 || self(n) == 2 * n"),
            law("forall n. self(n) == n + 1"),
            law("forall n. self(n) % 2 == 0"),
            law("forall n. n < 1 || self(n) == 2 * n + 1"),
        ];
        let nats: std::collections::BTreeSet<usize> = Default::default();
        let sequential = derive_self_facts(&sig, &refinements, &laws, &body, &nats, None, solver).1;
        let rounds = with_law_budget(Some(std::time::Duration::from_secs(20)), || derive_self_facts(&sig, &refinements, &laws, &body, &nats, None, solver).1);
        assert_eq!(rounds, sequential, "phase 3's rounds proved a different set");
        assert!(rounds.contains(&laws[0]) && rounds.contains(&laws[2]), "the true laws prove: {rounds:?}");
        assert!(!rounds.contains(&laws[1]) && !rounds.contains(&laws[3]), "a false law never proves");
    }

    /// Per-law hints steer the derivation: a law hinted to the rung that proves it is still proved
    /// (and that rung reported); hinted to another rung it is not attempted where it would prove;
    /// hinted UNPROVED it is not attempted at all (no solver query for it).
    #[test]
    fn law_hints_choose_the_rung_and_skip_the_unproved() {
        let Some(solver) = ["z3", "cvc5"].into_iter().find(|s| std::process::Command::new(s).arg("--version").output().is_ok_and(|o| o.status.success())) else { return };
        let law = |s: &str| crate::surface::parse_predicate(s).unwrap();
        let slit = |n: i64| json!({ "kind": "lit", "value": { "kind": "int", "value": n } });
        let n = json!({ "kind": "var", "name": "n" });
        // \n -> case lt(n, 1) of true -> 0 | false -> add(2, self(sub(n, 1)))
        let rec_call = json!({ "kind": "app", "op": "self", "args": [{ "kind": "app", "op": "sub", "args": [n.clone(), slit(1)] }] });
        let body = json!({ "kind": "lambda", "params": [{ "name": "n" }], "body": { "kind": "case",
            "scrutinee": { "kind": "app", "op": "lt", "args": [n, slit(1)] }, "arms": [
            { "pattern": { "kind": "lit", "value": { "kind": "bool", "value": true } }, "body": slit(0) },
            { "pattern": { "kind": "lit", "value": { "kind": "bool", "value": false } }, "body": { "kind": "app", "op": "add", "args": [slit(2), rec_call] } }] } });
        let sig = json!({ "kind": "fn", "params": [{ "kind": "builtin", "name": "int" }], "result": { "kind": "builtin", "name": "int" } });
        let refinements = vec![json!({ "kind": "post", "expr": law("result >= 0") })];
        let laws = vec![law("forall n. n < 0 || self(n) == 2 * n"), law("forall n. self(n) == n + 1")];
        let nats: std::collections::BTreeSet<usize> = Default::default();
        let derive = |h: Option<&LawHints>| derive_self_facts_hinted(&sig, &refinements, &laws, &body, &nats, None, h, solver);

        let plain = derive(None);
        assert_eq!(plain.proved, vec![laws[0].clone()], "unhinted: the true law proves");
        let rung = plain.rungs[0];

        let right: LawHints = [(law_key(&laws[0]), LawHint::ProvedBy(rung))].into_iter().collect();
        let d = derive(Some(&right));
        assert_eq!((d.proved, d.rungs), (vec![laws[0].clone()], vec![rung]), "hinted to its own rung it still proves there");

        let other = if rung == Rung::ContractInduction { Rung::SelfContract } else { Rung::ContractInduction };
        let wrong: LawHints = [(law_key(&laws[0]), LawHint::ProvedBy(other))].into_iter().collect();
        assert!(!derive(Some(&wrong)).proved.contains(&laws[0]), "hinted to a rung that cannot prove it, it is not proved");

        let unproved: LawHints = [(law_key(&laws[0]), LawHint::Unproved), (law_key(&laws[1]), LawHint::Unproved)].into_iter().collect();
        let before = crate::prove::queries_run();
        let none = derive(Some(&unproved));
        let spent = crate::prove::queries_run() - before;
        let posts_only = {
            let b = crate::prove::queries_run();
            let _ = check_refinements(&sig, &refinements, &body, solver);
            crate::prove::queries_run() - b
        };
        assert!(none.proved.is_empty(), "laws hinted unproved are not attempted");
        assert_eq!(spent, posts_only, "no solver query beyond the posts' own");
    }

    /// Another VERSION of a function in the closure is not a witness (the agent loop's view holds
    /// every candidate's fetched closure; an older `rat_normalize` was derived beside the newer).
    #[test]
    fn the_witness_scan_skips_another_version_of_a_function_in_the_set() {
        let (a, ab) = record_named("same_name");
        let (mut b, bb) = record_named("same_name");
        b["examples"][0]["args"][0]["value"] = json!(3);
        b["examples"][0]["result"]["value"] = json!(6);
        b["hash"] = json!(crate::hash_artifact_with_kind(&b, crate::ArtifactKind::FunctionRecord).unwrap());
        let (ha, hb) = (a["hash"].as_str().unwrap().to_string(), b["hash"].as_str().unwrap().to_string());
        // `b`'s post applies `a` by address — a witness of `a`, but for its own name.
        b["signature"]["refinements"] = json!([{ "kind": "post", "expr":
            { "kind": "app", "op": "eq", "args": [ { "kind": "var", "name": "result" },
              { "kind": "app", "op": ha.clone(), "args": [ { "kind": "var", "name": "n" } ] } ] } }]);
        let records: HashMap<String, J> = [(ha.clone(), a), (hb.clone(), b)].into_iter().collect();
        let bodies: HashMap<String, J> = [(ha.clone(), ab), (hb.clone(), bb)].into_iter().collect();
        let view = LinkedView::posts_only(&records, &bodies, "z3");
        let set = view.contracts_closure([ha.clone()].into_iter().collect());
        assert!(!set.contains_key(&hb), "another version of `same_name` was taken as a witness");
    }

    #[test]
    fn progress_lines_shorten_callee_addresses() {
        let h = "29c0f131454ed433bfc54ffbdae2f0afc0448a41d9833351e6824ec0b787d8f6";
        assert_eq!(short_addresses(&format!("b == 0 || fn_{h}(a, b) == 1")), "b == 0 || fn_29c0f131…(a, b) == 1");
        assert_eq!(short_addresses("fn_abc(x) && no_fn_here"), "fn_abc(x) && no_fn_here");
    }

    /// Nor is another version of the record BEING CERTIFIED, though no contract in the set names
    /// it: certifying the new `gi_rem`, the old one (its post applies `gi_norm`) was a witness of
    /// `gi_norm` and derived in full (stranger 22, 2026-11-04).
    #[test]
    fn the_witness_scan_skips_another_version_of_the_record_being_certified() {
        let (helper, helper_body) = record_named("helper");
        let hh = helper["hash"].as_str().unwrap().to_string();
        let applies_helper = json!([{ "kind": "post", "expr":
            { "kind": "app", "op": "eq", "args": [ { "kind": "var", "name": "result" },
              { "kind": "app", "op": hh.clone(), "args": [ { "kind": "var", "name": "n" } ] } ] } }]);
        let version = |seed: i64| {
            let (mut r, b) = record_named("target");
            r["examples"][0]["args"][0]["value"] = json!(seed);
            r["examples"][0]["result"]["value"] = json!(2 * seed);
            r["signature"]["refinements"] = applies_helper.clone();
            r["hash"] = json!(crate::hash_artifact_with_kind(&r, crate::ArtifactKind::FunctionRecord).unwrap());
            (r, b)
        };
        let ((new, new_body), (old, old_body)) = (version(3), version(5));
        let (hn, ho) = (new["hash"].as_str().unwrap().to_string(), old["hash"].as_str().unwrap().to_string());
        // The body applies the helper, so the helper is in the set and the old version's post names it.
        let body = json!({ "kind": "lambda", "params": [{ "name": "n" }], "body":
            { "kind": "app", "fn": { "kind": "fn_ref", "target": hh.clone() }, "args": [{ "kind": "var", "name": "n" }] } });
        let records: HashMap<String, J> =
            [(hh.clone(), helper), (hn.clone(), new.clone()), (ho.clone(), old)].into_iter().collect();
        let bodies: HashMap<String, J> =
            [(hh.clone(), helper_body), (hn.clone(), new_body), (ho.clone(), old_body)].into_iter().collect();
        let view = LinkedView::posts_only(&records, &bodies, "z3");
        let set = view.contracts_for_record(&new, &body);
        assert!(set.contains_key(&hh), "the helper the body applies is in the set");
        assert!(!set.contains_key(&ho), "another version of the record being certified was taken as a witness");
        assert!(view.certifying.borrow().is_none(), "the outermost call clears what it set");
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

    /// `param_docs` (2026-11-02): schema-valid, one line per parameter, and a count mismatch is a
    /// hard failure; a record without them gets no row.
    #[test]
    fn param_docs_are_one_line_per_parameter() {
        let rehash = |mut rec: J| {
            rec["hash"] = json!("fn_".to_string() + &"0".repeat(64));
            let h = crate::hash_artifact_with_kind(&rec, crate::ArtifactKind::FunctionRecord).unwrap();
            rec["hash"] = json!(h);
            rec
        };
        let (rec, body) = record_named("double");
        let plain = certify_record(&rec, &body, &HashMap::new(), "z3");
        assert!(plain.checks.iter().all(|c| c.check != "param_docs"));

        let mut one = rec.clone();
        one["param_docs"] = json!(["the number to double"]);
        let one = rehash(one);
        let cert = certify_record(&one, &body, &HashMap::new(), "z3");
        assert_eq!(schema_check(&cert).verdict, "VALID", "{:?}", schema_check(&cert));
        let row = cert.checks.iter().find(|c| c.check == "param_docs").expect("a param_docs row");
        assert_eq!(row.verdict, "SOUND");

        let mut two = rec.clone();
        two["param_docs"] = json!(["the number", "a second line for a parameter that does not exist"]);
        let two = rehash(two);
        let cert = certify_record(&two, &body, &HashMap::new(), "z3");
        let row = cert.checks.iter().find(|c| c.check == "param_docs").expect("a param_docs row");
        assert_eq!(row.verdict, "MISMATCH");
        assert!(!cert.certified, "a count mismatch revokes certification");
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

    /// A witness candidate that applies a record still mid-derivation is NOT derived inside that
    /// derivation (it would see the dependency refused and come out short), and a contract derived
    /// under a cycle refusal is never memoized. The Gaussian tier's shape: `gi_div`'s witness scan
    /// derived `gi_rem` (whose post names `gi_norm`) while `gi_div` itself was in progress, `gi_rem`
    /// lost its norm post, and `gi_gcd`'s measured descent read UNVERIFIABLE (2026-11-01).
    #[test]
    fn a_witness_scan_never_derives_and_keeps_a_contract_short_of_a_dependency_in_progress() {
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
        let call = |h: &str, a: J| json!({ "kind": "app", "fn": { "kind": "lit", "value": { "kind": "fn_ref", "target": h } }, "args": [a] });
        let ge0 = |e: J| json!({ "kind": "app", "op": "ge", "args": [e, { "kind": "lit", "value": 0 }] });
        let napp = |h: &str| json!({ "kind": "app", "op": h, "args": [var("result")] });
        // N = \x -> abs(x), post result >= 0 (the norm).
        let (hn, rn, bn) = mk("n", lam(json!({ "kind": "app", "fn": var("abs"), "args": [var("x")] })), vec![ge0(var("result"))]);
        // P = \x -> N(x), posts result >= 0 and N(result) >= 0 (names N: P's closure reaches N).
        let (hp, rp, bp) = mk("p", lam(call(&hn, var("x"))), vec![ge0(var("result")), ge0(napp(&hn))]);
        // R = \x -> P(x), the same posts: `result >= 0` holds only through P's contract; its post
        // naming N makes R a witness candidate while P's contract is being derived.
        let (hr, rr, br) = mk("r", lam(call(&hp, var("x"))), vec![ge0(var("result")), ge0(napp(&hn))]);
        let mut records = HashMap::new();
        let mut bodies = HashMap::new();
        for (h, r, b) in [(hn.clone(), rn, bn), (hp.clone(), rp, bp), (hr.clone(), rr, br)] {
            records.insert(h.clone(), r);
            bodies.insert(h, b);
        }
        let view = LinkedView::new(&records, &bodies, solver);
        assert_eq!(view.contract(&hp).map(|c| c.posts.len()), Some(2), "P: both posts");
        assert_eq!(view.contract(&hr).map(|c| c.posts.len()), Some(2), "R: both posts, derived with P's contract in hand");
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
