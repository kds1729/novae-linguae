"""The `.nlb` commons-bundle format (spec/resilience.md).

A portable, self-verifying archive for moving records out-of-band — seed/disaster-recovery, or a
release artifact any project can publish. An `.nlb` is a **gzipped tar** containing:

    manifest.json   {format_version, count, schema_versions[], bundle_digest, blobs?, source?, producer?}
    records.jsonl   one content-addressed record per line, sorted by hash
    blobs/<sha256>  (optional) the blobs the bundled records reference — a by-address example value
                    (`examples[].result_blob`), a weights manifest file — so a restored record is
                    CHECKABLE, not merely resolvable, on a node that has never seen the origin

Two properties, both falling out of the project's design:

  - **Self-verifying.** Every record is re-verified by hash (+ signature) on ingest, so the producer
    is untrusted (principle 7). Blob members are self-verifying by NAME — the sha256 is recomputed on
    both write and read, so a lying member is refused, never stored. `bundle_digest` is a cheap
    whole-payload integrity pre-check; it is NOT the security boundary — per-item verification is.
  - **Deterministic.** The same content always produces identical bytes (records sorted by hash,
    blobs sorted by sha256, manifest keys sorted, USTAR tar with fixed mtime/owner, gzip mtime=0), so
    bundles dedupe and diff cleanly and can themselves be content-addressed by a consumer if desired.
    A bundle with no blobs is byte-identical to one produced before blob carriage existed.

Stdlib only — so a standalone packager (for projects without a node) can reuse this module verbatim.
"""

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


class BundleError(Exception):
    pass


def _crypto():
    """Lazily load the shared signing module (tooling/crypto-python/nl_crypto.py). Only needed for
    signed bundles, so unsigned export/import never depends on it."""
    tool = Path(__file__).resolve().parents[2]            # .../tooling
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


def _jsonl(records):
    """Records as compact, sorted-key JSON lines, ordered by hash (deterministic)."""
    ordered = sorted(records, key=lambda r: r["hash"])
    return [json.dumps(r, sort_keys=True, separators=(",", ":")) for r in ordered]


def build_manifest(records, source=None, producer=None, blob_sizes=None):
    hashes = [r["hash"] for r in records]
    manifest = {
        "format_version": FORMAT_VERSION,
        "count": len(records),
        "schema_versions": sorted({r["schema_version"] for r in records if r.get("schema_version")}),
        "bundle_digest": bundle_digest(hashes),
    }
    if blob_sizes:
        # Advisory totals only — the blob members are self-verifying by name, and a blobless
        # manifest is byte-identical to the pre-blob-carriage format.
        manifest["blobs"] = {"count": len(blob_sizes), "bytes": sum(blob_sizes)}
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


def write_bundle(dest, records, source=None, producer=None, sign_seed=None, blobs=None):
    """Write records (an iterable of raw record dicts) to an `.nlb`. `dest` is a path or a binary
    file object. If `sign_seed` is given, the manifest is signed (advisory provenance: it gains a
    `producer` did:nova and an Ed25519 `signature`). `blobs` is an optional mapping
    ``sha256 -> bytes | Path`` — the blobs the bundled records reference; each is re-hashed here and
    a mismatch refuses the WRITE (a lying bundle is never produced). Returns the manifest."""
    records = list(records)
    blob_items = []
    for sha in sorted(blobs or {}):
        data = blobs[sha]
        if not isinstance(data, (bytes, bytearray)):
            data = Path(data).read_bytes()
        if hashlib.sha256(data).hexdigest() != sha:
            raise BundleError(f"blob {sha} content hashes elsewhere — refusing to write a lying bundle")
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
    manifest, records, _ = read_bundle_full(src)
    return manifest, records


def read_bundle_full(src):
    """Like read_bundle, but also returns the carried blobs as ``{sha256: bytes}``. Every blob member
    is re-hashed and MUST match the sha256 it is named by — a lying member fails the whole read
    (per-item verification is the boundary, exactly as with records at the ingest gate)."""
    data = src.read() if hasattr(src, "read") else open(src, "rb").read()
    try:
        raw = gzip.decompress(data)
    except (OSError, EOFError) as exc:
        raise BundleError(f"not a gzip bundle: {exc}")
    blobs = {}
    try:
        with tarfile.open(fileobj=io.BytesIO(raw), mode="r") as tar:
            mbytes = _member(tar, MANIFEST_NAME)
            rbytes = _member(tar, RECORDS_NAME)
            for name in tar.getnames():
                if name.startswith(BLOB_PREFIX):
                    sha = name[len(BLOB_PREFIX):]
                    content = _member(tar, name) or b""
                    if hashlib.sha256(content).hexdigest() != sha:
                        raise BundleError(f"blob member {name} content hashes elsewhere — refusing the bundle")
                    blobs[sha] = content
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
    return manifest, records, blobs


def verify_manifest(manifest):
    """Check a manifest's optional provenance signature. Returns (status, producer) where status is
    'unsigned' | 'valid' | 'invalid'. Advisory — record-level hash verification on ingest is the real
    admission gate; this only attests who produced the bundle."""
    return _crypto().verify_manifest(manifest)
