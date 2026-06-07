"""Replication worker tasks (DEPLOYMENT.md) — mirror verified records from peers.

A node lists peers in ``COMMONS_PEERS`` (hints, never authority). The worker pages each peer's
``GET /v0/sync`` feed from a stored cursor, fetches records it does not already hold, and admits them
through the **same verification gate as a direct publish** (commons/verify.py). Peers are untrusted by
design: a record that does not verify is silently skipped, and a bad peer can at worst waste a little
work — it cannot corrupt the store (principle 7, self-verifying records).

Egress note: this node only *pulls*. Bounding what *others* pull from us (the bigger egress risk) is the
egress governor's job (commons/egress.py); per-peer pull volume here is bounded by COMMONS_REPLICATE_BATCH.
"""

import json
import urllib.request

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


@shared_task
def replicate_peer(peer):
    """Mirror up to COMMONS_REPLICATE_BATCH new verified records from one peer. Returns a summary."""
    base = peer.rstrip("/")
    cursor_key = f"replicate_cursor:{base}"
    since = int(cache.get(cursor_key) or 0)
    remaining = settings.COMMONS_REPLICATE_BATCH
    scanned = mirrored = 0

    while remaining > 0:
        limit = min(settings.COMMONS_SYNC_PER_PEER_LIMIT, remaining)
        try:
            feed = _get_json(f"{base}/v0/sync?since={since}&limit={limit}")
        except Exception:
            break  # peer unreachable this run; cursor is preserved, retry next interval
        hashes = feed.get("hashes", []) or []
        for h in hashes:
            scanned += 1
            if Record.objects.filter(hash=h).exists():
                continue
            try:
                raw = _get_json(f"{base}/v0/records/{h}")
                kind, version = V.verify_record(raw)            # untrusted-peer admission gate
                if not Record.objects.filter(hash=raw["hash"]).exists():
                    create_record(raw, kind, version)
                    mirrored += 1
            except Exception:
                continue  # unverifiable / malformed / hash-mismatch — skip, do not trust the peer
        since = int(feed.get("cursor", since) or since)
        cache.set(cursor_key, since, None)                      # durable cursor (no expiry)
        remaining -= len(hashes)
        if feed.get("complete") or not hashes:
            break

    return {"peer": base, "scanned": scanned, "mirrored": mirrored, "cursor": since}


@shared_task
def replicate_all():
    """Run replicate_peer for every configured peer (the beat-scheduled entry point)."""
    return [replicate_peer(p) for p in settings.COMMONS_PEERS]


@shared_task
def embed_pending(limit=500):
    """Backfill embeddings for records missing the current model's vector (DEPLOYMENT.md async embed).

    Publishing admits records best-effort with a null embedding when the model server is busy/down; this
    task (and the `embedrecords` command) fills them in, so search becomes complete without ever having
    blocked a publish on inference."""
    from .embedding import get_embedder
    from .vectorindex import store_vector

    emb = get_embedder()
    chunk = list(Record.objects.exclude(embedding_model=emb.model_id).order_by("id")[:limit])
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
