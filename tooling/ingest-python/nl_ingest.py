#!/usr/bin/env python3
"""nl-ingest (Python): parse public Python functions and emit Nova Lingua v0.1 function records.

Each public top-level ``def``/``async def`` in the given source files becomes one JSON record
on stdout (JSONL by default, ``--pretty`` for readable). The record satisfies the
``function-record.schema.json`` structural requirements; ``hash`` and ``body_hash`` are real
BLAKE3-256 content-addresses computed exactly as ``spec/canonical-serialization.md`` prescribes
(JCS / RFC 8785 over UTF-8 JSON, then BLAKE3-256). ``body_hash`` is the hash of the function
body's normalised source (``ast.unparse``) rather than a proper Nova Lingua body-expression
AST — that translation is future work, mirroring the Rust ``nl-ingest``.

This is the Python sibling of the reference Rust ``nl-ingest``. It is the second ingestion
adapter (Rust was first) and, like Rust, every emitted record passes ``nl-validator validate``
and ``nl-validator verify``: the two tools agree byte-for-byte on canonical form and hash, which
is the cross-implementation conformance contract the project requires (see
``tooling/validator/README.md`` and ``spec/conformance/``).

Design: stdlib-only, zero third-party dependencies. BLAKE3 and JCS are vendored below so the
tool runs with nothing but ``python3``. If the native ``blake3`` package happens to be installed
it is used for speed; otherwise the pure-Python implementation (verified against the reference
test vector and against ``nl-validator``) is used.

CAVEATS (all addressable in future iterations):
  - "Public" = a top-level function whose name does not start with ``_``; if the module defines
    ``__all__`` (a list/tuple/set of string literals), that list is authoritative instead.
    ``--include-private`` ingests every top-level function regardless.
  - Methods inside ``class`` bodies are skipped; only module-level functions are ingested.
  - ``examples.args`` contains one ``null`` per fixed parameter; ``result`` is ``null``. The
    arity is correct; fill in real values after ingestion.
  - ``signature.terminates`` is always ``"unknown"``. Static analysis is future work.
  - ``effects``, ``properties``, ``intent_tags`` are empty; add them after review.
  - ``*args`` / ``**kwargs`` are omitted from the type string and the arity count.
  - Unannotated parameter/return types render as ``unknown`` (a placeholder, not a Nova builtin).
"""

from __future__ import annotations

import argparse
import ast
import json
import sys
from pathlib import Path

# Higher-fidelity (v0.2) helpers live in the shared ingest-common dir: structured type ASTs and real
# examples extracted from doctests. Imported only for --v2; the v0.1 path stays self-contained.
sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "ingest-common"))
from nl_examples import examples_from_docstring  # noqa: E402
from nl_predicates import PredicateError, predicate_from_py  # noqa: E402
from nl_types import python_function_type  # noqa: E402

# ---------------------------------------------------------------------------
# BLAKE3-256 (vendored, pure-Python). Faithful to the official reference
# implementation (https://github.com/BLAKE3-team/BLAKE3-specs). Unkeyed hash only.
# ---------------------------------------------------------------------------

_OUT_LEN = 32
_BLOCK_LEN = 64
_CHUNK_LEN = 1024

_CHUNK_START = 1 << 0
_CHUNK_END = 1 << 1
_PARENT = 1 << 2
_ROOT = 1 << 3

_IV = [
    0x6A09E667, 0xBB67AE85, 0x3C6EF372, 0xA54FF53A,
    0x510E527F, 0x9B05688C, 0x1F83D9AB, 0x5BE0CD19,
]

_MSG_PERMUTATION = [2, 6, 3, 10, 7, 0, 4, 13, 1, 11, 12, 5, 9, 14, 15, 8]

_MASK32 = 0xFFFFFFFF


def _add32(a: int, b: int) -> int:
    return (a + b) & _MASK32


def _rotr32(x: int, n: int) -> int:
    return ((x >> n) | (x << (32 - n))) & _MASK32


