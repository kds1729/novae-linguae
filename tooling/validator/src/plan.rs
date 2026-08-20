//! World-state plan checking (spec/world-state.md — evolution/gcp-sdk-poc open question 4).
//!
//! An effectful record's real contract is about the WORLD: `put_item` ensures `item(name)`
//! exists, `delete_item` ensures it is absent, a cloud operation requires a VPC and guarantees a
//! subnet. Those contracts are `requires`/`ensures` refinements over an abstract resource
//! (function-record.v0.2.schema.json `$defs/world_refinement`), and this module discharges them
//! for a PLAN — a sequence of concrete applications — by symbolic execution over a ground-resource
//! state map, so a multi-step effectful plan is checked BEFORE any effect is performed.
//!
//! Honest grading: the check verifies the plan against the DECLARATIONS, never the declarations
//! against the world — a `requires`/`ensures` is the record author's stated contract, priced like
//! any other testimony (and exposed by the effectful run itself when false). Three-valued by
//! construction: a requirement the symbolic state CONTRADICTS rejects the plan; one the state
//! says NOTHING about makes it unverifiable (state the assumption or reorder), never a silent
//! pass; only a plan whose every requirement discharges is sound.

use anyhow::{anyhow, Result};
use serde_json::Value as J;
use std::collections::HashMap;

/// A ground resource: class + key values instantiated at a step's actual arguments.
#[derive(Clone, PartialEq)]
struct Ground {
    class: String,
    key: Vec<J>,
}

impl Ground {
    fn render(&self) -> String {
        let parts: Vec<String> = self.key.iter().map(crate::plan::render_value).collect();
        format!("{}({})", self.class, parts.join(", "))
    }
}

/// A compact human rendering of a value expression for report lines.
fn render_value(v: &J) -> String {
    match v.get("kind").and_then(|k| k.as_str()) {
        Some("string") => format!("{:?}", v.get("value").and_then(|s| s.as_str()).unwrap_or("")),
        Some("int") | Some("nat") | Some("bool") | Some("float") => {
            v.get("value").map(|x| x.to_string()).unwrap_or_default()
        }
        _ => v.to_string(),
    }
}

/// How the plan check came out.
pub enum PlanOutcome {
    /// Every requirement discharged from assumptions and prior ensures.
    Sound,
    /// No contradiction, but at least one requirement the symbolic state says nothing about.
    Unverifiable(Vec<String>),
    /// A requirement the symbolic state contradicts — the plan must not run.
    Rejected(String),
}

pub struct PlanReport {
    /// One line per checked requirement / applied ensure, in plan order.
    pub lines: Vec<String>,
    pub outcome: PlanOutcome,
}

fn lambda_params(body: &J) -> Option<Vec<String>> {
    if body.get("kind").and_then(|k| k.as_str()) != Some("lambda") {
        return None;
    }
    Some(
        body.get("params")?
            .as_array()?
            .iter()
            .filter_map(|p| p.get("name").and_then(|n| n.as_str()).map(String::from))
            .collect(),
    )
}

