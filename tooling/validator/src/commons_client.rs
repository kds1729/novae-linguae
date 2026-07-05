//! Remote-commons client — the agent loop over a **live node** (spec/commons.md).
//!
//! Until now every "end-to-end" flow (`run --records`, `orchestrate`) resolved records and bodies
//! from a *local directory*; the deployed commons was a store no loop actually used. This module
//! closes that gap: discovery (`POST /v0/query`), resolution (`GET /v0/records/{hash}`), and
//! publication (`POST /v0/records`) against a node, materializing the same `record_map`/`link_map`
//! shapes every existing code path already consumes — so `orchestrate --node <url>` is the
//! *unchanged* agent loop, just fed from the network.
//!
//! The store is **untrusted infrastructure** (principle 7): every fetched artifact is re-hashed
//! locally and must equal the address it was requested by, so a lying or corrupted node can only
//! *fail* a run, never spoof a function into it. Verification is re-execution — the trust model
//! does not care that the bytes came over HTTP.

use crate::{hash_artifact_with_kind, ArtifactKind};
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value as J};
use std::collections::{HashMap, HashSet, VecDeque};

/// Cap on how many artifacts one closure walk may fetch — a malformed (or malicious) reference
/// graph can't turn discovery into an unbounded crawl.
const MAX_FETCHES: usize = 64;

fn kind_for_address(addr: &str) -> Result<ArtifactKind> {
    Ok(match addr.split('_').next().unwrap_or("") {
        "fn" => ArtifactKind::FunctionRecord,
        "expr" => ArtifactKind::BodyExpression,
        "msg" => ArtifactKind::Message,
        "cert" => ArtifactKind::Certification,
        other => bail!("unknown content-address prefix `{other}` in {addr}"),
    })
}

/// Fetch one artifact by content-address and **verify it locally**: the recomputed canonical hash
/// must equal the requested address, else the node lied (or corrupted) and we bail.
pub fn fetch_artifact(node: &str, addr: &str) -> Result<J> {
    let url = format!("{}/v0/records/{}", node.trim_end_matches('/'), addr);
    let text = crate::interp::http_request("GET", &url, None)?;
    let v: J = serde_json::from_str(&text)
        .map_err(|e| anyhow!("node returned non-JSON for {addr}: {e}"))?;
    if let Some(err) = v.get("error").and_then(|e| e.as_str()) {
        bail!("node error for {addr}: {err}");
    }
    let recomputed = hash_artifact_with_kind(&v, kind_for_address(addr)?)?;
    if recomputed != addr {
        bail!("hash mismatch from node for {addr}: content hashes to {recomputed} — refusing the artifact (the store is untrusted)");
    }
    Ok(v)
}