def _g(state, a, b, c, d, mx, my):
    state[a] = _add32(_add32(state[a], state[b]), mx)
    state[d] = _rotr32(state[d] ^ state[a], 16)
    state[c] = _add32(state[c], state[d])
    state[b] = _rotr32(state[b] ^ state[c], 12)
    state[a] = _add32(_add32(state[a], state[b]), my)
    state[d] = _rotr32(state[d] ^ state[a], 8)
    state[c] = _add32(state[c], state[d])
    state[b] = _rotr32(state[b] ^ state[c], 7)


def _round(state, m):
    # columns
    _g(state, 0, 4, 8, 12, m[0], m[1])
    _g(state, 1, 5, 9, 13, m[2], m[3])
    _g(state, 2, 6, 10, 14, m[4], m[5])
    _g(state, 3, 7, 11, 15, m[6], m[7])
    # diagonals
    _g(state, 0, 5, 10, 15, m[8], m[9])
    _g(state, 1, 6, 11, 12, m[10], m[11])
    _g(state, 2, 7, 8, 13, m[12], m[13])
    _g(state, 3, 4, 9, 14, m[14], m[15])


def _permute(m):
    return [m[_MSG_PERMUTATION[i]] for i in range(16)]


def _compress(chaining_value, block_words, counter, block_len, flags):
    state = [
        chaining_value[0], chaining_value[1], chaining_value[2], chaining_value[3],
        chaining_value[4], chaining_value[5], chaining_value[6], chaining_value[7],
        _IV[0], _IV[1], _IV[2], _IV[3],
        counter & _MASK32, (counter >> 32) & _MASK32, block_len, flags,
    ]
    block = list(block_words)
    for _ in range(6):
        _round(state, block)
        block = _permute(block)
    _round(state, block)  # 7th round, no trailing permutation
    for i in range(8):
        state[i] ^= state[i + 8]
        state[i + 8] ^= chaining_value[i]
    return state


def _words_from_le_bytes(b: bytes):
    return [int.from_bytes(b[i:i + 4], "little") for i in range(0, len(b), 4)]


class _Output:
    __slots__ = ("input_cv", "block_words", "counter", "block_len", "flags")

    def __init__(self, input_cv, block_words, counter, block_len, flags):
        self.input_cv = input_cv
        self.block_words = block_words
        self.counter = counter
        self.block_len = block_len
        self.flags = flags

    def chaining_value(self):
        return _compress(self.input_cv, self.block_words, self.counter, self.block_len, self.flags)[:8]

    def root_bytes(self, length: int) -> bytes:
        out = bytearray()
        counter = 0
        while len(out) < length:
            words = _compress(self.input_cv, self.block_words, counter, self.block_len, self.flags | _ROOT)
            for w in words:
                out += w.to_bytes(4, "little")
            counter += 1
        return bytes(out[:length])


class _ChunkState:
    __slots__ = ("cv", "chunk_counter", "block", "block_len", "blocks_compressed", "flags")

    def __init__(self, key_words, chunk_counter, flags):
        self.cv = list(key_words)
        self.chunk_counter = chunk_counter
        self.block = bytearray()
        self.block_len = 0
        self.blocks_compressed = 0
        self.flags = flags

    def length(self):
        return _BLOCK_LEN * self.blocks_compressed + self.block_len

    def _start_flag(self):
        return _CHUNK_START if self.blocks_compressed == 0 else 0

    def update(self, data: bytes):
        pos = 0
        n = len(data)
        while pos < n:
            if self.block_len == _BLOCK_LEN:
                words = _words_from_le_bytes(self.block)
                self.cv = _compress(self.cv, words, self.chunk_counter, _BLOCK_LEN,
                                    self.flags | self._start_flag())[:8]
                self.blocks_compressed += 1
                self.block = bytearray()
                self.block_len = 0
            take = min(_BLOCK_LEN - self.block_len, n - pos)
            self.block += data[pos:pos + take]
            self.block_len += take
            pos += take

    def output(self):
        block = bytes(self.block) + b"\x00" * (_BLOCK_LEN - self.block_len)
        words = _words_from_le_bytes(block)
        return _Output(self.cv, words, self.chunk_counter, self.block_len,
                       self.flags | self._start_flag() | _CHUNK_END)


