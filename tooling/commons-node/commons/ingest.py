"""The single store path for verified records.

Used by both `POST /v0/records` (views.records) and the `loadrecords` management command, so the
queryable columns (verify.extract) AND the semantic-search embedding are computed in exactly one
place and the two ingest entry points cannot drift. Callers verify first; this only stores.
"""

import hashlib
import json
import os

from django.conf import settings

from . import verify as V
from .embedding import get_embedder
from .models import Record
from .vectorindex import store_vector


def canonical_body_bytes(raw):
    """Deterministic JSON bytes for a bare body. Any JSON serialization of the same object
    re-verifies to the same `expr_…` address (the validator canonicalizes before hashing), so a
    stable local rendering is all tiering needs."""
    return json.dumps(raw, separators=(",", ":"), sort_keys=True).encode()


def _tier_body(data):
    """Store an oversized bare body's canonical bytes in the blob store (temp file + rename, like
    blob replication — a crash can never leave half a body serving under a content address).
    Returns (sha256, byte_count)."""
    sha = hashlib.sha256(data).hexdigest()
    blob_dir = settings.COMMONS_BLOB_DIR
    os.makedirs(blob_dir, exist_ok=True)
    dest = os.path.join(blob_dir, sha)
    if not os.path.exists(dest):
        tmp = os.path.join(blob_dir, f".{sha}.part")
        with open(tmp, "wb") as f:
            f.write(data)
        os.replace(tmp, dest)
    return sha, len(data)


def materialized_raw(row):
    """The full record for a stored row — reading a tiered body back from the blob store. Inline
    rows return `raw` as-is. Raises if a tiered body's blob is missing (a store inconsistency a
    caller must not paper over)."""
    if row.blob_sha256:
        path = os.path.join(settings.COMMONS_BLOB_DIR, row.blob_sha256)
        with open(path, "rb") as f:
            return json.load(f)
    return row.raw


def create_record(raw, kind, version, address=None):
    """Create and return a Record for an already-verified record. `address` is the content
    address `verify_record` returned; it defaults to the embedded `hash` for the artifact kinds
    that carry one (a bare body expression doesn't — the node computed its `expr_…` address).

    The embedding is computed best-effort: if the embedder (e.g. a neural model server) is momentarily
    unavailable, the record is still admitted with a null embedding and `embedding_model` left unset, so
    publishing never blocks on inference (DEPLOYMENT.md). The `embed_pending` worker task and the
    `embedrecords` command both backfill any record missing the current model's embedding."""
    emb = get_embedder()
    try:
        vector = emb.embed(raw)
    except Exception:
        vector = None
    # Body storage tiering (commons.md open question 4): a bare body past the record cap keeps only
    # a pointer row — the canonical bytes go to the blob store, resolve streams them back. Applied
    # ONLY above the cap, so every record that could exist before tiering is stored exactly as it
    # always was; the tier is a new capability, not a behavior change.
    blob_sha256, blob_bytes = None, None
    if kind == "body":
        data = canonical_body_bytes(raw)
        if len(data) > settings.COMMONS_MAX_RECORD_BYTES:
            blob_sha256, blob_bytes = _tier_body(data)
            raw = {}
    row = Record.objects.create(
        hash=address or raw["hash"], kind=kind, schema_version=version, raw=raw,
        blob_sha256=blob_sha256, blob_bytes=blob_bytes,
        embedding=vector, embedding_model=(emb.model_id if vector else None),
        **V.extract(raw, kind),
    )
    store_vector(row.hash, vector)   # syncs the pgvector ANN column on Postgres; no-op on SQLite / None
    return row


def ingest_records(records, on_reject=None):
    """Verify-then-store an iterable of raw record dicts (the shared admission path for loadrecords
    and loadbundle). Idempotent by hash. Returns (stored, skipped, failed). on_reject(code, detail)
    is invoked for each record that fails verification (for CLI logging)."""
    stored = skipped = failed = 0
    for raw in records:
        try:
            kind, version, address = V.verify_record(raw)
        except V.VerifyError as exc:
            failed += 1
            if on_reject:
                on_reject(exc.code, exc.detail)
            continue
        if Record.objects.filter(hash=address).exists():
            skipped += 1
            continue
        create_record(raw, kind, version, address)
        stored += 1
    return stored, skipped, failed
