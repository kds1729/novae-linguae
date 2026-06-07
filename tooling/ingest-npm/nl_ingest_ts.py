#!/usr/bin/env python3
"""nl-ingest-ts: parse exported TypeScript/JavaScript functions into Nova Lingua v0.1 records.

Each exported top-level function in the given ``.ts`` / ``.d.ts`` / ``.js`` / ``.mjs`` files
becomes one JSON record on stdout (JSONL by default, ``--pretty`` for readable). Records satisfy
``function-record.schema.json``; ``hash`` and ``body_hash`` are real BLAKE3-256 content-addresses
computed exactly as ``spec/canonical-serialization.md`` prescribes, so every record agrees
byte-for-byte with the Rust ``nl-validator`` (and with the Rust/Python/Haskell adapters).

This is the **npm-ecosystem** adapter. It is a *lightweight* front end: a string scanner that
recognises the common exported-function forms — it does not run the TypeScript compiler. A future
full-fidelity version could use the TypeScript compiler API (`ts.createSourceFile`) via Node.
The hashing/record core is shared (``tooling/ingest-common/nl_core.py``).

Recognised export forms:
  - ``export function f<T>(a: A, b: B): R { … }`` (and ``async``, ``export default function``)
  - ``export declare function f(a: A): R;`` and other ``.d.ts`` ambient declarations
  - ``export const f = (a: A): R => …`` (and ``async``, generics, ``= function (…) {…}``)
  - ``export const f = x => …`` (single bare parameter)

CAVEATS (all addressable in future iterations):
  - Only **exported** functions are ingested. Class methods, object-method shorthand, overload
    signature merging, re-exports (``export { x } from …``), and namespaces are not handled.
  - ``signature.type`` is built from the TS annotations as a source-flavored string
    (``forall T. (A, B) -> R``), not the Nova Lingua type AST; unannotated positions render as
    ``unknown``. Bare object-literal **return** types (``: { a: number }``) are not parsed and may
    truncate the rendered return type (named/`Promise<…>`/array/union returns are fine).
  - ``arity`` counts declared parameters (a leading TS ``this`` parameter is excluded).
  - ``body_hash`` is a synthetic ``expr_`` BLAKE3 of the declaration's source slice — not a Nova
    Lingua body AST.
  - ``effects``, ``terminates``, ``properties``, ``intent_tags``, real ``examples`` are not inferred.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
from pathlib import Path

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "ingest-common"))
from nl_core import build_record, build_v2_record, name_hints as _name_hints  # noqa: E402
from nl_effects import effects_from_tokens, terminates_from_tokens  # noqa: E402
from nl_body import body_ast_from_ts  # noqa: E402
from nl_toolchain import run_enricher, tool_on_path  # noqa: E402
from property_catalog import match_catalog  # noqa: E402

_TS_ENRICH_JS = Path(__file__).resolve().parent / "ts_enrich.js"


def ts_enrich(source: str) -> str:
    """Toolchain enricher: emit fully-typed `.d.ts` declarations via the TypeScript compiler so the
    scanner reads resolved signatures. Falls back to `source` when node / typescript are unavailable."""
    if not (tool_on_path("node") and _TS_ENRICH_JS.exists()):
        return source
    return run_enricher(["node", str(_TS_ENRICH_JS)], source)
from nl_types import VarCtx, apply, builtin, fn, quantify, tuple_, var  # noqa: E402
from nl_values import ValueEncodeError, to_value_ast  # noqa: E402

_QUOTES = {"'", '"', "`"}
_IDENT_START = re.compile(r"[A-Za-z_$]")
_IDENT_CHAR = re.compile(r"[\w$]")


# ---------------------------------------------------------------------------
# Low-level scanning (string- and comment-aware).
# ---------------------------------------------------------------------------

def _skip_ws(s: str, i: int) -> int:
    n = len(s)
    while i < n and s[i].isspace():
        i += 1
    return i


def _skip_string(s: str, i: int) -> int:
    """Given i at a quote, return the index just past the closing quote (handles template ${})."""
    q = s[i]
    i += 1
    n = len(s)
    while i < n:
        c = s[i]
        if c == "\\":
            i += 2
            continue
        if q == "`" and s[i:i + 2] == "${":
            depth = 1
            i += 2
            while i < n and depth > 0:
                if s[i] in _QUOTES:
                    i = _skip_string(s, i)
                    continue
                if s[i] == "{":
                    depth += 1
                elif s[i] == "}":
                    depth -= 1
                i += 1
            continue
        if c == q:
            return i + 1
        i += 1
    return i


def strip_comments(s: str) -> str:
    """Remove // and /* */ comments, preserving string literals and newline count."""
    out = []
    i, n = 0, len(s)
    while i < n:
        c = s[i]
        if c in _QUOTES:
            j = _skip_string(s, i)
            out.append(s[i:j])
            i = j
            continue
        if s[i:i + 2] == "//":
            while i < n and s[i] != "\n":
                i += 1
            continue
        if s[i:i + 2] == "/*":
            i += 2
            while i < n and s[i:i + 2] != "*/":
                out.append("\n" if s[i] == "\n" else "")
                i += 1
            i += 2
            continue
        out.append(c)
        i += 1
    return "".join(out)


