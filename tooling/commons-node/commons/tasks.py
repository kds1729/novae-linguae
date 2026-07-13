"""Replication worker tasks (DEPLOYMENT.md) — mirror verified records (and their blobs) from peers.

A node lists peers in ``COMMONS_PEERS`` (hints, never authority). The worker pages each peer's
``GET /v0/sync`` feed from a stored cursor, fetches records it does not already hold, and admits them
through the **same verification gate as a direct publish** (commons/verify.py). Peers are untrusted by
design: a record that does not verify is silently skipped, and a bad peer can at worst waste a little
work — it cannot corrupt the store (principle 7, self-verifying records). The blobs those records
reference (by-address example values, weights manifests) are mirrored alongside by ``replicate_blobs``
— sha256-verified per download — so a mirrored record stays *checkable*, not just resolvable, when
its origin node is gone.

Egress note: this node only *pulls*. Bounding what *others* pull from us (the bigger egress risk) is the
egress governor's job (commons/egress.py); per-peer pull volume here is bounded by COMMONS_REPLICATE_BATCH.
"""

import hashlib
import json
import urllib.request
from pathlib import Path

from celery import shared_task
from django.conf import settings
from django.core.cache import cache

from . import verify as V
from .ingest import create_record
from .models import Record


def _get_json(url, timeout=30):
    req = urllib.request.Request(url, headers={"Accept": "application/json"})
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        return json.loads(resp.read())


def _fetch_blob(url, dest_tmp, timeout=300):
    """Stream a blob to `dest_tmp`, returning its sha256 — hashed WHILE downloading, so a
    multi-hundred-MB adapter file never sits in memory."""
    h = hashlib.sha256()
    with urllib.request.urlopen(urllib.request.Request(url), timeout=timeout) as resp:
        with open(dest_tmp, "wb") as out:
            for chunk in iter(lambda: resp.read(1 << 20), b""):
                h.update(chunk)
                out.write(chunk)
    return h.hexdigest()


def _referenced_blobs(raw):
    """The sha256 blob addresses a record makes load-bearing: a function record's by-address
    example values (`examples[].result_blob.sha256`) and a weights record's file manifest
    (`files[].sha256`). These are what a replica must also hold for the mirrored record to stay
    CHECKABLE (run's example check, weights fetch) without the origin node."""
    out = set()
    if not isinstance(raw, dict):
        return out
    for ex in raw.get("examples") or []:
        blob = ex.get("result_blob") if isinstance(ex, dict) else None
        sha = blob.get("sha256") if isinstance(blob, dict) else None
        if isinstance(sha, str) and len(sha) == 64:
            out.add(sha)
    for f in raw.get("files") or []:
        sha = f.get("sha256") if isinstance(f, dict) else None
        if isinstance(sha, str) and len(sha) == 64:
            out.add(sha)
    return out


@shared_task
def replicate_peer(peer):
    """Mirror up to COMMONS_REPLICATE_BATCH new verified records from one peer. Returns a summary.

    Failure semantics matter here (found by the first full production mirror, which silently
    missed 12 records): a record that FAILS THE GATE (unverifiable, malformed, wrong address) is a
    legitimate permanent skip — a bad peer must not be able to wedge the cursor. But a TRANSIENT
    FETCH failure (timeout, connection reset mid-run) is not a judgment about the record; if the
    durable cursor advanced past it, the record would be missed forever. So fetch failures stop
    the run *without* committing the cursor past their page — the next interval retries them."""
    base = peer.rstrip("/")
    cursor_key = f"replicate_cursor:{base}"
    since = int(cache.get(cursor_key) or 0)
    remaining = settings.COMMONS_REPLICATE_BATCH
    scanned = mirrored = fetch_failures = 0

    while remaining > 0:
        limit = min(settings.COMMONS_SYNC_PER_PEER_LIMIT, remaining)
        try:
            feed = _get_json(f"{base}/v0/sync?since={since}&limit={limit}")
        except Exception:
            break  # peer unreachable this run; cursor is preserved, retry next interval
        hashes = feed.get("hashes", []) or []
        page_fetch_failed = False
        for h in hashes:
            scanned += 1
            if Record.objects.filter(hash=h).exists():
                continue
            try:
                raw = _get_json(f"{base}/v0/records/{h}")
            except Exception:
                page_fetch_failed = True  # transient — retry this page next run
                fetch_failures += 1
                continue
            try:
                kind, version, address = V.verify_record(raw)   # untrusted-peer admission gate
                if address != h:
                    continue  # the peer served different content under this address — refuse it
                if not Record.objects.filter(hash=address).exists():
                    create_record(raw, kind, version, address)
                    mirrored += 1
            except Exception:
                continue  # unverifiable / malformed / hash-mismatch — skip, do not trust the peer
        if page_fetch_failed:
            break  # do NOT commit the cursor past records we never actually saw
        since = int(feed.get("cursor", since) or since)
        cache.set(cursor_key, since, None)                      # durable cursor (no expiry)
        remaining -= len(hashes)
        if feed.get("complete") or not hashes:
            break

    return {"peer": base, "scanned": scanned, "mirrored": mirrored,
            "fetch_failures": fetch_failures, "cursor": since}


