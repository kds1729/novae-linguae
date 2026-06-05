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
