"""Semantic discovery — spec/commons.md `POST /v0/search`.

Best-effort, node-local ranking by embedding cosine similarity (the content-addressed guarantee
applies only after a result is resolved and verified). Two request shapes:

    {"query": "<free text>",  "k": N, "filter": {...}}   # embed the text
    {"like":  "<hash>",       "k": N, "filter": {...}}   # use the target record's stored embedding

Returns {"results": [{"hash", "score"}, ...], "model": "<id>"} ranked high-to-low. The optional
typed `filter` means exactly what it does in `POST /v0/query` (shared via query.candidate_records).
"""

from .embedding import get_embedder
from .models import Record
from .vectorindex import get_vector_index

_DEFAULT_K = 20
_MAX_K = 100


class SearchError(Exception):
    def __init__(self, code, detail="", status=400):
        super().__init__(code)
        self.code = code
        self.detail = detail
        self.status = status


def _k(body):
    try:
        return max(1, min(int(body.get("k", _DEFAULT_K)), _MAX_K))
    except (TypeError, ValueError):
        return _DEFAULT_K


def run_search(body):
    """Return (results, model_id, truncated). results = [{"hash", "score"}] high-to-low."""
    if not isinstance(body, dict):
        raise SearchError("bad_request", "body must be a JSON object")
    emb = get_embedder()

    # Resolve the query vector from either a free-text 'query' or a 'like' target hash.
    if body.get("like") is not None:
        target = Record.objects.filter(hash=body["like"]).first()
        if target is None:
            raise SearchError("absent", f"unknown 'like' target {body['like']!r}", status=404)
        if not target.embedding:
            raise SearchError("not_embedded", "target has no embedding (run embedrecords)", status=422)
        qvec = target.embedding
    elif isinstance(body.get("query"), str) and body["query"].strip():
        qvec = emb.embed(body["query"])
    else:
        raise SearchError("bad_request", "provide a non-empty 'query' string or a 'like' hash")

    flt = body.get("filter") or {}
    if not isinstance(flt, dict):
        raise SearchError("bad_request", "'filter' must be an object")

    # Ranking is delegated to the active backend (pgvector ANN on Postgres, Python cosine on SQLite).
    # Only vectors from the same model are comparable, so the model id is passed through.
    results, truncated = get_vector_index().search(qvec, _k(body), flt, emb.model_id)
    return results, emb.model_id, truncated
