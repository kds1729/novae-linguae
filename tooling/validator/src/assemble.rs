//! Goal-directed assembly from the commons (spec/agent-loop.md — "assemble, don't write").
//!
//! Given a GOAL — a set of examples, each mapping an **argument list** `[primary, aux…]` to an
//! output — search the commons for a sequence of functions whose *composition* reproduces every
//! example, then verify the assembled pipeline: it must `compose` (stage-to-stage type composability
//! + derived composite metadata), its synthesized composite body must RUN each example through the
//! resolved stages, and — under `require_certified` — every stage must itself certify ("assemble only
//! from verified parts"). The result is a first-class derived composite record whose body chains the
//! stages by content-address (`fn_ref`), so the assembled whole is itself runnable/certifiable.
//!
//! **Multi-argument stages.** The threaded value feeds each stage's *first* parameter (exactly as
//! `compose` models it); a stage of arity `k` additionally consumes `k-1` values from the goal's
//! **auxiliary pool** — `args[1..]`, drawn left-to-right across the pipeline, matching `compose`'s
//! "auxiliaries gathered left to right". So the composite is `(primary, aux…) -> output`, and a
//! pipeline is accepted only when it consumes the pool *exactly* (its composite arity equals the
//! goal's). A unary pipeline is the no-auxiliary special case.
//!
//! `compose` checks a *given* order and `orchestrate` chains *given* intent tags — neither *finds*
//! the pipeline. `assemble` does the search (example-driven, breadth-first so the shortest pipeline
//! wins, pruned by execution + arity), then reuses `compose`/`certify_record`/the interpreter.

use std::collections::{HashMap, HashSet, VecDeque};

use anyhow::{anyhow, Result};
use serde_json::{json, Value as J};

use crate::compose::{compose, CompositionMetadata};

