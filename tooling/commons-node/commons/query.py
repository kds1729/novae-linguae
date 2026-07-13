"""Typed discovery — translate a query filter (spec/commons.md `POST /v0/query`) to results.

Scalar fields (kind, schema_version, terminates, type substring) filter in the database. The array
predicates (effects/capabilities/intent_tags membership, name-hint prefix) are confirmed in Python by
``_array_ok`` on every backend — that is the obviously-correct source of truth. On Postgres they are
*also* pushed into the database (GIN-indexed JSONB `@>` containment, migration 0004) to narrow the
scan before the Python pass; on SQLite the pushdown is skipped and the Python pass does all the work.
Pagination is by the `id` sequence cursor.
"""

import functools
import json
import operator

from django.db import connection
from django.db.models import Q

from .models import Record
from . import typematch

_DB_SCAN_CAP = 2000  # bound the per-request scan for the MVP
_CHARS_PER_TOKEN = 4  # rough, tokenizer-free heuristic; the budget is an estimate, documented as such

# Fields whose value must be an object predicate, and the predicate keys they accept.
_ARRAY_FIELDS = ("effects", "capabilities", "intent_tags")
_ARRAY_PRED_KEYS = {"all", "any", "none", "subset_of"}


class QueryError(ValueError):
    """A malformed typed-query filter — surfaced as HTTP 400, not a 500."""


def validate_filter(flt):
    """Raise QueryError if `flt` is malformed. Array predicates (effects / capabilities /
    intent_tags) must be objects — e.g. ``{"all": [...]}`` / ``{"any": [...]}`` /
    ``{"subset_of": [...]}`` / ``{"none": true}`` — never a bare list, so a wrong shape is a clean
    400 instead of an AttributeError 500. Returns `flt` unchanged on success."""
    if not isinstance(flt, dict):
        raise QueryError("query filter must be a JSON object")
    for field in _ARRAY_FIELDS:
        if field not in flt:
            continue
        pred = flt[field]
        if not isinstance(pred, dict):
            raise QueryError(
                f'`{field}` must be an object predicate like {{"all": [...]}} / {{"any": [...]}}, '
                f"not {type(pred).__name__}"
            )
        for key, val in pred.items():
            if key not in _ARRAY_PRED_KEYS:
                raise QueryError(f"`{field}` has unknown predicate key `{key}` "
                                 f"(allowed: {', '.join(sorted(_ARRAY_PRED_KEYS))})")
            if key == "none":
                if not isinstance(val, bool):
                    raise QueryError(f"`{field}.none` must be a boolean")
            elif not isinstance(val, list):
                raise QueryError(f"`{field}.{key}` must be an array")
    if "type_pattern" in flt:
        try:
            typematch.validate_pattern(flt["type_pattern"])
        except typematch.PatternError as exc:
            raise QueryError(str(exc))
    if "token_budget" in flt:
        check_token_budget(flt["token_budget"])
    return flt


def check_token_budget(tb):
    """Raise QueryError unless `tb` is a positive integer. Shared by `/v0/query` (filter field) and
    `/v0/search` (body field) so a bad budget is a clean 400 on either endpoint. `bool` is an `int`
    subclass, so reject it explicitly — a JSON `true` must not be read as the budget `1`."""
    if isinstance(tb, bool) or not isinstance(tb, int) or tb < 1:
        raise QueryError("`token_budget` must be a positive integer")


