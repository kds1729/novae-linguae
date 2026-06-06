"""Vector index behind a backend seam — how `POST /v0/search` ranks by embedding similarity.

Two implementations, picked by the active DB backend so the node spans a spectrum from zero-infra to
industrial with no protocol change:

  - ScanIndex (SQLite / default): cosine in Python over the portable `embedding` JSONField. Simple,
    obviously correct, fine for a PoC corpus. Reuses query.candidate_records + embedding.cosine.
  - PgVectorIndex (Postgres): true ANN via pgvector's `<=>` cosine-distance operator over an HNSW
    index. Scales to millions of records; this is the production path.

Both take a query vector (a plain list of floats — its origin, free-text or a `like` record's stored
embedding, doesn't matter) and return ranked [{"hash","score"}] high-to-low, scoring only against
vectors from the same `model_id` (cross-model/dim vectors are not comparable).
"""

from .embedding import cosine
from .query import candidate_records

_SEARCH_SCAN_CAP = 5000   # bound the per-request candidate set for the MVP (flagged when hit)


class ScanIndex:
    backend = "scan"

    def search(self, qvec, k, flt, model_id):
        candidates, truncated = candidate_records(flt or {}, cap=_SEARCH_SCAN_CAP)
        scored = []
        for r in candidates:
            if not r.embedding or r.embedding_model != model_id:
                continue
            scored.append((cosine(qvec, r.embedding), r.hash))
        scored.sort(key=lambda t: (t[0], t[1]), reverse=True)
        return [{"hash": h, "score": round(s, 6)} for s, h in scored[:k]], truncated


class PgVectorIndex:
    backend = "pgvector"

    def search(self, qvec, k, flt, model_id):
        from django.db import connection

        vec = _vector_literal(qvec)
        where = ["embedding_vec IS NOT NULL", "embedding_model = %s"]
        params = [vec, model_id]          # SELECT vec, then WHERE model
        truncated = False

        # A typed filter restricts the ANN candidates via the shared filter logic. For very large
        # filtered sets this trades ANN efficiency (an operator would push predicates into SQL); for
        # the PoC it keeps "filter means the same thing in query and search".
        if flt:
            cand, truncated = candidate_records(flt, cap=_SEARCH_SCAN_CAP)
            hashes = [r.hash for r in cand]
            if not hashes:
                return [], truncated
            where.append("hash = ANY(%s)")
            params.append(hashes)

        params += [vec, int(k)]           # ORDER BY vec, then LIMIT
        sql = (
            "SELECT hash, 1 - (embedding_vec <=> %s::vector) AS score "
            "FROM commons_record "
            f"WHERE {' AND '.join(where)} "
            "ORDER BY embedding_vec <=> %s::vector LIMIT %s"
        )
        with connection.cursor() as cur:
            cur.execute(sql, params)
            rows = cur.fetchall()
        return [{"hash": h, "score": round(float(s), 6)} for h, s in rows], truncated


def get_vector_index():
    """Return the ranking backend for the active database (pgvector ANN on Postgres, else scan)."""
    from django.db import connection
    return PgVectorIndex() if connection.vendor == "postgresql" else ScanIndex()


def _vector_literal(vector):
    return "[" + ",".join(repr(float(x)) for x in vector) + "]"


def store_vector(record_hash, vector):
    """Sync the Postgres `embedding_vec` column for a record. No-op off Postgres (the JSON `embedding`
    column is the portable source of truth; this just keeps the ANN index populated)."""
    from django.db import connection
    if connection.vendor != "postgresql" or not vector:
        return
    with connection.cursor() as cur:
        cur.execute("UPDATE commons_record SET embedding_vec = %s::vector WHERE hash = %s",
                    [_vector_literal(vector), record_hash])