/// Typed discovery: `POST /v0/query` with an intent-tag filter; returns matched `fn_…` addresses.
pub fn query_intent(node: &str, intent: &str, limit: usize) -> Result<Vec<String>> {
    let url = format!("{}/v0/query", node.trim_end_matches('/'));
    let filter = json!({ "intent_tags": { "any": [intent] }, "limit": limit }).to_string();
    let text = crate::interp::http_request("POST", &url, Some(&filter))?;
    let v: J = serde_json::from_str(&text).map_err(|e| anyhow!("query response not JSON: {e}"))?;
    if let Some(err) = v.get("error").and_then(|e| e.as_str()) {
        bail!("node rejected the query: {err}");
    }
    Ok(v.get("results")
        .and_then(|r| r.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default())
}

/// Publish an artifact (`POST /v0/records` — the node re-verifies on ingest, so publication is
/// through the same gate as everything else). Returns the node's response for reporting.
pub fn publish_artifact(node: &str, artifact: &J) -> Result<J> {
    let url = format!("{}/v0/records", node.trim_end_matches('/'));
    let text = crate::interp::http_request("POST", &url, Some(&artifact.to_string()))?;
    let v: J = serde_json::from_str(&text).map_err(|e| anyhow!("publish response not JSON: {e}"))?;
    if let Some(err) = v.get("error").and_then(|e| e.as_str()) {
        bail!("node rejected the publish: {err} {}", v.get("detail").and_then(|d| d.as_str()).unwrap_or(""));
    }
    Ok(v)
}

/// Every `fn_…`/`expr_…` content-address mentioned anywhere in an artifact (body hashes, `fn_ref`
/// targets in examples and bodies) — the edges of the reference graph the closure walk follows.
fn referenced_addresses(v: &J, out: &mut Vec<String>) {
    match v {
        J::String(s) => {
            let looks = (s.starts_with("fn_") || s.starts_with("expr_"))
                && s.len() > 5
                && s[s.find('_').unwrap() + 1..].chars().all(|c| c.is_ascii_hexdigit());
            if looks {
                out.push(s.clone());
            }
        }
        J::Array(items) => items.iter().for_each(|x| referenced_addresses(x, out)),
        J::Object(m) => m.values().for_each(|x| referenced_addresses(x, out)),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_scan_finds_fn_and_expr_addresses_only() {
        let rec = json!({
            "hash": "fn_".to_owned() + &"a".repeat(64),
            "body_hash": "expr_".to_owned() + &"b".repeat(64),
            "examples": [{ "args": [{ "kind": "fn_ref", "target": "fn_".to_owned() + &"c".repeat(64) }] }],
            "notes": ["msg_".to_owned() + &"d".repeat(64), "fn_not_a_hash", "plain"]
        });
        let mut refs = Vec::new();
        referenced_addresses(&rec, &mut refs);
        assert!(refs.contains(&("fn_".to_owned() + &"a".repeat(64))));
        assert!(refs.contains(&("expr_".to_owned() + &"b".repeat(64))));
        assert!(refs.contains(&("fn_".to_owned() + &"c".repeat(64))));
        assert!(!refs.iter().any(|r| r.starts_with("msg_")), "messages aren't in the runnable closure scan");
        assert!(!refs.contains(&"fn_not_a_hash".to_string()), "non-hex suffixes are not addresses");
    }

    #[test]
    fn address_prefixes_map_to_kinds() {
        assert!(matches!(kind_for_address("fn_ab").unwrap(), ArtifactKind::FunctionRecord));
        assert!(matches!(kind_for_address("expr_ab").unwrap(), ArtifactKind::BodyExpression));
        assert!(kind_for_address("weird_ab").is_err());
    }
}

/// Materialize the `(record_map, link_map)` pair every existing loop consumes, from a node, by
/// walking the reference closure of `seeds` (records pull their bodies; `fn_ref`s pull their
/// helper records; bounded by [`MAX_FETCHES`]). Each fetched artifact is hash-verified. Mirrors
/// [`crate::build_record_map`] / [`crate::build_link_map`] for the remote case: a record's body is
/// indexed under both its `expr_…` address and the record's own `fn_…` address.
pub fn maps_from_node(node: &str, seeds: &[String]) -> Result<(HashMap<String, J>, HashMap<String, J>)> {
    let mut record_map: HashMap<String, J> = HashMap::new();
    let mut link_map: HashMap<String, J> = HashMap::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<String> = seeds.iter().cloned().collect();
    let mut fetched = 0usize;
    // record fn_ -> its body_hash, so the body (whenever fetched) is also indexed under the fn_.
    let mut body_of: HashMap<String, String> = HashMap::new();

    while let Some(addr) = queue.pop_front() {
        if !seen.insert(addr.clone()) {
            continue;
        }
        if !(addr.starts_with("fn_") || addr.starts_with("expr_")) {
            continue; // messages/certs aren't part of the runnable closure
        }
        if fetched >= MAX_FETCHES {
            bail!("remote closure exceeded {MAX_FETCHES} artifacts — refusing an unbounded crawl");
        }
        fetched += 1;
        let art = fetch_artifact(node, &addr)?;
        let mut refs = Vec::new();
        referenced_addresses(&art, &mut refs);
        if addr.starts_with("fn_") {
            if let Some(bh) = art.get("body_hash").and_then(|b| b.as_str()) {
                body_of.insert(addr.clone(), bh.to_string());
            }
            record_map.insert(addr.clone(), art);
        } else {
            link_map.insert(addr.clone(), art);
        }
        for r in refs {
            if !seen.contains(&r) {
                queue.push_back(r);
            }
        }
    }
    // Alias each record's fn_ address to its (fetched) body, exactly as build_link_map does.
    for (fn_addr, expr_addr) in &body_of {
        if let Some(b) = link_map.get(expr_addr) {
            link_map.insert(fn_addr.clone(), b.clone());
        }
    }
    Ok((record_map, link_map))
}