def _array_ok(record, flt):
    # Defensive: only honor an array predicate when it is the documented object shape, so even an
    # unvalidated caller (e.g. best-effort search) can never crash here — a malformed predicate is
    # simply not applied. `validate_filter` rejects malformed filters up front for `/v0/query`.
    eff = flt.get("effects")
    if isinstance(eff, dict):
        rec_eff = set(record.effects)
        if eff.get("none") and rec_eff:
            return False
        if isinstance(eff.get("subset_of"), list) and not rec_eff.issubset(set(eff["subset_of"])):
            return False
        if isinstance(eff.get("all"), list) and not set(eff["all"]).issubset(rec_eff):
            return False
        if isinstance(eff.get("any"), list) and not (set(eff["any"]) & rec_eff):
            return False

    cap = flt.get("capabilities")
    if isinstance(cap, dict):
        rec_cap = set(record.capabilities)
        if cap.get("none") and rec_cap:
            return False
        if isinstance(cap.get("all"), list) and not set(cap["all"]).issubset(rec_cap):
            return False
        if isinstance(cap.get("any"), list) and not (set(cap["any"]) & rec_cap):
            return False

    tags = flt.get("intent_tags")
    if isinstance(tags, dict):
        rec_tags = set(record.intent_tags)
        if isinstance(tags.get("all"), list) and not set(tags["all"]).issubset(rec_tags):
            return False
        if isinstance(tags.get("any"), list) and not (set(tags["any"]) & rec_tags):
            return False

    prefix = flt.get("name_hint_prefix")
    if prefix and not any(n.startswith(prefix) for n in record.name_hints):
        return False

    # Structured type matching (spec/commons.md `type_pattern`): unification against the stored
    # v0.2 type AST. Runs in the Python confirm pass — it has no DB pushdown (the AST lives in a
    # text column), so like the array predicates it narrows the bounded scan, exactly and portably.
    pattern = flt.get("type_pattern")
    if isinstance(pattern, dict) and not typematch.matches_type(pattern, record.type_str):
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


def record_summary(record):
    """A compact projection of a stored record — the fields an agent needs to JUDGE relevance and
    signature-compatibility (type, effects, capabilities, intent tags, termination, complexity,
    certification) WITHOUT resolving the full record (body, examples, properties, proof certificates).

    This is the **discovery-cost** lever (spec/commons.md open problem): one `?include=summary` response
    lets a client rank a whole candidate set in a single round-trip, instead of fetching N full records
    just to read their signatures. All summary fields are *extracted columns* — computed on ingest — so
    this reparses nothing. Empty/absent fields are omitted to keep the projection small; `hash` and `kind`
    are always present."""
    out = {"hash": record.hash, "kind": record.kind}
    if record.name_hints:
        out["name_hints"] = record.name_hints
    if record.type_str:
        out["type"] = record.type_str
    if record.effects:
        out["effects"] = record.effects
    if record.capabilities:
        out["capabilities"] = record.capabilities
    if record.intent_tags:
        out["intent_tags"] = record.intent_tags
    if record.terminates:
        out["terminates"] = record.terminates
    if record.complexity:
        out["complexity"] = record.complexity
    if record.certified is not None:
        out["certified"] = record.certified
    if record.body_hash:
        out["body_hash"] = record.body_hash
    return out


def _relevance(record, flt):
    """A node-local relevance score for a typed-query hit. Every hit *satisfies* the filter equally
    (typed query is exact), so pure boolean matching discards a real signal: how well each hit fits the
    filter's SOFT preferences. This scores that —
      • **intent fit**: the count of requested `intent_tags.any` the record actually carries (a record
        matching two of the requested tags outranks one matching a single tag; the dominant term);
      • **name primacy**: a `name_hint_prefix` that matches the record's *primary* name (hint 0) outranks
        one that matches a later alias (`1/(i+1)`, ≤ 1);
      • **certification**: a small boost for a verified record (agents assembling prefer certified).
    Higher is better; the caller breaks ties by `id` (stable, insertion order). Heuristic and node-local
    like `search` — it re-orders the exact set, it does not change it."""
    score = 0.0
    tags = flt.get("intent_tags")
    if isinstance(tags, dict) and isinstance(tags.get("any"), list):
        score += len(set(tags["any"]) & set(record.intent_tags))
    prefix = flt.get("name_hint_prefix")
    if prefix:
        for i, hint in enumerate(record.name_hints):
            if hint.startswith(prefix):
                score += 1.0 / (i + 1)
                break
    if record.certified:
        score += 0.5
    return score


