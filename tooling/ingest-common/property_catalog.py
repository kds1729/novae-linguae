"""Curated algebraic-law catalog (property_catalog.json) + matcher.

Ingestion cannot DERIVE algebraic laws from source — but it can recognise well-known functions and
attach their standard laws. `match_catalog(name_hints, arity)` returns the laws whose match keys fit
a function, for the adapters' opt-in `--properties` flag. Attached laws are then checkable with
`nl-validator check-properties` against the record's worked examples, so a mis-match that contradicts
an example is caught rather than silently trusted.

Provenance note: the v0.2 `property` object forbids extra fields, so a catalog law carries its
provenance via (a) the deterministic catalog `id` (external) and (b) the unioned modifier intent_tag
(in-record). A first-class `property.source` field is a v0.3 schema item.
"""

import json
from pathlib import Path

_CATALOG_PATH = Path(__file__).resolve().parent / "property_catalog.json"
_CATALOG = None


def _load():
    global _CATALOG
    if _CATALOG is None:
        _CATALOG = json.loads(_CATALOG_PATH.read_text(encoding="utf-8"))
    return _CATALOG


def match_catalog(name_hints, arity=None):
    """(properties, intent_tags) for laws that fit. A law matches when one of its `name_hints` is
    among the record's `name_hints` and its `arity` (if set) equals `arity`. Properties are
    de-duplicated by name; intent_tags by value, in catalog order."""
    hints = set(name_hints or [])
    props, tags, seen = [], [], set()
    for law in _load().get("laws", []):
        m = law.get("match", {})
        if not (hints & set(m.get("name_hints", []))):
            continue
        if m.get("arity") is not None and arity is not None and m["arity"] != arity:
            continue
        prop = law["property"]
        if prop["name"] not in seen:
            props.append(prop)
            seen.add(prop["name"])
        tag = law.get("intent_tag")
        if tag and tag not in tags:
            tags.append(tag)
    return props, tags
