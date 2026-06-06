"""nl_core — shared core for Novae Linguae ingestion adapters.

Provides the language-neutral half of an ingestion adapter:

  - BLAKE3-256 (vendored pure-Python, faithful to the official reference impl).
  - JCS / RFC 8785 canonicalization (the subset needed for function records).
  - Content-address computation (strip → JCS → BLAKE3) and the v0.1
    function-record skeleton builder.
  - Small bracket-aware string helpers shared by the per-language parsers.

A language adapter (Haskell, npm/TypeScript, …) supplies only the *front end*:
extract each public function's name, type string, arity, and a body text to
hash, then call :func:`build_record`. Everything below produces records that
pass ``nl-validator validate`` and ``verify`` and whose hashes agree byte-for-byte
with the Rust reference implementation (see ``spec/canonical-serialization.md``).

Stdlib-only, zero third-party dependencies. The original ``ingest-python`` tool
predates this module and carries its own copy of the same core; this module is
the shared home for adapters written after it.
"""

from __future__ import annotations

import json

# ---------------------------------------------------------------------------
# BLAKE3-256 (vendored, pure-Python). Unkeyed hash only.
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
    _g(state, 0, 4, 8, 12, m[0], m[1])
    _g(state, 1, 5, 9, 13, m[2], m[3])
    _g(state, 2, 6, 10, 14, m[4], m[5])
    _g(state, 3, 7, 11, 15, m[6], m[7])
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
    _round(state, block)
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


def blake3_256_pure(data: bytes) -> bytes:
    h = _Hasher(_IV, 0)
    h.update(data)
    return h.finalize(_OUT_LEN)


try:  # prefer the native extension for speed if present; the pure impl is the contract.
    import blake3 as _native_blake3

    def blake3_256(data: bytes) -> bytes:
        return _native_blake3.blake3(data).digest()
except Exception:  # pragma: no cover
    blake3_256 = blake3_256_pure


# ---------------------------------------------------------------------------
# JCS canonicalization (RFC 8785) — the subset needed for function records.
# ---------------------------------------------------------------------------

def _jcs_string(s: str) -> str:
    return json.dumps(s, ensure_ascii=False)


def _es_number(x: float) -> str:
    """A finite double as the canonical JCS decimal: ECMAScript ``Number::toString`` conditioned per
    RFC 8785 §3.2.2.3. Matches the reference Rust validator (serde_jcs) byte-for-byte (pinned by the
    conformance tests). Hand-rolling number serialization is the #1 cross-implementation drift source
    (spec/canonical-serialization.md), so this is validated against the validator over a battery."""
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
    while digits % 10 == 0:          # strip trailing zeros (digits != 0 since x != 0)
        digits //= 10
        e10 += 1
    s = str(digits)
    k = len(s)
    n = e10 + k                      # position of the decimal point (digits before it)
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
    if isinstance(obj, int):
        return str(obj)
    if isinstance(obj, float):
        return _es_number(obj)
    if isinstance(obj, (list, tuple)):
        return "[" + ",".join(_jcs_serialize(x) for x in obj) + "]"
    if isinstance(obj, dict):
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
# Record building.
# ---------------------------------------------------------------------------

def sanitize_hint(name: str) -> str:
    """Coerce a name to the name_hint pattern ^[a-z][a-z0-9_]*$, or '' if nothing valid remains.

    name_hints carry no semantic weight (identity is the hash), so lossy coercion is fine.
    """
    out = []
    for ch in name.lower():
        out.append(ch if (ch.isascii() and (ch.isalnum() or ch == "_")) else "_")
    return "".join(out).lstrip("_0123456789")


def name_hints(name: str, module_name: str | None = None, extra_hints=()) -> list:
    """name_hints for a record: the sanitized bare name, an optional '<module>_<name>', and any
    extra surface names. (name_hints carry no semantic weight — identity is the hash.)"""
    hints: list = []
    bare = sanitize_hint(name)
    if bare:
        hints.append(bare)
        # Only add a module-qualified hint when the bare name is itself a valid hint.
        if module_name:
            mh = sanitize_hint(module_name)
            combined = f"{mh}_{bare}" if mh else bare
            if combined not in hints:
                hints.append(combined)
    for h in extra_hints:
        s = sanitize_hint(h)
        if s and s not in hints:
            hints.append(s)
    return hints


