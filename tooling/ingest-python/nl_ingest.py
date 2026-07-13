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
from nl_body import body_ast_from_py, function_raises, function_subscript_reads  # noqa: E402
from nl_canon import canonical_dependency_artifacts  # noqa: E402
from nl_examples import examples_from_docstring  # noqa: E402
from nl_effects import effects_from_py, terminates_from_py  # noqa: E402
from nl_synth import SynthError, synth_args  # noqa: E402
from nl_values import ValueEncodeError, to_value_ast  # noqa: E402
from property_catalog import match_catalog  # noqa: E402
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
    """The function's ``body_hash``: the canonical content-address of a real Nova Lingua body AST
    when the body is in the v1 subset (single result expression of var/lit/app/field), else the
    synthetic BLAKE3 over the body's normalised source — byte-identical to before for the fallback."""
    body_ast = body_ast_from_py(func)
    if body_ast is not None:
        return format_hash("expr", blake3_256(canonicalize(body_ast)))
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


def stub_annotations(stub_source: str) -> dict:
    """name -> stub FunctionDef for every top-level, single-definition, non-overloaded function in
    a .pyi stub. Overloaded names (typeshed's `@overload` sets, or plain duplicates) are dropped —
    a set of overloads does not determine ONE type, and we never pick one silently."""
    tree = ast.parse(stub_source)
    seen: dict = {}
    dropped = set()
    for node in tree.body:
        if not isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
            continue
        overloaded = any((isinstance(d, ast.Name) and d.id == "overload")
                         or (isinstance(d, ast.Attribute) and d.attr == "overload")
                         for d in node.decorator_list)
        if overloaded or node.name in seen:
            dropped.add(node.name)
        seen[node.name] = node
    return {k: v for k, v in seen.items() if k not in dropped}


def _graft_stub(func, stub) -> None:
    """Copy parameter/return annotations from a stub def onto the SOURCE def, positionally, where
    the source lacks them (a source annotation always wins — the stub is secondary description).
    Refuses (no-op) on a positional-arity mismatch: pairing would be a guess."""
    src_params = list(func.args.posonlyargs) + list(func.args.args)
    stub_params = list(stub.args.posonlyargs) + list(stub.args.args)
    if len(src_params) != len(stub_params):
        return
    for sp, tp in zip(src_params, stub_params):
        if sp.annotation is None:
            sp.annotation = tp.annotation
    if func.returns is None:
        func.returns = stub.returns


_EXEC_MODULES: dict = {}


def _exec_module(path):
    """Import the source file being ingested (cached per path), for `--exec-examples` observation
    runs. Returns None when the module cannot be imported standalone — the caller falls back to
    v0.1 rather than guessing."""
    import importlib.util

    key = str(path)
    if key not in _EXEC_MODULES:
        try:
            spec = importlib.util.spec_from_file_location(f"_nl_exec_{len(_EXEC_MODULES)}", key)
            mod = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(mod)
            _EXEC_MODULES[key] = mod
        except BaseException:
            _EXEC_MODULES[key] = None
    return _EXEC_MODULES[key]


def _executed_examples(func, exec_path, param_types, result_type, totalized, effects):
    """OBSERVED worked examples for an annotated, pure, lifted function with no doctest: the
    annotation licenses the record (structured type), the source documents no value — so run the
    REAL function once per synthesized argument set and record what it answers (the license/observe
    split of spec/expressiveness.md, applied to source code; opt-in via --exec-examples exactly as
    the OpenAPI adapter's live gate is via --verify-against). The lifted body is then held to the
    observation by `run`/`certify`: a lifting that disagrees with the source's real semantics fails
    rather than publishes. Honest bounds: only effect-free functions run (a `panic` effect is
    allowed only when the body was raise-totalized — a raising run IS the None example then), every
    call runs under a 2s alarm (a hang is a refusal, not a wait), and an exception from a
    non-totalized function refuses that example."""
    import signal

    if effects not in ([], ["panic"]) or (effects == ["panic"] and not totalized):
        return []
    if result_type is None:
        return []
    try:
        arg_sets = synth_args(param_types)
    except SynthError:
        return []
    mod = _exec_module(exec_path)
    fn = getattr(mod, func.name, None) if mod else None
    if not callable(fn):
        return []
    out = []
    for args in arg_sets:
        def _timeout(signum, frame):
            raise TimeoutError
        old = signal.signal(signal.SIGALRM, _timeout)
        signal.alarm(2)
        try:
            result_py = fn(*args)
        except TimeoutError:
            signal.alarm(0)
            signal.signal(signal.SIGALRM, old)
            return []                      # a hang refuses the whole synthesis, not just one example
        except Exception:
            signal.alarm(0)
            signal.signal(signal.SIGALRM, old)
            if totalized:                  # a raising run IS the None-case example
                try:
                    enc_args = [to_value_ast(a, t) for a, t in zip(args, param_types)]
                except ValueEncodeError:
                    continue
                out.append({"args": enc_args, "result": {"kind": "variant", "tag": "None"}})
            continue
        signal.alarm(0)
        signal.signal(signal.SIGALRM, old)
        try:
            enc_args = [to_value_ast(a, t) for a, t in zip(args, param_types)]
            result = to_value_ast(result_py, result_type)
        except ValueEncodeError:
            continue
        out.append({"args": enc_args, "result": result})
    return out


