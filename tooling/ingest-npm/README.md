# nl-ingest-ts — TypeScript/JavaScript source → Nova Lingua function records

`nl-ingest-ts` reads one or more `.ts` / `.d.ts` / `.js` / `.mjs` files, finds every **exported**
top-level function, and emits one Nova Lingua v0.1 function record per function as compact JSON
(JSONL — one object per line). It is the **npm-ecosystem** adapter, sibling to the Rust `nl-ingest`,
the [Python `nl-ingest-py`](../ingest-python/README.md), and the
[Haskell `nl-ingest-hs`](../ingest-haskell/README.md). Every record passes `nl-validator validate`
and `verify` and agrees byte-for-byte with the reference implementation on canonical form and hash.

## Zero dependencies

The tool runs with only `python3` (3.10+). The hashing/record core (BLAKE3-256 + JCS/RFC 8785) is
the shared, stdlib-only [`nl_core`](../ingest-common/) module — no `pip install`, no `npm install`,
and **no Node/TypeScript toolchain** is required. The tool reads source with a string scanner that
recognises the common export forms; it does not run `tsc`.

## Usage

```bash
python3 nl_ingest_ts.py path/to/index.ts            # compact JSONL to stdout
python3 nl_ingest_ts.py --pretty path/to/types.d.ts # readable
python3 nl_ingest_ts.py --module mypkg src/*.ts      # add '<module>_<fn>' name_hints
```

The file is executable, so `./nl_ingest_ts.py …` also works.

### Recognised export forms

```ts
export function f<T>(a: A, b: B): R { … }            // incl. async, export default function
export declare function f(a: A): R;                   // .d.ts ambient declarations
export const f = (a: A): R => …                       // incl. async, generics, = function (…) {…}
export const f = x => …                               // single bare parameter
```

Only **exported** functions are ingested; everything else (internal helpers, non-function consts) is
skipped.

## What it populates

| Field | How populated |
|-------|---------------|
| `hash` | Real `fn_` BLAKE3 content-address of the record |
| `name_hints` | Sanitized function name; `<module>_<fn>` form if `--module` given |
| `signature.type` | `forall T. (A, B) -> R`, built from the TS annotations (source-flavored string); unannotated positions and missing return types render as `unknown` |
| `examples` | One placeholder per parameter: `args` = `[null, …]`, `result` = `null` |
| `body_hash` | Synthetic `expr_` BLAKE3 of the declaration's source slice — not a Nova Lingua body AST |
| `signature.terminates` | `"unknown"` |
| `effects`, `properties`, `intent_tags`, `refinements` | `[]` |

The scanner is string- and comment-aware: it balances `()`, `[]`, `{}`, and `<>`, treats `=>` as a
unit (so a `>` in an arrow type does not close a generic), and ignores comment markers and brackets
inside string/template literals. A leading TypeScript `this` parameter is excluded from the arity.

## Limitations (all addressable in future iterations)

- Only exported functions are ingested. Class methods, object-method shorthand, overload-signature
  merging, re-exports (`export { x } from …`), and namespaces are not handled.
- `signature.type` is a source-flavored **string**, not the Nova Lingua type AST.
- Bare object-literal **return** types (`: { a: number }`) are not parsed and may truncate the
  rendered return type. Named, `Promise<…>`, array, and union return types are fine.
- `body_hash` is a synthetic address over the declaration source, not a Nova Lingua body AST.
- A full-fidelity version could parse with the TypeScript compiler API (`ts.createSourceFile`) via
  Node instead of the string scanner.

## Tests

```bash
python3 -m unittest discover -s tests
```

Covers comment stripping (incl. markers inside strings), every recognised export form, generics and
nested arrow-type parameters (commas not mis-split), optional/rest params, the `this`-parameter
exclusion, and — when the `nl-validator` release binary is built — cross-validation that all nine
records from `tests/sample.ts` pass `validate` + `verify` and that the validator's hash equals the
Python-computed hash.

## License

Dual-licensed under Apache-2.0 OR MIT, same as the rest of *Novae Linguae*.
