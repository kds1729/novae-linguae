#!/usr/bin/env python3
"""Seed a commons node with signed **certifications** for the record+body pairs it can verify.

The commons stores certifications but does not produce them (it never has the bodies — bodies aren't
stored). This tool runs where the bodies ARE (a directory of records + `body-*.json`, e.g. `spec/examples`,
or any adapter/corpus output kept alongside its bodies): for each function record whose body is present it
calls `nl-validator certify --sign <seed>` to produce a signed certification (`cert_…`), then publishes the
certification — and optionally the record itself — to the node's public `POST /v0/records` gate, and finally
reads back `GET /v0/records/{fn}/certifications` to confirm the node serves it.

This is how a certifier "wires" a node (Arca or any deployment) to serve certifications: it never trusts the
node (every artifact is signature-checked on ingest), and the node never judges the certifications it serves
(principle 7) — a consumer decides trust under its own policy.

    python3 seed_certifications.py --node https://nl.1105software.com \
        --seed "$CERTIFIER_SEED" --records ../../spec/examples --publish-records
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
_DEFAULT_RECORDS = os.path.normpath(os.path.join(_HERE, "..", "..", "spec", "examples"))
_DEFAULT_VALIDATOR = os.path.normpath(
    os.path.join(_HERE, "..", "validator", "target", "release", "nl-validator")
)


def _post_record(node, obj, timeout):
    data = json.dumps(obj).encode()
    req = urllib.request.Request(
        node.rstrip("/") + "/v0/records", data=data,
        headers={"content-type": "application/json"}, method="POST",
    )
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return r.status, json.loads(r.read() or b"{}")
    except urllib.error.HTTPError as e:
        try:
            body = json.loads(e.read() or b"{}")
        except ValueError:
            body = {}
        return e.code, body


def _get_certifications(node, subject, timeout):
    url = node.rstrip("/") + f"/v0/records/{subject}/certifications"
    with urllib.request.urlopen(url, timeout=timeout) as r:
        return json.loads(r.read())


def _validator_hash(validator, path):
    out = subprocess.run([validator, "hash", path], capture_output=True, text=True)
    return out.stdout.strip() if out.returncode == 0 else None


def _certify(validator, record_path, body_path, seed):
    # `certify` exits 1 when the record is NOT certified but still prints the (negative) certificate on
    # stdout, so accept either exit code and key off whether we got a JSON certificate.
    out = subprocess.run(
        [validator, "certify", record_path, "--body", body_path, "--sign", seed],
        capture_output=True, text=True,
    )
    if not out.stdout.strip():
        return None
    try:
        return json.loads(out.stdout)
    except ValueError:
        return None


def main(argv=None):
    ap = argparse.ArgumentParser(description="Seed a commons node with signed certifications.")
    ap.add_argument("--node", required=True, help="base URL of the node, e.g. https://nl.1105software.com")
    ap.add_argument("--seed", required=True, help="certifier signing seed (its did:nova identity signs the certs)")
    ap.add_argument("--records", default=_DEFAULT_RECORDS, help="directory of records + body-*.json")
    ap.add_argument("--validator", default=_DEFAULT_VALIDATOR, help="path to the nl-validator binary")
    ap.add_argument("--publish-records", action="store_true",
                    help="also publish the function records themselves (not just their certifications)")
    ap.add_argument("--timeout", type=float, default=30.0)
    args = ap.parse_args(argv)

    if not os.path.exists(args.validator):
        sys.exit(f"nl-validator not found at {args.validator} — build it or pass --validator")

    # Index every available body by its expr-address, so a record can be paired with its body via body_hash.
    bodies = {}
    for bf in glob.glob(os.path.join(args.records, "body-*.json")):
        h = _validator_hash(args.validator, bf)
        if h:
            bodies[h] = bf

    seeded = published = skipped = failed = 0
    for rf in sorted(glob.glob(os.path.join(args.records, "*.json"))):
        try:
            rec = json.load(open(rf))
        except ValueError:
            continue
        if not (isinstance(rec, dict) and str(rec.get("hash", "")).startswith("fn_")):
            continue
        body_path = bodies.get(rec.get("body_hash"))
        if not body_path:
            continue  # no body available → can't certify this record here
        cert = _certify(args.validator, rf, body_path, args.seed)
        if cert is None:
            print(f"skip  {rec['hash'][:20]}  (certify produced no certificate)")
            skipped += 1
            continue

        if args.publish_records:
            st, _ = _post_record(args.node, rec, args.timeout)
            if st in (200, 201):
                published += 1
            else:
                print(f"warn  record {rec['hash'][:20]} POST -> {st}")

        st, resp = _post_record(args.node, cert, args.timeout)
        if st in (200, 201):
            seeded += 1
            state = "stored" if st == 201 else "already-present"
            print(f"cert  {cert['hash'][:20]}  subject={rec['hash'][:20]}  certified={cert['certified']}  ({state})")
        else:
            failed += 1
            print(f"FAIL  cert for {rec['hash'][:20]} -> {st} {resp.get('error', '')}")

    # Verify the node now serves the certifications for one seeded subject.
    verified = None
    for rf in sorted(glob.glob(os.path.join(args.records, "*.json"))):
        try:
            rec = json.load(open(rf))
        except ValueError:
            continue
        if isinstance(rec, dict) and str(rec.get("hash", "")).startswith("fn_") and bodies.get(rec.get("body_hash")):
            try:
                served = _get_certifications(args.node, rec["hash"], args.timeout)
                if served.get("count", 0) > 0:
                    verified = (rec["hash"], served["count"])
                    break
            except urllib.error.URLError:
                pass

    print(f"\nseeded={seeded} published_records={published} skipped={skipped} failed={failed}")
    if verified:
        print(f"verified: GET /v0/records/{verified[0][:20]}…/certifications -> {verified[1]} certification(s)")
    return 0 if failed == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
