#!/usr/bin/env python3
"""Certify every uncertified function record a node holds the body for, and publish the certs.

The efficiency report (measure_efficiency.py) measures certification coverage; this is the
lever that moves it. For each v0.2 function record on the node without a served
certification: fetch the record, its body, its fn_ref closure, and its examples' traces
(hash-verified — the store is untrusted), run `nl-validator certify --sign <seed>` locally,
and publish the resulting cert_… through the node's verify-then-store gate. Records that
refuse to certify are listed with the reason — honest gaps, not silent skips.

    python3 cert_sweep.py --node https://nl.1105software.com --seed <certifier-seed>
"""

import argparse
import json
import os
import subprocess
import tempfile
import time
import urllib.error
import urllib.request

_HERE = os.path.dirname(os.path.abspath(__file__))
_DEFAULT_VALIDATOR = os.path.normpath(
    os.path.join(_HERE, "..", "validator", "target", "release", "nl-validator"))

_MIN_GAP = 0.15
_LAST_REQ = [0.0]


def _open(req_or_url, timeout=60):
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
        except (urllib.error.URLError, TimeoutError, OSError):
            # Transient transport hiccups (TLS handshake timeout, reset) retry like a 429.
            if attempt < 5:
                time.sleep(2 ** attempt)
                continue
            raise


def get_json(node, path):
    with _open(f"{node}{path}") as r:
        return json.loads(r.read().decode())


def post_query(node, body, include=None):
    url = f"{node}/v0/query" + (f"?include={include}" if include else "")
    req = urllib.request.Request(url, data=json.dumps(body).encode(),
                                 headers={"content-type": "application/json"},
                                 method="POST")
    with _open(req) as r:
        return json.loads(r.read().decode())


def post_record(node, path):
    req = urllib.request.Request(f"{node}/v0/records", data=open(path, "rb").read(),
                                 headers={"content-type": "application/json"},
                                 method="POST")
    with _open(req) as r:
        return json.loads(r.read().decode())


def hash_of(validator, path):
    r = subprocess.run([validator, "hash", path], capture_output=True, text=True)
    return r.stdout.strip()


def closure_into(node, validator, top_addr, td):
    """Fetch top record + fn_ref closure + traces into td, hash-verifying each artifact."""
    seen, todo, records = set(), [top_addr], {}

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
        art = get_json(node, f"/v0/records/{addr}")
        path = os.path.join(td, f"{addr}.json")
        json.dump(art, open(path, "w"))
        if hash_of(validator, path) != addr:
            raise RuntimeError(f"node lied: {addr}")
        if addr.startswith("fn_"):
            records[addr] = art
            if art.get("body_hash"):
                todo.append(art["body_hash"])
            for ex in art.get("examples", []):
                if ex.get("trace"):
                    todo.append(ex["trace"])
        elif addr.startswith("expr_"):
            scan(art)
    return records


def main():
    ap = argparse.ArgumentParser(description="Certify a node's uncertified records from "
                                             "its own hosted bodies.")
    ap.add_argument("--node", default="https://nl.1105software.com")
    ap.add_argument("--validator", default=_DEFAULT_VALIDATOR)
    ap.add_argument("--seed", required=True, help="certifier signing seed")
    ap.add_argument("--dry-run", action="store_true", help="certify but do not publish")
    args = ap.parse_args()

    # enumerate all function records
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

    certified = refused = skipped = published = 0
    refusals = []
    for s in fns:
        addr = s["hash"]
        t = s.get("type")
        structured = False
        if isinstance(t, str):
            try:
                structured = isinstance(json.loads(t), dict)
            except (ValueError, TypeError):
                structured = False
        if not structured:
            skipped += 1          # v0.1 surface-typed: not certifiable as-is
            continue
        existing = get_json(args.node, f"/v0/records/{addr}/certifications")
        if existing.get("certifications"):
            skipped += 1
            continue
        try:
            with tempfile.TemporaryDirectory() as td:
                records = closure_into(args.node, args.validator, addr, td)
                rec = records[addr]
                body_hash = rec.get("body_hash")
                if not body_hash:
                    refused += 1
                    refusals.append((addr, "no body_hash"))
                    continue
                rec_path = os.path.join(td, f"{addr}.json")
                body_path = os.path.join(td, f"{body_hash}.json")
                if not os.path.exists(body_path):
                    refused += 1
                    refusals.append((addr, "body not hosted"))
                    continue
                r = subprocess.run([args.validator, "certify", rec_path,
                                    "--body", body_path, "--records", td,
                                    "--sign", args.seed],
                                   capture_output=True, text=True)
                if r.returncode != 0:
                    refused += 1
                    reason = (r.stdout + r.stderr).strip().splitlines()
                    refusals.append((addr, reason[-1][:120] if reason else "?"))
                    continue
                # `certify --sign` writes the cert JSON to stdout (the signed record)
                cert_path = os.path.join(td, "cert.json")
                open(cert_path, "w").write(r.stdout)
                certified += 1
                name = (s.get("name_hints") or ["?"])[0]
                if args.dry_run:
                    print(f"certified   {addr[:16]}… {name} (dry-run, not published)")
                    continue
                resp = post_record(args.node, cert_path)
                published += 1
                print(f"certified   {addr[:16]}… {name} -> {resp.get('hash', '?')[:20]}…")
        except Exception as e:
            refused += 1
            refusals.append((addr, f"error: {e}"))

    print(f"\nswept: certified={certified} published={published} "
          f"refused={refused} skipped={skipped} (already-certified or v0.1)")
    for addr, why in refusals:
        print(f"  refused  {addr[:16]}…  {why}")


if __name__ == "__main__":
    main()