def _parent_output(left_cv, right_cv, key_words, flags):
    return _Output(key_words, left_cv + right_cv, 0, _BLOCK_LEN, _PARENT | flags)


class _Hasher:
    def __init__(self, key_words, flags):
        self.chunk_state = _ChunkState(key_words, 0, flags)
        self.key_words = list(key_words)
        self.cv_stack = []
        self.flags = flags

    def _add_chunk_cv(self, new_cv, total_chunks):
        while total_chunks & 1 == 0:
            new_cv = _parent_output(self.cv_stack.pop(), new_cv, self.key_words, self.flags).chaining_value()
            total_chunks >>= 1
        self.cv_stack.append(new_cv)

    def update(self, data: bytes):
        pos = 0
        n = len(data)
        while pos < n:
            if self.chunk_state.length() == _CHUNK_LEN:
                chunk_cv = self.chunk_state.output().chaining_value()
                total = self.chunk_state.chunk_counter + 1
                self._add_chunk_cv(chunk_cv, total)
                self.chunk_state = _ChunkState(self.key_words, total, self.flags)
            take = min(_CHUNK_LEN - self.chunk_state.length(), n - pos)
            self.chunk_state.update(data[pos:pos + take])
            pos += take

    def finalize(self, length: int = _OUT_LEN) -> bytes:
        output = self.chunk_state.output()
        remaining = len(self.cv_stack)
        while remaining > 0:
            remaining -= 1
            output = _parent_output(self.cv_stack[remaining], output.chaining_value(),
                                    self.key_words, self.flags)
        return output.root_bytes(length)


def _blake3_256_pure(data: bytes) -> bytes:
    h = _Hasher(_IV, 0)
    h.update(data)
    return h.finalize(_OUT_LEN)


try:  # prefer the native extension for speed if present; pure impl is the contract.
    import blake3 as _native_blake3

    def blake3_256(data: bytes) -> bytes:
        return _native_blake3.blake3(data).digest()
except Exception:  # pragma: no cover - exercised only when the package is installed
    blake3_256 = _blake3_256_pure


# ---------------------------------------------------------------------------
# JCS canonicalization (RFC 8785) — the subset needed for function records.
# ---------------------------------------------------------------------------

def _jcs_string(s: str) -> str:
    # Python's json string encoding already matches JCS: it escapes only ", \, and the
    # control characters U+0000–U+001F, using the short forms (\n \t \r \b \f) where
    # defined and \uXXXX otherwise, and leaves all other characters (incl. non-ASCII)
    # verbatim when ensure_ascii=False.
    return json.dumps(s, ensure_ascii=False)


def _es_number(x: float) -> str:
    """A finite double as the canonical JCS decimal: ECMAScript ``Number::toString`` conditioned per
    RFC 8785 §3.2.2.3 — matches the reference Rust validator (serde_jcs) byte-for-byte (pinned by the
    conformance tests). Needed for v0.2 example values that are floats."""
    if x != x or x == float("inf") or x == float("-inf"):
        raise ValueError("NaN/Infinity have no JCS representation")
    if x == 0:
        return "0"
    sign = "-" if x < 0 else ""
    r = repr(abs(x)).replace("E", "e")
    mant, _, exp = r.partition("e")
    exp = int(exp) if exp else 0
    intp, _, frac = mant.partition(".")
    digits = int(intp + frac)
    e10 = exp - len(frac)
    while digits % 10 == 0:
        digits //= 10
        e10 += 1
    s = str(digits)
    k = len(s)
    n = e10 + k
    if k <= n <= 21:
        body = s + "0" * (n - k)
    elif 0 < n <= 21:
        body = s[:n] + "." + s[n:]
    elif -6 < n <= 0:
        body = "0." + "0" * (-n) + s
    else:
        e = n - 1
        body = (s if k == 1 else s[0] + "." + s[1:]) + "e" + ("+" if e >= 0 else "-") + str(abs(e))
    return sign + body