def _read_ident(s: str, i: int):
    n = len(s)
    if i < n and _IDENT_START.match(s[i]):
        j = i + 1
        while j < n and _IDENT_CHAR.match(s[j]):
            j += 1
        return s[i:j], j
    return None, i


def _kw(s: str, i: int, word: str):
    """If ``word`` sits at i as a whole token, return the index after it; else None."""
    if s[i:i + len(word)] == word:
        k = i + len(word)
        if k >= len(s) or not _IDENT_CHAR.match(s[k]):
            if i == 0 or not _IDENT_CHAR.match(s[i - 1]):
                return k
    return None


def _match_bracket(s: str, i: int) -> int:
    """Index of the bracket matching the one at s[i] (one of ([{), skipping strings; -1 if none."""
    opener = s[i]
    closer = {"(": ")", "[": "]", "{": "}"}[opener]
    depth = 0
    n = len(s)
    while i < n:
        c = s[i]
        if c in _QUOTES:
            i = _skip_string(s, i)
            continue
        if c == opener:
            depth += 1
        elif c == closer:
            depth -= 1
            if depth == 0:
                return i
        i += 1
    return -1


def _skip_generics(s: str, i: int):
    """Given i at '<', return (text_including_brackets, index_after). Treats => as a unit and
    each '>' as one close (so Foo<Bar<Baz>> works). Returns (None, i) if unbalanced."""
    if i >= len(s) or s[i] != "<":
        return None, i
    depth = 0
    start = i
    n = len(s)
    while i < n:
        if s[i:i + 2] == "=>":
            i += 2
            continue
        c = s[i]
        if c in _QUOTES:
            i = _skip_string(s, i)
            continue
        if c == "<":
            depth += 1
        elif c == ">":
            depth -= 1
            if depth == 0:
                return s[start:i + 1], i + 1
        i += 1
    return None, start


def _scan_type(s: str, i: int, stops: str, track_braces: bool):
    """Scan a type starting at i; stop at a depth-0 char in ``stops``.

    Tracks ()[]<> depth (and {} if track_braces), skips strings, and treats => as a unit so the
    '>' in an arrow type does not close a generic. Returns (text, stop_index, stop_char)."""
    depth = 0
    n = len(s)
    buf = []
    opens = "([<" + ("{" if track_braces else "")
    closes = ")]>" + ("}" if track_braces else "")
    while i < n:
        if s[i:i + 2] == "=>":
            buf.append("=>")
            i += 2
            continue
        c = s[i]
        if c in _QUOTES:
            j = _skip_string(s, i)
            buf.append(s[i:j])
            i = j
            continue
        if depth == 0 and c in stops:
            return "".join(buf), i, c
        if c in opens:
            depth += 1
        elif c in closes and depth > 0:
            depth -= 1
        buf.append(c)
        i += 1
    return "".join(buf), n, ""


