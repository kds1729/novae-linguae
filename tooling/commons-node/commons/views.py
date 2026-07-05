"""HTTP views implementing the commons protocol (spec/commons.md, /v0/ binding)."""

import json
import shutil

from django.conf import settings
from django.http import HttpResponse, HttpResponseNotAllowed, JsonResponse
from django.views.decorators.csrf import csrf_exempt

from . import verify as V
from .egress import usage as egress_usage
from .embedding import get_embedder
from .ingest import create_record
from .equiv import EquivError, run_equiv
from .models import Record
from .prove import ProveError, run_prove
from .query import (QueryError, check_token_budget, greedy_budget, record_summary,
                    run_query, summary_tokens)
from .search import run_search, SearchError

_SCHEMA_VERSIONS = ["0.1.0", "0.2.0"]
_KINDS = ["function-record", "message", "body", "type", "certification", "weights", "eval-attestation"]

# Verification failures that mean "the record is not valid" (422) vs. node-side problems.
_UNPROCESSABLE = {"schema_invalid", "hash_mismatch", "signature_invalid", "unsupported_kind"}


def _json_body(request):
    return json.loads(request.body or b"{}")


@csrf_exempt
def records(request):
    """POST /v0/records — verify then store (idempotent by hash)."""
    if request.method != "POST":
        return HttpResponseNotAllowed(["POST"])
    if len(request.body) > settings.COMMONS_MAX_RECORD_BYTES:
        return JsonResponse({"error": "too_large"}, status=413)
    try:
        raw = json.loads(request.body)
    except ValueError as exc:
        return JsonResponse({"error": "malformed_json", "detail": str(exc)}, status=400)

    try:
        kind, version, address = V.verify_record(raw)
    except V.VerifyError as exc:
        if exc.code == "verifier_unavailable":
            return JsonResponse({"error": exc.code, "detail": exc.detail}, status=503)
        status = 422 if exc.code in _UNPROCESSABLE else 400
        return JsonResponse({"error": exc.code, "detail": exc.detail}, status=status)

    if Record.objects.filter(hash=address).exists():
        return JsonResponse({"hash": address, "stored": False}, status=200)

    create_record(raw, kind, version, address)
    return JsonResponse({"hash": address, "stored": True}, status=201)


@csrf_exempt
def record(request, address):
    """GET /v0/records/{hash} — resolve; HEAD — exists."""
    if request.method == "HEAD":
        present = Record.objects.filter(hash=address).exists()
        return HttpResponse(status=200 if present else 404)
    if request.method != "GET":
        return HttpResponseNotAllowed(["GET", "HEAD"])
    row = Record.objects.filter(hash=address).first()
    if row is None:
        return JsonResponse({"error": "absent"}, status=404)
    resp = JsonResponse(row.raw)
    # Content-addressed records are immutable, so this is safe to cache forever. Lets a CDN/edge front
    # `resolve` at ~100% hit rate and absorb traffic spikes off the origin's metered egress.
    resp["Cache-Control"] = "public, max-age=31536000, immutable"
    return resp


@csrf_exempt
def certifications(request, address):
    """GET /v0/records/{fn-hash}/certifications — the signed certification records about a function.

    The network face of trust-delegation: a client that resolved a function fetches its certifications
    here, then verifies each (hash + signature) and decides — under *its own* local policy — whether any
    certifier is trusted (`nl-validator certified`, `Policy.certification_verdict`). The node does not
    judge; it stores and serves signed attestations (principle 7). `?certified=true` returns only positive
    certifications (the common case); by default all stored certifications about the subject are returned.
    """
    if request.method != "GET":
        return HttpResponseNotAllowed(["GET"])
    rows = Record.objects.filter(kind="certification", subject=address)
    if request.GET.get("certified") == "true":
        rows = rows.filter(certified=True)
    certs = [r.raw for r in rows.order_by("id")]
    resp = JsonResponse({"subject": address, "certifications": certs, "count": len(certs)})
    # Certifications are content-addressed and immutable, but the SET about a subject grows as new ones
    # are published, so cache only briefly (unlike an individual immutable `resolve`).
    resp["Cache-Control"] = "public, max-age=10"
    return resp


@csrf_exempt
def attestations(request, address):
    """GET /v0/records/{wgt-hash}/attestations — the signed eval attestations about a weights record.

    The weights counterpart of `certifications` (spec/weights.md rung 3): a client that resolved a
    weights pointer fetches the eval attestations here, verifies each (hash + signature), and decides —
    under *its own* local policy — whether any attesting certifier is trusted (`nl-validator certified
    --subject wgt_…`, `Policy.eval_attestation_verdict`). The node does not judge (principle 7).
    """
    if request.method != "GET":
        return HttpResponseNotAllowed(["GET"])
    rows = Record.objects.filter(kind="eval-attestation", subject=address)
    atts = [r.raw for r in rows.order_by("id")]
    resp = JsonResponse({"subject": address, "attestations": atts, "count": len(atts)})
    # Immutable individually, but the set about a subject grows — cache briefly, like certifications.
    resp["Cache-Control"] = "public, max-age=10"
    return resp


