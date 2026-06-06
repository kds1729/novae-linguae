"""Typed discovery — translate a query filter (spec/commons.md `POST /v0/query`) to results.

Scalar fields (kind, schema_version, terminates, type substring) filter in the database; the array
predicates (effects/capabilities/intent_tags membership, name-hint prefix) are applied in Python.
On a Postgres backend these array predicates become GIN-indexed `contains`/overlap queries; doing
them in Python here keeps the SQLite MVP simple and obviously correct. Pagination is by the `id`
sequence cursor.
"""

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


def _scalar_qs(flt):
    """Queryset with the scalar (DB-side) predicates of a typed filter applied, ordered by id."""
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