def _scan_arrow_rettype(s: str, i: int):
    """Scan an arrow function's return type, stopping at the terminating depth-0 ``=>``.

    A ``=>`` *inside* brackets (a function type like ``Promise<() => void>``) is skipped as a unit
    so its ``>`` does not close a generic; only the depth-0 ``=>`` terminates. Returns (text, end_i)
    with end_i at the terminating ``=>`` (or len(s) if none found)."""
    depth = 0
    n = len(s)
    buf = []
    while i < n:
        if s[i:i + 2] == "=>":
            if depth == 0:
                return "".join(buf), i
            buf.append("=>")
            i += 2
            continue
        c = s[i]
        if c in _QUOTES:
            j = _skip_string(s, i)
            buf.append(s[i:j])
            i = j
            continue
        if c in "([{<":
            depth += 1
        elif c in ")]}>" and depth > 0:
            depth -= 1
        buf.append(c)
        i += 1
    return "".join(buf), n


def _find_top(s: str, token: str, brackets: str = "()[]{}<>"):
    """Index of ``token`` at bracket depth 0 (string-aware, => treated as a unit), or None."""
    opens = brackets[0::2]
    closes = brackets[1::2]
    depth = 0
    i, n, t = 0, len(s), len(token)
    while i < n:
        if s[i:i + 2] == "=>":
            i += 2
            continue
        c = s[i]
        if c in _QUOTES:
            i = _skip_string(s, i)
            continue
        if depth == 0 and s[i:i + t] == token:
            return i
        if c in opens:
            depth += 1
        elif c in closes and depth > 0:
            depth -= 1
        i += 1
    return None


def _split_top(s: str, sep: str, brackets: str = "()[]{}<>"):
    """Split ``s`` on depth-0 ``sep`` (string-aware, => treated as a unit)."""
    opens = brackets[0::2]
    closes = brackets[1::2]
    depth = 0
    parts, buf = [], []
    i, n, slen = 0, len(s), len(sep)
    while i < n:
        if s[i:i + 2] == "=>":
            buf.append("=>")
            i += 2
            continue
        c = s[i]
        if c in _QUOTES:
            j = _skip_string(s, i)
            buf.append(s[i:j])
            i = j
            continue
        if depth == 0 and s[i:i + slen] == sep:
            parts.append("".join(buf))
            buf = []
            i += slen
            continue
        if c in opens:
            depth += 1
        elif c in closes and depth > 0:
            depth -= 1
        buf.append(c)
        i += 1
    parts.append("".join(buf))
    return parts


def _normalize_ws(s: str) -> str:
    return re.sub(r"\s+", " ", s).strip()


# ---------------------------------------------------------------------------
# Parameter + type rendering.
# ---------------------------------------------------------------------------

def _param_types(params_text: str):
    """Return (list_of_type_strings, arity) for a parameter-list body (between the parens)."""
    types = []
    for part in _split_top(params_text, ","):
        p = part.strip()
        if not p:
            continue
        ci = _find_top(p, ":")
        if ci is None:
            name = p.lstrip(".").rstrip("?").strip()
            if name == "this":
                continue
            types.append("unknown")
            continue
        name = p[:ci].lstrip(".").rstrip("?").strip()
        if name == "this":
            continue
        rest = p[ci + 1:]
        eq = _find_top(rest, "=")
        if eq is not None:
            rest = rest[:eq]
        types.append(_normalize_ws(rest) or "unknown")
    return types, len(types)


def _parse_typevars(gtext: str):
    inner = gtext.strip()
    if inner.startswith("<"):
        inner = inner[1:]
    if inner.endswith(">"):
        inner = inner[:-1]
    out = []
    for part in _split_top(inner, ","):
        p = re.sub(r"^const\s+", "", part.strip())
        m = re.match(r"[A-Za-z_$][\w$]*", p)
        if m:
            out.append(m.group(0))
    return out