/// One stage of an assembled pipeline: the commons function chosen at that position, with its arity.
#[derive(Debug, Clone)]
pub struct Stage {
    pub hash: String,
    pub name: String,
    pub arity: usize,
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

fn record_name(record: &J) -> String {
    record.pointer("/name_hints/0").and_then(|n| n.as_str()).unwrap_or("fn").to_string()
}

/// A candidate stage: its address, body, and arity.
struct Candidate {
    hash: String,
    body: J,
    arity: usize,
}

/// Search state: the pipeline so far, each example's running (threaded) value, and how many of the
/// auxiliary pool have been consumed (the same index across all examples — the pipeline is one shape).
type State = (Vec<String>, Vec<J>, usize);

fn state_key(running: &[J], next_aux: usize) -> String {
    let vals = running.iter().map(|v| v.to_string()).collect::<Vec<_>>().join("\u{1}");
    format!("{next_aux}\u{2}{vals}")
}

/// Search the commons for a pipeline whose composition maps every example's argument list to its
/// output — threading the primary through each stage's first parameter and consuming `k-1` auxiliaries
/// per arity-`k` stage from the pool, left-to-right. Breadth-first (shortest pipeline first), deduped
/// by (running values, aux index), bounded by `max_stages` and a node cap. Returns ordered hashes.
fn search(candidates: &[Candidate], examples: &[(Vec<J>, J)], max_stages: usize) -> Option<Vec<String>> {
    let m = examples[0].0.len(); // argument arity (primary + auxiliaries); uniform across examples
    let outputs: Vec<J> = examples.iter().map(|(_, o)| o.clone()).collect();
    let init: Vec<J> = examples.iter().map(|(args, _)| args[0].clone()).collect();

    // Identity: a 1-argument goal whose primary already equals its output (no auxiliaries to consume).
    if m == 1 && init == outputs {
        return Some(vec![]);
    }

    let mut seen: HashSet<String> = HashSet::new();
    seen.insert(state_key(&init, 1));
    let mut frontier: VecDeque<State> = VecDeque::new();
    frontier.push_back((vec![], init, 1));
    let mut budget = 300_000usize;

    while let Some((pipeline, running, next_aux)) = frontier.pop_front() {
        if pipeline.len() >= max_stages {
            continue;
        }
        for c in candidates {
            if budget == 0 {
                return None;
            }
            budget -= 1;
            // Enough auxiliaries left for this stage's non-first parameters?
            if next_aux + (c.arity - 1) > m {
                continue;
            }
            let mut next_running = Vec::with_capacity(running.len());
            let mut ok = true;
            for (e, r) in running.iter().enumerate() {
                let args_of = &examples[e].0;
                let mut call = Vec::with_capacity(c.arity);
                call.push(r.clone());
                for j in 0..(c.arity - 1) {
                    call.push(args_of[next_aux + j].clone());
                }
                match crate::eval_body(&c.body, &call) {
                    // A fully-applied stage yields a value; a partial application encodes as
                    // `{kind:"function"}` (shouldn't happen — we pass the exact arity) — reject.
                    Ok(res) if res.get("kind").is_some() && res.get("kind") != Some(&json!("function")) => {
                        next_running.push(res)
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
            let new_aux = next_aux + (c.arity - 1);
            // Accept only when the pool is consumed exactly (composite arity == goal arity).
            if next_running == outputs && new_aux == m {
                let mut pl = pipeline.clone();
                pl.push(c.hash.clone());
                return Some(pl);
            }
            let key = state_key(&next_running, new_aux);
            if seen.insert(key) {
                let mut pl = pipeline.clone();
                pl.push(c.hash.clone());
                frontier.push_back((pl, next_running, new_aux));
            }
        }
    }
    None
}

/// Synthesize the composite body — a lambda over the `m` composite parameters `x0` (primary) `x1…`
/// (auxiliaries), applying each stage by `fn_ref` to the running value plus its share of the pool.
fn composite_body(stages: &[(String, usize)], m: usize) -> J {
    let params: Vec<J> = (0..m).map(|i| json!({ "name": format!("x{i}") })).collect();
    let mut running = json!({ "kind": "var", "name": "x0" });
    let mut aux = 1usize;
    for (h, k) in stages {
        let mut args = vec![running];
        for j in 0..(k - 1) {
            args.push(json!({ "kind": "var", "name": format!("x{}", aux + j) }));
        }
        aux += k - 1;
        running = json!({ "kind": "app",
            "fn": { "kind": "lit", "value": { "kind": "fn_ref", "target": h } }, "args": args });
    }
    json!({ "kind": "lambda", "params": params, "body": running })
}

/// Assemble a pipeline from the commons that satisfies `examples`, then verify it. Each example is
/// `(argument_list, output)` — `argument_list[0]` is the primary (threaded) input, the rest the
/// auxiliary pool. All examples must share one argument arity. `Ok(None)` if no pipeline within
/// `max_stages` fits.
pub fn assemble(
    records: &HashMap<String, J>,
    bodies: &HashMap<String, J>,
    examples: &[(Vec<J>, J)],
    max_stages: usize,
    require_certified: bool,
    solver: &str,
) -> Result<Option<Assembled>> {
    if examples.is_empty() {
        return Err(anyhow!("a goal needs at least one example"));
    }
    let m = examples[0].0.len();
    if m == 0 {
        return Err(anyhow!("each example needs at least one argument (the primary input)"));
    }
    if examples.iter().any(|(a, _)| a.len() != m) {
        return Err(anyhow!("every example must have the same number of arguments (composite arity)"));
    }

    // The resolver lets a candidate whose own body uses `fn_ref` execute during the search, and lets
    // the synthesized composite body run stage-to-stage.
    crate::set_resolver(bodies.clone());

    let mut candidates: Vec<Candidate> = Vec::new();
    for (hash, rec) in records {
        if let Some(k) = arity(rec) {
            if k >= 1 && k <= m {
                if let Some(bh) = rec.pointer("/body_hash").and_then(|b| b.as_str()) {
                    if let Some(body) = bodies.get(bh).or_else(|| bodies.get(hash)) {
                        candidates.push(Candidate { hash: hash.clone(), body: body.clone(), arity: k });
                    }
                }
            }
        }
    }
    candidates.sort_by(|a, b| a.hash.cmp(&b.hash));

    let found = search(&candidates, examples, max_stages);
    let Some(stage_hashes) = found else {
        crate::clear_resolver();
        return Ok(None);
    };

    let stages: Vec<Stage> = stage_hashes
        .iter()
        .map(|h| Stage { hash: h.clone(), name: record_name(&records[h]), arity: arity(&records[h]).unwrap_or(1) })
        .collect();

    // Verify (1): the pipeline composes (stage-to-stage type composability + composite metadata).
    let stage_records: Vec<J> = stage_hashes.iter().map(|h| records[h].clone()).collect();
    let composite = if stage_records.is_empty() {
        identity_metadata()
    } else {
        let c = compose(&stage_records);
        if !c.composable {
            crate::clear_resolver();
            return Err(anyhow!("assembled pipeline does not compose: {}", c.reason));
        }
        c
    };

    // Verify (2): the synthesized composite body runs every example through the resolved stages.
    let arities: Vec<(String, usize)> = stages.iter().map(|s| (s.hash.clone(), s.arity)).collect();
    let body = composite_body(&arities, m);
    let mut examples_verified = 0;
    for (args, output) in examples {
        let got = crate::eval_body(&body, args)?;
        if &got != output {
            crate::clear_resolver();
            return Err(anyhow!("composite body disagrees with an example: got {got} want {output}"));
        }
        examples_verified += 1;
    }

    // Verify (3, optional): every stage certifies — assemble only from verified parts.
    let certified = if require_certified {
        stage_hashes.iter().all(|h| {
            let rec = &records[h];
            match rec.pointer("/body_hash").and_then(|b| b.as_str()).and_then(|bh| bodies.get(bh)) {
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

/// Composite metadata for the empty (identity) pipeline: `a -> a`, pure, always, O(1).
fn identity_metadata() -> CompositionMetadata {
    CompositionMetadata {
        composable: true,
        reason: "the identity pipeline".into(),
        input_type: Some(json!({ "kind": "var", "name": "a" })),
        output_type: Some(json!({ "kind": "var", "name": "a" })),
        extra_input_types: vec![],
        effects: vec![],
        capabilities: vec![],
        terminates: "always".into(),
        complexity: "O(1)".into(),
        complexity_basis: "identity".into(),
    }
}

/// Build the derived composite function record (its body_hash addresses the synthesized composite body).
fn build_composite_record(
    stages: &[Stage],
    composite: &CompositionMetadata,
    examples: &[(Vec<J>, J)],
    body: &J,
) -> Result<J> {
    let name = if stages.is_empty() {
        "assembled_identity".to_string()
    } else {
        let joined = stages.iter().map(|s| s.name.as_str()).collect::<Vec<_>>().join("_then_");
        let mut n: String = joined.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
        if !n.chars().next().map(|c| c.is_ascii_lowercase() || c == '_').unwrap_or(false) {
            n = format!("f_{n}");
        }
        n
    };
    // Composite type: (primary, aux…) -> output.
    let mut params = vec![composite.input_type.clone().unwrap_or(json!({ "kind": "var", "name": "a" }))];
    params.extend(composite.extra_input_types.clone());
    let ty = json!({ "kind": "fn", "params": params,
        "result": composite.output_type.clone().unwrap_or(json!({ "kind": "var", "name": "a" })) });
    let examples_j: Vec<J> = examples.iter().map(|(args, o)| json!({ "args": args, "result": o })).collect();
    let body_hash = crate::hash_artifact_with_kind(body, crate::ArtifactKind::BodyExpression)?;
    let mut record = json!({
        "schema_version": "0.2.0",
        "hash": "fn_".to_string() + &"0".repeat(64),
        "name_hints": [name],
        "signature": {
            "type": ty, "refinements": [],
            "effects": composite.effects.clone(), "capabilities": composite.capabilities.clone(),
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

    /// A `\n -> op(n, n)` unary int→int commons function.
    fn add_unary(name: &str, op: &str, records: &mut HashMap<String, J>, bodies: &mut HashMap<String, J>) {
        let body = json!({ "kind": "lambda", "params": [{ "name": "n" }], "body":
            { "kind": "app", "fn": { "kind": "var", "name": op },
              "args": [{ "kind": "var", "name": "n" }, { "kind": "var", "name": "n" }] } });
        insert(name, &json!({ "kind": "fn", "params": [{ "kind": "builtin", "name": "int" }],
            "result": { "kind": "builtin", "name": "int" } }), body, records, bodies);
    }

    /// A `\a b -> op(a, b)` binary int→int→int commons function.
    fn add_binary(name: &str, op: &str, records: &mut HashMap<String, J>, bodies: &mut HashMap<String, J>) {
        let body = json!({ "kind": "lambda", "params": [{ "name": "a" }, { "name": "b" }], "body":
            { "kind": "app", "fn": { "kind": "var", "name": op },
              "args": [{ "kind": "var", "name": "a" }, { "kind": "var", "name": "b" }] } });
        insert(name, &json!({ "kind": "fn", "params": [{ "kind": "builtin", "name": "int" }, { "kind": "builtin", "name": "int" }],
            "result": { "kind": "builtin", "name": "int" } }), body, records, bodies);
    }

    fn insert(name: &str, ty: &J, body: J, records: &mut HashMap<String, J>, bodies: &mut HashMap<String, J>) {
        let bh = crate::hash_artifact_with_kind(&body, crate::ArtifactKind::BodyExpression).unwrap();
        let mut rec = json!({
            "schema_version": "0.2.0", "hash": "fn_".to_string() + &"0".repeat(64), "name_hints": [name],
            "signature": { "type": ty, "refinements": [], "effects": [], "capabilities": [], "terminates": "always" },
            "examples": [{ "args": [int(2)], "result": int(2) }],
            "intent_tags": [], "derived_from": J::Null, "supersedes": J::Null, "body_hash": bh });
        let h = crate::hash_artifact_with_kind(&rec, crate::ArtifactKind::FunctionRecord).unwrap();
        rec["hash"] = json!(h.clone());
        records.insert(h.clone(), rec);
        bodies.insert(bh, body.clone());
        bodies.insert(h, body);
    }

    #[test]
    fn assembles_a_unary_pipeline() {
        let (mut r, mut b) = (HashMap::new(), HashMap::new());
        add_unary("double", "add", &mut r, &mut b);
        add_unary("square", "mul", &mut r, &mut b);
        // 3->36, 2->16 pins `double` then `square`. One-argument goal (no auxiliaries).
        let ex = vec![(vec![int(3)], int(36)), (vec![int(2)], int(16))];
        let a = assemble(&r, &b, &ex, 3, false, "z3").unwrap().expect("a pipeline");
        assert_eq!(a.stages.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["double", "square"]);
        assert_eq!(a.examples_verified, 2);
        // Identity goal.
        assert!(assemble(&r, &b, &[(vec![int(5)], int(5))], 3, false, "z3").unwrap().unwrap().stages.is_empty());
        // Unreachable.
        assert!(assemble(&r, &b, &[(vec![int(3)], int(7))], 3, false, "z3").unwrap().is_none());
    }

    #[test]
    fn assembles_a_multi_argument_pipeline() {
        let (mut r, mut b) = (HashMap::new(), HashMap::new());
        add_unary("double", "add", &mut r, &mut b);   // \n -> n + n
        add_binary("plus", "add", &mut r, &mut b);    // \a b -> a + b
        add_binary("times", "mul", &mut r, &mut b);   // \a b -> a * b
        // Goal: (primary, aux) -> output, args [x, k]. double(x) then plus(_, k):
        //   (3, 10) -> double 3 = 6, + 10 = 16 ; (5, 1) -> 10 + 1 = 11. Pipeline [double, plus].
        let ex = vec![(vec![int(3), int(10)], int(16)), (vec![int(5), int(1)], int(11))];
        let a = assemble(&r, &b, &ex, 3, false, "z3").unwrap().expect("a multi-arg pipeline");
        assert_eq!(a.stages.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["double", "plus"]);
        assert_eq!(a.stages.iter().map(|s| s.arity).collect::<Vec<_>>(), [1, 2]);
        assert_eq!(a.examples_verified, 2);
        // The composite is (int, int) -> int — one auxiliary input gathered from `plus`.
        assert_eq!(a.composite.extra_input_types.len(), 1);
        // The composite record carries a 2-parameter type and 2-arg examples.
        assert_eq!(a.composite_record["signature"]["type"]["params"].as_array().unwrap().len(), 2);
        assert_eq!(a.composite_record["examples"][0]["args"].as_array().unwrap().len(), 2);

        // A single binary stage also solves a 2-arg goal: (6, 2) -> 12 via `times`.
        let ex2 = vec![(vec![int(6), int(2)], int(12)), (vec![int(3), int(4)], int(12))];
        let a2 = assemble(&r, &b, &ex2, 3, false, "z3").unwrap().expect("a one-binary pipeline");
        assert_eq!(a2.stages.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["times"]);
    }

    #[test]
    fn require_certified_gates_stages() {
        let (mut r, mut b) = (HashMap::new(), HashMap::new());
        add_unary("double", "add", &mut r, &mut b);
        add_unary("square", "mul", &mut r, &mut b);
        let ex = vec![(vec![int(3)], int(36)), (vec![int(2)], int(16))];
        let a = assemble(&r, &b, &ex, 3, true, "z3").unwrap().expect("certified");
        assert!(a.certified);
    }
}
