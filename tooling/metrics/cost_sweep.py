#!/usr/bin/env python3
"""Give a node's PURE function records the v0.3 `cost` metadata — measured, verified, superseding.

The composition metric (REPORT.md §4) found 400/400 type-plausible pairs compose with sound
COARSE metadata but 0 with PRECISE complexity — no production record carried `cost` (they
predate v0.3). This sweep closes that the verified-by-default way: for each v0.2 record that is
pure (declared effects `[]`), body-hosted, not already costed, and not already superseded, it

  1. INFERS the time class from `nl-validator check-complexity`'s structural analysis
     (the sound bound the checker itself reports — never a guess),
  2. finds the TIGHTEST `output_size` the checker verifies (constant -> bounded -> preserving ->
     quadratic -> cubic; a record whose output class the analysis cannot establish is SKIPPED —
     no false precision, `unknown` would defeat the point of the field),
  3. publishes a SUPERSEDING record carrying the verified `cost` plus its signed certification
     through the node's verify-then-store gate.

Effectful records are out of scope on purpose: their time is dominated by the effect, and the
structural analysis calls their `http`/`fs` cores opaque — an honest boundary, not a gap.

    python3 cost_sweep.py --node https://nl.1105software.com --seed <certifier-seed> [--dry-run]
"""
import argparse
import json
import os
import re
import subprocess
import sys
import tempfile

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from cert_sweep import (_DEFAULT_VALIDATOR, closure_into, get_json,  # noqa: E402
                        hash_of, post_query, post_record)

_OUTPUT_SIZES = ["constant", "bounded", "preserving", "quadratic", "cubic"]
_INFERRED = re.compile(r"sound structural bound is (O\([^)]+\))")


def check_cost(validator, record_path, body_path):
    r = subprocess.run([validator, "check-complexity", record_path, "--body", body_path],
                       capture_output=True, text=True)
    return r.stdout


def verified_cost(validator, rec, body_path, td):
    """The tightest (time, output_size) the checker VERIFIES for this record, or None."""
    probe = dict(rec)
    sig = dict(rec["signature"])
    sig.pop("cost", None)
    probe["signature"] = sig
    rp = os.path.join(td, ".probe.json")
    json.dump(probe, open(rp, "w"))
    m = _INFERRED.search(check_cost(validator, rp, body_path))
    if not m:
        return None  # opaque/higher-order/unknown time — nothing precise to declare
    time_class = m.group(1)
    for out_size in _OUTPUT_SIZES:
        sig["cost"] = {"time": time_class, "output_size": out_size, "measure": "size"}
        json.dump(probe, open(rp, "w"))
        out = check_cost(validator, rp, body_path)
        lines = [l for l in out.splitlines() if "cost." in l]
        if lines and all(l.startswith(("SOUND", "VERIFIED")) for l in lines):
            return sig["cost"]
    return None


def main():
    ap = argparse.ArgumentParser(description="Verified cost metadata for a node's pure records.")
    ap.add_argument("--node", default="https://nl.1105software.com")
    ap.add_argument("--validator", default=_DEFAULT_VALIDATOR)
    ap.add_argument("--seed", required=True, help="certifier signing seed")
    ap.add_argument("--dry-run", action="store_true", help="measure + verify, publish nothing")
    args = ap.parse_args()

    fns, cursor = [], None
    while True:
        body = {"kind": "function-record", "limit": 200}
        if cursor:
            body["cursor"] = cursor
        resp = post_query(args.node, body, include="summary")
        fns.extend(resp["results"])
        cursor = resp.get("cursor")
        if not cursor or resp.get("complete", True) or not resp["results"]:
            break

    costed = published = skipped = superseded_skip = 0
    refusals = []
    # First pass: every record another record supersedes is a PREDECESSOR — costing it would
    # fork history; the chain head is the one to cost.
    superseded = set()
    summaries = []
    for s in fns:
        rec = get_json(args.node, f"/v0/records/{s['hash']}")
        summaries.append(rec)
        if isinstance(rec.get("supersedes"), str):
            superseded.add(rec["supersedes"])

    for rec in summaries:
        addr = rec["hash"]
        if rec.get("schema_version") != "0.2.0" or not isinstance(rec.get("signature"), dict):
            skipped += 1
            continue
        if rec["signature"].get("effects"):
            skipped += 1
            continue
        if rec["signature"].get("cost"):
            skipped += 1  # already costed
            continue
        if addr in superseded:
            superseded_skip += 1
            continue
        with tempfile.TemporaryDirectory(prefix="nl-cost-") as td:
            try:
                closure_into(args.node, args.validator, addr, td)
            except Exception as e:
                refusals.append((addr, f"closure: {e}"))
                continue
            body_path = os.path.join(td, f"{rec['body_hash']}.json")
            if not os.path.exists(body_path):
                refusals.append((addr, "body not hosted"))
                continue
            cost = verified_cost(args.validator, rec, body_path, td)
            if cost is None:
                refusals.append((addr, "no verifiable precise cost (opaque/higher-order time "
                                       "or unestablished output class)"))
                continue
            new = dict(rec)
            sig = dict(rec["signature"])
            sig["cost"] = cost
            new["signature"] = sig
            new["supersedes"] = addr
            new.pop("hash", None)
            np = os.path.join(td, "new.json")
            json.dump(new, open(np, "w"))
            new_hash = hash_of(args.validator, np)
            new["hash"] = new_hash
            json.dump(new, open(np, "w"))
            costed += 1
            name = (rec.get("name_hints") or ["?"])[0]
            print(f"cost  {name:28} {cost['time']:12} {cost['output_size']:10} "
                  f"{addr[:16]}… -> {new_hash[:16]}…")
            if args.dry_run:
                continue
            cert = subprocess.run(
                [args.validator, "certify", np, "--body", body_path, "--records", td,
                 "--sign", args.seed], capture_output=True, text=True)
            if not cert.stdout.strip():
                refusals.append((new_hash, "certify produced no certificate"))
                continue
            cp = os.path.join(td, "cert.json")
            open(cp, "w").write(cert.stdout)
            post_record(args.node, np)
            post_record(args.node, cp)
            published += 1

    print(f"\ncosted={costed} published={published} skipped={skipped} "
          f"(non-v0.2/effectful/already-costed) predecessors={superseded_skip} "
          f"refused={len(refusals)}")
    for addr, why in refusals[:20]:
        print(f"  refused {addr[:20]}…: {why}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