def _make_type(typevars, params_text, rettype) -> str:
    types, _ = _param_types(params_text)
    ret = rettype.strip() if rettype.strip() else "unknown"
    forall = f"forall {' '.join(typevars)}. " if typevars else ""
    return f"{forall}({', '.join(types)}) -> {ret}"


# ---------------------------------------------------------------------------
# v0.2: structured type AST (from TS type strings) + examples from JSDoc @example.
# ---------------------------------------------------------------------------

# TS `number` is an IEEE-754 double -> float (and numeric literals are encoded as floats to match).
_TS_ATOMIC = {"number": "float", "bigint": "int", "string": "string", "boolean": "bool",
              "void": "unit", "undefined": "unit", "null": "unit", "never": "never"}


def _tsapply(name, args):
    return apply(builtin(name), args)


def _ts_type(s, tvset, ctx):
    """Map a TS type string to a Nova Lingua type AST. Bounded: arrow and object types, and unknown
    named types, become fresh forall-bound type variables (no `unknown` builtin exists)."""
    s = s.strip()
    if not s:
        return var(ctx.fresh_var())
    parts = _split_top(s, "|")
    if len(parts) > 1:                                   # union: T | null -> Maybe T
        members = [p.strip() for p in parts]
        rest = [m for m in members if m not in ("null", "undefined")]
        has_none = len(rest) != len(members)
        if not rest:
            return builtin("unit")
        inner = _ts_type(rest[0], tvset, ctx) if len(rest) == 1 else var(ctx.fresh_var())
        return _tsapply("Maybe", [inner]) if has_none else inner
    if _find_top(s, "=>") is not None:
        return var(ctx.fresh_var())                      # function type (bounded)
    if s.startswith("(") and s.endswith(")"):
        return _ts_type(s[1:-1], tvset, ctx)
    if s.endswith("[]"):
        return _tsapply("List", [_ts_type(s[:-2], tvset, ctx)])
    if s.startswith("[") and s.endswith("]"):            # tuple [A, B] (or 1-tuple [T] -> T)
        inner = s[1:-1].strip()
        if not inner:
            return builtin("unit")
        elems = [e.strip() for e in _split_top(inner, ",")]
        if len(elems) == 1:
            return _ts_type(elems[0], tvset, ctx)
        return tuple_([_ts_type(e, tvset, ctx) for e in elems])
    if s.startswith("{") and s.endswith("}"):
        return var(ctx.fresh_var())                      # object type (bounded)
    lt = _find_top(s, "<")
    if lt is not None and s.endswith(">"):
        head = s[:lt].strip().split(".")[-1]
        args = [_ts_type(a, tvset, ctx) for a in _split_top(s[lt + 1:-1], ",") if a.strip()]
        if head in ("Array", "ReadonlyArray") and args:
            return _tsapply("List", [args[0]])
        if head in ("Set", "ReadonlySet", "WeakSet") and args:
            return _tsapply("Set", [args[0]])
        if head in ("Map", "Record", "ReadonlyMap", "WeakMap") and len(args) >= 2:
            return _tsapply("Map", [args[0], args[1]])
        if head == "Promise" and args:
            return args[0]                               # unwrap Promise<T> -> T
        return var(ctx.named_var(head)) if head in tvset else var(ctx.fresh_var())
    if re.fullmatch(r"[A-Za-z_$][\w$]*", s):
        if s in tvset:
            return var(ctx.named_var(s))
        if s in _TS_ATOMIC:
            return builtin(_TS_ATOMIC[s])
        return var(ctx.fresh_var())                      # any/unknown/object/user type
    return var(ctx.fresh_var())