/// Instantiate a declared resource at a step's actual arguments: `var` key parts substitute the
/// argument at the parameter's position, `lit` parts stand as themselves. Errors (not verdicts):
/// a key naming an unknown parameter is a malformed declaration, not a world question.
fn ground(resource: &J, params: &[String], args: &[J]) -> Result<Ground> {
    let class = resource
        .get("class")
        .and_then(|c| c.as_str())
        .ok_or_else(|| anyhow!("world resource has no class"))?
        .to_string();
    let mut key = Vec::new();
    for part in resource.get("key").and_then(|k| k.as_array()).cloned().unwrap_or_default() {
        match part.get("kind").and_then(|k| k.as_str()) {
            Some("var") => {
                let name = part.get("name").and_then(|n| n.as_str()).unwrap_or("");
                let pos = params
                    .iter()
                    .position(|p| p == name)
                    .ok_or_else(|| anyhow!("world resource key names `{name}`, not a parameter"))?;
                key.push(args.get(pos).cloned().ok_or_else(|| anyhow!("argument {pos} missing"))?);
            }
            Some("lit") => {
                key.push(part.get("value").cloned().ok_or_else(|| anyhow!("lit key part has no value"))?);
            }
            // Finding 9 (evolution/gcp-sdk-poc): the REST creation idiom names the new resource
            // INSIDE the request body, so a create's `ensures` needs a key part read from a
            // top-level field of a JSON body parameter. Grounded from the step's LITERAL
            // argument — the checker parses the plan's own concrete data, never runtime values —
            // and anything that cannot ground (non-string arg, unparseable JSON, absent field,
            // non-scalar field) is a malformed plan/declaration pair: an error, not a verdict.
            Some("body-field") => {
                let pname = part.get("param").and_then(|n| n.as_str()).unwrap_or("");
                let fname = part.get("field").and_then(|f| f.as_str()).unwrap_or("");
                let pos = params
                    .iter()
                    .position(|p| p == pname)
                    .ok_or_else(|| anyhow!("world resource key reads body field `{fname}` of `{pname}`, not a parameter"))?;
                let arg = args.get(pos).ok_or_else(|| anyhow!("argument {pos} missing"))?;
                let s = (arg.get("kind").and_then(|k| k.as_str()) == Some("string"))
                    .then(|| arg.get("value").and_then(|v| v.as_str()))
                    .flatten()
                    .ok_or_else(|| anyhow!("`{pname}` is not a string argument — a body-field key needs a JSON body literal"))?;
                let doc: J = serde_json::from_str(s)
                    .map_err(|_| anyhow!("the plan's `{pname}` argument is not JSON — the body-field key `{fname}` cannot ground"))?;
                let v = doc
                    .get(fname)
                    .ok_or_else(|| anyhow!("the plan's `{pname}` argument carries no field `{fname}` — the resource this call is declared to affect is unnamed"))?;
                let encoded = match v {
                    J::String(x) => serde_json::json!({ "kind": "string", "value": x }),
                    J::Bool(x) => serde_json::json!({ "kind": "bool", "value": x }),
                    J::Number(n) if n.is_i64() => {
                        serde_json::json!({ "kind": "int", "value": n.as_i64() })
                    }
                    _ => {
                        return Err(anyhow!(
                            "field `{fname}` of `{pname}` is not a scalar — a resource key must be"
                        ))
                    }
                };
                key.push(encoded);
            }
            other => return Err(anyhow!("unsupported world key part kind {other:?}")),
        }
    }
    Ok(Ground { class, key })
}

/// The symbolic world: each ground resource's state plus WHERE that state came from (an
/// assumption or a step's ensure) — provenance makes every verdict line auditable.
struct World {
    entries: Vec<(Ground, String, String)>, // (resource, state, origin)
}

impl World {
    fn get(&self, g: &Ground) -> Option<(&str, &str)> {
        self.entries.iter().find(|(e, _, _)| e == g).map(|(_, s, o)| (s.as_str(), o.as_str()))
    }
    fn set(&mut self, g: Ground, state: String, origin: String) {
        if let Some(e) = self.entries.iter_mut().find(|(e, _, _)| *e == g) {
            e.1 = state;
            e.2 = origin;
        } else {
            self.entries.push((g, state, origin));
        }
    }
}

