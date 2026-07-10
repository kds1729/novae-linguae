//! Goal-directed assembly from the commons (spec/agent-loop.md — "assemble, don't write").
//!
//! Given a GOAL — a set of input→output examples (and an implied input/output type) — search the
//! commons for a sequence of functions whose *composition* reproduces every example, then verify the
//! assembled pipeline: it must `compose` (stage-to-stage type composability + derived composite
//! metadata), its synthesized composite body must RUN each example through the resolved stages, and —
//! under `require_certified` — every stage must itself certify ("assemble only from verified parts").
//! The result is a first-class derived composite record whose body chains the stages by
//! content-address (`fn_ref`), so the assembled whole is itself runnable, certifiable, publishable.
//!
//! This is the missing middle the recon identified: `compose` checks a *given* order and `orchestrate`
//! chains *given* intent tags, but neither *finds* the pipeline that achieves a goal. `assemble` does
//! the search (example-driven, breadth-first so the shortest pipeline wins, type-pruned by `compose`),
//! then reuses `compose`/`certify_record`/the interpreter to verify it.

use std::collections::{HashMap, HashSet, VecDeque};

use anyhow::{anyhow, Result};
use serde_json::{json, Value as J};

use crate::compose::{compose, CompositionMetadata};

/// One stage of an assembled pipeline: the commons function chosen at that position.
#[derive(Debug, Clone)]
pub struct Stage {
    pub hash: String,
    pub name: String,
}

/// A verified assembled pipeline.
#[derive(Debug, Clone)]
pub struct Assembled {
    pub stages: Vec<Stage>,
    pub composite: CompositionMetadata,
    /// Every stage independently certifies (only computed when `require_certified`).
    pub certified: bool,
    pub examples_verified: usize,
    /// The derived composite record and its synthesized `fn_ref`-chain body.
    pub composite_record: J,
    pub composite_body: J,
}

/// The declared arity of a function record (its fn-type parameter count), unwrapping `forall`.
fn arity(record: &J) -> Option<usize> {
    let mut t = record.pointer("/signature/type")?;
    if t.get("kind").and_then(|k| k.as_str()) == Some("forall") {
        t = t.get("body")?;
    }
    if t.get("kind").and_then(|k| k.as_str()) == Some("fn") {
        Some(t.get("params")?.as_array()?.len())
    } else {
        None
    }
}

/// The primary function name of a record.
fn record_name(record: &J) -> String {
    record
        .pointer("/name_hints/0")
        .and_then(|n| n.as_str())
        .unwrap_or("fn")
        .to_string()
}

/// A stable key for a vector of intermediate values (dedup the BFS frontier).
fn state_key(values: &[J]) -> String {
    values.iter().map(|v| v.to_string()).collect::<Vec<_>>().join("\u{1}")
}

/// Search the commons for a pipeline of **unary** functions whose composition maps every example's
/// input to its output. Breadth-first (shortest pipeline first), deduping by the intermediate-value
/// vector, bounded by `max_stages` and a global node cap. Returns the ordered stage hashes, or None.
fn search(
    unary: &[(String, J)], // (hash, body) of arity-1 records, in a stable order
    examples: &[(J, J)],
    max_stages: usize,
) -> Option<Vec<String>> {
    let inputs: Vec<J> = examples.iter().map(|(i, _)| i.clone()).collect();
    let outputs: Vec<J> = examples.iter().map(|(_, o)| o.clone()).collect();

    // The identity pipeline already satisfies the goal (input == output for every example).
    if inputs == outputs {
        return Some(vec![]);
    }

    let mut seen: HashSet<String> = HashSet::new();
    seen.insert(state_key(&inputs));
    let mut frontier: VecDeque<(Vec<String>, Vec<J>)> = VecDeque::new();
    frontier.push_back((vec![], inputs));
    let mut budget = 200_000usize;

    while let Some((pipeline, values)) = frontier.pop_front() {
        if pipeline.len() >= max_stages {
            continue;
        }
        for (hash, body) in unary {
            if budget == 0 {
                return None;
            }
            budget -= 1;
            // Apply this candidate to every example's running value; skip if it errors on any
            // (partial / type-mismatched) — the search only follows total-on-these-inputs stages.
            let mut next = Vec::with_capacity(values.len());
            let mut ok = true;
            for v in &values {
                match crate::eval_body(body, std::slice::from_ref(v)) {
                    // A fully-applied unary stage yields a value; a partial application encodes as
                    // `{kind: "function"}` (arity mismatch) — reject it.
                    Ok(r) if r.get("kind").is_some() && r.get("kind") != Some(&json!("function")) => {
                        next.push(r)
                    }
                    _ => {
                        ok = false;
                        break;
                    }
                }
            }
            if !ok {
                continue;
            }
            if next == outputs {
                let mut pl = pipeline.clone();
                pl.push(hash.clone());
                return Some(pl);
            }
            let key = state_key(&next);
            if seen.insert(key) {
                let mut pl = pipeline.clone();
                pl.push(hash.clone());
                frontier.push_back((pl, next));
            }
        }
    }
    None
}