def ts_function_type(typevars, params_text, rettype):
    """(type AST, param-type ASTs, result-type AST) for a TS signature."""
    ctx = VarCtx()
    tv = set(typevars)
    ptypes, _ = _param_types(params_text)
    params = [_ts_type(t, tv, ctx) for t in ptypes]
    result = _ts_type(rettype, tv, ctx) if rettype.strip() else var(ctx.fresh_var())
    return quantify(fn(params, result), ctx.used), params, result


def _js_lit_py(s):
    """Parse a JS/TS literal expression into a Python value (then nl_values.to_value_ast). Raises
    ValueError for anything that is not a plain literal (so that example is skipped). TS numbers are
    doubles -> Python float."""
    s = s.strip()
    if s in ("true", "false"):
        return s == "true"
    if s in ("null", "undefined"):
        return None
    if len(s) >= 2 and s[0] == s[-1] and s[0] in ("'", '"', "`"):
        if s[0] == "`" and "${" in s:
            raise ValueError("template interpolation")
        return bytes(s[1:-1], "utf-8").decode("unicode_escape")
    if s.startswith("[") and s.endswith("]"):
        inner = s[1:-1].strip()
        return [] if not inner else [_js_lit_py(e) for e in _split_top(inner, ",") if e.strip()]
    if s.startswith("{") and s.endswith("}"):
        d = {}
        for part in _split_top(s[1:-1], ","):
            part = part.strip()
            if not part:
                continue
            ci = _find_top(part, ":")
            if ci is None:
                raise ValueError("bad object entry")
            d[part[:ci].strip().strip("'\"")] = _js_lit_py(part[ci + 1:].strip())
        return d
    if re.fullmatch(r"-?\d+(\.\d+)?([eE][+-]?\d+)?", s) or re.fullmatch(r"-?\.\d+", s):
        return float(s)
    raise ValueError(f"unparseable JS literal: {s!r}")


def _ts_call_name(call):
    m = re.match(r"\s*([A-Za-z_$][\w$]*)\s*\(", call)
    return m.group(1) if m else None


def _example_pairs(code):
    """(call_expr, result_expr) pairs from one @example body, recognising the common conventions:
    `f(x) // => r`, `assert.equal(f(x), r)`, `expect(f(x)).toBe(r)`."""
    pairs = []
    for raw in code.split("\n"):
        line = raw.strip().rstrip(";").strip()
        if not line:
            continue
        m = re.match(r"(?:chai\.)?assert\.(?:equal|strictEqual|deepEqual|deepStrictEqual)\((.*)\)$", line)
        if m:
            parts = _split_top(m.group(1), ",")
            if len(parts) >= 2:
                pairs.append((parts[0].strip(), ",".join(parts[1:]).strip()))
            continue
        m = re.match(r"expect\((.*)\)\.(?:toBe|toEqual|toStrictEqual)\((.*)\)$", line)
        if m:
            pairs.append((m.group(1).strip(), m.group(2).strip()))
            continue
        ci = _find_top(line, "//")
        if ci is not None:
            call, rest = line[:ci].strip(), line[ci + 2:].strip()
            rest = re.sub(r"^(?:=>|=|⇒)\s*", "", rest).strip()
            if call and rest:
                pairs.append((call, rest))
    return pairs


def jsdoc_examples(source):
    """{fn_name: [(call_expr, result_expr), ...]} from @example snippets in /** */ JSDoc comments
    (read from the ORIGINAL source, since comments are stripped before scanning)."""
    out = {}
    for block in re.findall(r"/\*\*(.*?)\*/", source, re.S):
        for m in re.finditer(r"@example\b(.*?)(?=@\w|\Z)", block, re.S):
            code = "\n".join(re.sub(r"^\s*\*\s?", "", ln) for ln in m.group(1).split("\n"))
            code = re.sub(r"```[A-Za-z]*", "", code)     # drop code-fence markers
            for call, result in _example_pairs(code):
                name = _ts_call_name(call)
                if name and result:
                    out.setdefault(name, []).append((call, result))
    return out


