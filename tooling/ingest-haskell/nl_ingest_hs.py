#!/usr/bin/env python3
"""nl-ingest-hs: parse exported Haskell functions and emit Nova Lingua v0.1 function records.

Each exported top-level function that carries a type signature in the given ``.hs`` files
becomes one JSON record on stdout (JSONL by default, ``--pretty`` for readable). Records satisfy
``function-record.schema.json``; ``hash`` and ``body_hash`` are real BLAKE3-256 content-addresses
computed exactly as ``spec/canonical-serialization.md`` prescribes, so every record agrees
byte-for-byte with the Rust ``nl-validator`` (and with the Rust/Python ingestion adapters).

This is a *lightweight* front end: it reads top-level type signatures and the module export list
with a focused, layout-aware parser — it does not run GHC. It deliberately handles the common,
well-formed cases; a future full-fidelity version could use ``haskell-src-exts`` or the GHC API.
The hashing/record core is shared with the other adapters (``tooling/ingest-common/nl_core.py``).

CAVEATS (all addressable in future iterations):
  - Only **exported** top-level functions that have a top-level ``name :: Type`` signature are
    ingested. A module with no explicit export list exports everything; ``--include-private``
    ingests every signature regardless of the export list. Functions without a signature are
    skipped (no type to record).
  - ``signature.type`` is the Haskell type rendered as a string (source-flavored, like the Rust
    tool's Rust types), normalised to single spaces. It is not the Nova Lingua type AST.
  - ``arity`` is the count of top-level ``->`` arrows after stripping ``forall``/contexts; curried
    types make this a best-effort count.
  - ``body_hash`` is a synthetic ``expr_`` BLAKE3 of the function's defining equations (or its
    signature if no equation is found) — not a Nova Lingua body AST.
  - ``effects``, ``terminates``, ``properties``, ``intent_tags`` and real ``examples`` are not
    inferred. Class/instance methods, GADT/record-syntax fields, and TH splices are not ingested.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
from pathlib import Path

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "ingest-common"))
from nl_core import build_record, find_matching, split_top, count_top  # noqa: E402

_IDENT = r"[a-z_][A-Za-z0-9_']*"
_SYMBOL_CHARS = set("!#$%&*+./<=>?@\\^|~-:")


# ---------------------------------------------------------------------------
# Comment stripping.
# ---------------------------------------------------------------------------

def _strip_block_comments(src: str) -> str:
    """Remove nested ``{- ... -}`` comments, preserving newlines so line numbers are stable."""
    out = []
    i, n, depth = 0, len(src), 0
    while i < n:
        two = src[i:i + 2]
        if two == "{-":
            depth += 1
            i += 2
            continue
        if depth > 0:
            if two == "-}":
                depth -= 1
                i += 2
                continue
            out.append("\n" if src[i] == "\n" else " ")
            i += 1
            continue
        out.append(src[i])
        i += 1
    return "".join(out)


def _strip_line_comments(src: str) -> str:
    """Remove ``--`` line comments (a run of dashes not followed by a symbol char)."""
    result = []
    for line in src.split("\n"):
        cut = None
        j, L = 0, len(line)
        while j < L - 1:
            if line[j] == "-" and line[j + 1] == "-":
                k = j
                while k < L and line[k] == "-":
                    k += 1
                nxt = line[k] if k < L else ""
                if nxt == "" or nxt not in _SYMBOL_CHARS:
                    cut = j
                    break
                j = k
                continue
            j += 1
        result.append(line if cut is None else line[:cut])
    return "\n".join(result)


def strip_comments(src: str) -> str:
    return _strip_line_comments(_strip_block_comments(src))


# ---------------------------------------------------------------------------
# Module header + export list.
# ---------------------------------------------------------------------------

def _top_index(s: str, token: str) -> int | None:
    """Index of ``token`` at bracket depth 0 (tracking ()[]{}), or None."""
    depth = 0
    i, n, t = 0, len(s), len(token)
    while i < n:
        ch = s[i]
        if ch in "([{":
            depth += 1
            i += 1
        elif ch in ")]}":
            if depth > 0:
                depth -= 1
            i += 1
        elif depth == 0 and s[i:i + t] == token:
            return i
        else:
            i += 1
    return None


def parse_module(src: str):
    """Return (module_name | None, exported_funcs | None).

    exported_funcs is None when the module exports everything (no explicit list).
    """
    m = re.search(r"\bmodule\b\s+([\w.]+)", src)
    if not m:
        return None, None
    module_name = m.group(1)
    i = m.end()
    while i < len(src) and src[i].isspace():
        i += 1
    if i >= len(src) or src[i] != "(":
        return module_name, None  # `module M where` -> exports everything
    j = find_matching(src, i)
    if j == -1:
        return module_name, None
    inside = src[i + 1:j]
    funcs = set()
    for entry in split_top(inside, ","):
        e = entry.strip()
        if not e or e.startswith("module "):
            continue
        mop = re.fullmatch(r"\(([^)]+)\)", e)
        if mop:  # operator export, e.g. (<>)
            funcs.add(mop.group(1).strip())
            continue
        head = re.match(r"[A-Za-z_][\w']*", e)
        if not head:
            continue
        name = head.group(0)
        rest = e[head.end():].strip()
        if rest.startswith("("):  # Type(..)/Class(..) export -> not a function
            continue
        if name[0].islower() or name[0] == "_":
            funcs.add(name)
    return module_name, funcs


# ---------------------------------------------------------------------------
# Signatures + equations.
# ---------------------------------------------------------------------------

def _parse_sig_lhs(lhs: str):
    """Parse the left of ``::`` into a list of function names, or [] if it isn't a clean signature."""
    names = []
    for part in split_top(lhs, ","):
        p = part.strip()
        if re.fullmatch(_IDENT, p):
            names.append(p)
        elif re.fullmatch(r"\([^)]+\)", p):
            names.append(p[1:-1].strip())  # operator
        else:
            return []
    return names


