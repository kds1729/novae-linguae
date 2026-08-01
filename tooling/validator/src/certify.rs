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
    analyze_complexity, analyze_output_size, check_refinements, infer_effects,
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

    // 3. Refinements (type-implied `nat` + declared pre/post).
    if let Some(sig_type) = record.pointer("/signature/type") {
        let refinements: Vec<J> =
            record.pointer("/signature/refinements").and_then(|r| r.as_array()).cloned().unwrap_or_default();
        for r in check_refinements(sig_type, &refinements, body, solver) {
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
    match crate::terminate::analyze_termination_typed(body, &crate::terminate::nat_param_positions(record)) {
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
    let complexity = analyze_complexity(body);
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