def ts_examples(name, pairs, param_types, result_type):
    out = []
    for call, result_str in pairs:
        m = re.match(r"^[A-Za-z_$][\w$]*\s*\((.*)\)\s*$", call)
        if not m:
            continue
        arg_strs = [a for a in _split_top(m.group(1), ",") if a.strip()]
        try:
            args = [to_value_ast(_js_lit_py(a), param_types[i] if i < len(param_types) else None)
                    for i, a in enumerate(arg_strs)]
            result = to_value_ast(_js_lit_py(result_str), result_type)
        except (ValueError, ValueEncodeError):
            continue
        out.append({"args": args, "result": result})
    return out


def _build_ts_record(name, typevars, params, rettype, slice_text, module_name, v2, examples_map,
                     with_properties=False):
    if v2:
        type_ast, param_types, result_type = ts_function_type(typevars, params, rettype)
        examples = ts_examples(name, examples_map.get(name, []), param_types, result_type)
        if examples:
            body_repr = body_ast_from_ts(name, slice_text) or slice_text
            props, tags = (match_catalog(_name_hints(name, module_name), len(param_types))
                           if with_properties else ([], []))
            return build_v2_record(name, type_ast, examples, body_repr, module_name=module_name,
                                   effects=effects_from_tokens(slice_text, "ts"),
                                   terminates=terminates_from_tokens(name, slice_text, "ts"),
                                   properties=props, intent_tags=tags)
    return build_record(name, _make_type(typevars, params, rettype),
                        _param_types(params)[1], slice_text, module_name=module_name)


# ---------------------------------------------------------------------------
# Body-end / declaration-slice helpers.
# ---------------------------------------------------------------------------

def _body_end_after(s: str, i: int) -> int:
    """At i (after params/return type of a function decl): consume a {…} body or a ; declaration."""
    i = _skip_ws(s, i)
    if i < len(s) and s[i] == "{":
        m = _match_bracket(s, i)
        return (m + 1) if m != -1 else len(s)
    if i < len(s) and s[i] == ";":
        return i + 1
    return i


def _arrow_body_end(s: str, i: int) -> int:
    """At i (just after =>): consume a {…} body or an expression up to a depth-0 ; or newline."""
    i = _skip_ws(s, i)
    if i < len(s) and s[i] == "{":
        m = _match_bracket(s, i)
        return (m + 1) if m != -1 else len(s)
    depth = 0
    n = len(s)
    while i < n:
        c = s[i]
        if c in _QUOTES:
            i = _skip_string(s, i)
            continue
        if c in "([{":
            depth += 1
        elif c in ")]}":
            if depth == 0:
                return i
            depth -= 1
        elif depth == 0 and (c == ";" or c == "\n"):
            return i + 1 if c == ";" else i
        i += 1
    return n


# ---------------------------------------------------------------------------
# Callable / declaration parsers. Each returns (typevars, params, rettype, end) or None.
# ---------------------------------------------------------------------------

def _parse_function_decl(s: str, i: int):
    """i points just after the 'function' keyword. Returns (name, typevars, params, rettype, end)."""
    i = _skip_ws(s, i)
    if i < len(s) and s[i] == "*":
        i = _skip_ws(s, i + 1)
    name, j = _read_ident(s, i)
    i = _skip_ws(s, j)
    typevars = []
    if i < len(s) and s[i] == "<":
        g, i = _skip_generics(s, i)
        if g is None:
            return None
        typevars = _parse_typevars(g)
        i = _skip_ws(s, i)
    if i >= len(s) or s[i] != "(":
        return None
    pc = _match_bracket(s, i)
    if pc == -1:
        return None
    params = s[i + 1:pc]
    i = _skip_ws(s, pc + 1)
    rettype = ""
    if i < len(s) and s[i] == ":":
        rettype, i, _ = _scan_type(s, i + 1, "{;", track_braces=False)
    end = _body_end_after(s, i)
    return (name or "default", typevars, params, _normalize_ws(rettype), end)


