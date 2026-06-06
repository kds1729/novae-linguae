"""HTTP views implementing the commons protocol (spec/commons.md, /v0/ binding)."""

import json

from django.conf import settings
from django.http import HttpResponse, HttpResponseNotAllowed, JsonResponse
from django.views.decorators.csrf import csrf_exempt

from . import verify as V
from .embedding import get_embedder
from .ingest import create_record
from .models import Record
from .query import run_query
from .search import run_search, SearchError

_SCHEMA_VERSIONS = ["0.1.0", "0.2.0"]
_KINDS = ["function-record", "message", "body", "type"]

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
        kind, version = V.verify_record(raw)
    except V.VerifyError as exc:
        if exc.code == "verifier_unavailable":
            return JsonResponse({"error": exc.code, "detail": exc.detail}, status=503)
        status = 422 if exc.code in _UNPROCESSABLE else 400
        return JsonResponse({"error": exc.code, "detail": exc.detail}, status=status)

    address = raw["hash"]
    if Record.objects.filter(hash=address).exists():
        return JsonResponse({"hash": address, "stored": False}, status=200)

    create_record(raw, kind, version)
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
def query(request):
    """POST /v0/query — typed (exact, portable) discovery."""
    if request.method != "POST":
        return HttpResponseNotAllowed(["POST"])
    try:
        flt = _json_body(request)
    except ValueError as exc:
        return JsonResponse({"error": "malformed_json", "detail": str(exc)}, status=400)

    hashes, cursor, complete = run_query(flt)
    if request.GET.get("include") == "record":
        by_hash = {r.hash: r.raw for r in Record.objects.filter(hash__in=hashes)}
        return JsonResponse({"records": [by_hash[h] for h in hashes if h in by_hash],
                             "cursor": cursor, "complete": complete})
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
    try:
        results, model_id, truncated = run_search(body)
    except SearchError as exc:
        return JsonResponse({"error": exc.code, "detail": exc.detail}, status=exc.status)
    payload = {"results": results, "model": model_id}
    if truncated:
        payload["truncated"] = True   # scan cap hit; some records were not ranked (MVP bound)
    return JsonResponse(payload)


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
    resp = JsonResponse({
        "protocol": "v0",
        "schema_versions": _SCHEMA_VERSIONS,
        "kinds": _KINDS,
        "embedding_model": get_embedder().model_id,
        "record_count": Record.objects.count(),
        "peers": settings.COMMONS_PEERS,
        "retains_messages": "durable",    # MVP keeps everything; a TTL tier comes with Redis
    })
    resp["Cache-Control"] = "public, max-age=10"   # cheap to serve; brief cache smooths bursts
    return resp
