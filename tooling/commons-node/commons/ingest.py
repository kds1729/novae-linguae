"""The single store path for verified records.

Used by both `POST /v0/records` (views.records) and the `loadrecords` management command, so the
queryable columns (verify.extract) AND the semantic-search embedding are computed in exactly one
place and the two ingest entry points cannot drift. Callers verify first; this only stores.
"""

from . import verify as V
from .embedding import get_embedder
from .models import Record
from .vectorindex import store_vector


def create_record(raw, kind, version):
    """Create and return a Record for an already-verified record.

    The embedding is computed best-effort: if the embedder (e.g. a neural model server) is momentarily
    unavailable, the record is still admitted with a null embedding and `embedding_model` left unset, so
    publishing never blocks on inference (DEPLOYMENT.md). The `embed_pending` worker task and the
    `embedrecords` command both backfill any record missing the current model's embedding."""
    emb = get_embedder()
    try:
        vector = emb.embed(raw)
    except Exception:
        vector = None
    row = Record.objects.create(
        hash=raw["hash"], kind=kind, schema_version=version, raw=raw,
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
            kind, version = V.verify_record(raw)
        except V.VerifyError as exc:
            failed += 1
            if on_reject:
                on_reject(exc.code, exc.detail)
            continue
        if Record.objects.filter(hash=raw.get("hash")).exists():
            skipped += 1
            continue
        create_record(raw, kind, version)
        stored += 1
    return stored, skipped, failed