def _parse_callable_rhs(s: str, i: int):
    """i points at the RHS of '=' (after optional async). Parse an arrow or function expr.
    Returns (typevars, params, rettype, end) or None."""
    i = _skip_ws(s, i)
    kk = _kw(s, i, "function")
    if kk is not None:
        i = _skip_ws(s, kk)
        if i < len(s) and s[i] == "*":
            i = _skip_ws(s, i + 1)
        _, i = _read_ident(s, i)  # optional expression name, ignored (binding name wins)
        i = _skip_ws(s, i)
        typevars = []
        if i < len(s) and s[i] == "<":
            g, i = _skip_generics(s, i)
            if g is None:
                return None
            typevars = _parse_typevars(g)
            i = _skip_ws(s, i)
        if i >= len(s) or s[i] != "(":
            return None
        pc = _match_bracket(s, i)
        if pc == -1:
            return None
        params = s[i + 1:pc]
        i = _skip_ws(s, pc + 1)
        rettype = ""
        if i < len(s) and s[i] == ":":
            rettype, i, _ = _scan_type(s, i + 1, "{;", track_braces=False)
        end = _body_end_after(s, i)
        return (typevars, params, _normalize_ws(rettype), end)

    # Arrow forms.
    typevars = []
    if i < len(s) and s[i] == "<":
        g, j = _skip_generics(s, i)
        k = _skip_ws(s, j)
        if g is not None and k < len(s) and s[k] == "(":
            typevars = _parse_typevars(g)
            i = k
        else:
            return None
    if i < len(s) and s[i] == "(":
        pc = _match_bracket(s, i)
        if pc == -1:
            return None
        params = s[i + 1:pc]
        i = _skip_ws(s, pc + 1)
        rettype = ""
        if i < len(s) and s[i] == ":":
            rettype, i = _scan_arrow_rettype(s, i + 1)
        i = _skip_ws(s, i)
        if s[i:i + 2] != "=>":
            return None
        end = _arrow_body_end(s, i + 2)
        return (typevars, params, _normalize_ws(rettype), end)

    # Single bare-identifier arrow parameter: `x => …`
    name, j = _read_ident(s, i)
    if name is not None:
        k = _skip_ws(s, j)
        if s[k:k + 2] == "=>":
            return ([], name, "", _arrow_body_end(s, k + 2))
    return None


def _scan_to_assign(s: str, i: int):
    """From i (after a var's ':' annotation), return the index of the assignment '=' (or None)."""
    depth = 0
    n = len(s)
    while i < n:
        if s[i:i + 2] == "=>":
            i += 2
            continue
        c = s[i]
        if c in _QUOTES:
            i = _skip_string(s, i)
            continue
        if c in "([{<":
            depth += 1
        elif c in ")]}>":
            if depth > 0:
                depth -= 1
        elif depth == 0 and c == "=" and s[i + 1:i + 2] not in ("=", ">"):
            return i
        i += 1
    return None


def _parse_export(s: str, export_start: int, module_name, v2=False, examples_map=None,
                  with_properties=False):
    """export_start points at 'export'. Returns (record | None, end_index)."""
    examples_map = examples_map or {}
    i = _skip_ws(s, export_start + len("export"))
    for word in ("default", "declare"):
        kk = _kw(s, i, word)
        if kk is not None:
            i = _skip_ws(s, kk)
    kk = _kw(s, i, "async")
    if kk is not None:
        i = _skip_ws(s, kk)

    fkk = _kw(s, i, "function")
    if fkk is not None:
        parsed = _parse_function_decl(s, fkk)
        if parsed is None:
            return None, i
        name, typevars, params, rettype, end = parsed
        rec = _build_ts_record(name, typevars, params, rettype, s[export_start:end],
                               module_name, v2, examples_map, with_properties)
        return rec, end

    for word in ("const", "let", "var"):
        vkk = _kw(s, i, word)
        if vkk is None:
            continue
        j = _skip_ws(s, vkk)
        name, j = _read_ident(s, j)
        if name is None:
            return None, j
        j = _skip_ws(s, j)
        if j < len(s) and s[j] == ":":
            a = _scan_to_assign(s, j + 1)
            if a is None:
                return None, j
            j = a
        if j >= len(s) or s[j] != "=":
            return None, j
        j = _skip_ws(s, j + 1)
        akk = _kw(s, j, "async")
        if akk is not None:
            j = _skip_ws(s, akk)
        parsed = _parse_callable_rhs(s, j)
        if parsed is None:
            return None, j
        typevars, params, rettype, end = parsed
        rec = _build_ts_record(name, typevars, params, rettype, s[export_start:end],
                               module_name, v2, examples_map, with_properties)
        return rec, end

    return None, i