def _jcs_serialize(obj) -> str:
    if obj is True:
        return "true"
    if obj is False:
        return "false"
    if obj is None:
        return "null"
    if isinstance(obj, str):
        return _jcs_string(obj)
    if isinstance(obj, int):  # bool already handled above
        return str(obj)
    if isinstance(obj, float):
        return _es_number(obj)
    if isinstance(obj, (list, tuple)):
        return "[" + ",".join(_jcs_serialize(x) for x in obj) + "]"
    if isinstance(obj, dict):
        # JCS sorts members by their UTF-16 code-unit representation. Comparing the
        # UTF-16-BE byte encoding lexicographically reproduces that order exactly
        # (incl. correct surrogate ordering); all our keys are ASCII regardless.
        items = sorted(obj.items(), key=lambda kv: kv[0].encode("utf-16-be"))
        return "{" + ",".join(_jcs_string(k) + ":" + _jcs_serialize(v) for k, v in items) + "}"
    raise TypeError(f"cannot JCS-serialize value of type {type(obj).__name__}")


def canonicalize(obj) -> bytes:
    """Return the JCS (RFC 8785) canonical UTF-8 bytes of ``obj`` (no trailing newline)."""
    return _jcs_serialize(obj).encode("utf-8")


def format_hash(prefix: str, digest: bytes) -> str:
    return f"{prefix}_{digest.hex()}"


def content_hash(record: dict, prefix: str, strip: tuple = ("hash",)) -> str:
    """Compute a Nova Lingua content-address: strip fields, JCS-canonicalize, BLAKE3-256."""
    stripped = {k: v for k, v in record.items() if k not in strip}
    return format_hash(prefix, blake3_256(canonicalize(stripped)))


# ---------------------------------------------------------------------------
# Python type annotation -> Nova Lingua v0.1 surface type string.
# ---------------------------------------------------------------------------

# Atomic Python types -> Nova Lingua builtin atomic types (lowercase per type-expression.schema.json).
_ATOMIC = {
    "int": "int",
    "bool": "bool",
    "float": "float",
    "str": "string",
    "bytes": "bytes",
    "bytearray": "bytes",
    "None": "unit",
    "NoneType": "unit",
    "object": "unknown",
    "Any": "unknown",
}

# Python container constructors -> Nova Lingua builtin constructors (PascalCase).
_LIST_CTORS = {"list", "List", "Sequence", "Iterable", "MutableSequence"}
_SET_CTORS = {"set", "Set", "frozenset", "FrozenSet", "MutableSet", "AbstractSet"}
_MAP_CTORS = {"dict", "Dict", "Mapping", "MutableMapping"}


def _attr_name(node: ast.AST) -> str:
    """Last component of a dotted name: ``typing.List`` -> ``List``; bare ``List`` -> ``List``."""
    if isinstance(node, ast.Attribute):
        return node.attr
    if isinstance(node, ast.Name):
        return node.id
    return ""


def _render_type(node, typevars: set) -> str:
    if node is None:
        return "unknown"

    if isinstance(node, ast.Constant):
        if node.value is None:
            return "unit"
        if isinstance(node.value, str):  # forward reference, e.g. -> "MyType"
            return node.value
        if node.value is Ellipsis:
            return "unknown"
        return str(node.value)

    if isinstance(node, ast.Name):
        if node.id in typevars:
            return _tvar(node.id)
        if node.id in _ATOMIC:
            return _ATOMIC[node.id]
        return node.id  # a user/class type name; kept verbatim as a hint

    if isinstance(node, ast.Attribute):
        name = node.attr
        if name in _ATOMIC:
            return _ATOMIC[name]
        return name

    if isinstance(node, ast.Subscript):
        return _render_subscript(node, typevars)

    if isinstance(node, ast.BinOp) and isinstance(node.op, ast.BitOr):
        return _render_union(_flatten_bitor(node), typevars)

    # Fallback: best-effort source rendering.
    try:
        return ast.unparse(node)
    except Exception:
        return "unknown"


def _tvar(name: str) -> str:
    # Nova type variables match ^[a-z][a-zA-Z0-9_']* ; Python TypeVars are usually
    # PascalCase (T, KT, T_co). Lowercase to fit, preserving the rest.
    lowered = name.lower()
    return lowered if lowered and lowered[0].isalpha() else "a"