def summary_tokens(summary):
    """Estimate the token cost of a compact summary — its canonical-JSON byte length divided by a fixed
    chars-per-token factor. Tokenizer-free (no model dependency) and therefore an ESTIMATE: it is meant
    to let a client budget a discovery round-trip in the same unit its context window is measured in, not
    to be exact. Never returns 0, so every included item advances the budget."""
    return max(1, len(json.dumps(summary, separators=(",", ":"), sort_keys=True)) // _CHARS_PER_TOKEN)


def greedy_budget(items, cost_of, token_budget, total):
    """Greedily keep `items` (in their given order — id/ranked/similarity) until the next would overrun
    `token_budget`, using `cost_of(item)` for each item's token cost. Returns (kept, budget_report).

    The token budget is the honest discovery-cost cap: a count limit bounds the RESULT COUNT, but summaries
    vary in size (a long type string and many intent tags cost more than a bare scalar), so a client with a
    fixed context window wants "as many results as fit in T tokens", not a count. The top (most relevant)
    item is ALWAYS kept even if it alone exceeds the budget, so a small budget still yields the best
    candidate rather than an empty page — `tokens_estimated` then reports the overrun. `total` is the size
    of the full candidate set this page was drawn from, so `more` reflects results dropped by the budget."""
    kept, used = [], 0
    for it in items:
        cost = cost_of(it)
        if kept and used + cost > token_budget:
            break
        used += cost
        kept.append(it)
    report = {"token_budget": token_budget, "tokens_estimated": used, "returned": len(kept),
              "more": len(kept) < total}
    return kept, report


def _pack_budget(page, matched_total, token_budget):
    """Budget-trim a page of matched Record rows for `/v0/query` (measures each row's compact summary)."""
    return greedy_budget(page, lambda r: summary_tokens(record_summary(r)), token_budget, matched_total)


def run_query(flt, rank=False, budget=False):
    """Return (hashes, cursor, complete, budget_report) for a query filter. Raises QueryError on a
    malformed filter. `budget_report` is None unless a `token_budget` cap was applied.

    With ``rank=True`` the matched candidate set (bounded scan) is ordered by [`_relevance`] instead of by
    `id`, and the single best-`limit` page is returned (`cursor` is `None`): relevance re-orders the set,
    so id-cursor pagination doesn't apply — ranking is a "surface the best few" view, not a paged feed.

    With ``budget=True`` and a `token_budget` in the filter, the count-limited page is further trimmed to
    the summaries that fit the token budget (see [`_pack_budget`]) — the discovery-cost cap for the
    `?include=summary` tier. The cursor then points past the last INCLUDED record, so pagination resumes
    where the budget cut off."""
    validate_filter(flt)
    qs = _scalar_qs(flt)

    try:
        limit = max(1, min(int(flt.get("limit", 100)), 1000))
    except (TypeError, ValueError):
        limit = 100
    token_budget = flt.get("token_budget") if budget else None

    if rank:
        scanned = list(qs[:_DB_SCAN_CAP])
        matched = [r for r in scanned if _array_ok(r, flt)]
        matched.sort(key=lambda r: (-_relevance(r, flt), r.id))
        page = matched[:limit]
        report = None
        if token_budget is not None:
            page, report = _pack_budget(page, len(matched), token_budget)
        return [r.hash for r in page], None, len(scanned) < _DB_SCAN_CAP, report

    cursor = flt.get("cursor")
    if cursor is not None:
        qs = qs.filter(id__gt=int(cursor))

    scanned = list(qs[:_DB_SCAN_CAP])
    matched = [r for r in scanned if _array_ok(r, flt)]
    page = matched[:limit]
    report = None
    if token_budget is not None:
        page, report = _pack_budget(page, len(matched), token_budget)

    hashes = [r.hash for r in page]
    next_cursor = str(page[-1].id) if page else cursor
    # "complete" iff this scan reached the end of the corpus (not truncated by the scan cap) and
    # we returned every matched row (so there is nothing obvious left — a budget-trimmed page is not).
    complete = len(scanned) < _DB_SCAN_CAP and len(page) == len(matched)
    return hashes, next_cursor, complete, report
