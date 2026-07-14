#!/usr/bin/env python3
"""Body-completeness sweep: find fn records whose bodies the node does NOT hold, and publish
any that exist locally.

A record's `body_hash` names its body by content-address, but publishing the record never
required publishing the body — so a consumer of an early-published record may be unable to
resolve the very artifact that makes it runnable, certifiable, or NF-classable (the 2026-07-14
solver-tier sweep measured 73 such references dark on production). This sweep closes what it can
and measures what it can't: enumerate every fn record's `body_hash`, probe which bodies the node
serves, recompute the expr address of every local `.json` candidate (BLAKE3 over JCS — the same
rule the gate re-verifies), and POST the matches through the ordinary verify-then-store gate.
The residue — bodies with no local copy — is reported per record, names included, so the gap is
a named list rather than a silent property of the store.

Expected residue, not loss: v0.1 description-tier records whose source is outside the ingestion
subset carry a SYNTHETIC body_hash — a fingerprint of the normalised source (nl-ingest-py
`_body_hash`), kept so re-ingesting the same source reproduces the same record identity — which
no stored artifact answers, by construction (function-record.schema.json). On production those
are the stdlib v0.1 tier (statistics/base64/textwrap/colorsys/json/gzip). They are why the
report names each residue record: a dark reference you can DIAGNOSE is a boundary; one you
can't is a mystery.

    python3 body_sweep.py --node https://nl.1105software.com --dir <records-dir> … [--dry-run]
"""

import argparse
import collections
import json
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "ingest-common"))
from nl_core import content_hash  # noqa: E402

SKIP_DIR_TOKENS = ("venv", "node_modules", ".git", "__pycache__", "target")


def get_json(node, path):
    """GET with edge awareness: back off on 429 (honoring Retry-After) AND retry transient
    transport errors (TLS-handshake timeouts killed prior metric walks — the cost_sweep lesson)."""
    for attempt in range(6):
        try:
            with urllib.request.urlopen(urllib.request.Request(node + path), timeout=120) as r:
                return json.load(r)
        except urllib.error.HTTPError as exc:
            if exc.code != 429 or attempt == 5:
                raise
            retry_after = exc.headers.get("Retry-After")
            time.sleep(float(retry_after) if retry_after else 2.0 * (attempt + 1))
        except urllib.error.URLError:
            if attempt == 5:
                raise
            time.sleep(2.0 * (attempt + 1))
    raise RuntimeError("unreachable")


def post_json(node, path, payload):
    req = urllib.request.Request(node + path, data=json.dumps(payload).encode(),
                                 headers={"content-type": "application/json"}, method="POST")
    with urllib.request.urlopen(req, timeout=120) as r:
        return json.load(r)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--node", required=True)
    ap.add_argument("--dir", action="append", type=Path, default=[],
                    help="local directory to scan (recursively) for body candidates; repeatable")
    ap.add_argument("--max-bytes", type=int, default=2_000_000,
                    help="skip local files larger than this (bodies are small; blobs are not)")
    ap.add_argument("--dry-run", action="store_true", help="measure and match; publish nothing")
    args = ap.parse_args()
    node = args.node.rstrip("/")

    # 1. Every fn record's body_hash, with names for honest residue reporting.
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
    holders = collections.defaultdict(list)   # body_hash -> [(fn, names)]
    for s in summaries:
        if s.get("body_hash"):
            holders[s["body_hash"]].append((s["hash"], ",".join(s.get("name_hints") or ["?"])))
    print(f"functions: {len(summaries)}; distinct referenced bodies: {len(holders)}")

    # 2. Probe which referenced bodies the node actually serves.
    missing = []
    for bh in sorted(holders):
        try:
            get_json(node, f"/v0/records/{bh}")
        except urllib.error.HTTPError as exc:
            if exc.code == 404:
                missing.append(bh)
            else:
                raise
    print(f"missing on the node: {len(missing)} body(ies), referenced by "
          f"{sum(len(holders[bh]) for bh in missing)} record(s)")

    # 3. Recompute the expr address of every local candidate (same rule as the gate).
    local = {}
    for root in args.dir:
        for p in sorted(root.rglob("*.json")):
            if any(tok in p.parts for tok in SKIP_DIR_TOKENS):
                continue
            try:
                if p.stat().st_size > args.max_bytes:
                    continue
                obj = json.loads(p.read_text())
            except (OSError, ValueError):
                continue
            if not isinstance(obj, dict):
                continue
            try:
                local.setdefault(content_hash(obj, "expr"), p)
            except Exception:
                continue
    print(f"local candidates scanned: {len(local)} distinct expr address(es) "
          f"across {len(args.dir)} dir(s)")

    # 4. Publish the matches through the gate; name the residue.
    published = failed = 0
    residue = []
    for bh in missing:
        p = local.get(bh)
        if p is None:
            residue.append(bh)
            continue
        if args.dry_run:
            print(f"dry-run: would publish {bh[:24]}… from {p}")
            continue
        time.sleep(0.3)
        try:
            resp = post_json(node, "/v0/records", json.loads(p.read_text()))
            published += 1
            print(f"published {bh[:24]}… from {p} ({resp.get('status', 'stored')})")
        except urllib.error.HTTPError as exc:
            failed += 1
            print(f"! gate refused {bh[:24]}… from {p}: {exc.read().decode()[:200]}", file=sys.stderr)
    print(f"\npublished {published}, failed {failed}, no local copy {len(residue)}")
    for bh in residue:
        for fn, names in holders[bh]:
            print(f"  residue {bh[:24]}…  <- {fn[:24]}…  ({names})")


if __name__ == "__main__":
    main()