def _subscript_args(node: ast.Subscript):
    sl = node.slice
    if isinstance(sl, ast.Tuple):
        return list(sl.elts)
    return [sl]


def _render_subscript(node: ast.Subscript, typevars: set) -> str:
    ctor = _attr_name(node.value)
    args = _subscript_args(node)

    if ctor == "Optional":
        inner = _render_type(args[0], typevars) if args else "unknown"
        return f"Maybe {_paren(inner)}"

    if ctor == "Union":
        return _render_union(args, typevars)

    if ctor in _LIST_CTORS:
        inner = _render_type(args[0], typevars) if args else "unknown"
        return f"List {_paren(inner)}"

    if ctor in _SET_CTORS:
        inner = _render_type(args[0], typevars) if args else "unknown"
        return f"Set {_paren(inner)}"

    if ctor in _MAP_CTORS:
        k = _render_type(args[0], typevars) if len(args) > 0 else "unknown"
        v = _render_type(args[1], typevars) if len(args) > 1 else "unknown"
        return f"Map {_paren(k)} {_paren(v)}"

    if ctor in ("tuple", "Tuple"):
        elems = [_render_type(a, typevars) for a in args]
        if len(elems) >= 2:
            return "(" + ", ".join(elems) + ")"
        return elems[0] if elems else "unit"

    if ctor == "Callable":
        # Callable[[A, B], R] -> (A, B) -> R
        if len(args) == 2 and isinstance(args[0], ast.List):
            params = ", ".join(_render_type(a, typevars) for a in args[0].elts)
            ret = _render_type(args[1], typevars)
            return f"({params}) -> {ret}"
        return "unknown"

    # Unknown constructor applied to args -> render as application, keeping the name.
    ctor_name = ctor if ctor not in _ATOMIC else _ATOMIC[ctor]
    rendered = " ".join(_paren(_render_type(a, typevars)) for a in args)
    return f"{ctor_name} {rendered}".strip()


def _flatten_bitor(node) -> list:
    if isinstance(node, ast.BinOp) and isinstance(node.op, ast.BitOr):
        return _flatten_bitor(node.left) + _flatten_bitor(node.right)
    return [node]


def _is_none(node) -> bool:
    return isinstance(node, ast.Constant) and node.value is None

def _render_union(members: list, typevars: set) -> str:
    non_none = [m for m in members if not _is_none(m)]
    has_none = any(_is_none(m) for m in members)
    rendered = [_render_type(m, typevars) for m in non_none]
    if has_none and len(rendered) == 1:
        return f"Maybe {_paren(rendered[0])}"
    body = " | ".join(rendered) if rendered else "unit"
    if has_none:
        body = f"Maybe ({body})"
    return body


def _paren(t: str) -> str:
    """Parenthesize a rendered type if it contains a space (so application nests correctly)."""
    if " " in t and not (t.startswith("(") and t.endswith(")")):
        return f"({t})"
    return t


def _collect_referenced_typevars(annotations, typevars: set) -> list:
    """Type variables (lowercased) actually referenced in the given annotation nodes, sorted."""
    seen = set()
    for node in annotations:
        if node is None:
            continue
        for sub in ast.walk(node):
            if isinstance(sub, ast.Name) and sub.id in typevars:
                seen.add(_tvar(sub.id))
    return sorted(seen)


# ---------------------------------------------------------------------------
# Function -> record.
# ---------------------------------------------------------------------------

def _fixed_params(func) -> list:
    """Positional-or-keyword + positional-only + keyword-only args (excludes *args/**kwargs)."""
    a = func.args
    return list(a.posonlyargs) + list(a.args) + list(a.kwonlyargs)


def _sanitize_hint(name: str) -> str:
    """Coerce a name to the name_hint pattern ^[a-z][a-z0-9_]*$ (best effort; hints carry no weight)."""
    out = []
    for ch in name.lower():
        out.append(ch if (ch.isascii() and (ch.isalnum() or ch == "_")) else "_")
    s = "".join(out).lstrip("_0123456789")
    return s or "fn"