@shared_task
def replicate_blobs(peer):
    """Mirror the blobs the locally-held records reference, so a mirrored record stays CHECKABLE
    without its origin node — the blob-store half of replication (records: replicate_peer).

    Design: SELF-HEALING, no cursor. Each run scans the stored function/weights records for
    referenced blob addresses, and fetches up to COMMONS_REPLICATE_BLOB_BATCH of the missing ones
    from the peer's gate-free /v0/blobs store. Every download is sha256-verified against the
    address it was requested by (the peer is untrusted; mismatched bytes are discarded, never
    stored under a lying name). Any blob still missing — transient failure, refused bytes, budget
    exhausted — is simply re-counted next run, so nothing can be silently lost the way a
    mis-advanced cursor loses records. Blob writes are atomic (temp file + rename): a crashed
    download can never leave a half blob serving under a content address."""
    base = peer.rstrip("/")
    blob_dir = Path(settings.COMMONS_BLOB_DIR)
    blob_dir.mkdir(parents=True, exist_ok=True)
    wanted = set()
    for record in Record.objects.filter(kind__in=["function-record", "weights"]).iterator():
        wanted |= _referenced_blobs(record.raw)
    missing = sorted(sha for sha in wanted if not (blob_dir / sha).exists())
    fetched = failures = 0
    for sha in missing[: settings.COMMONS_REPLICATE_BLOB_BATCH]:
        tmp = blob_dir / f".{sha}.part"
        try:
            digest = _fetch_blob(f"{base}/v0/blobs/{sha}", tmp)
        except Exception:
            tmp.unlink(missing_ok=True)
            failures += 1  # transient (or absent on this peer) — re-counted next run
            continue
        if digest != sha:
            tmp.unlink(missing_ok=True)
            failures += 1  # the peer served different bytes under this address — refuse them
            continue
        tmp.rename(blob_dir / sha)
        fetched += 1
    return {"peer": base, "referenced": len(wanted), "missing": len(missing),
            "fetched": fetched, "failures": failures}


@shared_task
def replicate_all():
    """Run record + blob replication for every configured peer (the beat-scheduled entry point)."""
    return [{"records": replicate_peer(p), "blobs": replicate_blobs(p)}
            for p in settings.COMMONS_PEERS]


@shared_task
def embed_pending(limit=500):
    """Backfill embeddings for records missing the current model's vector (DEPLOYMENT.md async embed).

    Publishing admits records best-effort with a null embedding when the model server is busy/down; this
    task (and the `embedrecords` command) fills them in, so search becomes complete without ever having
    blocked a publish on inference."""
    from .embedding import get_embedder
    from .vectorindex import store_vector

    emb = get_embedder()
    # Tiered bodies (blob-backed pointer rows) embed at ingest from the real content; their stored
    # `raw` is a stub, so backfill skips them rather than embed the stub.
    chunk = list(Record.objects.exclude(embedding_model=emb.model_id)
                 .filter(blob_sha256__isnull=True).order_by("id")[:limit])
    if not chunk:
        return {"embedded": 0, "model": emb.model_id}
    vectors = emb.embed_batch([r.raw for r in chunk])
    for r, v in zip(chunk, vectors):
        r.embedding = v
        r.embedding_model = emb.model_id
    Record.objects.bulk_update(chunk, ["embedding", "embedding_model"])
    for r in chunk:
        store_vector(r.hash, r.embedding)
    return {"embedded": len(chunk), "model": emb.model_id}
