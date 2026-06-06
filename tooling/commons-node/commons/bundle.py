"""The `.nlb` commons-bundle format (spec/resilience.md).

A portable, self-verifying archive for moving records out-of-band — seed/disaster-recovery, or a
release artifact any project can publish. An `.nlb` is a **gzipped tar** containing exactly:

    manifest.json   {format_version, count, schema_versions[], bundle_digest, source?, producer?}
    records.jsonl   one content-addressed record per line, sorted by hash

Two properties, both falling out of the project's design:

  - **Self-verifying.** Every record is re-verified by hash (+ signature) on ingest, so the producer
    is untrusted (principle 7). `bundle_digest` is a cheap whole-payload integrity pre-check; it is
    NOT the security boundary — per-record verification is.
  - **Deterministic.** The same record set always produces identical bytes (records sorted by hash,
    manifest keys sorted, USTAR tar with fixed mtime/owner, gzip mtime=0), so bundles dedupe and diff
    cleanly and can themselves be content-addressed by a consumer if desired.

Stdlib only — so a standalone packager (for projects without a node) can reuse this module verbatim.
"""

import gzip
import hashlib
import io
import json
import tarfile

FORMAT_VERSION = "nlb/1"
MANIFEST_NAME = "manifest.json"
RECORDS_NAME = "records.jsonl"


class BundleError(Exception):
    pass


def bundle_digest(record_hashes):
    """A blake2b fingerprint of the record SET (order-independent: hashes are sorted first)."""
    h = hashlib.blake2b(digest_size=32)
    for rh in sorted(record_hashes):
        h.update(rh.encode("utf-8"))
        h.update(b"\n")
    return "blake2b:" + h.hexdigest()


def _jsonl(records):
    """Records as compact, sorted-key JSON lines, ordered by hash (deterministic)."""
    ordered = sorted(records, key=lambda r: r["hash"])
    return [json.dumps(r, sort_keys=True, separators=(",", ":")) for r in ordered]


def build_manifest(records, source=None, producer=None):
    hashes = [r["hash"] for r in records]
    manifest = {
        "format_version": FORMAT_VERSION,
        "count": len(records),
        "schema_versions": sorted({r["schema_version"] for r in records if r.get("schema_version")}),
        "bundle_digest": bundle_digest(hashes),
    }
    if source:
        manifest["source"] = source        # advisory provenance, e.g. {"repo": ..., "release": ...}
    if producer:
        manifest["producer"] = producer     # advisory provenance, e.g. a did:nova
    return manifest


def _add(tar, name, data):
    info = tarfile.TarInfo(name)
    info.size = len(data)
    info.mtime = 0                          # fixed -> deterministic bytes
    info.mode = 0o644
    info.uid = info.gid = 0
    info.uname = info.gname = ""
    tar.addfile(info, io.BytesIO(data))


def write_bundle(dest, records, source=None, producer=None):
    """Write records (an iterable of raw record dicts) to an `.nlb`. `dest` is a path or a binary
    file object. Returns the manifest."""
    records = list(records)
    manifest = build_manifest(records, source=source, producer=producer)
    manifest_bytes = json.dumps(manifest, sort_keys=True, separators=(",", ":")).encode("utf-8") + b"\n"
    lines = _jsonl(records)
    records_bytes = ("\n".join(lines) + ("\n" if lines else "")).encode("utf-8")

    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w", format=tarfile.USTAR_FORMAT) as tar:
        _add(tar, MANIFEST_NAME, manifest_bytes)
        _add(tar, RECORDS_NAME, records_bytes)
    gz = gzip.compress(buf.getvalue(), mtime=0)

    if hasattr(dest, "write"):
        dest.write(gz)
    else:
        with open(dest, "wb") as f:
            f.write(gz)
    return manifest


def _member(tar, name):
    try:
        f = tar.extractfile(name)
    except KeyError:
        return None
    return f.read() if f is not None else None


def read_bundle(src):
    """Read an `.nlb` (path or binary file object). Returns (manifest, records). Raises BundleError on
    a malformed bundle or a bundle_digest mismatch. Does NOT verify records — that is the ingest gate's
    job (loadbundle re-verifies every record by hash)."""
    data = src.read() if hasattr(src, "read") else open(src, "rb").read()
    try:
        raw = gzip.decompress(data)
    except (OSError, EOFError) as exc:
        raise BundleError(f"not a gzip bundle: {exc}")
    try:
        with tarfile.open(fileobj=io.BytesIO(raw), mode="r") as tar:
            mbytes = _member(tar, MANIFEST_NAME)
            rbytes = _member(tar, RECORDS_NAME)
    except tarfile.TarError as exc:
        raise BundleError(f"not a tar bundle: {exc}")
    if mbytes is None:
        raise BundleError(f"bundle missing {MANIFEST_NAME}")

    manifest = json.loads(mbytes.decode("utf-8"))
    records = []
    for line in (rbytes or b"").decode("utf-8").splitlines():
        line = line.strip()
        if line:
            records.append(json.loads(line))

    expected = manifest.get("bundle_digest")
    actual = bundle_digest([r["hash"] for r in records if isinstance(r, dict) and "hash" in r])
    if expected is not None and expected != actual:
        raise BundleError(f"bundle_digest mismatch (manifest {expected} != computed {actual})")
    return manifest, records
