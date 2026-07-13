#!/usr/bin/env python3
"""nl-bundle — package Nova Lingua records into a portable `.nlb` commons bundle.

For projects that do NOT run a commons node but want to ship a commons-ready release artifact (like a
wheel or a crate). Reads records as JSONL — exactly what the `nl-ingest-*` adapters emit — and writes
a `.nlb` (spec/resilience.md). Any commons node ingests it with `loadbundle`, re-verifying every
record by hash, so the producer is untrusted.

    nl-ingest-py mylib/ | nl_bundle.py --source-repo https://github.com/org/lib \
        --source-release v1.2.3 -o mylib-1.2.3.nlb

**Zero dependencies** — Python standard library only (3.8+). This is a self-contained sibling of the
node's `commons/bundle.py`; the two produce **byte-identical** bundles for the same record set (pinned
by a conformance test). Packaging does not verify hashes — that is the ingesting node's job.
"""

import argparse
import gzip
import hashlib
import io
import json
import sys
import tarfile
from pathlib import Path

FORMAT_VERSION = "nlb/1"
MANIFEST_NAME = "manifest.json"
RECORDS_NAME = "records.jsonl"
BLOB_PREFIX = "blobs/"


def _crypto():
    """Lazily load the shared signing module (tooling/crypto-python/nl_crypto.py). Only needed when
    --sign-seed is given, so unsigned packaging stays a pure single-file tool."""
    tool = Path(__file__).resolve().parents[1]            # .../tooling
    for p in (str(tool / "crypto-python"), str(tool / "ingest-common")):
        if p not in sys.path:
            sys.path.insert(0, p)
    import nl_crypto
    return nl_crypto


def bundle_digest(record_hashes):
    """A blake2b fingerprint of the record SET (order-independent: hashes are sorted first)."""
    h = hashlib.blake2b(digest_size=32)
    for rh in sorted(record_hashes):
        h.update(rh.encode("utf-8"))
        h.update(b"\n")
    return "blake2b:" + h.hexdigest()


def _sort_key(r):
    # Hash-carrying records by address; hashless self-addressing artifacts (bare bodies, traces)
    # by canonical serialization, after every address (`~` > `a-z`). Mirrors commons-node bundle.py.
    return r.get("hash") or "~" + json.dumps(r, sort_keys=True, separators=(",", ":"))


def _jsonl(records):
    ordered = sorted(records, key=_sort_key)
    return [json.dumps(r, sort_keys=True, separators=(",", ":")) for r in ordered]


def build_manifest(records, source=None, producer=None, blob_sizes=None):
    # Digest = the hash-carrying record set only; hashless self-addressing artifacts ride outside
    # it (the reader recomputes the digest without a validator; the ingest gate verifies them).
    manifest = {
        "format_version": FORMAT_VERSION,
        "count": len(records),
        "schema_versions": sorted({r["schema_version"] for r in records if r.get("schema_version")}),
        "bundle_digest": bundle_digest([r["hash"] for r in records
                                        if isinstance(r, dict) and "hash" in r]),
    }
    if blob_sizes:
        # Advisory totals; blob members are self-verifying by name, and a blobless manifest is
        # byte-identical to the pre-blob-carriage format.
        manifest["blobs"] = {"count": len(blob_sizes), "bytes": sum(blob_sizes)}
    if source:
        manifest["source"] = source
    if producer:
        manifest["producer"] = producer
    return manifest


def _add(tar, name, data):
    info = tarfile.TarInfo(name)
    info.size = len(data)
    info.mtime = 0
    info.mode = 0o644
    info.uid = info.gid = 0
    info.uname = info.gname = ""
    tar.addfile(info, io.BytesIO(data))


