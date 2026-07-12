#!/usr/bin/env python3
"""Measure the efficiency headline against a LIVE commons node (default: Arca).

The project's core claim — assemble-from-the-commons beats write-from-scratch on context
cost, and verification is checked, not re-derived — has never been a *number*. This tool
produces the numbers, deterministically, with no model API calls:

  1. discovery-cost   tokens to discover by intent: hashes vs summary projection vs full
                      records vs a token_budget-capped summary (production round-trips)
  2. assembly-cost    the GW15 pagination chain: what an assembling agent holds in context
                      (decision summary + address) vs what a from-scratch author holds
                      (every body's surface source + signatures), plus the runtime-closure
                      size for honesty
  3. verification     certificate tokens vs the artifact tokens the certificate spares a
                      consumer from re-deriving (body + examples + properties), across the
                      certified population
  4. composition      of all type-compatible ordered pairs of certified unary v0.2
                      functions, how many `nl-validator compose` accepts, and how many get
                      PRECISE (cost-based) composite complexity
  5. cert-coverage    fraction of function records carrying a served certification, split
                      v0.1-ingested vs v0.2-authored

Tokens: the Qwen2.5 tokenizer when `transformers` is importable (run under the ft venv for
exact BPE counts), else a bytes/4 estimate — the report records which was used. Everything
else is stdlib + `nl-validator`.

    python3 measure_efficiency.py --node https://nl.1105software.com --out report.json
"""

import argparse
import json
import os
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request

_MIN_GAP = 0.15   # polite pacing between node requests
_LAST_REQ = [0.0]


def _open(req_or_url, timeout=60):
    """Paced urlopen with 429 backoff — the node rate-limits bursts."""
    for attempt in range(6):
        wait = _MIN_GAP - (time.monotonic() - _LAST_REQ[0])
        if wait > 0:
            time.sleep(wait)
        try:
            _LAST_REQ[0] = time.monotonic()
            return urllib.request.urlopen(req_or_url, timeout=timeout)
        except urllib.error.HTTPError as e:
            if e.code == 429 and attempt < 5:
                time.sleep(2 ** attempt)
                continue
            raise

_HERE = os.path.dirname(os.path.abspath(__file__))
_DEFAULT_VALIDATOR = os.path.normpath(
    os.path.join(_HERE, "..", "validator", "target", "release", "nl-validator"))

# ---------------------------------------------------------------- tokens

_TOKENIZER = None
_TOKENIZER_ID = "bytes/4 estimate"


def _init_tokenizer():
    global _TOKENIZER, _TOKENIZER_ID
    try:
        from transformers import AutoTokenizer  # type: ignore
        _TOKENIZER = AutoTokenizer.from_pretrained("Qwen/Qwen2.5-Coder-1.5B-Instruct")
        _TOKENIZER_ID = "Qwen/Qwen2.5-Coder-1.5B-Instruct (exact BPE)"
    except Exception:
        _TOKENIZER = None


