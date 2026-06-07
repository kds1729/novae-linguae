"""Typed discovery — translate a query filter (spec/commons.md `POST /v0/query`) to results.

Scalar fields (kind, schema_version, terminates, type substring) filter in the database. The array
predicates (effects/capabilities/intent_tags membership, name-hint prefix) are confirmed in Python by
``_array_ok`` on every backend — that is the obviously-correct source of truth. On Postgres they are
*also* pushed into the database (GIN-indexed JSONB `@>` containment, migration 0004) to narrow the
scan before the Python pass; on SQLite the pushdown is skipped and the Python pass does all the work.
Pagination is by the `id` sequence cursor.
"""

import functools
import operator

from django.db import connection
from django.db.models import Q

from .models import Record

_DB_SCAN_CAP = 2000  # bound the per-request scan for the MVP


def _array_ok(record, flt):
    eff = flt.get("effects")
    if eff:
        rec_eff = set(record.effects)
        if eff.get("none") and rec_eff:
            return False
        if "subset_of" in eff and not rec_eff.issubset(set(eff["subset_of"])):
            return False
        if "all" in eff and not set(eff["all"]).issubset(rec_eff):
            return False
        if "any" in eff and not (set(eff["any"]) & rec_eff):
            return False

    cap = flt.get("capabilities")
    if cap:
        rec_cap = set(record.capabilities)
        if cap.get("none") and rec_cap:
            return False
        if "all" in cap and not set(cap["all"]).issubset(rec_cap):
            return False
        if "any" in cap and not (set(cap["any"]) & rec_cap):
            return False

    tags = flt.get("intent_tags")
    if tags:
        rec_tags = set(record.intent_tags)
        if "all" in tags and not set(tags["all"]).issubset(rec_tags):
            return False
        if "any" in tags and not (set(tags["any"]) & rec_tags):
            return False

    prefix = flt.get("name_hint_prefix")
    if prefix and not any(n.startswith(prefix) for n in record.name_hints):
        return False
    return True


def _pushdown_array(qs, field, pred):
    """Narrow `qs` by a single array predicate using GIN-indexable JSONB lookups (Postgres only).

    Conservative: applies only containment forms that `_array_ok` also enforces, so this can only ever
    *shrink* the candidate set — the Python post-filter still has the final say. `any` becomes an OR of
    single-element `@>` containments (each GIN-indexed); `all` a single `@>`; `subset_of`/`none` map to
    `<@`/empty. Unknown shapes are left for the Python pass."""
    if not isinstance(pred, dict):
        return qs
    if pred.get("none"):
        qs = qs.filter(**{field: []})
    if isinstance(pred.get("all"), list) and pred["all"]:
        qs = qs.filter(**{f"{field}__contains": pred["all"]})
    if isinstance(pred.get("any"), list) and pred["any"]:
        qs = qs.filter(functools.reduce(operator.or_,
                                         (Q(**{f"{field}__contains": [x]}) for x in pred["any"])))
    if isinstance(pred.get("subset_of"), list):
        qs = qs.filter(**{f"{field}__contained_by": pred["subset_of"]})
    return qs


def _scalar_qs(flt):
    """Queryset with the DB-side predicates of a typed filter applied, ordered by id.

    Always applies the scalar predicates; on Postgres also pushes the array predicates into the database
    (the Python `_array_ok` pass still confirms every row, so correctness does not depend on this)."""
    qs = Record.objects.all().order_by("id")
    if "kind" in flt:
        qs = qs.filter(kind=flt["kind"])
    if "schema_version" in flt:
        qs = qs.filter(schema_version=flt["schema_version"])
    if "terminates" in flt:
        t = flt["terminates"]
        qs = qs.filter(terminates__in=(t if isinstance(t, list) else [t]))
    if flt.get("type_contains"):
        qs = qs.filter(type_str__icontains=flt["type_contains"])
    if connection.vendor == "postgresql":
        for field in ("effects", "capabilities", "intent_tags"):
            if flt.get(field):
                qs = _pushdown_array(qs, field, flt[field])
    return qs


def candidate_records(flt, cap=_DB_SCAN_CAP):
    """Records matching a typed filter (scalar + array predicates), no pagination. Bounded scan.

    Returns (records, truncated). Shared by run_query and semantic search (search.run_search) so a
    typed `filter` means exactly the same thing in both endpoints.
    """
    scanned = list(_scalar_qs(flt)[:cap])
    matched = [r for r in scanned if _array_ok(r, flt)]
    return matched, len(scanned) >= cap


def run_query(flt):
    """Return (hashes, cursor, complete) for a query filter."""
    qs = _scalar_qs(flt)

    cursor = flt.get("cursor")
    if cursor is not None:
        qs = qs.filter(id__gt=int(cursor))

    try:
        limit = max(1, min(int(flt.get("limit", 100)), 1000))
    except (TypeError, ValueError):
        limit = 100

    scanned = list(qs[:_DB_SCAN_CAP])
    matched = [r for r in scanned if _array_ok(r, flt)]
    page = matched[:limit]

    hashes = [r.hash for r in page]
    next_cursor = str(page[-1].id) if page else cursor
    # "complete" iff this scan reached the end of the corpus (not truncated by the scan cap) and
    # we did not fill the page (so there is nothing obvious left to return).
    complete = len(scanned) < _DB_SCAN_CAP and len(page) == len(matched)
    return hashes, next_cursor, complete
