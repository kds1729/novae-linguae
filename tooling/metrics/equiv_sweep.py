#!/usr/bin/env python3
"""Cluster-then-assert sweep: publish proved `equivalent` claims over a live node's functions.

The commons-maintenance move for semantic equivalence (the cert_sweep/cost_sweep precedent): walk
every function record, bucket by canonical NORMAL FORM (`nl-validator normalize --hash` — equal
normal forms are a solver-free equivalence proof), and for every class with more than one member
publish signed `equivalent` claims (`assert-equivalent --publish`, which re-proves before signing)
in a star around the class's first member — enough for any consumer's union-find to reconstruct
the class, e.g. the node's `?collapse=equivalent` view or the agent loop's `collapse` step.

Deliberately NF-tier only: the inductive prover can decide equivalences normalization cannot, but
a blind pairwise solver sweep pays a timeout for every NON-equivalent same-shape pair — that tier
stays per-pair, on demand (`assert-equivalent --f … --g …`). Idempotent: pairs already claimed on
the node (per `GET /v0/records/{hash}/equivalences`) are skipped.

    python3 equiv_sweep.py --node https://nl.1105software.com [--seed …] [--dry-run]
"""

import argparse
import collections
import json
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path

VALIDATOR = str(Path(__file__).resolve().parents[1] / "validator" / "target" / "release" / "nl-validator")


def get_json(node, path):
    """GET with edge-rate-limit awareness: the reference node's Caddy answers 429 per client IP,
    so a bulk walk backs off (honoring Retry-After when present) instead of shedding work."""
    for attempt in range(6):
        try:
            with urllib.request.urlopen(urllib.request.Request(node + path), timeout=120) as r:
                return json.load(r)
        except urllib.error.HTTPError as exc:
            if exc.code != 429 or attempt == 5:
                raise
            retry_after = exc.headers.get("Retry-After")
            time.sleep(float(retry_after) if retry_after else 2.0 * (attempt + 1))
    raise RuntimeError("unreachable")


def post_json(node, path, payload):
    req = urllib.request.Request(node + path, data=json.dumps(payload).encode(),
                                 headers={"content-type": "application/json"}, method="POST")
    with urllib.request.urlopen(req, timeout=120) as r:
        return json.load(r)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--node", required=True)
    ap.add_argument("--seed", default="novae-linguae-example-certifier",
                    help="signing identity for the published claims")
    ap.add_argument("--dry-run", action="store_true", help="report classes; publish nothing")
    args = ap.parse_args()
    node = args.node.rstrip("/")

    # 1. Every function record's (hash, body_hash) — the terminates filter enumerates functions.
    summaries, cursor = [], None
    while True:
        flt = {"terminates": ["always", "conditional", "unknown"], "limit": 1000}
        if cursor:
            flt["cursor"] = cursor
        got = post_json(node, "/v0/query?include=summary", flt)
        summaries += got["results"]
        cursor = got.get("cursor")
        if got.get("complete") or not got["results"]:
            break
    with_body = [(s["hash"], s["body_hash"]) for s in summaries if s.get("body_hash")]
    print(f"functions: {len(summaries)} ({len(with_body)} with a resolvable body)")

    # 2. Normal-form address per distinct body (fetched once, normalized once).
    tmp = Path(tempfile.mkdtemp(prefix="nl-equiv-sweep-"))
    nf_of_body = {}
    for _, bh in with_body:
        if bh in nf_of_body:
            continue
        try:
            body = get_json(node, f"/v0/records/{bh}")
        except Exception as exc:
            nf_of_body[bh] = None
            print(f"  ! {bh[:24]}… unfetchable ({exc}); skipped", file=sys.stderr)
            continue
        p = tmp / f"{bh[:16]}.json"
        p.write_text(json.dumps(body))
        r = subprocess.run([VALIDATOR, "normalize", "--body", str(p), "--hash"],
                           capture_output=True, text=True)
        nf_of_body[bh] = r.stdout.strip() if r.returncode == 0 else None
        if r.returncode != 0:
            print(f"  ! {bh[:24]}… does not normalize: {(r.stderr or '').strip()}", file=sys.stderr)

    # 3. Classes: same normal form = same behavior (solver-free proof).
    classes = collections.defaultdict(list)
    for fn, bh in with_body:
        nf = nf_of_body.get(bh)
        if nf:
            classes[nf].append(fn)
    multi = {nf: sorted(set(fns)) for nf, fns in classes.items() if len(set(fns)) > 1}
    print(f"normal-form classes: {len(classes)}; with >1 member: {len(multi)}")

    # 4. Star-publish each class (skipping already-claimed pairs).
    published = skipped = failed = 0
    for nf, members in sorted(multi.items()):
        rep, rest = members[0], members[1:]
        print(f"class {nf[:24]}…  {len(members)} member(s): {', '.join(m[:20] + '…' for m in members)}")
        try:
            existing = get_json(node, f"/v0/records/{rep}/equivalences")["equivalences"]
            claimed = {tuple(sorted((e["body"]["claim"]["a"], e["body"]["claim"]["b"])))
                       for e in existing}
        except Exception:
            claimed = set()
        for other in rest:
            if tuple(sorted((rep, other))) in claimed:
                skipped += 1
                continue
            if args.dry_run:
                print(f"  dry-run: would assert {rep[:20]}… ≡ {other[:20]}…")
                continue
            time.sleep(0.5)  # pace the per-pair crawls under the edge rate limit
            r = subprocess.run(
                [VALIDATOR, "assert-equivalent", "--f", rep, "--g", other,
                 "--node", node, "--seed", args.seed, "--publish",
                 "--out", str(tmp / "assert.json")],
                capture_output=True, text=True)
            if r.returncode == 0:
                published += 1
                line = next((ln for ln in r.stdout.splitlines() if ln.startswith("published")), "")
                print(f"  {line or 'published'}")
            else:
                failed += 1
                print(f"  ! assert failed: {(r.stdout or r.stderr).strip().splitlines()[-1]}",
                      file=sys.stderr)
    print(f"\npublished {published} claim(s), skipped {skipped} already-claimed, {failed} failed")


if __name__ == "__main__":
    main()