/// Synthesize the composite body `\x -> fn(fn_ref stageN) (… (fn(fn_ref stage1) x))` — each stage
/// applied to the previous result by content-address (`fn_ref`), the "assemble, don't write" primitive.
fn composite_body(stage_hashes: &[String]) -> J {
    let mut inner = json!({ "kind": "var", "name": "x" });
    for h in stage_hashes {
        inner = json!({ "kind": "app",
            "fn": { "kind": "lit", "value": { "kind": "fn_ref", "target": h } },
            "args": [inner] });
    }
    json!({ "kind": "lambda", "params": [{ "name": "x" }], "body": inner })
}

/// Assemble a pipeline from the commons that satisfies `examples`, then verify it. `records` maps
/// `fn_…` → record; `bodies` maps `fn_…`/`expr_…` → body AST (from `build_record_map`/`build_link_map`
/// or a node). Returns `Ok(None)` if no pipeline within `max_stages` reproduces every example.
pub fn assemble(
    records: &HashMap<String, J>,
    bodies: &HashMap<String, J>,
    examples: &[(J, J)],
    max_stages: usize,
    require_certified: bool,
    solver: &str,
) -> Result<Option<Assembled>> {
    if examples.is_empty() {
        return Err(anyhow!("a goal needs at least one input→output example"));
    }
    // The resolver lets a candidate whose own body uses `fn_ref` (a higher-order commons function)
    // execute during the search, and lets the synthesized composite body run stage-to-stage.
    crate::set_resolver(bodies.clone());

    // Candidate stages: arity-1 records with a resolvable body, in a deterministic order.
    let mut unary: Vec<(String, J)> = Vec::new();
    for (hash, rec) in records {
        if arity(rec) == Some(1) {
            if let Some(bh) = rec.pointer("/body_hash").and_then(|b| b.as_str()) {
                if let Some(body) = bodies.get(bh).or_else(|| bodies.get(hash)) {
                    unary.push((hash.clone(), body.clone()));
                }
            }
        }
    }
    unary.sort_by(|a, b| a.0.cmp(&b.0));

    let found = search(&unary, examples, max_stages);
    let Some(stage_hashes) = found else {
        crate::clear_resolver();
        return Ok(None);
    };

    let stages: Vec<Stage> = stage_hashes
        .iter()
        .map(|h| Stage { hash: h.clone(), name: record_name(&records[h]) })
        .collect();

    // Verify (1): the pipeline composes end to end — stage-to-stage type composability + derived
    // composite metadata.
    let stage_records: Vec<J> = stage_hashes.iter().map(|h| records[h].clone()).collect();
    let composite = compose(&stage_records);
    if !stage_records.is_empty() && !composite.composable {
        crate::clear_resolver();
        return Err(anyhow!("assembled pipeline does not compose: {}", composite.reason));
    }

    // Verify (2): the synthesized composite body runs every example through the resolved stages.
    let body = composite_body(&stage_hashes);
    let mut examples_verified = 0;
    for (input, output) in examples {
        let got = crate::eval_body(&body, std::slice::from_ref(input))?;
        if &got != output {
            crate::clear_resolver();
            return Err(anyhow!(
                "composite body disagrees with an example: got {got} want {output}"
            ));
        }
        examples_verified += 1;
    }

    // Verify (3, optional): every stage certifies — assemble only from verified parts.
    let certified = if require_certified {
        stage_hashes.iter().all(|h| {
            let rec = &records[h];
            let bh = rec.pointer("/body_hash").and_then(|b| b.as_str());
            match bh.and_then(|bh| bodies.get(bh)) {
                Some(sb) => crate::certify_record(rec, sb, records, solver).certified,
                None => false,
            }
        })
    } else {
        false
    };

    crate::clear_resolver();

    if require_certified && !certified {
        return Err(anyhow!("--require-certified: at least one stage is not certified"));
    }

    let composite_record = build_composite_record(&stages, &composite, examples, &body)?;
    Ok(Some(Assembled { stages, composite, certified, examples_verified, composite_record, composite_body: body }))
}

