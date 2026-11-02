"""Semantic discovery — spec/commons.md `POST /v0/search`.

Best-effort, node-local ranking by embedding cosine similarity (the content-addressed guarantee
applies only after a result is resolved and verified). Two request shapes:

    {"query": "<free text>",  "k": N, "filter": {...}}   # embed the text
    {"like":  "<hash>",       "k": N, "filter": {...}}   # use the target record's stored embedding

Returns {"results": [{"hash", "score"}, ...], "model": "<id>"} ranked high-to-low. The optional
typed `filter` means exactly what it does in `POST /v0/query` (shared via query.candidate_records).

Versions are grouped by default: function records sharing a first name hint, a type and intent tags
take ONE result slot, shown by the strongest (see `_strength`), with the others' hashes in
`versions`. The commons keeps every version (content addressing), so re-minted records otherwise
crowd the ranking — a stranger agent (2026-11-02) met `apply_rate` three times, the OLDEST first.
Grouping is a presentation choice, not a claim of equal behaviour: nothing is hidden (every hash
stays listed) and `{"group_versions": false}` returns the flat ranking.
"""

from .embedding import get_embedder
from .models import Record
from .query import QueryError, certification_count, validate_filter
from .vectorindex import get_vector_index

_DEFAULT_K = 20
_MAX_K = 100
_GROUP_WINDOW = 4   # rank k × this many hits before grouping, so k groups usually fill


class SearchError(Exception):
    def __init__(self, code, detail="", status=400):
        super().__init__(code)
        self.code = code
        self.detail = detail
        self.status = status


def _k(body):
    # `limit` is accepted as `k`: a stranger agent (2026-11-02) wrote `limit` — the /v0/query word —
    # and it was silently ignored, returning the default 20.
    try:
        return max(1, min(int(body.get("k", body.get("limit", _DEFAULT_K))), _MAX_K))
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
    try:
        validate_filter(flt)  # same typed-filter contract as POST /v0/query
    except QueryError as exc:
        raise SearchError("malformed_filter", str(exc), status=400) from exc

    # Ranking is delegated to the active backend (pgvector ANN on Postgres, Python cosine on SQLite).
    # Only vectors from the same model are comparable, so the model id is passed through.
    k = _k(body)
    if body.get("group_versions", True) is False:
        results, truncated = get_vector_index().search(qvec, k, flt, emb.model_id)
        return results, emb.model_id, truncated
    results, truncated = get_vector_index().search(qvec, min(k * _GROUP_WINDOW, _MAX_K), flt, emb.model_id)
    return group_versions(results)[:k], emb.model_id, truncated


def _version_key(r):
    if r.kind != "function-record" or not r.name_hints:
        return None
    return (r.name_hints[0], r.type_str or "", tuple(sorted(r.intent_tags or [])))


def _strength(r):
    """Which version speaks for the group: certified first, then the larger contract (posts + laws:
    a re-mint that ADDED a tie rule promises more — certification checked each one), then the one
    that documents its parameters, then the newest."""
    raw = r.raw or {}
    posts = sum(1 for x in (raw.get("signature") or {}).get("refinements") or []
                if isinstance(x, dict) and x.get("kind") == "post")
    laws = len(raw.get("properties") or [])
    return (certification_count(r.hash) > 0, posts + laws, bool(raw.get("param_docs")), r.created_at, r.hash)


def group_versions(results):
    """Fold ranked hits into one per version group, at the group's best score, keeping rank order."""
    rows = {r.hash: r for r in Record.objects.filter(hash__in=[x["hash"] for x in results])}
    groups, order = {}, []
    for x in results:
        r = rows.get(x["hash"])
        key = _version_key(r) if r is not None else None
        key = key if key is not None else ("", x["hash"])
        if key not in groups:
            groups[key] = []
            order.append(key)
        groups[key].append(x)
    out = []
    for key in order:
        hits = groups[key]
        if len(hits) == 1:
            out.append(hits[0])
            continue
        best = max(hits, key=lambda x: _strength(rows[x["hash"]]))
        out.append({"hash": best["hash"], "score": max(h["score"] for h in hits),
                    "versions": [h["hash"] for h in hits if h["hash"] != best["hash"]]})
    return out
