"""Anchor cross-node WITNESSING (commons.md open question 2, the federated half).

An anchor's tamper-evidence comes from a copy the origin node cannot rewrite. `anchor.py`
supplies the operator's half (pipe the signed statement into an external append-only log); this
module makes OTHER NODES that log: a peer fetches the origin's `GET /v0/anchors`, verifies each
anchor's Ed25519 signature, checks ROOT AGREEMENT where it can — when the witness's own
replicated corpus computes the same Merkle root, agreement is a verified fact, not testimony —
and COUNTERSIGNS with its own anchor identity. The witness statement embeds the origin's full
signed anchor verbatim, so a third party verifies both signatures independently and needs
neither node's honesty: an origin that later rewrites its `/v0/anchors` history is contradicted
by every witness that countersigned the original.

Composition with replication is the point: a replica already converges on the origin's record
set (`replicate_peer` + `reconcile_peer`), so in steady state its local root EQUALS the origin's
anchored root and witnessing upgrades from "unverified" (signature seen, set not yet compared)
to "root-matched" (the witness independently held the same corpus). The witness log is
append-only in spirit — an upgrade appends a second statement, it never rewrites the first.

Same gate as anchoring: disabled without COMMONS_ANCHOR_SEED (the witness must have an identity
to countersign with)."""

import datetime

from django.conf import settings

from .bundle import _crypto, verify_manifest
from .merkle import set_digest
from .models import Record, Witness

FORMAT_VERSION = "nl-witness/1"

AGREEMENT_MATCHED = "root-matched"
AGREEMENT_UNVERIFIED = "unverified"


def witness_statement(origin, anchor_payload, agreement, at=None):
    """The signed witness statement over a VERIFIED origin anchor (embedded verbatim, so the
    origin's own signature stays checkable inside the countersignature). Raises when witnessing
    is not configured."""
    seed = settings.COMMONS_ANCHOR_SEED
    if not seed:
        raise RuntimeError("witnessing is not configured (set COMMONS_ANCHOR_SEED)")
    statement = {
        "format_version": FORMAT_VERSION,
        "origin": origin,
        "anchor": anchor_payload,
        "agreement": agreement,
        "at": at or datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds"),
    }
    return _crypto().sign_manifest(statement, seed)


def witness_peer_anchors(origin, anchors):
    """Verify + countersign a peer's anchor list (already fetched — the task does the HTTP).

    Per anchor: an invalid/unsigned signature is SKIPPED (never countersigned); a valid one is
    countersigned once per (origin, producer, root) — except that an "unverified" witness may
    later gain a "root-matched" companion when the replicated corpus catches up (append, never
    rewrite). Returns a summary dict for the task log."""
    if not settings.COMMONS_ANCHOR_SEED:
        return {"enabled": False}
    local_root = set_digest(list(Record.objects.values_list("hash", flat=True)))
    witnessed = invalid = already = malformed = 0
    for payload in anchors:
        if not isinstance(payload, dict):
            malformed += 1
            continue
        status, producer = verify_manifest(payload)
        if status != "valid":
            invalid += 1
            continue
        root = payload.get("root")
        if not isinstance(root, str) or not root:
            malformed += 1
            continue
        agreement = AGREEMENT_MATCHED if root == local_root else AGREEMENT_UNVERIFIED
        existing = Witness.objects.filter(origin=origin, producer=producer, root=root)
        if existing.filter(agreement=AGREEMENT_MATCHED).exists() or (
            agreement == AGREEMENT_UNVERIFIED and existing.exists()
        ):
            already += 1
            continue
        stmt = witness_statement(origin, payload, agreement)
        Witness.objects.create(origin=origin, producer=producer, root=root,
                               agreement=agreement, payload=stmt)
        witnessed += 1
    return {"enabled": True, "witnessed": witnessed, "already": already,
            "invalid": invalid, "malformed": malformed}
