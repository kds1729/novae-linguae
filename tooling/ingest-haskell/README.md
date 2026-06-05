# nl-ingest-hs — Haskell source → Nova Lingua function records

`nl-ingest-hs` reads one or more `.hs` files, finds every **exported** top-level function that has
a type signature, and emits one Nova Lingua v0.1 function record per function as compact JSON
(JSONL — one object per line). It is the Haskell sibling of the Rust `nl-ingest`, the
[Python `nl-ingest-py`](../ingest-python/README.md), and the [npm `nl-ingest-ts`](../ingest-npm/README.md)
adapters. Every record either tool emits passes `nl-validator validate` and `verify` and agrees
byte-for-byte with the reference implementation on canonical form and content-hash.

## Zero dependencies

The tool runs with only `python3` (3.10+). The hashing/record core (BLAKE3-256 + JCS/RFC 8785) is
the shared, stdlib-only [`nl_core`](../ingest-common/) module; no `pip install`, no network, and
**no Haskell toolchain** is required — the tool reads `.hs` *source* with a focused, layout-aware
parser; it does not run GHC.

## Usage

```bash
python3 nl_ingest_hs.py path/to/Module.hs          # compact JSONL to stdout
python3 nl_ingest_hs.py --pretty path/to/Module.hs # readable
python3 nl_ingest_hs.py --module Data.Foo Foo.hs    # override the <module>_<fn> hint
python3 nl_ingest_hs.py --include-private Foo.hs     # ignore the export list; ingest every signature
```

The file is executable, so `./nl_ingest_hs.py …` also works.

### What counts as "public"

1. If the module has an explicit export list — `module M (foo, bar, (<+>)) where` — exactly the
   functions in it are ingested (type/class exports like `Baz(..)` are ignored).
2. If there is no export list — `module M where` — every top-level function with a signature.
3. `--include-private` ingests every top-level signature regardless of the export list.

A function is ingested only if it has a top-level `name :: Type` signature (single-line or with the
`::` on a continuation line). Functions without a signature are skipped — there is no type to record.

## What it populates

| Field | How populated |
|-------|---------------|
| `hash` | Real `fn_` BLAKE3 content-address of the record |
| `name_hints` | Sanitized function name; `<module>_<fn>` form (module from the header or `--module`). Operators (e.g. `<+>`) sanitize to nothing, so their `name_hints` is empty (valid) |
| `signature.type` | The Haskell type as a source-flavored string, whitespace-normalised |
| `examples` | One placeholder per argument: `args` = `[null, …]`, `result` = `null` |
| `body_hash` | Synthetic `expr_` BLAKE3 of the function's defining equations (or its signature if none found) — not a Nova Lingua body AST |
| `signature.terminates` | `"unknown"` |
| `effects`, `properties`, `intent_tags`, `refinements` | `[]` |

### Arity

`arity` is the count of top-level `->` arrows after stripping a leading `forall ….` and any
typeclass context (`… =>`). Nested arrows inside parentheses are not counted, so
`(b -> c) -> (a -> b) -> a -> c` has arity 3 and `Semigroup a => a -> a -> a` has arity 2.

## Limitations (all addressable in future iterations)

- Only exported, signature-bearing **top-level** functions are ingested. Class/instance method
  signatures (indented), GADT/record fields, pattern synonyms, and Template Haskell are not.
- `signature.type` is a v0.1 source-flavored **string**, not the Nova Lingua type AST.
- `body_hash` is a synthetic address over the equation text, not a Nova Lingua body AST (the same
  limitation the Rust and Python adapters carry).
- Comment handling covers `--` line comments (respecting symbol operators like `-->`) and nested
  `{- … -}` blocks; it does not track string/char literals, which essentially never contain comment
  markers inside a type signature.
- A full-fidelity version could parse with `haskell-src-exts` or the GHC API instead.

## Tests

```bash
python3 -m unittest discover -s tests
```

Covers comment stripping, export-list parsing, single- and multi-line signatures, arity (nested
arrows, contexts, `forall`), operator handling, and — when the `nl-validator` release binary is
built — cross-validation that every record from `tests/Sample.hs` passes `validate` + `verify` and
that the validator's hash equals the Python-computed hash.

## License

Dual-licensed under Apache-2.0 OR MIT, same as the rest of *Novae Linguae*.