def build_v2_record(func, module_name: str | None, imports=None, with_properties=False,
                    exec_path=None) -> dict | None:
    """Build a v0.2 record: a STRUCTURED type AST (nl_types) + REAL examples from the function's
    doctests (nl_examples). Returns None when there are no usable doctest examples — v0.2 requires
    >=1 — so the caller falls back to a v0.1 record. With ``exec_path`` (--exec-examples), a
    doctest-less function that is fully annotated, effect-free, and lifted instead gets OBSERVED
    examples by executing the real source function on type-synthesized arguments (see
    ``_executed_examples``). ``imports`` is the (alias, fromimp) module-map
    pair (see nl_effects) used to classify qualified calls when inferring effects. When
    ``with_properties`` is set, well-known functions get curated algebraic laws from the catalog."""
    type_ast = python_function_type(func)
    # Raise-totalization (the None<->Maybe boundary): when the lifted body totalizes a raising
    # function (raise -> the None variant, returns Just-wrapped), the record's declared result is
    # `Maybe T` — the type IS the transform — and the body no longer panics, so the inferred
    # `panic` effect is dropped. Only when the body actually lifts: a fallback source-hash body
    # keeps the plain type it had.
    totalized = body_ast_from_py(func) is not None \
        and (function_raises(func) or function_subscript_reads(func))
    if totalized:
        t = type_ast["body"] if type_ast.get("kind") == "forall" else type_ast
        t["result"] = {"kind": "apply", "ctor": {"kind": "builtin", "name": "Maybe"},
                       "args": [t["result"]]}
    param_types, result_type = _fn_param_result_types(type_ast)
    alias, fromimp = imports if imports else ({}, {})
    effects = effects_from_py(func, alias, fromimp)
    examples = examples_from_docstring(func.name, ast.get_docstring(func), param_types, result_type)
    if not examples and exec_path is not None and body_ast_from_py(func) is not None:
        # No doctest, but the annotation licenses the record and --exec-examples sanctions an
        # observation: the type AST must be concrete (synth_args refuses type variables) and the
        # forall wrapper absent — a polymorphic signature has no synthesized inhabitant.
        if type_ast.get("kind") != "forall":
            examples = _executed_examples(func, exec_path, param_types, result_type,
                                          totalized, effects)
    if not examples:
        return None
    hints = _name_hints(func.name, module_name)
    properties, tags = match_catalog(hints, len(param_types)) if with_properties else ([], [])
    if totalized:
        effects = [e for e in effects if e != "panic"]
    record = {
        "schema_version": "0.2.0",
        "hash": "fn_" + "0" * 64,
        "name_hints": hints,
        "signature": {
            "type": type_ast,
            "refinements": _preconditions(func),
            "effects": effects,
            "capabilities": [],
            "terminates": terminates_from_py(func),
        },
        "examples": examples,
        "intent_tags": tags,
        "derived_from": None,
        "supersedes": None,
        "body_hash": _body_hash(func),
    }
    # `properties` is optional in v0.2; include it only when laws were attached, so records without
    # laws hash exactly as before.
    if properties:
        record["properties"] = properties
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


def _module_imports(tree: ast.Module):
    """(alias, fromimp) maps for effect inference: ``alias`` maps an ``import x [as y]`` local root
    to its dotted module; ``fromimp`` maps a ``from m import n [as a]`` bound name to module m."""
    alias: dict = {}
    fromimp: dict = {}
    for node in tree.body:
        if isinstance(node, ast.Import):
            for n in node.names:
                if n.asname:
                    alias[n.asname] = n.name
                else:
                    root = n.name.split(".")[0]
                    alias[root] = root
        elif isinstance(node, ast.ImportFrom) and node.module and (node.level or 0) == 0:
            for n in node.names:
                fromimp[n.asname or n.name] = node.module
    return alias, fromimp


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
                        v2: bool = False, with_properties: bool = False, exec_path=None,
                        stubs=None) -> list:
    tree = ast.parse(source)
    module_tvars = _module_typevars(tree)
    imports = _module_imports(tree) if v2 else None
    out = []
    for fn in iter_functions(tree, include_private):
        # A stub (.pyi) is secondary DESCRIPTION: its annotations graft onto an unannotated
        # source def (source annotations always win), so the stub supplies the type, the source
        # supplies the body, and (under --exec-examples) an execution supplies the example.
        if stubs and fn.name in stubs:
            _graft_stub(fn, stubs[fn.name])
        # In --v2 mode, emit a structured v0.2 record when the function has usable doctest examples
        # (or, under --exec-examples, observed ones); otherwise fall back to a v0.1 record so no
        # function is dropped.
        rec = build_v2_record(fn, module_name, imports, with_properties,
                              exec_path=exec_path) if v2 else None
        if rec is None:
            rec = build_record(fn, module_name, module_tvars)
        out.append(rec)
    return out