def write_bundle(dest, records, source=None, producer=None, sign_seed=None, blobs=None):
    """Write records (raw dicts) to an `.nlb`. dest is a path or binary file object. If sign_seed is
    given, the manifest is signed (advisory provenance). `blobs` is an optional mapping
    ``sha256 -> bytes | Path`` — the blobs the records reference (by-address example values, weights
    files), re-hashed here so a lying bundle is never produced. Returns the manifest. Byte-identical
    to commons/bundle.py:write_bundle (the signing path shares the same nl_crypto module)."""
    records = list(records)
    blob_items = []
    for sha in sorted(blobs or {}):
        data = blobs[sha]
        if not isinstance(data, (bytes, bytearray)):
            data = Path(data).read_bytes()
        if hashlib.sha256(data).hexdigest() != sha:
            raise SystemExit(f"nl-bundle: blob {sha} content hashes elsewhere — refusing to write a lying bundle")
        blob_items.append((sha, bytes(data)))
    manifest = build_manifest(records, source=source, producer=producer,
                              blob_sizes=[len(d) for _, d in blob_items])
    if sign_seed:
        manifest = _crypto().sign_manifest(manifest, sign_seed)
    manifest_bytes = json.dumps(manifest, sort_keys=True, separators=(",", ":")).encode("utf-8") + b"\n"
    lines = _jsonl(records)
    records_bytes = ("\n".join(lines) + ("\n" if lines else "")).encode("utf-8")

    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w", format=tarfile.USTAR_FORMAT) as tar:
        _add(tar, MANIFEST_NAME, manifest_bytes)
        _add(tar, RECORDS_NAME, records_bytes)
        for sha, data in blob_items:
            _add(tar, BLOB_PREFIX + sha, data)
    gz = gzip.compress(buf.getvalue(), mtime=0)

    if hasattr(dest, "write"):
        dest.write(gz)
    else:
        with open(dest, "wb") as f:
            f.write(gz)
    return manifest


def _read_records(paths):
    records = []
    streams = [open(p, encoding="utf-8") for p in paths] if paths else [sys.stdin]
    try:
        for stream in streams:
            for n, line in enumerate(stream, 1):
                line = line.strip()
                if not line:
                    continue
                try:
                    rec = json.loads(line)
                except ValueError as exc:
                    raise SystemExit(f"nl-bundle: invalid JSON on a record line: {exc}")
                if not isinstance(rec, dict) or not isinstance(rec.get("hash"), str):
                    raise SystemExit("nl-bundle: each record must be a JSON object with a string 'hash'")
                records.append(rec)
    finally:
        for stream in streams:
            if stream is not sys.stdin:
                stream.close()
    return records


def main(argv=None):
    ap = argparse.ArgumentParser(description="Package Nova Lingua records (JSONL) into a .nlb bundle.")
    ap.add_argument("files", nargs="*", help="JSONL record files (default: stdin)")
    ap.add_argument("-o", "--output", default="-", help="output .nlb path (default: - for stdout)")
    ap.add_argument("--source-repo", help="provenance: source repository URL")
    ap.add_argument("--source-release", help="provenance: release tag/version")
    ap.add_argument("--sign-seed", help="sign the manifest with the did:nova derived from this seed")
    ap.add_argument("--blob", action="append", default=[], metavar="FILE",
                    help="carry a referenced blob (repeatable): a `blob-<sha256>.json` sidecar as the "
                         "nl-ingest-* adapters emit for by-address example values, or a bare "
                         "`<sha256>`-named file; the name's sha256 is verified against the content")
    args = ap.parse_args(argv)

    records = _read_records(args.files)
    blobs = {}
    for name in args.blob:
        p = Path(name)
        stem = p.name
        if stem.startswith("blob-") and stem.endswith(".json"):
            stem = stem[len("blob-"):-len(".json")]
        if not (len(stem) == 64 and all(c in "0123456789abcdef" for c in stem)):
            raise SystemExit(f"nl-bundle: --blob {name}: the filename must carry the sha256 "
                             "(blob-<sha256>.json or <sha256>)")
        blobs[stem] = p
    source = {k: v for k, v in (("repo", args.source_repo), ("release", args.source_release)) if v} or None

    dest = sys.stdout.buffer if args.output == "-" else args.output
    manifest = write_bundle(dest, records, source=source, sign_seed=args.sign_seed, blobs=blobs)
    signed = f"  signed-by={manifest['producer']}" if manifest.get("signature") else ""
    blob_note = f"  blobs={manifest['blobs']['count']}" if manifest.get("blobs") else ""
    sys.stderr.write(f"nl-bundle: packaged {manifest['count']} records{blob_note}  "
                     f"schema_versions={manifest['schema_versions']}  "
                     f"digest={manifest['bundle_digest']}{signed}\n")


if __name__ == "__main__":
    main()