def _format_signature(func, typevars: set) -> str:
    params = _fixed_params(func)
    param_types = [_render_type(p.annotation, typevars) for p in params]
    ret = _render_type(func.returns, typevars)

    annotations = [p.annotation for p in params] + [func.returns]
    tvars = _collect_referenced_typevars(annotations, typevars)
    prefix = f"forall {' '.join(tvars)}. " if tvars else ""
    return f"{prefix}({', '.join(param_types)}) -> {ret}"


def _name_hints(fn_name: str, module_name: str | None) -> list:
    """Sanitized bare name, optionally also '<module>_<fn>'."""
    hints = [_sanitize_hint(fn_name)]
    if module_name:
        combined = _sanitize_hint(f"{module_name}_{fn_name}")
        if combined not in hints:
            hints.append(combined)
    return hints


def _body_hash(func) -> str:
    """BLAKE3 over the body's normalised source (signature excluded), like the Rust tool's token
    stream. Not a Nova Lingua body AST."""
    body_src = "\n".join(ast.unparse(stmt) for stmt in func.body)
    return format_hash("expr", blake3_256(body_src.encode("utf-8")))


def build_record(func, module_name: str | None, module_typevars: set) -> dict:
    """Build a Nova Lingua v0.1 function record (string type, placeholder example) from a def node."""
    typevars = set(module_typevars)
    # PEP 695 per-function type parameters (def f[T](...)).
    for tp in getattr(func, "type_params", []) or []:
        typevars.add(tp.name)

    arity = len(_fixed_params(func))
    record = {
        "schema_version": "0.1.0",
        "hash": "fn_" + "0" * 64,  # placeholder; recomputed below
        "name_hints": _name_hints(func.name, module_name),
        "signature": {
            "type": _format_signature(func, typevars),
            "refinements": [],
            "effects": [],
            "capabilities": [],
            "terminates": "unknown",
        },
        "examples": [{"args": [None] * arity, "result": None}],
        "properties": [],
        "intent_tags": [],
        "derived_from": None,
        "supersedes": None,
        "body_hash": _body_hash(func),
    }
    record["hash"] = content_hash(record, "fn", strip=("hash",))
    return record


def _fn_param_result_types(type_ast: dict):
    """(param_types, result_type) from a (possibly forall-wrapped) fn type AST — value-encoding hints."""
    t = type_ast["body"] if type_ast.get("kind") == "forall" else type_ast
    if t.get("kind") == "fn":
        return t.get("params", []), t.get("result")
    return [], None


def _preconditions(func) -> list:
    """Leading `assert <cond>` statements (before any real logic) become refinement preconditions
    {kind: 'pre', expr: <predicate AST>}. Asserts whose condition isn't an expressible predicate are
    skipped. The assert *message* is ignored."""
    body = func.body
    start = 1 if (body and isinstance(body[0], ast.Expr)
                  and isinstance(body[0].value, ast.Constant)
                  and isinstance(body[0].value.value, str)) else 0   # skip a docstring
    refs = []
    for stmt in body[start:]:
        if not isinstance(stmt, ast.Assert):
            break                                                    # only a leading run of asserts
        try:
            refs.append({"kind": "pre", "expr": predicate_from_py(stmt.test)})
        except PredicateError:
            continue
    return refs


def build_v2_record(func, module_name: str | None) -> dict | None:
    """Build a v0.2 record: a STRUCTURED type AST (nl_types) + REAL examples from the function's
    doctests (nl_examples). Returns None when there are no usable doctest examples — v0.2 requires
    >=1 — so the caller falls back to a v0.1 record."""
    type_ast = python_function_type(func)
    param_types, result_type = _fn_param_result_types(type_ast)
    examples = examples_from_docstring(func.name, ast.get_docstring(func), param_types, result_type)
    if not examples:
        return None
    record = {
        "schema_version": "0.2.0",
        "hash": "fn_" + "0" * 64,
        "name_hints": _name_hints(func.name, module_name),
        "signature": {
            "type": type_ast,
            "refinements": _preconditions(func),
            "effects": [],
            "capabilities": [],
            "terminates": "unknown",
        },
        "examples": examples,
        "intent_tags": [],
        "derived_from": None,
        "supersedes": None,
        "body_hash": _body_hash(func),
    }
    record["hash"] = content_hash(record, "fn", strip=("hash",))
    return record


