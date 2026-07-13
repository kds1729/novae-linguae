"""Canonical in-language iteration records — the `json_get` precedent applied to counted
iteration and list indexing (spec/expressiveness.md, statement-subset frontier).

`nth` / `range_from` / `range` are ordinary certified commons records in ``spec/examples``
(self-recursive, pure, no new builtin, no ``alloc`` gate — the iteration structure is data built
by structural recursion, the language's own idiom). Ingested bodies reference them BY
CONTENT-ADDRESS (an applied ``fn_ref`` literal), so an adapter that emits such a body must ship
the referenced records alongside it — ``canonical_dependency_artifacts`` is that bundle.

The hashes are PINNED: a drifted ``spec/examples`` fails loudly at load time rather than
emitting bodies that resolve to different code.
"""
from __future__ import annotations

import json
from pathlib import Path

NTH_HASH = "fn_a66c7c1dc7da034766da855e747238b863345540950369c6661ee1573a47bb0e"
RANGE_FROM_HASH = "fn_fdd603455dd8e7512fb54075c0e764eb95e56885e717978148744658d2b244c1"
RANGE_HASH = "fn_f983d969eaa348a105d4936f7a946c8256898cde78cabacb4c66dae153c99788"

_EXAMPLES = Path(__file__).resolve().parent.parent.parent / "spec" / "examples"
_CANON = (("nth", NTH_HASH), ("range-from", RANGE_FROM_HASH), ("range", RANGE_HASH))


def canonical_dependency_artifacts() -> list[tuple[str, dict]]:
    """``[(filename, artifact), …]`` — the canonical records and their bodies, named by
    content-address, for adapters to write into their output directory so emitted ``fn_ref``s
    link (``nl-validator run --records <out>``)."""
    out: list[tuple[str, dict]] = []
    for name, pinned in _CANON:
        record = json.loads((_EXAMPLES / f"{name}.v0.2.json").read_text(encoding="utf-8"))
        body = json.loads((_EXAMPLES / f"body-{name}.json").read_text(encoding="utf-8"))
        if record["hash"] != pinned:
            raise RuntimeError(
                f"canonical record {name} drifted: spec/examples has {record['hash']}, "
                f"the adapters pin {pinned}")
        out.append((f"{record['hash']}.json", record))
        out.append((f"{record['body_hash']}.json", body))
    return out