fn world_refinements(record: &J) -> Vec<J> {
    record
        .pointer("/signature/refinements")
        .and_then(|r| r.as_array())
        .map(|rs| {
            rs.iter()
                .filter(|r| {
                    matches!(r.get("kind").and_then(|k| k.as_str()), Some("requires") | Some("ensures"))
                })
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

/// Check a plan — `{ "assume": [ {resource, state}… ], "steps": [ {target, args}… ] }` — against
/// the records' declared world refinements. See the module doc for the semantics.
pub fn check_plan(
    plan: &J,
    records: &HashMap<String, J>,
    bodies: &HashMap<String, J>,
) -> Result<PlanReport> {
    let mut world = World { entries: Vec::new() };
    let mut lines = Vec::new();
    let mut unknowns = Vec::new();

    for (i, a) in plan.get("assume").and_then(|a| a.as_array()).cloned().unwrap_or_default().iter().enumerate() {
        let resource = a.get("resource").ok_or_else(|| anyhow!("assumption {i} has no resource"))?;
        // An assumption's key must be ground already — a `var` names nothing here.
        let g = ground(resource, &[], &[])
            .map_err(|e| anyhow!("assumption {i}: {e} (assumption keys must be literals)"))?;
        let state = a
            .get("state")
            .and_then(|s| s.as_str())
            .ok_or_else(|| anyhow!("assumption {i} has no state"))?
            .to_string();
        lines.push(format!("assume       {} = {}", g.render(), state));
        world.set(g, state, "the plan's stated assumption".into());
    }

    let steps = plan
        .get("steps")
        .and_then(|s| s.as_array())
        .ok_or_else(|| anyhow!("plan must have a `steps` array"))?;
    for (i, step) in steps.iter().enumerate() {
        let n = i + 1;
        let target = step
            .get("target")
            .and_then(|t| t.as_str())
            .ok_or_else(|| anyhow!("step {n} has no target"))?;
        let args: Vec<J> = step.get("args").and_then(|a| a.as_array()).cloned().unwrap_or_default();
        let record = records
            .get(target)
            .ok_or_else(|| anyhow!("step {n}: record {target} not in the commons view"))?;
        let name = record.pointer("/name_hints/0").and_then(|s| s.as_str()).unwrap_or(target);
        let body = record
            .pointer("/body_hash")
            .and_then(|b| b.as_str())
            .and_then(|bh| bodies.get(bh))
            .ok_or_else(|| anyhow!("step {n}: body of {name} not in the commons view"))?;
        let params = lambda_params(body).ok_or_else(|| anyhow!("step {n}: body of {name} is not a lambda"))?;
        if params.len() != args.len() {
            return Err(anyhow!("step {n}: {name} takes {} argument(s), the plan supplies {}", params.len(), args.len()));
        }

        let refs = world_refinements(record);
        // All requires first (against the state BEFORE this step), then all ensures.
        for r in refs.iter().filter(|r| r.get("kind").and_then(|k| k.as_str()) == Some("requires")) {
            let g = ground(r.get("resource").ok_or_else(|| anyhow!("step {n}: requires has no resource"))?, &params, &args)?;
            let want = r.get("state").and_then(|s| s.as_str()).unwrap_or("");
            match world.get(&g) {
                Some((have, origin)) if have == want => {
                    lines.push(format!("step {n}       {name} requires {} = {want}  ✓ ({origin})", g.render()));
                }
                Some((have, origin)) => {
                    let msg = format!(
                        "step {n}: {name} requires {} = {want}, but it is {have} ({origin})",
                        g.render()
                    );
                    lines.push(format!("step {n}       {name} requires {} = {want}  ✗ CONTRADICTED — it is {have} ({origin})", g.render()));
                    return Ok(PlanReport { lines, outcome: PlanOutcome::Rejected(msg) });
                }
                None => {
                    let msg = format!(
                        "step {n}: {name} requires {} = {want} — nothing establishes it (state the assumption or reorder the plan)",
                        g.render()
                    );
                    lines.push(format!("step {n}       {name} requires {} = {want}  ? UNKNOWN", g.render()));
                    unknowns.push(msg);
                }
            }
        }
        for r in refs.iter().filter(|r| r.get("kind").and_then(|k| k.as_str()) == Some("ensures")) {
            let g = ground(r.get("resource").ok_or_else(|| anyhow!("step {n}: ensures has no resource"))?, &params, &args)?;
            let state = r.get("state").and_then(|s| s.as_str()).unwrap_or("").to_string();
            lines.push(format!("step {n}       {name} ensures  {} = {state}", g.render()));
            world.set(g, state, format!("step {n} ({name})"));
        }
    }

    let outcome = if unknowns.is_empty() {
        PlanOutcome::Sound
    } else {
        PlanOutcome::Unverifiable(unknowns)
    };
    Ok(PlanReport { lines, outcome })
}

/// What probing the plan's assumptions found.
pub struct ProbeReport {
    /// One line per assumption, in plan order.
    pub lines: Vec<String>,
    /// Assumptions a probe CONTRADICTED — the plan rests on false testimony and must not run.
    pub refuted: Vec<String>,
    /// Assumptions no probe could decide (no probe bound for the class, or an inconclusive
    /// status) — they stay testimony, stated as such.
    pub unconfirmed: Vec<String>,
}

/// Spot-verify the plan's ASSUMPTIONS by observation (spec/world-state.md — the "observation
/// probes" rung). An assumption is the exact place testimony enters a plan check: `check_plan`
/// prices it like any other declaration, and this upgrades it — or refutes it — by a read-only
/// call. A **probe** is an ordinary commons record bound per resource CLASS by the operator
/// (`class -> fn_…`): its parameters are the class's key parts in order, and its observed
/// status decides the state by the absent-name convention — 2xx = exists, 404 = absent,
/// anything else inconclusive (an auth failure or a throttle is not a world observation).
/// `exec` performs one read-only application (body, args) -> observed status; injecting it
/// keeps the decision logic testable without a live service.
pub fn probe_assumptions(
    plan: &J,
    records: &HashMap<String, J>,
    bodies: &HashMap<String, J>,
    probes: &HashMap<String, String>,
    exec: &mut dyn FnMut(&J, &[J]) -> Result<i64>,
) -> Result<ProbeReport> {
    let mut lines = Vec::new();
    let mut refuted = Vec::new();
    let mut unconfirmed = Vec::new();
    for (i, a) in
        plan.get("assume").and_then(|a| a.as_array()).cloned().unwrap_or_default().iter().enumerate()
    {
        let resource = a.get("resource").ok_or_else(|| anyhow!("assumption {i} has no resource"))?;
        let g = ground(resource, &[], &[])
            .map_err(|e| anyhow!("assumption {i}: {e} (assumption keys must be literals)"))?;
        let want = a
            .get("state")
            .and_then(|s| s.as_str())
            .ok_or_else(|| anyhow!("assumption {i} has no state"))?;
        let Some(target) = probes.get(&g.class) else {
            lines.push(format!(
                "probe        {} = {want}  — no probe bound for class `{}`; the assumption stays testimony",
                g.render(),
                g.class
            ));
            unconfirmed.push(format!("{} = {want} (unprobed)", g.render()));
            continue;
        };
        let record = records
            .get(target)
            .ok_or_else(|| anyhow!("probe for class `{}`: record {target} not in the commons view", g.class))?;
        let name = record.pointer("/name_hints/0").and_then(|s| s.as_str()).unwrap_or(target);
        let body = record
            .pointer("/body_hash")
            .and_then(|b| b.as_str())
            .and_then(|bh| bodies.get(bh))
            .ok_or_else(|| anyhow!("probe for class `{}`: body of {name} not in the commons view", g.class))?;
        let params = lambda_params(body)
            .ok_or_else(|| anyhow!("probe for class `{}`: body of {name} is not a lambda", g.class))?;
        if params.len() != g.key.len() {
            return Err(anyhow!(
                "probe for class `{}`: {name} takes {} argument(s) but the class's key has {} part(s) — a probe's parameters must be the key, in order",
                g.class,
                params.len(),
                g.key.len()
            ));
        }
        let status = exec(body, &g.key)?;
        let observed = match status {
            200..=299 => Some("exists"),
            404 => Some("absent"),
            _ => None,
        };
        match observed {
            Some(o) if o == want => {
                lines.push(format!(
                    "probe        {} = {want}  ✓ OBSERVED ({name} answered {status}) — the assumption is confirmed",
                    g.render()
                ));
            }
            Some(o) => {
                lines.push(format!(
                    "probe        {} = {want}  ✗ REFUTED ({name} answered {status} = {o}) — the assumption is false",
                    g.render()
                ));
                refuted.push(format!(
                    "{} was assumed {want} but the probe observed {o} ({name} answered {status})",
                    g.render()
                ));
            }
            None => {
                lines.push(format!(
                    "probe        {} = {want}  ? INCONCLUSIVE ({name} answered {status} — not a world observation); the assumption stays testimony",
                    g.render()
                ));
                unconfirmed.push(format!("{} = {want} (probe inconclusive: {status})", g.render()));
            }
        }
    }
    Ok(ProbeReport { lines, refuted, unconfirmed })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn s(v: &str) -> J {
        json!({ "kind": "string", "value": v })
    }

    /// A record named `name` over one string parameter `x`, with the given world refinements.
    fn rec(name: &str, refs: Vec<J>, records: &mut HashMap<String, J>, bodies: &mut HashMap<String, J>) -> String {
        let body = json!({ "kind": "lambda", "params": [{ "name": "x" }],
            "body": { "kind": "var", "name": "x" } });
        let bh = crate::hash_artifact_with_kind(&body, crate::ArtifactKind::BodyExpression).unwrap();
        let mut r = json!({
            "schema_version": "0.2.0", "hash": "fn_".to_string() + &"0".repeat(64),
            "name_hints": [name],
            "signature": { "type": { "kind": "fn", "params": [{ "kind": "builtin", "name": "string" }],
                                     "result": { "kind": "builtin", "name": "int" } },
                           "refinements": refs, "effects": ["net.write"], "capabilities": [],
                           "terminates": "always" },
            "examples": [], "intent_tags": [], "derived_from": J::Null, "supersedes": J::Null,
            "body_hash": bh });
        let h = crate::hash_artifact_with_kind(&r, crate::ArtifactKind::FunctionRecord).unwrap();
        r["hash"] = json!(h.clone());
        records.insert(h.clone(), r);
        bodies.insert(bh, body);
        h
    }

    fn item(kpart: J) -> J {
        json!({ "class": "item", "key": [kpart] })
    }
    fn var_x() -> J {
        json!({ "kind": "var", "name": "x" })
    }
    fn lit(v: &str) -> J {
        json!({ "kind": "lit", "value": s(v) })
    }
    fn requires(resource: J, state: &str) -> J {
        json!({ "kind": "requires", "resource": resource, "state": state })
    }
    fn ensures(resource: J, state: &str) -> J {
        json!({ "kind": "ensures", "resource": resource, "state": state })
    }

    fn trio(records: &mut HashMap<String, J>, bodies: &mut HashMap<String, J>) -> (String, String, String) {
        let put = rec("put_item", vec![ensures(item(var_x()), "exists")], records, bodies);
        let get = rec("item_status", vec![requires(item(var_x()), "exists")], records, bodies);
        let del = rec("delete_item",
                      vec![requires(item(var_x()), "exists"), ensures(item(var_x()), "absent")],
                      records, bodies);
        (put, get, del)
    }

    fn plan(assume: Vec<J>, steps: Vec<(&str, &str)>) -> J {
        json!({ "assume": assume,
                "steps": steps.iter().map(|(t, a)| json!({ "target": t, "args": [s(a)] }))
                              .collect::<Vec<_>>() })
    }

    #[test]
    fn lifecycle_plan_is_sound() {
        let (mut r, mut b) = (HashMap::new(), HashMap::new());
        let (put, get, del) = trio(&mut r, &mut b);
        // create -> verify -> delete: every requires discharged by a prior ensures.
        let p = plan(vec![], vec![(&put, "w"), (&get, "w"), (&del, "w")]);
        let rep = check_plan(&p, &r, &b).unwrap();
        assert!(matches!(rep.outcome, PlanOutcome::Sound), "{:?}", rep.lines);
    }

    #[test]
    fn use_after_delete_is_rejected_before_any_effect() {
        let (mut r, mut b) = (HashMap::new(), HashMap::new());
        let (put, get, del) = trio(&mut r, &mut b);
        let p = plan(vec![], vec![(&put, "w"), (&del, "w"), (&get, "w")]);
        let rep = check_plan(&p, &r, &b).unwrap();
        match rep.outcome {
            PlanOutcome::Rejected(msg) => {
                assert!(msg.contains("step 3"), "{msg}");
                assert!(msg.contains("absent"), "{msg}");
            }
            _ => panic!("use-after-delete must reject: {:?}", rep.lines),
        }
    }

    #[test]
    fn unestablished_requirement_is_unverifiable_never_silent() {
        let (mut r, mut b) = (HashMap::new(), HashMap::new());
        let (_put, get, _del) = trio(&mut r, &mut b);
        // A bare read: nothing says the item exists — unverifiable, not sound, not rejected.
        let p = plan(vec![], vec![(&get, "w")]);
        let rep = check_plan(&p, &r, &b).unwrap();
        match rep.outcome {
            PlanOutcome::Unverifiable(msgs) => assert!(msgs[0].contains("nothing establishes")),
            _ => panic!("must be unverifiable: {:?}", rep.lines),
        }
        // The same plan under the assumption discharges.
        let p = plan(
            vec![json!({ "resource": item(lit("w")), "state": "exists" })],
            vec![(&get, "w")],
        );
        assert!(matches!(check_plan(&p, &r, &b).unwrap().outcome, PlanOutcome::Sound));
    }

    #[test]
    fn ground_resources_never_collide_across_keys() {
        let (mut r, mut b) = (HashMap::new(), HashMap::new());
        let (put, get, _del) = trio(&mut r, &mut b);
        // Creating "a" establishes item("a"), not item("b") — the read of "b" stays unknown.
        let p = plan(vec![], vec![(&put, "a"), (&get, "b")]);
        let rep = check_plan(&p, &r, &b).unwrap();
        assert!(matches!(rep.outcome, PlanOutcome::Unverifiable(_)), "{:?}", rep.lines);
    }

    #[test]
    fn later_ensures_overwrite_earlier_state() {
        let (mut r, mut b) = (HashMap::new(), HashMap::new());
        let (put, get, del) = trio(&mut r, &mut b);
        // delete then re-create then read: the re-create's ensures supersedes the delete's.
        let p = plan(
            vec![json!({ "resource": item(lit("w")), "state": "exists" })],
            vec![(&del, "w"), (&put, "w"), (&get, "w")],
        );
        let rep = check_plan(&p, &r, &b).unwrap();
        assert!(matches!(rep.outcome, PlanOutcome::Sound), "{:?}", rep.lines);
    }

    /// A record over (container, body) — the REST creation shape: the new resource's name
    /// travels INSIDE the JSON body argument (finding 9).
    fn rec2(name: &str, refs: Vec<J>, records: &mut HashMap<String, J>, bodies: &mut HashMap<String, J>) -> String {
        let body = json!({ "kind": "lambda",
            "params": [{ "name": "container" }, { "name": "body" }],
            "body": { "kind": "var", "name": "container" } });
        let bh = crate::hash_artifact_with_kind(&body, crate::ArtifactKind::BodyExpression).unwrap();
        let mut r = json!({
            "schema_version": "0.2.0", "hash": "fn_".to_string() + &"0".repeat(64),
            "name_hints": [name],
            "signature": { "type": { "kind": "fn",
                                     "params": [{ "kind": "builtin", "name": "string" },
                                                { "kind": "builtin", "name": "string" }],
                                     "result": { "kind": "builtin", "name": "int" } },
                           "refinements": refs, "effects": ["net.write"], "capabilities": [],
                           "terminates": "always" },
            "examples": [], "intent_tags": [], "derived_from": J::Null, "supersedes": J::Null,
            "body_hash": bh });
        let h = crate::hash_artifact_with_kind(&r, crate::ArtifactKind::FunctionRecord).unwrap();
        r["hash"] = json!(h.clone());
        records.insert(h.clone(), r);
        bodies.insert(bh, body);
        h
    }

    fn body_field(param: &str, field: &str) -> J {
        json!({ "kind": "body-field", "param": param, "field": field })
    }

    #[test]
    fn create_names_its_resource_in_the_body_and_the_lifecycle_discharges() {
        // Finding 9 (evolution/gcp-sdk-poc): `insert(container, body)` creates the resource
        // NAMED IN the body — `ensures bucket(body.name) exists` was inexpressible, so the
        // correct create -> verify plan could only come back UNVERIFIABLE. With a body-field
        // key it grounds from the plan's literal body argument, and the same ground resource
        // discharges a later step's parameter-keyed requires.
        let (mut r, mut b) = (HashMap::new(), HashMap::new());
        let ins = rec2("insert", vec![ensures(
            json!({ "class": "bucket", "key": [body_field("body", "name")] }), "exists")],
            &mut r, &mut b);
        let get = rec("get", vec![requires(
            json!({ "class": "bucket", "key": [var_x()] }), "exists")], &mut r, &mut b);
        let p = json!({ "steps": [
            { "target": ins, "args": [s("proj"), s("{\"name\": \"nl-demo\"}")] },
            { "target": get, "args": [s("nl-demo")] },
        ]});
        let rep = check_plan(&p, &r, &b).unwrap();
        assert!(matches!(rep.outcome, PlanOutcome::Sound), "{:?}", rep.lines);
        // A DIFFERENT name in the body does not discharge the read — unknown, never silent.
        let p = json!({ "steps": [
            { "target": ins, "args": [s("proj"), s("{\"name\": \"other\"}")] },
            { "target": get, "args": [s("nl-demo")] },
        ]});
        assert!(matches!(check_plan(&p, &r, &b).unwrap().outcome, PlanOutcome::Unverifiable(_)));
    }

    #[test]
    fn malformed_declarations_are_errors_not_verdicts() {
        let (mut r, mut b) = (HashMap::new(), HashMap::new());
        // A key naming a non-parameter is a broken declaration.
        let bad = rec("bad", vec![ensures(json!({ "class": "item",
            "key": [{ "kind": "var", "name": "nope" }] }), "exists")], &mut r, &mut b);
        let p = plan(vec![], vec![(&bad, "w")]);
        assert!(check_plan(&p, &r, &b).is_err());
        // An arity mismatch likewise.
        let (put, _, _) = trio(&mut r, &mut b);
        let p = json!({ "steps": [{ "target": put, "args": [] }] });
        assert!(check_plan(&p, &r, &b).is_err());
    }

    #[test]
    fn ungroundable_body_field_is_an_error_not_a_verdict() {
        // A body-field key that cannot ground from the plan's literal body argument — not
        // JSON, or the field absent — is a malformed plan/declaration pair: an error, never a
        // silent verdict (the resource the call is declared to affect would be unnamed).
        let (mut r, mut b) = (HashMap::new(), HashMap::new());
        let ins = rec2("insert", vec![ensures(
            json!({ "class": "bucket", "key": [body_field("body", "name")] }), "exists")],
            &mut r, &mut b);
        let p = json!({ "steps": [{ "target": ins, "args": [s("proj"), s("not json")] }] });
        assert!(check_plan(&p, &r, &b).is_err());
        let p = json!({ "steps": [{ "target": ins, "args": [s("proj"), s("{\"other\": 1}")] }] });
        assert!(check_plan(&p, &r, &b).is_err());
    }

    fn probes_for(class: &str, target: &str) -> HashMap<String, String> {
        let mut m = HashMap::new();
        m.insert(class.to_string(), target.to_string());
        m
    }

    #[test]
    fn probe_confirms_a_true_assumption() {
        let (mut r, mut b) = (HashMap::new(), HashMap::new());
        let (_put, get, _del) = trio(&mut r, &mut b);
        let p = plan(vec![json!({ "resource": item(lit("w")), "state": "exists" })], vec![]);
        let rep = probe_assumptions(&p, &r, &b, &probes_for("item", &get), &mut |_, args| {
            assert_eq!(args.len(), 1, "probe args are the resource key, in order");
            Ok(200)
        })
        .unwrap();
        assert!(rep.refuted.is_empty() && rep.unconfirmed.is_empty(), "{:?}", rep.lines);
        assert!(rep.lines[0].contains("OBSERVED"), "{:?}", rep.lines);
    }

    #[test]
    fn probe_refutes_a_false_assumption() {
        // The whole point: an assumption is testimony, and a 404 at the assumed-existing
        // resource proves it false BEFORE any effect runs on top of it.
        let (mut r, mut b) = (HashMap::new(), HashMap::new());
        let (_put, get, _del) = trio(&mut r, &mut b);
        let p = plan(vec![json!({ "resource": item(lit("w")), "state": "exists" })], vec![]);
        let rep =
            probe_assumptions(&p, &r, &b, &probes_for("item", &get), &mut |_, _| Ok(404)).unwrap();
        assert_eq!(rep.refuted.len(), 1, "{:?}", rep.lines);
        assert!(rep.refuted[0].contains("assumed exists") && rep.refuted[0].contains("absent"));
    }

    #[test]
    fn inconclusive_and_unprobed_assumptions_stay_testimony() {
        // A 403 is an auth fact, not a world observation — and a class with no probe bound
        // was never observed at all. Neither refutes; both are STATED as testimony.
        let (mut r, mut b) = (HashMap::new(), HashMap::new());
        let (_put, get, _del) = trio(&mut r, &mut b);
        let p = plan(
            vec![
                json!({ "resource": item(lit("w")), "state": "exists" }),
                json!({ "resource": { "class": "unprobed", "key": [lit("k")] }, "state": "absent" }),
            ],
            vec![],
        );
        let rep =
            probe_assumptions(&p, &r, &b, &probes_for("item", &get), &mut |_, _| Ok(403)).unwrap();
        assert!(rep.refuted.is_empty());
        assert_eq!(rep.unconfirmed.len(), 2, "{:?}", rep.lines);
        assert!(rep.lines[0].contains("INCONCLUSIVE"));
        assert!(rep.lines[1].contains("no probe bound"));
    }

    #[test]
    fn probe_arity_mismatch_is_an_error_not_a_verdict() {
        // A probe's parameters must BE the class's key, in order — a mismatch is a malformed
        // binding, never a world question.
        let (mut r, mut b) = (HashMap::new(), HashMap::new());
        let (_put, get, _del) = trio(&mut r, &mut b);
        let p = plan(
            vec![json!({ "resource": { "class": "pair", "key": [lit("a"), lit("b")] },
                         "state": "exists" })],
            vec![],
        );
        assert!(probe_assumptions(&p, &r, &b, &probes_for("pair", &get), &mut |_, _| Ok(200))
            .is_err());
    }
}
