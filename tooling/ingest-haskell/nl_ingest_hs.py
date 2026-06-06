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
from nl_core import build_record, build_v2_record, find_matching, split_top, count_top  # noqa: E402
from nl_effects import effects_from_tokens, terminates_from_tokens  # noqa: E402
from nl_body import body_ast_from_hs  # noqa: E402
from nl_types import VarCtx, apply, builtin, fn, quantify, tuple_, var  # noqa: E402
from nl_values import ValueEncodeError, to_value_ast  # noqa: E402

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
# v0.2: structured type AST (from the Haskell type string) + examples from haddock `-- >>>` doctests.
# ---------------------------------------------------------------------------

_HS_ATOMIC = {
    "Int": "int", "Integer": "int", "Int8": "int", "Int16": "int", "Int32": "int", "Int64": "int",
    "Word": "nat", "Word8": "nat", "Word16": "nat", "Word32": "nat", "Word64": "nat", "Natural": "nat",
    "Double": "float", "Float": "float", "Bool": "bool", "Char": "string", "String": "string",
    "Text": "string",
}
_HS_VAR = re.compile(r"[a-z_][A-Za-z0-9_']*")


def _applyc(ctor_name, args):
    return apply(builtin(ctor_name), args)


def _hs_atoms(s):
    """Top-level space-separated atoms of a Haskell type/expression; (), [], parens stay whole."""
    atoms, depth, start = [], 0, 0
    for i, c in enumerate(s):
        if c in "([":
            depth += 1
        elif c in ")]":
            depth -= 1
        elif c == " " and depth == 0:
            if i > start:
                atoms.append(s[start:i])
            start = i + 1
    if start < len(s):
        atoms.append(s[start:])
    return [a for a in (x.strip() for x in atoms) if a]


def hs_type_ast(type_str):
    """Map a Haskell type signature string to a Nova Lingua type AST (unknowns -> fresh forall vars)."""
    ctx = VarCtx()
    return quantify(_hs_type(type_str, ctx), ctx.used)


def _hs_type(s, ctx):
    s = s.strip()
    if s.startswith("forall"):
        dot = _top_index(s, ".")
        if dot is not None:
            s = s[dot + 1:].strip()
    ctxs = split_top(s, "=>")          # drop a typeclass context
    if len(ctxs) > 1:
        s = ctxs[-1].strip()
    parts = [p.strip() for p in split_top(s, "->")]
    types = [_hs_app(p, ctx) for p in parts]
    return types[0] if len(types) == 1 else fn(types[:-1], types[-1])


def _hs_app(s, ctx):
    atoms = _hs_atoms(s.strip())
    if len(atoms) == 1:
        return _hs_atom(atoms[0], ctx)
    head, args = atoms[0], [_hs_atom(a, ctx) for a in atoms[1:]]
    base = head.split(".")[-1]
    if base == "Maybe" and args:
        return _applyc("Maybe", [args[0]])
    if base == "Either" and len(args) >= 2:
        return _applyc("Result", [args[0], args[1]])
    if base in ("Map", "HashMap", "IntMap") and len(args) >= 2:
        return _applyc("Map", [args[0], args[1]])
    if base in ("Set", "HashSet", "IntSet") and args:
        return _applyc("Set", [args[0]])
    return var(ctx.fresh_var())            # unknown / higher-kinded application -> a fresh variable


def _hs_atom(a, ctx):
    a = a.strip()
    if a.startswith("[") and a.endswith("]"):
        inner = a[1:-1].strip()
        return _applyc("List", [_hs_type(inner, ctx)]) if inner else var(ctx.fresh_var())
    if a.startswith("(") and a.endswith(")"):
        inner = a[1:-1].strip()
        if not inner:
            return builtin("unit")
        elems = [e.strip() for e in split_top(inner, ",")]
        if len(elems) == 1:
            return _hs_type(elems[0], ctx)
        return tuple_([_hs_type(e, ctx) for e in elems])
    if _HS_VAR.fullmatch(a):
        return var(ctx.named_var(a))
    base = a.split(".")[-1]
    if base in _HS_ATOMIC:
        return builtin(_HS_ATOMIC[base])
    return var(ctx.fresh_var())            # unknown nullary constructor / user type


def _split_fn(type_ast):
    body = type_ast["body"] if type_ast.get("kind") == "forall" else type_ast
    if body.get("kind") == "fn":
        return body.get("params", []), body.get("result")
    return [], None