def blob(request, sha256):
    """GET /v0/blobs/{sha256} — serve a binary blob by content hash (spec/weights.md).

    Deliberately gate-free: blobs are opaque bytes outside the record store; a weights record's
    `files[].sha256` is what makes any host — including this one — safe to fetch from (the client
    verifies after download; the hash is the truth). Files live in COMMONS_BLOB_DIR keyed by their
    sha256; `addblob` puts them there. In production a static file server / CDN can front or replace
    this view entirely — the URL shape is the contract, not the implementation.
    """
    if request.method not in ("GET", "HEAD"):
        return HttpResponseNotAllowed(["GET", "HEAD"])
    import os

    from django.conf import settings as dj_settings
    from django.http import FileResponse

    blob_dir = getattr(dj_settings, "COMMONS_BLOB_DIR", None)
    path = os.path.join(blob_dir, sha256) if blob_dir else None
    if not path or not os.path.isfile(path):
        return JsonResponse({"error": "absent"}, status=404)
    if request.method == "HEAD":
        resp = HttpResponse(status=200)
        resp["Content-Length"] = str(os.path.getsize(path))
        return resp
    resp = FileResponse(open(path, "rb"), content_type="application/octet-stream")
    # Content-addressed, immutable — cache forever.
    resp["Cache-Control"] = "public, max-age=31536000, immutable"
    return resp


@csrf_exempt
def query(request):
    """POST /v0/query — typed (exact, portable) discovery."""
    if request.method != "POST":
        return HttpResponseNotAllowed(["POST"])
    try:
        flt = _json_body(request)
    except ValueError as exc:
        return JsonResponse({"error": "malformed_json", "detail": str(exc)}, status=400)

    include = request.GET.get("include")
    try:
        # A token budget only shapes the compact-summary tier (uniform-size hashes / heavy full records
        # aren't what a context window is spent on); ignored for other include modes.
        hashes, cursor, complete, budget = run_query(
            flt, rank=request.GET.get("rank") == "relevance", budget=include == "summary")
    except QueryError as exc:
        return JsonResponse({"error": "malformed_filter", "detail": str(exc)}, status=400)
    if include == "record":
        by_hash = {r.hash: r.raw for r in Record.objects.filter(hash__in=hashes)}
        return JsonResponse({"records": [by_hash[h] for h in hashes if h in by_hash],
                             "cursor": cursor, "complete": complete})
    if include == "summary":
        # Compact projection: the decision fields (type/effects/intent/…), not the full record — the
        # discovery-cost middle tier between hashes-only and `include=record`. `budget` (when a
        # `token_budget` was applied) reports the estimated token spend and whether more matched.
        by_hash = {r.hash: record_summary(r) for r in Record.objects.filter(hash__in=hashes)}
        payload = {"results": [by_hash[h] for h in hashes if h in by_hash],
                   "cursor": cursor, "complete": complete}
        if budget is not None:
            payload["budget"] = budget
        return JsonResponse(payload)
    return JsonResponse({"results": hashes, "cursor": cursor, "complete": complete})


@csrf_exempt
def search(request):
    """POST /v0/search — semantic discovery (best-effort, node-local; spec/commons.md)."""
    if request.method != "POST":
        return HttpResponseNotAllowed(["POST"])
    try:
        body = _json_body(request)
    except ValueError as exc:
        return JsonResponse({"error": "malformed_json", "detail": str(exc)}, status=400)
    want_summary = request.GET.get("include") == "summary"
    # A token budget shapes only the summary tier (see run_query); validate up front for a clean 400.
    token_budget = body.get("token_budget") if want_summary and isinstance(body, dict) else None
    if token_budget is not None:
        try:
            check_token_budget(token_budget)
        except QueryError as exc:
            return JsonResponse({"error": "malformed_filter", "detail": str(exc)}, status=400)
    try:
        results, model_id, truncated = run_search(body)
    except SearchError as exc:
        return JsonResponse({"error": exc.code, "detail": exc.detail}, status=exc.status)
    budget = None
    if want_summary:
        # Fold the compact projection into each ranked hit, so a client ranks AND judges candidates in a
        # single round-trip (the discovery-cost lever); the similarity `score` is preserved alongside.
        by_hash = {r.hash: record_summary(r)
                   for r in Record.objects.filter(hash__in=[x["hash"] for x in results])}
        results = [{**by_hash.get(x["hash"], {"hash": x["hash"]}), "score": x["score"]} for x in results]
        if token_budget is not None:
            # Cap the ranked, projected hits by estimated token cost (measured on the returned object,
            # score included), keeping the highest-similarity ones that fit — the /v0/search counterpart
            # of the /v0/query token budget. No cursor: search is a ranked view, not a paged feed.
            results, budget = greedy_budget(results, summary_tokens, token_budget, len(results))
    payload = {"results": results, "model": model_id}
    if budget is not None:
        payload["budget"] = budget
    if truncated:
        payload["truncated"] = True   # scan cap hit; some records were not ranked (MVP bound)
    return JsonResponse(payload)