def build_record(name: str, type_str: str, arity: int, body_text: str,
                 module_name: str | None = None, extra_hints=()) -> dict:
    """Assemble a Nova Lingua v0.1 function record from language-neutral inputs.

    ``name``       the function's source name (used for name_hints, sanitized).
    ``type_str``   the v0.1 signature.type string (source-flavored is fine; '' -> 'unknown').
    ``arity``      number of fixed parameters; produces one placeholder example.
    ``body_text``  text whose BLAKE3 becomes ``body_hash`` (a synthetic ``expr_`` address,
                   not a Nova Lingua body AST — same limitation as the Rust/Python tools).
    ``module_name`` if given, adds a '<module>_<name>' name_hint alongside the bare name.
    ``extra_hints`` additional surface names to include as hints.
    """
    hints = name_hints(name, module_name, extra_hints)
    body_hash = format_hash("expr", blake3_256(body_text.encode("utf-8")))

    record = {
        "schema_version": "0.1.0",
        "hash": "fn_" + "0" * 64,
        "name_hints": hints,
        "signature": {
            "type": type_str if type_str else "unknown",
            "refinements": [],
            "effects": [],
            "capabilities": [],
            "terminates": "unknown",
        },
        "examples": [{"args": [None] * max(arity, 0), "result": None}],
        "properties": [],
        "intent_tags": [],
        "derived_from": None,
        "supersedes": None,
        "body_hash": body_hash,
    }
    record["hash"] = content_hash(record, "fn", strip=("hash",))
    return record


def build_v2_record(name: str, type_ast: dict, examples: list, body_text: str,
                    module_name: str | None = None, extra_hints=(),
                    effects=None, terminates=None, refinements=None) -> dict:
    """Assemble a Nova Lingua v0.2 function record: a structured ``signature.type`` AST and real
    value-AST ``examples`` (must be non-empty — v0.2 requires >=1). Same name_hints / body_hash as
    build_record. Callers (the string-based adapters) build the type AST and examples per language.

    ``effects`` / ``terminates`` / ``refinements`` are best-effort inferred fields; when omitted they
    default to the conservative ``[]`` / ``"unknown"`` / ``[]`` (see nl_effects for the caveats —
    inferred effects are a LOWER BOUND, not a purity certificate)."""
    record = {
        "schema_version": "0.2.0",
        "hash": "fn_" + "0" * 64,
        "name_hints": name_hints(name, module_name, extra_hints),
        "signature": {
            "type": type_ast,
            "refinements": refinements if refinements is not None else [],
            "effects": effects if effects is not None else [],
            "capabilities": [],
            "terminates": terminates if terminates is not None else "unknown",
        },
        "examples": examples,
        "intent_tags": [],
        "derived_from": None,
        "supersedes": None,
        # ``body_text`` may be a synthetic source string (hash its UTF-8 bytes — IDENTICAL to before)
        # or a real body-expression AST dict (hash its canonical JCS form — a resolvable expr_ address).
        "body_hash": (format_hash("expr", blake3_256(canonicalize(body_text)))
                      if isinstance(body_text, dict)
                      else format_hash("expr", blake3_256(body_text.encode("utf-8")))),
    }
    record["hash"] = content_hash(record, "fn", strip=("hash",))
    return record


# ---------------------------------------------------------------------------
# Bracket-aware string helpers shared by the per-language parsers.
# ---------------------------------------------------------------------------

_OPEN = {"(": ")", "[": "]", "{": "}", "<": ">"}
_CLOSE = {v: k for k, v in _OPEN.items()}


def split_top(s: str, sep: str, brackets: str = "()[]{}") -> list:
    """Split ``s`` on ``sep`` occurrences that sit at bracket-depth 0.

    ``brackets`` lists the bracket pairs to track, e.g. "()[]{}" or "()[]{}<>".
    ``sep`` is matched as a literal substring. Returns stripped, non-empty-aware parts
    (empty parts are preserved so callers can detect a trailing separator).
    """
    opens = brackets[0::2]
    closes = brackets[1::2]
    pair = dict(zip(closes, opens))
    parts = []
    depth = 0
    buf = []
    i = 0
    n = len(s)
    slen = len(sep)
    while i < n:
        ch = s[i]
        if ch in opens:
            depth += 1
            buf.append(ch)
            i += 1
        elif ch in closes:
            if depth > 0:
                depth -= 1
            buf.append(ch)
            i += 1
        elif depth == 0 and s[i:i + slen] == sep:
            parts.append("".join(buf))
            buf = []
            i += slen
        else:
            buf.append(ch)
            i += 1
    parts.append("".join(buf))
    return parts


def count_top(s: str, token: str, brackets: str = "()[]{}") -> int:
    """Count occurrences of ``token`` at bracket-depth 0 in ``s``."""
    return len(split_top(s, token, brackets)) - 1


def find_matching(s: str, open_idx: int) -> int:
    """Given an index of an opening bracket in ``s``, return the index of its match, or -1."""
    opener = s[open_idx]
    if opener not in _OPEN:
        return -1
    closer = _OPEN[opener]
    depth = 0
    for i in range(open_idx, len(s)):
        if s[i] == opener:
            depth += 1
        elif s[i] == closer:
            depth -= 1
            if depth == 0:
                return i
    return -1