def haddock_doctests(source):
    """{fn_name: [(call_expr, expected), ...]} from `-- >>>` haddock doctests in the ORIGINAL source.

    Grouped by the leading identifier of the `>>>` expression (the function demonstrated)."""
    out = {}
    lines = source.split("\n")
    i, n = 0, len(lines)
    while i < n:
        m = re.match(r"\s*--\s*>>>\s*(.*)", lines[i])
        if not m:
            i += 1
            continue
        expr = m.group(1).strip()
        res, j = [], i + 1
        while j < n:
            rm = re.match(r"\s*--\s?(.*)", lines[j])
            if not rm:
                break
            content = rm.group(1).strip()
            if content == "" or content.startswith(">>>"):
                break
            res.append(content)
            j += 1
        expected = " ".join(res).strip()
        head = re.match(r"[A-Za-z_][A-Za-z0-9_']*", expr)
        if head and expected:
            out.setdefault(head.group(0), []).append((expr, expected))
        i = j
    return out


def _hs_lit_py(s):
    """Parse a Haskell literal expression into a Python value (then reuse nl_values.to_value_ast).
    Raises ValueError for anything that is not a plain literal — so that example is skipped."""
    s = s.strip()
    if s in ("True", "False"):
        return s == "True"
    if s == "()":
        return None
    if len(s) >= 2 and s[0] == '"' and s[-1] == '"':
        return bytes(s[1:-1], "utf-8").decode("unicode_escape")
    if len(s) == 3 and s[0] == "'" and s[-1] == "'":
        return s[1]
    if s.startswith("[") and s.endswith("]"):
        inner = s[1:-1].strip()
        return [] if not inner else [_hs_lit_py(e.strip()) for e in split_top(inner, ",")]
    if s.startswith("(") and s.endswith(")"):
        inner = s[1:-1].strip()
        if not inner:
            return None
        elems = [e.strip() for e in split_top(inner, ",")]
        return _hs_lit_py(elems[0]) if len(elems) == 1 else tuple(_hs_lit_py(e) for e in elems)
    if re.fullmatch(r"-?\d+", s):
        return int(s)
    if re.fullmatch(r"-?\d+\.\d+([eE][+-]?\d+)?", s) or re.fullmatch(r"-?\d+[eE][+-]?\d+", s):
        return float(s)
    raise ValueError(f"unparseable Haskell literal: {s!r}")


def hs_examples(name, doctests, param_types, result_type):
    """Turn `(>>> name args, expected)` doctests into {args, result} value-AST examples."""
    out = []
    for expr, expected in doctests:
        atoms = _hs_atoms(expr)
        if not atoms or atoms[0] != name:
            continue
        try:
            args = []
            for i, a in enumerate(atoms[1:]):
                hint = param_types[i] if i < len(param_types) else None
                args.append(to_value_ast(_hs_lit_py(a), hint))
            result = to_value_ast(_hs_lit_py(expected), result_type)
        except (ValueError, ValueEncodeError):
            continue
        out.append({"args": args, "result": result})
    return out


def _build_v2(name, type_str, doctests, body, module_name):
    type_ast = hs_type_ast(type_str)
    param_types, result_type = _split_fn(type_ast)
    examples = hs_examples(name, doctests, param_types, result_type)
    if not examples:
        return None
    body_repr = body_ast_from_hs(name, body) or body  # real body AST when in subset, else source text
    return build_v2_record(name, type_ast, examples, body_repr, module_name=module_name,
                           effects=effects_from_tokens(body, "hs"),
                           terminates=terminates_from_tokens(name, body, "hs"))


# ---------------------------------------------------------------------------
# Record assembly.
# ---------------------------------------------------------------------------

def records_from_source(source: str, module_override: str | None, include_private: bool,
                        v2: bool = False):
    src = strip_comments(source)
    module_name, exports = parse_module(src)
    mod_hint = module_override or module_name
    lines = src.split("\n")
    doctests = haddock_doctests(source) if v2 else {}   # from the original source (comments intact)

    records = []
    for names, type_str in parse_signatures(src):
        for name in names:
            if not include_private and exports is not None and name not in exports:
                continue
            body = equations_for(name, lines) or type_str
            rec = _build_v2(name, type_str, doctests.get(name, []), body, mod_hint) if v2 else None
            if rec is None:
                rec = build_record(name, type_str, arity_of(type_str), body, module_name=mod_hint)
            records.append(rec)
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
    p.add_argument("--v2", action="store_true",
                   help="higher fidelity: emit v0.2 records (structured type AST + real examples from "
                        "haddock `-- >>>` doctests) for functions with usable doctests; v0.1 otherwise")
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
        for record in records_from_source(source, args.module, args.include_private, v2=args.v2):
            if args.pretty:
                print(json.dumps(record, indent=2, ensure_ascii=False))
            else:
                print(json.dumps(record, separators=(",", ":"), ensure_ascii=False))
    return exit_code


if __name__ == "__main__":
    raise SystemExit(main())
