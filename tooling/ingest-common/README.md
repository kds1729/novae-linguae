# nl_core — shared core for ingestion adapters

`nl_core.py` is the language-neutral half of a *Novae Linguae* ingestion adapter, shared by the
Haskell ([`ingest-haskell`](../ingest-haskell/)) and npm/TypeScript ([`ingest-npm`](../ingest-npm/))
adapters. It is **stdlib-only** (zero third-party dependencies) and provides:

- **BLAKE3-256** — a vendored pure-Python implementation faithful to the official reference, plus
  a transparent fast path to the native `blake3` package when it happens to be installed.
- **JCS / RFC 8785** canonicalization (the subset needed for function records).
- **Content-addressing** — `content_hash(record, prefix)` does *strip → JCS → BLAKE3*; and
  `build_record(name, type_str, arity, body_text, …)` assembles a schema-valid v0.1
  function record with a real `fn_` hash and `expr_` `body_hash`.
- **Bracket-aware string helpers** (`split_top`, `count_top`, `find_matching`, `sanitize_hint`)
  used by the per-language parsers.

Two more modules support **higher-fidelity, structured-AST ingestion** (toward v0.2 records):

- **`nl_values.py`** — `to_value_ast(py_value, expected=None)` maps a Python value to the structured
  Nova Lingua **value-expression AST** ([`spec/value-expression.schema.json`](../../spec/value-expression.schema.json))
  used for `examples.args[i]` / `examples.result`. Eleven value kinds; big ints become decimal
  strings; `bool` is handled before `int`; a `nat` type hint promotes a non-negative int. Values with
  **no** value-AST form (sets, `Map` values, non-identifier dict keys, custom objects, non-finite
  floats) raise `ValueEncodeError` so the caller skips that example — nothing is fabricated or
  lossily coerced.
- **`nl_examples.py`** — **example enrichment**: `examples_from_docstring(func, docstring, …)` and the
  `python3 nl_examples.py <module.py>` CLI extract *real* worked examples from **Python doctests**.
  It parses `>>> func(<literal args>)` calls and their literal expected output and `ast.literal_eval`s
  **only the literals — it never executes the function** — then encodes inputs/outputs as value ASTs.
  Non-literal or unrepresentable doctests are skipped. This fills the gap that blocks adapter drafts
  from becoming complete v0.2 records (which require ≥1 worked example as value ASTs). Execution-based
  generation (synthesise inputs from a type, run pure functions, capture outputs) is a planned
  follow-on for functions that lack doctests.

A language adapter supplies only the *front end*: parse the source, extract each public function's
name, type string, arity, and a body text to hash, then call `build_record`. Everything produced
this way passes `nl-validator validate` and `verify`, and its hashes agree byte-for-byte with the
Rust reference implementation — the cross-implementation contract pinned by
[`spec/canonical-serialization.md`](../../spec/canonical-serialization.md) and
[`spec/conformance/`](../../spec/conformance/).

> The original Python-source adapter ([`ingest-python`](../ingest-python/)) predates this module
> and carries its own self-contained copy of the same core; this directory is the shared home for
> adapters written after it. All copies are kept honest by each adapter's test suite, which checks
> BLAKE3 against the official vectors and cross-validates emitted records against `nl-validator`.

## License

Dual-licensed under Apache-2.0 OR MIT, same as the rest of *Novae Linguae*.