def records_from_source(source: str, module_name: str | None = None, v2: bool = False,
                        enrich=None, with_properties: bool = False):
    """``enrich``: an optional source -> source transform applied before scanning (the toolchain
    seam, see nl_toolchain). None = scanner only — the deterministic, zero-dependency default."""
    if enrich is not None:
        source = enrich(source)
    examples_map = jsdoc_examples(source) if v2 else {}   # from original source (JSDoc intact)
    s = strip_comments(source)
    records = []
    i, n = 0, len(s)
    while i < n:
        if (s[i] == "e" and (i == 0 or not _IDENT_CHAR.match(s[i - 1]))
                and _kw(s, i, "export") is not None):
            rec, end = _parse_export(s, i, module_name, v2, examples_map, with_properties)
            if rec is not None:
                records.append(rec)
            i = max(end, i + 1)
        else:
            i += 1
    return records


# ---------------------------------------------------------------------------
# CLI.
# ---------------------------------------------------------------------------

def _build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(
        prog="nl-ingest-ts",
        description="Parse exported TypeScript/JavaScript functions and emit Nova Lingua v0.1 records (JSONL).",
    )
    p.add_argument("files", nargs="*", type=Path, help="one or more .ts/.d.ts/.js source files")
    p.add_argument("--module", dest="module", default=None,
                   help="package/module name; adds '<module>_<fn>' to name_hints")
    p.add_argument("--pretty", action="store_true", help="pretty-print each record")
    p.add_argument("--v2", action="store_true",
                   help="higher fidelity: emit v0.2 records (structured type AST + real examples from "
                        "JSDoc @example) for functions with usable examples; v0.1 otherwise")
    p.add_argument("--toolchain", action="store_true",
                   help="opt in to the TypeScript-compiler backend (node + typescript) to resolve "
                        "signatures before scanning; falls back to the scanner if unavailable. "
                        "Non-deterministic across compiler versions — off by default (principle 5)")
    p.add_argument("--properties", action="store_true",
                   help="attach curated algebraic laws (property_catalog.json) to recognised functions; "
                        "implies --v2. Verify with `nl-validator check-properties`")
    return p


def main(argv=None) -> int:
    args = _build_parser().parse_args(argv)
    if not args.files:
        print("nl-ingest-ts: no files given — pass one or more .ts/.js paths", file=sys.stderr)
        return 1
    exit_code = 0
    for path in args.files:
        try:
            source = path.read_text(encoding="utf-8")
        except OSError as e:
            print(f"nl-ingest-ts: reading {path}: {e}", file=sys.stderr)
            exit_code = 1
            continue
        enrich = ts_enrich if args.toolchain else None
        v2 = args.v2 or args.properties  # --properties implies --v2
        for record in records_from_source(source, args.module, v2=v2, enrich=enrich,
                                          with_properties=args.properties):
            if args.pretty:
                print(json.dumps(record, indent=2, ensure_ascii=False))
            else:
                print(json.dumps(record, separators=(",", ":"), ensure_ascii=False))
    return exit_code


if __name__ == "__main__":
    raise SystemExit(main())
