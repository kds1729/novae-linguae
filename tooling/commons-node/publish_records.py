#!/usr/bin/env python3
"""Publish a directory of adapter output to a commons node — bodies, traces, blobs, records, and
(optionally) freshly signed certifications — through the node's verify-then-store gate.

    python3 tooling/commons-node/publish_records.py <records-dir> [--node URL] [--sign SEED]

<records-dir> is what `nl-ingest-graphql` / `nl-ingest-openapi --out` wrote: `<name>.v0.2.json`
records, `body-<name>.json` bodies, `trace-<name>-<i>.json` traces, `blob-<sha256>.json` sidecars.
Order matters and is handled here: bodies and traces first (a record's `body_hash` and example
`trace` must resolve), then records, then certifications. Blobs (by-address example values) cannot
go through `/v0/records` — they belong in the node's gate-free blob store (`manage.py addblob` on
the node); this script lists them and tells you.

The node answers `201 {"hash","stored":true}` for a new artifact and `200 {"hash","stored":false}`
for one it already holds (publishing is idempotent — the address IS the identity); a rejection is a
4xx with an `error`. Stdlib only; needs `nl-validator` (`NL_VALIDATOR` or the sibling build) only
for `--sign`.
"""
import argparse
import glob
import json
import os
import subprocess
import sys
import urllib.error
import urllib.request

_HERE = os.path.dirname(os.path.abspath(__file__))
_VALIDATOR = os.environ.get("NL_VALIDATOR") or os.path.normpath(
    os.path.join(_HERE, "..", "validator", "target", "release", "nl-validator"))


def post(node, obj):
    req = urllib.request.Request(node.rstrip("/") + "/v0/records", data=json.dumps(obj).encode(),
                                 headers={"content-type": "application/json"}, method="POST")
    try:
        with urllib.request.urlopen(req, timeout=120) as r:
            body = json.loads(r.read() or b"{}")
            return r.status, body
    except urllib.error.HTTPError as e:
        try:
            return e.code, json.loads(e.read() or b"{}")
        except ValueError:
            return e.code, {"error": "non-JSON error body"}


def label(st, body):
    if st == 201:
        return "stored"
    if st == 200 and body.get("stored") is False:
        return "already present"
    return f"REJECTED {st}: {body.get('error', body)}"


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("records_dir")
    ap.add_argument("--node", default="https://nl.1105software.com")
    ap.add_argument("--sign", metavar="SEED", default=None,
                    help="also certify each record (nl-validator certify --sign SEED) and publish the certification")
    a = ap.parse_args(argv)
    d = a.records_dir
    names = sorted(os.path.basename(p)[:-len(".v0.2.json")] for p in glob.glob(os.path.join(d, "*.v0.2.json")))
    if not names:
        sys.exit(f"{d}: no *.v0.2.json records")
    blobs = sorted(glob.glob(os.path.join(d, "blob-*.json")))
    failures = 0
    for p in sorted(glob.glob(os.path.join(d, "body-*.json"))) + sorted(glob.glob(os.path.join(d, "trace-*.json"))):
        st, body = post(a.node, json.load(open(p)))
        print(f"{os.path.basename(p):40} {label(st, body)}  {body.get('hash', '')[:20]}")
        failures += st >= 400
    for n in names:
        rp = os.path.join(d, f"{n}.v0.2.json")
        rec = json.load(open(rp))
        st, body = post(a.node, rec)
        print(f"{n + ' (record)':40} {label(st, body)}  {rec['hash'][:20]}")
        failures += st >= 400
        if a.sign and st < 400:
            bp = os.path.join(d, f"body-{n}.json")
            r = subprocess.run([_VALIDATOR, "certify", rp, "--body", bp, "--records", d, "--sign", a.sign],
                               capture_output=True, text=True)
            if r.returncode != 0:
                print(f"{n + ' (cert)':40} certify FAILED: {(r.stderr or r.stdout).strip()[:120]}")
                failures += 1
                continue
            cert = json.loads(r.stdout)
            st, body = post(a.node, cert)
            print(f"{n + ' (cert)':40} {label(st, body)}  {cert['hash'][:20]}  certified={cert.get('certified')}")
            failures += st >= 400
    if blobs:
        print(f"\n{len(blobs)} by-address blob(s) are NOT published by this script — add them to the node's blob "
              f"store (on the node: manage.py addblob <file>), else the records' result_blob pointers dangle:")
        for p in blobs:
            print("  " + os.path.basename(p))
    print(f"\n{len(names)} records; {failures} failure(s)")
    sys.exit(1 if failures else 0)


if __name__ == "__main__":
    main()