def tokens(text: str) -> int:
    if _TOKENIZER is not None:
        return len(_TOKENIZER.encode(text))
    return max(1, len(text.encode("utf-8")) // 4)


# ---------------------------------------------------------------- node I/O

def post_query(node, body, include=None):
    url = f"{node}/v0/query" + (f"?include={include}" if include else "")
    req = urllib.request.Request(url, data=json.dumps(body).encode(),
                                 headers={"content-type": "application/json"},
                                 method="POST")
    with _open(req) as r:
        raw = r.read().decode()
    return raw, json.loads(raw)


def get_record(node, addr):
    with _open(f"{node}/v0/records/{addr}") as r:
        raw = r.read().decode()
    return raw, json.loads(raw)


def get_certs(node, addr):
    with _open(f"{node}/v0/records/{addr}/certifications") as r:
        resp = json.loads(r.read().decode())
    return resp.get("certifications", []) if isinstance(resp, dict) else resp


# ---------------------------------------------------------------- validator

def validator_run(validator, args, files=None):
    """Run nl-validator with record JSON values written to temp files."""
    paths = []
    with tempfile.TemporaryDirectory() as td:
        for i, art in enumerate(files or []):
            p = os.path.join(td, f"r{i}.json")
            json.dump(art, open(p, "w"))
            paths.append(p)
        r = subprocess.run([validator] + args + paths, capture_output=True, text=True)
        return r.returncode, r.stdout.strip(), r.stderr.strip()


def surface_of_body(validator, body_json):
    with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
        json.dump(body_json, f)
        p = f.name
    try:
        r = subprocess.run([validator, "unparse-body", p], capture_output=True, text=True)
        return r.stdout.strip() if r.returncode == 0 else json.dumps(body_json)
    finally:
        os.unlink(p)


# ---------------------------------------------------------------- metrics

def walk_all_functions(node):
    """Every function record hash on the node, with its summary."""
    out, cursor = [], None
    while True:
        body = {"kind": "function-record", "limit": 200}
        if cursor:
            body["cursor"] = cursor
        _, resp = post_query(node, body, include="summary")
        out.extend(resp["results"])
        cursor = resp.get("cursor")
        if not cursor or resp.get("complete", True) or not resp["results"]:
            break
    return out


def metric_discovery_cost(node, intents):
    rows = []
    for intent in intents:
        q = {"kind": "function-record", "intent_tags": {"any": [intent]}}
        raw_h, resp_h = post_query(node, q)
        raw_s, _ = post_query(node, q, include="summary")
        raw_r, _ = post_query(node, q, include="record")
        raw_b, resp_b = post_query(node, dict(q, token_budget=2000), include="summary")
        n = len(resp_h["results"])
        rows.append({
            "intent": intent, "matches": n,
            "hashes_tokens": tokens(raw_h),
            "summary_tokens": tokens(raw_s),
            "record_tokens": tokens(raw_r),
            "budgeted_summary_tokens": tokens(raw_b),
            "budgeted_returned": resp_b.get("returned", len(resp_b.get("results", []))),
        })
    return rows


def fn_ref_closure(node, top_addr):
    """The top record + every body in its fn_ref closure (address -> artifact)."""
    seen, todo, bodies, records = set(), [top_addr], {}, {}

    def scan(x):
        if isinstance(x, dict):
            if x.get("kind") == "fn_ref" and isinstance(x.get("target"), str):
                todo.append(x["target"])
            for v in x.values():
                scan(v)
        elif isinstance(x, list):
            for v in x:
                scan(v)

    while todo:
        addr = todo.pop()
        if addr in seen:
            continue
        seen.add(addr)
        raw, art = get_record(node, addr)
        if addr.startswith("fn_"):
            records[addr] = art
            if art.get("body_hash"):
                todo.append(art["body_hash"])
        elif addr.startswith("expr_"):
            bodies[addr] = art
            scan(art)
    return records, bodies


def metric_assembly(node, validator, top_addr, intent):
    """Assemble-vs-author on a real composed production chain."""
    records, bodies = fn_ref_closure(node, top_addr)
    top = records[top_addr]

    # what the assembling agent HOLDS: one budget-capped summary query + the top record's
    # decision summary + the address it applies (+ the served cert it checks instead of
    # re-deriving trust)
    raw_disc, _ = post_query(node, {"kind": "function-record",
                                    "intent_tags": {"any": [intent]},
                                    "token_budget": 2000}, include="summary")
    summary_fields = {k: top.get(k) for k in
                      ("name_hints", "intent_tags", "body_hash") if k in top}
    summary_fields["type"] = top.get("signature", {}).get("type")
    summary_fields["effects"] = top.get("signature", {}).get("effects")
    certs = get_certs(node, top_addr)
    assemble_tokens = (tokens(raw_disc) + tokens(json.dumps(summary_fields))
                       + tokens(top_addr) + tokens(json.dumps(certs[:1])))

    # what the from-scratch author HOLDS/produces: every body in the closure as surface
    # source + every signature
    author_tokens = 0
    for addr, rec in records.items():
        author_tokens += tokens(json.dumps(rec.get("signature", {})))
    for addr, body in bodies.items():
        author_tokens += tokens(surface_of_body(validator, body))

    closure_tokens = sum(tokens(json.dumps(a)) for a in list(records.values()) + list(bodies.values()))
    # REUSE: a later use under a cached trust verdict holds only the decision summary and
    # the address — discovery and the cert check are first-use costs. Authoring pays its
    # full cost every time the function doesn't exist yet somewhere reachable.
    reuse_tokens = tokens(json.dumps(summary_fields)) + tokens(top_addr)
    return {
        "chain_top": top_addr, "intent": intent,
        "closure_records": len(records), "closure_bodies": len(bodies),
        "assemble_context_tokens": assemble_tokens,
        "reuse_context_tokens": reuse_tokens,
        "author_context_tokens": author_tokens,
        "ratio_first_use": round(author_tokens / assemble_tokens, 2) if assemble_tokens else None,
        "ratio_reuse": round(author_tokens / reuse_tokens, 2) if reuse_tokens else None,
        "runtime_closure_tokens": closure_tokens,
    }


def metric_verification(node, fn_summaries, cap=40):
    """Certificate tokens vs the artifact tokens the certificate spares re-deriving."""
    rows, checked = [], 0
    for s in fn_summaries:
        if checked >= cap:
            break
        addr = s["hash"]
        certs = get_certs(node, addr)
        if not certs:
            continue
        raw_rec, rec = get_record(node, addr)
        body_tokens = 0
        if rec.get("body_hash"):
            try:
                raw_body, _ = get_record(node, rec["body_hash"])
                body_tokens = tokens(raw_body)
            except Exception:
                pass
        rows.append({
            "fn": addr[:16],
            "cert_tokens": tokens(json.dumps(certs[0])),
            "rederive_tokens": tokens(raw_rec) + body_tokens,
        })
        checked += 1
    if not rows:
        return {"population": 0}
    cert_t = sum(r["cert_tokens"] for r in rows)
    red_t = sum(r["rederive_tokens"] for r in rows)
    return {"population": len(rows), "cert_tokens_total": cert_t,
            "rederive_tokens_total": red_t,
            "ratio": round(red_t / cert_t, 2) if cert_t else None}


def metric_verify_compute(node, validator, fn_summaries, certified_addrs, sample=5):
    """Wall-clock asymmetry: re-running `certify` vs reading the served certificate.

    The token ratio undersells the cert — the compute it spares (typecheck, effect walk,
    refinement/termination/complexity analysis, property proof, example runs) is the real
    asymmetry. Sampled on certified records whose closure we can stage locally."""
    timings = []
    for s in fn_summaries:
        if len(timings) >= sample:
            break
        if s["hash"] not in certified_addrs:
            continue
        addr = s["hash"]
        try:
            records, bodies = fn_ref_closure(node, addr)
        except Exception:
            continue
        with tempfile.TemporaryDirectory() as td:
            for a, art in list(records.items()) + list(bodies.items()):
                json.dump(art, open(os.path.join(td, f"{a}.json"), "w"))
            # traces referenced by examples must be present for offline replay
            for rec in records.values():
                for ex in rec.get("examples", []):
                    t = ex.get("trace")
                    if t:
                        try:
                            _, art = get_record(node, t)
                            json.dump(art, open(os.path.join(td, f"{t}.json"), "w"))
                        except Exception:
                            pass
            rec_path = os.path.join(td, f"{addr}.json")
            body_hash = records[addr].get("body_hash")
            if not body_hash or body_hash not in bodies:
                continue
            body_path = os.path.join(td, f"{body_hash}.json")
            t0 = time.monotonic()
            r = subprocess.run([validator, "certify", rec_path, "--body", body_path,
                                "--records", td],
                               capture_output=True, text=True)
            dt = time.monotonic() - t0
            if r.returncode == 0:
                timings.append(dt)
    if not timings:
        return {"sample": 0}
    return {"sample": len(timings),
            "certify_seconds_mean": round(sum(timings) / len(timings), 2),
            "cert_check": "O(1) signature + hash verification (milliseconds)"}


def _param_kinds(sig_type):
    if not isinstance(sig_type, dict):
        return None
    t = sig_type
    if t.get("kind") == "forall":
        t = t.get("body", {})
    if t.get("kind") != "fn":
        return None
    return t.get("params", []), t.get("result", {})


def metric_composition(node, validator, fn_summaries, certified_addrs, cap_pairs=400):
    """Pairwise compose over certified v0.2 unary-viewable functions."""
    cands = []
    for s in fn_summaries:
        if s["hash"] not in certified_addrs or not s.get("type"):
            continue
        try:
            t = json.loads(s["type"]) if isinstance(s["type"], str) else s["type"]
        except (ValueError, TypeError):
            continue
        pr = _param_kinds(t)
        if pr is None:
            continue
        params, result = pr
        _, rec = get_record(node, s["hash"])
        cands.append({"addr": s["hash"], "params": params, "result": result, "record": rec})

    unary = [c for c in cands if len(c["params"]) == 1]
    attempted = composed = precise = 0
    for c1 in cands:
        for c2 in unary:
            if attempted >= cap_pairs:
                break
            if c1["addr"] == c2["addr"]:
                continue
            # cheap plausibility: same top-level kind, or either side polymorphic
            r, p = c1["result"], c2["params"][0]
            plausible = (r.get("kind") == "var" or p.get("kind") == "var" or r == p or
                         (r.get("kind") == p.get("kind") == "builtin" and r.get("name") == p.get("name")) or
                         (r.get("kind") == p.get("kind") == "apply"))
            if not plausible:
                continue
            attempted += 1
            rc, out, _ = validator_run(validator, ["compose"], [c1["record"], c2["record"]])
            if rc == 0:
                composed += 1
                if "cost-basis" in out and "precise" in out:
                    precise += 1
    return {"certified_typed_candidates": len(cands), "unary_stage2": len(unary),
            "plausible_pairs_attempted": attempted, "composed_ok": composed,
            "success_rate": round(composed / attempted, 3) if attempted else None,
            "precise_complexity": precise}


def metric_cert_coverage(node, fn_summaries):
    v01 = v02 = v01_cert = v02_cert = 0
    certified_addrs = set()
    for s in fn_summaries:
        structured = False
        t = s.get("type")
        if isinstance(t, str):
            try:
                structured = isinstance(json.loads(t), dict)
            except (ValueError, TypeError):
                structured = False
        elif isinstance(t, dict):
            structured = True
        has_cert = bool(get_certs(node, s["hash"]))
        if has_cert:
            certified_addrs.add(s["hash"])
        if structured:
            v02 += 1
            v02_cert += has_cert
        else:
            v01 += 1
            v01_cert += has_cert
    return {
        "functions_total": v01 + v02,
        "v01_ingested": v01, "v01_certified": v01_cert,
        "v02_authored": v02, "v02_certified": v02_cert,
        "coverage_total": round((v01_cert + v02_cert) / (v01 + v02), 3) if (v01 + v02) else None,
        "coverage_v02": round(v02_cert / v02, 3) if v02 else None,
    }, certified_addrs


# ---------------------------------------------------------------- main

def main():
    ap = argparse.ArgumentParser(description="Measure commons efficiency against a live node.")
    ap.add_argument("--node", default="https://nl.1105software.com")
    ap.add_argument("--validator", default=_DEFAULT_VALIDATOR)
    ap.add_argument("--out", default=None, help="write the full JSON report here")
    ap.add_argument("--chain", default=None,
                    help="fn_ address of a composed production chain (default: discover "
                         "fetch_pages by name)")
    ap.add_argument("--chain-intent", default="query/pages")
    ap.add_argument("--intents", nargs="*", default=[
        "io/network/http", "parse", "string", "query/pages", "arithmetic"])
    args = ap.parse_args()

    _init_tokenizer()
    report = {"node": args.node, "tokenizer": _TOKENIZER_ID}

    print(f"node        {args.node}")
    print(f"tokenizer   {_TOKENIZER_ID}")

    fn_summaries = walk_all_functions(args.node)
    print(f"functions   {len(fn_summaries)} records enumerated")

    report["discovery_cost"] = metric_discovery_cost(args.node, args.intents)
    for row in report["discovery_cost"]:
        print(f"discovery   {row['intent']:>18}  n={row['matches']:<3} "
              f"hashes={row['hashes_tokens']:<6} summary={row['summary_tokens']:<6} "
              f"full={row['record_tokens']:<7} budgeted={row['budgeted_summary_tokens']}"
              f" (returned {row['budgeted_returned']})")

    chain = args.chain
    if not chain:
        _, resp = post_query(args.node, {"kind": "function-record",
                                         "name_hint_prefix": "fetch_pages"}, include="summary")
        hits = [r for r in resp["results"] if r.get("name_hints")
                and "fetch_pages" in r["name_hints"]]
        chain = hits[0]["hash"] if hits else None
    if chain:
        report["assembly"] = metric_assembly(args.node, args.validator, chain,
                                             args.chain_intent)
        a = report["assembly"]
        print(f"assembly    chain {a['chain_top'][:16]}…  closure {a['closure_records']}fn/"
              f"{a['closure_bodies']}body  assemble={a['assemble_context_tokens']} "
              f"reuse={a['reuse_context_tokens']} author={a['author_context_tokens']}  "
              f"first-use={a['ratio_first_use']}x reuse={a['ratio_reuse']}x")

    report["cert_coverage"], certified_addrs = metric_cert_coverage(args.node, fn_summaries)
    c = report["cert_coverage"]
    print(f"coverage    total {c['coverage_total']}  v0.2 {c['coverage_v02']} "
          f"({c['v02_certified']}/{c['v02_authored']} authored; "
          f"{c['v01_certified']}/{c['v01_ingested']} ingested)")

    report["verification"] = metric_verification(args.node, [
        s for s in fn_summaries if s["hash"] in certified_addrs])
    v = report["verification"]
    if v.get("population"):
        print(f"verify      certs {v['population']}  cert={v['cert_tokens_total']} vs "
              f"re-derive={v['rederive_tokens_total']}  ratio={v['ratio']}x")

    report["verify_compute"] = metric_verify_compute(args.node, args.validator,
                                                     fn_summaries, certified_addrs)
    vc = report["verify_compute"]
    if vc.get("sample"):
        print(f"verify-cpu  certify mean {vc['certify_seconds_mean']}s over "
              f"{vc['sample']} records vs {vc['cert_check']}")

    report["composition"] = metric_composition(args.node, args.validator, fn_summaries,
                                               certified_addrs)
    m = report["composition"]
    print(f"compose     candidates {m['certified_typed_candidates']} "
          f"attempted {m['plausible_pairs_attempted']} ok {m['composed_ok']} "
          f"rate={m['success_rate']}  precise-complexity {m['precise_complexity']}")

    if args.out:
        json.dump(report, open(args.out, "w"), indent=1)
        print(f"report      {args.out}")


if __name__ == "__main__":
    main()
