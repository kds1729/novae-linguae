"""Semantic discovery — spec/commons.md `POST /v0/search`.

Best-effort, node-local ranking by embedding cosine similarity (the content-addressed guarantee
applies only after a result is resolved and verified). Two request shapes:

    {"query": "<free text>",  "k": N, "filter": {...}}   # embed the text
    {"like":  "<hash>",       "k": N, "filter": {...}}   # use the target record's stored embedding

Returns {"results": [{"hash", "score"}, ...], "model": "<id>"} ranked high-to-low. The optional
typed `filter` means exactly what it does in `POST /v0/query` (shared via query.candidate_records).
"""

from .embedding import cosine, get_embedder
from .models import Record
from .query import candidate_records

_DEFAULT_K = 20
_MAX_K = 100
_SEARCH_SCAN_CAP = 5000   # bound the per-request similarity scan for the MVP (flagged when hit)


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

    candidates, truncated = candidate_records(flt, cap=_SEARCH_SCAN_CAP)
    scored = []
    for r in candidates:
        # Only rank records embedded by the same model — vectors from different models or dims are
        # not comparable. In normal operation every row shares the node's current model.
        if not r.embedding or r.embedding_model != emb.model_id:
            continue
        scored.append((cosine(qvec, r.embedding), r.hash))

    scored.sort(key=lambda t: (t[0], t[1]), reverse=True)
    results = [{"hash": h, "score": round(s, 6)} for s, h in scored[:_k(body)]]
    return results, emb.model_id, truncated