def bodies_from_source(source: str, include_private: bool) -> dict:
    """Map of body content-address -> executable body AST for every function whose body is in the
    supported subset. Used by ``--emit-dir`` to write the runnable bodies alongside the records so
    ``nl-validator run --records <dir>`` can execute the ingested functions against their examples.
    Functions outside the subset keep a synthetic ``body_hash`` and contribute no entry here."""
    tree = ast.parse(source)
    out: dict = {}
    for fn in iter_functions(tree, include_private):
        body_ast = body_ast_from_py(fn)
        if body_ast is not None:
            out[format_hash("expr", blake3_256(canonicalize(body_ast)))] = body_ast
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
    p.add_argument("--properties", action="store_true",
                   help="attach curated algebraic laws (property_catalog.json) to recognised functions "
                        "(map/filter/sort/reverse/id, ...) — implies --v2. Verify with "
                        "`nl-validator check-properties`")
    p.add_argument("--exec-examples", action="store_true",
                   help="observe worked examples by EXECUTING the ingested source (implies --v2): a "
                        "fully-annotated, effect-free, lifted function with no doctest runs once per "
                        "type-synthesized argument set and its real answers become the record's "
                        "examples — the lifted body is then held to them by run/certify. Opt-in "
                        "because it runs the code being ingested (the source-code counterpart of the "
                        "OpenAPI adapter's --verify-against live gate)")
    p.add_argument("--stubs", type=Path, default=None,
                   help="a .pyi stub file (or a directory of them, resolved as <module>.pyi via "
                        "--module) whose annotations graft onto UNANNOTATED source parameters/"
                        "returns — the stub is secondary description (typeshed being the canonical "
                        "source for the stdlib); source annotations always win, and overloaded stub "
                        "names are dropped rather than picked from")
    p.add_argument("--emit-dir", dest="emit_dir", type=Path, default=None,
                   help="also write a runnable directory: each record as <fn_hash>.json and each "
                        "executable body as <expr_hash>.json, so `nl-validator run --records <dir>` "
                        "can execute the ingested functions against their examples")
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
        stubs = None
        if args.stubs is not None:
            stub_path = args.stubs
            if stub_path.is_dir():
                stub_path = stub_path / f"{args.module or path.stem}.pyi"
            try:
                stubs = stub_annotations(stub_path.read_text(encoding="utf-8"))
            except (OSError, SyntaxError) as e:
                print(f"nl-ingest-py: stubs {stub_path}: {e}", file=sys.stderr)
                stubs = None
        try:
            v2 = args.v2 or args.properties or args.exec_examples  # both imply --v2
            records = records_from_source(source, args.module, args.include_private, v2=v2,
                                          with_properties=args.properties,
                                          exec_path=path if args.exec_examples else None,
                                          stubs=stubs)
        except SyntaxError as e:
            print(f"nl-ingest-py: parsing {path}: {e}", file=sys.stderr)
            exit_code = 1
            continue
        if args.emit_dir:
            try:
                args.emit_dir.mkdir(parents=True, exist_ok=True)
                for h, b in bodies_from_source(source, args.include_private).items():
                    (args.emit_dir / f"{h}.json").write_text(
                        json.dumps(b, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
                for record in records:
                    (args.emit_dir / f"{record['hash']}.json").write_text(
                        json.dumps(record, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
                # The canonical iteration records (nth / range_from / range): an emitted body may
                # apply them by fn_ref (subscripts, range loops, counting whiles), so the runnable
                # directory must carry them for `run --records` to link.
                for fname, artifact in canonical_dependency_artifacts():
                    (args.emit_dir / fname).write_text(
                        json.dumps(artifact, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
            except OSError as e:
                print(f"nl-ingest-py: writing emit dir {args.emit_dir}: {e}", file=sys.stderr)
                exit_code = 1
        for record in records:
            if args.pretty:
                print(json.dumps(record, indent=2, ensure_ascii=False))
            else:
                print(json.dumps(record, separators=(",", ":"), ensure_ascii=False))
    return exit_code


if __name__ == "__main__":
    raise SystemExit(main())