def _public_names(tree: ast.Module) -> set | None:
    """Names listed in a module-level ``__all__`` literal, or None if absent/unreadable."""
    for node in tree.body:
        targets = []
        if isinstance(node, ast.Assign):
            targets = node.targets
        elif isinstance(node, ast.AnnAssign) and node.target is not None:
            targets = [node.target]
        else:
            continue
        for t in targets:
            if isinstance(t, ast.Name) and t.id == "__all__":
                val = node.value
                if isinstance(val, (ast.List, ast.Tuple, ast.Set)):
                    names = set()
                    for elt in val.elts:
                        if isinstance(elt, ast.Constant) and isinstance(elt.value, str):
                            names.add(elt.value)
                    return names
    return None


def _module_typevars(tree: ast.Module) -> set:
    """Module-level names bound to TypeVar/TypeVarTuple/ParamSpec, plus PEP 695 module type aliases."""
    names = set()
    for node in tree.body:
        if isinstance(node, ast.Assign) and isinstance(node.value, ast.Call):
            callee = _attr_name(node.value.func)
            if callee in ("TypeVar", "TypeVarTuple", "ParamSpec"):
                for t in node.targets:
                    if isinstance(t, ast.Name):
                        names.add(t.id)
    return names


def iter_functions(tree: ast.Module, include_private: bool):
    """Yield top-level function defs that count as public."""
    explicit = _public_names(tree)
    for node in tree.body:
        if not isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
            continue
        if include_private:
            yield node
        elif explicit is not None:
            if node.name in explicit:
                yield node
        elif not node.name.startswith("_"):
            yield node


def records_from_source(source: str, module_name: str | None, include_private: bool,
                        v2: bool = False) -> list:
    tree = ast.parse(source)
    module_tvars = _module_typevars(tree)
    out = []
    for fn in iter_functions(tree, include_private):
        # In --v2 mode, emit a structured v0.2 record when the function has usable doctest examples;
        # otherwise fall back to a v0.1 record so no function is dropped.
        rec = build_v2_record(fn, module_name) if v2 else None
        if rec is None:
            rec = build_record(fn, module_name, module_tvars)
        out.append(rec)
    return out


# ---------------------------------------------------------------------------
# CLI.
# ---------------------------------------------------------------------------

def _build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(
        prog="nl-ingest-py",
        description="Parse public Python functions and emit Nova Lingua v0.1 function records (JSONL).",
    )
    p.add_argument("files", nargs="*", type=Path, help="one or more .py source files to ingest")
    p.add_argument("--module", dest="module", default=None,
                   help="module/package name; adds '<module>_<fn>' to name_hints alongside the bare name")
    p.add_argument("--pretty", action="store_true",
                   help="pretty-print each record (default: compact JSONL, one record per line)")
    p.add_argument("--include-private", action="store_true",
                   help="ingest every top-level function, including _-prefixed and non-__all__ ones")
    p.add_argument("--v2", action="store_true",
                   help="higher fidelity: emit v0.2 records (structured type AST + real examples from "
                        "doctests) for functions that have usable doctests; v0.1 otherwise")
    return p


def main(argv=None) -> int:
    args = _build_parser().parse_args(argv)
    if not args.files:
        print("nl-ingest-py: no files given — pass one or more .py paths", file=sys.stderr)
        return 1

    exit_code = 0
    for path in args.files:
        try:
            source = path.read_text(encoding="utf-8")
        except OSError as e:
            print(f"nl-ingest-py: reading {path}: {e}", file=sys.stderr)
            exit_code = 1
            continue
        try:
            records = records_from_source(source, args.module, args.include_private, v2=args.v2)
        except SyntaxError as e:
            print(f"nl-ingest-py: parsing {path}: {e}", file=sys.stderr)
            exit_code = 1
            continue
        for record in records:
            if args.pretty:
                print(json.dumps(record, indent=2, ensure_ascii=False))
            else:
                print(json.dumps(record, separators=(",", ":"), ensure_ascii=False))
    return exit_code


if __name__ == "__main__":
    raise SystemExit(main())