@csrf_exempt
def prove(request):
    """POST /v0/prove — prove a record's `forall` properties over the unbounded domain (best-effort,
    node-local; not an admission gate). Target it with a stored `{"hash": ...}` or an inline
    `{"record": {...}, "body": {...optional...}}`."""
    if request.method != "POST":
        return HttpResponseNotAllowed(["POST"])
    if len(request.body) > settings.COMMONS_MAX_RECORD_BYTES:
        return JsonResponse({"error": "too_large"}, status=413)
    try:
        body = _json_body(request)
    except ValueError as exc:
        return JsonResponse({"error": "malformed_json", "detail": str(exc)}, status=400)

    record, body_ast = None, None
    address = body.get("hash")
    if address:
        row = Record.objects.filter(hash=address).first()
        if row is None:
            return JsonResponse({"error": "absent", "detail": f"no record {address}"}, status=404)
        record = row.raw
        # Resolve the function's body if this node happens to hold it (bodies are usually not stored).
        body_hash = record.get("body_hash")
        if body_hash:
            brow = Record.objects.filter(hash=body_hash).first()
            if brow is not None:
                body_ast = brow.raw
    else:
        record = body.get("record")
        body_ast = body.get("body")
        if record is None:
            return JsonResponse({"error": "bad_request",
                                 "detail": "provide a stored `hash` or an inline `record`"}, status=400)

    try:
        result = run_prove(record, body_ast)
    except ProveError as exc:
        return JsonResponse({"error": exc.code, "detail": exc.detail}, status=exc.status)
    return JsonResponse(result)


@csrf_exempt
def equiv(request):
    """POST /v0/equiv — prove two functions semantically equivalent, `∀x. f(x) = g(x)` (best-effort,
    node-local; not an admission gate). Body: `{"f": <body-expr>, "g": <body-expr>}` (inline bodies —
    bodies are not stored in the commons)."""
    if request.method != "POST":
        return HttpResponseNotAllowed(["POST"])
    if len(request.body) > settings.COMMONS_MAX_RECORD_BYTES:
        return JsonResponse({"error": "too_large"}, status=413)
    try:
        body = _json_body(request)
    except ValueError as exc:
        return JsonResponse({"error": "malformed_json", "detail": str(exc)}, status=400)
    f, g = body.get("f"), body.get("g")
    if f is None or g is None:
        return JsonResponse({"error": "bad_request", "detail": "provide `f` and `g` body objects"}, status=400)
    try:
        return JsonResponse(run_equiv(f, g))
    except EquivError as exc:
        return JsonResponse({"error": exc.code, "detail": exc.detail}, status=exc.status)


def sync(request):
    """GET /v0/sync?since={cursor}&limit={n} — replication feed (hashes since a sequence cursor)."""
    if request.method != "GET":
        return HttpResponseNotAllowed(["GET"])
    try:
        since = int(request.GET.get("since", "0") or 0)
        limit = max(1, min(int(request.GET.get("limit", "500") or 500), 1000))
    except ValueError:
        return JsonResponse({"error": "bad_cursor"}, status=400)

    rows = list(Record.objects.filter(id__gt=since).order_by("id").values_list("id", "hash")[:limit])
    cursor = rows[-1][0] if rows else since
    return JsonResponse({"hashes": [h for _, h in rows], "cursor": cursor,
                         "complete": len(rows) < limit})


def info(request):
    """GET /v0/info — node metadata (peers are hints, not authority)."""
    if request.method != "GET":
        return HttpResponseNotAllowed(["GET"])
    used, budget, window = egress_usage()
    resp = JsonResponse({
        "protocol": "v0",
        "schema_versions": _SCHEMA_VERSIONS,
        "kinds": _KINDS,
        "embedding_model": get_embedder().model_id,
        "record_count": Record.objects.count(),
        "peers": settings.COMMONS_PEERS,
        "retains_messages": "durable",    # MVP keeps everything; a TTL tier comes with Redis
        # Optional /v0/prove service: which solver, and whether it's actually on PATH here (else every
        # property would report NO-SOLVER). Best-effort and node-local — advertised so clients can tell.
        "prove": {"solver": settings.COMMONS_SOLVER, "available": shutil.which(settings.COMMONS_SOLVER) is not None},
        # Egress-budget transparency (DEPLOYMENT.md): a node advertises its own cost posture so peers
        # can prefer a mirror before this one starts shedding load. budget_bytes 0 == no throttle.
        "egress": {"window": window, "used_bytes": used, "budget_bytes": budget},
    })
    resp["Cache-Control"] = "public, max-age=10"   # cheap to serve; brief cache smooths bursts
    return resp
