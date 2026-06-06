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
    """Create and return a Record for an already-verified record (embedding computed on store)."""
    emb = get_embedder()
    vector = emb.embed(raw)
    row = Record.objects.create(
        hash=raw["hash"], kind=kind, schema_version=version, raw=raw,
        embedding=vector, embedding_model=emb.model_id,
        **V.extract(raw, kind),
    )
    store_vector(row.hash, vector)   # syncs the pgvector ANN column on Postgres; no-op on SQLite
    return row
