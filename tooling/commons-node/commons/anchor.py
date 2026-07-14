"""Provenance anchoring (commons.md open question 2) — signed Merkle roots of the corpus.

An **anchor** is a small signed statement of what this node held at a moment: the corpus's Merkle
root (the same order-independent set digest `/v0/sync/merkle` serves at prefix ""), the record
count, and a timestamp — signed with the node's anchor identity (Ed25519, the bundle-manifest
construction, so `nl_crypto.verify_manifest` checks it). Anchors make retroactive tampering
TAMPER-EVIDENT: whoever holds an anchor can later recompute the root from a mirror, a bundle, or
the live `/v0/sync/merkle` walk and compare.

Honest scope: the anchor's value comes from landing somewhere the node cannot rewrite — an
external append-only log (a public git repo, a transparency log, a blockchain if desired). That
half is the OPERATOR'S choice by design; this module supplies the signed statements
(`manage.py anchorcorpus` prints one for piping into whatever log the operator trusts), keeps the
node's own history (`GET /v0/anchors` — useful, but a node can rewrite its own table, which is
exactly why the external copy matters), and the beat task emits one whenever the root has moved.
Anchoring is an auditability ADD-ON, never the store itself, and is disabled without an
`COMMONS_ANCHOR_SEED`.
"""

import datetime

from django.conf import settings

from .bundle import _crypto
from .merkle import set_digest
from .models import Anchor, Record

FORMAT_VERSION = "nl-anchor/1"


def build_anchor(at=None):
    """The signed anchor statement for the CURRENT corpus. Raises if anchoring is not configured."""
    seed = settings.COMMONS_ANCHOR_SEED
    if not seed:
        raise RuntimeError("anchoring is not configured (set COMMONS_ANCHOR_SEED)")
    hashes = list(Record.objects.values_list("hash", flat=True))
    statement = {
        "format_version": FORMAT_VERSION,
        "root": set_digest(hashes),
        "count": len(hashes),
        "at": at or datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds"),
    }
    # The bundle-manifest signing construction: canonical JSON minus `signature`, Ed25519, and a
    # `producer` did:nova — one signature scheme across every provenance surface.
    return _crypto().sign_manifest(statement, seed)


def record_anchor(force=False):
    """Emit-and-store an anchor iff the corpus root has moved since the last one (or `force`).
    Returns the stored payload, or None when anchoring is disabled / the root is unchanged."""
    if not settings.COMMONS_ANCHOR_SEED:
        return None
    payload = build_anchor()
    last = Anchor.objects.order_by("-id").first()
    if last and last.root == payload["root"] and not force:
        return None
    Anchor.objects.create(root=payload["root"], count=payload["count"], payload=payload)
    return payload
