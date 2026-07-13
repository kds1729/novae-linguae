"""Merkle set reconciliation for replication (`GET /v0/sync/merkle`, commons.md open question 1).

The cursor feed (`/v0/sync`) answers "what came after position N" — perfect for tailing a peer, but
it cannot answer "do we hold the same set?" without re-walking everything, and a cursor mis-step
(the class of bug the 2026-07-11 replication fix closed) leaves holes no amount of tailing finds.
This module answers set equality in one request and locates any divergence in O(log n) requests:

- Every record's content-address has a uniformly-distributed hex part (it IS a cryptographic
  hash), so the address space partitions naturally into a 16-ary trie over those hex nibbles —
  no derived column, no re-hashing.
- A node's digest is the bundle-digest construction (blake2b over the sorted address set), so it
  is order-independent and matches across implementations. Equal digest ⇒ equal set (up to hash
  collision); the reconciler descends only differing children and reads address LISTS only at
  small leaves.

Honest scope: the tree is an EFFICIENCY HINT, not a trust surface. Every located-missing record is
still fetched and admitted through the same verify-then-store gate as a direct publish, so a lying
digest can waste a little work or withhold records — exactly what a lying cursor feed could already
do (principle 7: peers are hints, never authority). The reference implementation buckets in memory
from a single hash scan per request — right for reference-node scale; a large node would maintain
a derived nibble-prefix column instead. The wire shape is the contract, not the implementation.
"""

import hashlib

from .models import Record

# Below this many addresses a node returns the address list itself — the reconciler diffs it
# directly instead of descending further.
LEAF_LIMIT = 64

_HEX = "0123456789abcdef"


class MerkleError(ValueError):
    """A malformed prefix — surfaced as HTTP 400."""


def _hex_part(address):
    """The uniformly-distributed hex tail of a content-address (`fn_ab12…` -> `ab12…`)."""
    _, _, hexpart = address.partition("_")
    return hexpart


def set_digest(addresses):
    """Order-independent digest of an address SET — the bundle_digest construction, shared so any
    correct implementation computes the same value."""
    h = hashlib.blake2b(digest_size=32)
    for a in sorted(addresses):
        h.update(a.encode("utf-8"))
        h.update(b"\n")
    return "blake2b:" + h.hexdigest()


def validate_prefix(prefix):
    if not isinstance(prefix, str) or len(prefix) > 64 or any(c not in _HEX for c in prefix):
        raise MerkleError("prefix must be 0-64 lowercase hex nibbles")
    return prefix


def merkle_node(prefix, addresses=None):
    """The tree node at `prefix`: its set digest + count, and either the 16 children's
    digests/counts (only non-empty children are listed) or — at or below LEAF_LIMIT — the sorted
    address list itself. `addresses` lets tests (and the reconciler's local half) supply a set;
    the default reads the store."""
    validate_prefix(prefix)
    if addresses is None:
        addresses = Record.objects.values_list("hash", flat=True)
    subset = [a for a in addresses if _hex_part(a).startswith(prefix)]
    node = {"prefix": prefix, "digest": set_digest(subset), "count": len(subset)}
    if len(subset) <= LEAF_LIMIT:
        node["hashes"] = sorted(subset)
        return node
    children = {}
    for a in subset:
        nib = _hex_part(a)[len(prefix):][:1]
        children.setdefault(nib, []).append(a)
    node["children"] = {
        nib: {"digest": set_digest(sub), "count": len(sub)}
        for nib, sub in sorted(children.items())
    }
    return node