def _normalize_ws(s: str) -> str:
    return re.sub(r"\s+", " ", s).strip()


def _logical_blocks(lines):
    """Group lines into top-level declarations: each col-0 line plus its indented continuations."""
    i, n = 0, len(lines)
    while i < n:
        line = lines[i]
        if not line.strip() or line[:1].isspace():
            i += 1
            continue
        block = [line]
        j = i + 1
        while j < n and lines[j].strip() and lines[j][:1].isspace():
            block.append(lines[j])
            j += 1
        yield block
        i = j


def parse_signatures(src: str):
    """Yield (names, type_str) for every top-level signature in the (comment-stripped) source.

    Joins each declaration with its indented continuation lines first, so a signature whose
    ``::`` sits on a continuation line (``name\\n  :: Type``) is handled.
    """
    out = []
    for block in _logical_blocks(src.split("\n")):
        joined = _normalize_ws(" ".join(block))
        idx = _top_index(joined, "::")
        if idx is None:
            continue
        names = _parse_sig_lhs(joined[:idx].strip())
        if not names:
            continue
        out.append((names, joined[idx + 2:].strip()))
    return out


def arity_of(type_str: str) -> int:
    """Best-effort arity: top-level ``->`` arrows after stripping forall and contexts."""
    t = type_str.strip()
    if t.startswith("forall"):
        dot = _top_index(t, ".")
        if dot is not None:
            t = t[dot + 1:].strip()
    parts = split_top(t, "=>")
    if len(parts) > 1:
        t = parts[-1]
    return count_top(t, "->")


def equations_for(name: str, lines) -> str:
    """Concatenate the defining-equation blocks for ``name`` (best effort; '' if none)."""
    if re.fullmatch(_IDENT, name):
        head = re.compile(r"^" + re.escape(name) + r"(?![A-Za-z0-9_'])")
    else:
        head = re.compile(r"^\(\s*" + re.escape(name) + r"\s*\)")
    blocks = []
    i, n = 0, len(lines)
    while i < n:
        line = lines[i]
        if not line.strip() or line[:1].isspace():
            i += 1
            continue
        if head.match(line) and _top_index(line, "::") is None:
            block = [line]
            j = i + 1
            while j < n and lines[j].strip() and lines[j][:1].isspace():
                block.append(lines[j])
                j += 1
            blocks.append("\n".join(block))
            i = j
        else:
            i += 1
    return "\n".join(blocks)


# ---------------------------------------------------------------------------
# Record assembly.
# ---------------------------------------------------------------------------

def records_from_source(source: str, module_override: str | None, include_private: bool):
    src = strip_comments(source)
    module_name, exports = parse_module(src)
    mod_hint = module_override or module_name
    lines = src.split("\n")

    records = []
    for names, type_str in parse_signatures(src):
        for name in names:
            if not include_private and exports is not None and name not in exports:
                continue
            body = equations_for(name, lines) or type_str
            records.append(build_record(name, type_str, arity_of(type_str), body, module_name=mod_hint))
    return records


# ---------------------------------------------------------------------------
# CLI.
# ---------------------------------------------------------------------------

def _build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(
        prog="nl-ingest-hs",
        description="Parse exported Haskell functions and emit Nova Lingua v0.1 function records (JSONL).",
    )
    p.add_argument("files", nargs="*", type=Path, help="one or more .hs source files to ingest")
    p.add_argument("--module", dest="module", default=None,
                   help="module name for the '<module>_<fn>' name_hint (default: the file's own module header)")
    p.add_argument("--pretty", action="store_true", help="pretty-print each record")
    p.add_argument("--include-private", action="store_true",
                   help="ingest every top-level signature, ignoring the module export list")
    return p


def main(argv=None) -> int:
    args = _build_parser().parse_args(argv)
    if not args.files:
        print("nl-ingest-hs: no files given — pass one or more .hs paths", file=sys.stderr)
        return 1
    exit_code = 0
    for path in args.files:
        try:
            source = path.read_text(encoding="utf-8")
        except OSError as e:
            print(f"nl-ingest-hs: reading {path}: {e}", file=sys.stderr)
            exit_code = 1
            continue
        for record in records_from_source(source, args.module, args.include_private):
            if args.pretty:
                print(json.dumps(record, indent=2, ensure_ascii=False))
            else:
                print(json.dumps(record, separators=(",", ":"), ensure_ascii=False))
    return exit_code


if __name__ == "__main__":
    raise SystemExit(main())