/// Build the derived composite function record (its body_hash addresses the synthesized composite body).
fn build_composite_record(
    stages: &[Stage],
    composite: &CompositionMetadata,
    examples: &[(J, J)],
    body: &J,
) -> Result<J> {
    let name = if stages.is_empty() {
        "assembled_identity".to_string()
    } else {
        let joined = stages.iter().map(|s| s.name.as_str()).collect::<Vec<_>>().join("_then_");
        // name_hints pattern is ^[a-z][a-zA-Z0-9_]*$ — sanitize.
        let mut n: String = joined.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
        if !n.chars().next().map(|c| c.is_ascii_lowercase() || c == '_').unwrap_or(false) {
            n = format!("f_{n}");
        }
        n
    };
    let ty = json!({
        "kind": "fn",
        "params": [composite.input_type.clone().unwrap_or(json!({ "kind": "var", "name": "a" }))],
        "result": composite.output_type.clone().unwrap_or(json!({ "kind": "var", "name": "a" })),
    });
    let examples_j: Vec<J> = examples.iter().map(|(i, o)| json!({ "args": [i], "result": o })).collect();
    let body_hash = crate::hash_artifact_with_kind(body, crate::ArtifactKind::BodyExpression)?;
    let mut record = json!({
        "schema_version": "0.2.0",
        "hash": "fn_".to_string() + &"0".repeat(64),
        "name_hints": [name],
        "signature": {
            "type": ty,
            "refinements": [],
            "effects": composite.effects.clone(),
            "capabilities": composite.capabilities.clone(),
            "terminates": composite.terminates.clone(),
        },
        "examples": examples_j,
        "intent_tags": [],
        "derived_from": stages.first().map(|s| json!(s.hash)).unwrap_or(J::Null),
        "supersedes": J::Null,
        "body_hash": body_hash,
    });
    let hash = crate::hash_artifact_with_kind(&record, crate::ArtifactKind::FunctionRecord)?;
    record["hash"] = json!(hash);
    Ok(record)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn int(n: i64) -> J {
        json!({ "kind": "int", "value": n })
    }

    /// A `\n -> op(n, n)` unary int→int commons function (record + body), keyed into the maps.
    fn add_unary(name: &str, op: &str, records: &mut HashMap<String, J>, bodies: &mut HashMap<String, J>) {
        let body = json!({ "kind": "lambda", "params": [{ "name": "n" }], "body":
            { "kind": "app", "fn": { "kind": "var", "name": op },
              "args": [{ "kind": "var", "name": "n" }, { "kind": "var", "name": "n" }] } });
        let bh = crate::hash_artifact_with_kind(&body, crate::ArtifactKind::BodyExpression).unwrap();
        let mut rec = json!({
            "schema_version": "0.2.0", "hash": "fn_".to_string() + &"0".repeat(64),
            "name_hints": [name],
            "signature": { "type": { "kind": "fn", "params": [{ "kind": "builtin", "name": "int" }],
                                     "result": { "kind": "builtin", "name": "int" } },
                           "refinements": [], "effects": [], "capabilities": [], "terminates": "always" },
            "examples": [{ "args": [int(3)], "result": int(if op == "add" { 6 } else { 9 }) }],
            "intent_tags": [], "derived_from": J::Null, "supersedes": J::Null, "body_hash": bh });
        let h = crate::hash_artifact_with_kind(&rec, crate::ArtifactKind::FunctionRecord).unwrap();
        rec["hash"] = json!(h.clone());
        records.insert(h.clone(), rec);
        bodies.insert(bh, body.clone());
        bodies.insert(h, body);
    }

    #[test]
    fn assembles_a_two_stage_pipeline_from_a_goal() {
        let mut records = HashMap::new();
        let mut bodies = HashMap::new();
        add_unary("double", "add", &mut records, &mut bodies); // \n -> n + n
        add_unary("square", "mul", &mut records, &mut bodies); // \n -> n * n

        // Goal: 3 -> 36, 2 -> 16. Only `double` then `square` fits both (3->6->36, 2->4->16);
        // `square` then `double` gives 3->9->18. Two examples pin the order.
        let examples = vec![(int(3), int(36)), (int(2), int(16))];
        let a = assemble(&records, &bodies, &examples, 3, false, "z3").unwrap().expect("a pipeline");
        assert_eq!(a.stages.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["double", "square"]);
        assert_eq!(a.examples_verified, 2);
        assert!(a.composite.composable);
        // The synthesized composite record has a runnable fn_ref-chain body and its own address.
        assert!(a.composite_record["hash"].as_str().unwrap().starts_with("fn_"));
        assert_eq!(a.composite_record["name_hints"][0], json!("double_then_square"));

        // A goal no single/2-stage pipeline reaches returns None (bounded search).
        let impossible = vec![(int(3), int(7))]; // 7 is not reachable by double/square from 3
        assert!(assemble(&records, &bodies, &impossible, 3, false, "z3").unwrap().is_none());

        // The identity goal (input == output) assembles the empty pipeline.
        let ident = vec![(int(5), int(5))];
        let e = assemble(&records, &bodies, &ident, 3, false, "z3").unwrap().expect("identity");
        assert!(e.stages.is_empty());
    }

    #[test]
    fn require_certified_gates_on_stage_certification() {
        let mut records = HashMap::new();
        let mut bodies = HashMap::new();
        add_unary("double", "add", &mut records, &mut bodies);
        add_unary("square", "mul", &mut records, &mut bodies);
        let examples = vec![(int(3), int(36)), (int(2), int(16))];
        // These stages certify (well-typed, pure, always-terminating, O(1)).
        let a = assemble(&records, &bodies, &examples, 3, true, "z3").unwrap().expect("certified pipeline");
        assert!(a.certified);
    }
}
