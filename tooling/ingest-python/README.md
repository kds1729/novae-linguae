# nl-ingest-py — Python source → Nova Lingua function records

`nl-ingest-py` reads one or more `.py` files, finds every public top-level function, and emits
one Nova Lingua v0.1 function record per function as compact JSON (JSONL — one object per line).

It is the **Python** ingestion adapter — the sibling of the reference Rust
[`nl-ingest`](../validator/README.md#nl-ingest--rust-source--function-records). The two tools
serve the same purpose for two ecosystems, and every record either of them emits passes
`nl-validator validate` and `nl-validator verify`: they agree **byte-for-byte** on canonical
form and content-hash. That agreement is the cross-implementation conformance contract pinned by
[`spec/conformance/`](../../spec/conformance/) and required by
[`spec/canonical-serialization.md`](../../spec/canonical-serialization.md).

## Zero dependencies

The tool is a single self-contained file, `nl_ingest.py`, that uses **only the Python standard
library**. BLAKE3-256 and JCS (RFC 8785) canonicalization are vendored inline, so it runs with
nothing but `python3` (3.10+) — no `pip install`, no network. If the native [`blake3`](https://pypi.org/project/blake3/)
package happens to be installed it is used for speed, but it is never required; the vendored
pure-Python BLAKE3 is the contract and is verified against the official reference test vectors.

## Usage

```bash
# Ingest a single file; print compact JSONL to stdout
python3 nl_ingest.py path/to/module.py

# Pretty-print for human review
python3 nl_ingest.py --pretty path/to/module.py

# Tag name_hints with the module/package name (emits "mymod_fn" alongside "fn")
python3 nl_ingest.py --module mymod path/to/module.py

# Ingest several files at once
python3 nl_ingest.py --module mylib src/a.py src/b.py

# Include private (_-prefixed / non-__all__) functions too
python3 nl_ingest.py --include-private path/to/module.py

# Higher fidelity: emit v0.2 records (structured type AST + real examples from doctests)
python3 nl_ingest.py --v2 --module mylib path/to/module.py
```

The file is executable (`chmod +x` already set), so `./nl_ingest.py …` also works.

### `--v2` — structured, higher-fidelity records

With `--v2`, a function that has **usable doctests** is emitted as a **v0.2** record:

- `signature.type` is a structured **type AST** (via [`ingest-common/nl_types.py`](../ingest-common/nl_types.py))
  instead of a flavored string; unannotated/`Any`/general-`Union`/user types become fresh
  `forall`-bound type variables (no `unknown` builtin exists).
- `examples` are **real** input/output pairs extracted from the function's Python **doctests** (via
  [`ingest-common/nl_examples.py`](../ingest-common/nl_examples.py)), encoded as value ASTs — never
  fabricated or executed.
- `signature.refinements` gains a **precondition** (`{kind: "pre", expr}`) for each leading `assert`
  statement whose condition is an expressible predicate (via
  [`ingest-common/nl_predicates.py`](../ingest-common/nl_predicates.py)); e.g. `assert b != 0` →
  `app neq [var b, lit 0]`. (Algebraic *properties* aren't inferred — those are agent-authored.)

Functions without usable doctests fall back to a **v0.1** record (so none are dropped); a single run
can emit a mix. Float example values are canonicalized per JCS / ECMAScript Number-to-String (matching
the Rust validator, pinned by `spec/conformance/` canonicalization vectors). Current limit: `body_hash`
is still the normalised-source hash, not a body AST. Every `--v2` record passes `nl-validator validate`
against `function-record.v0.2.schema.json` and `nl-validator verify`.

### What counts as "public"

1. If the module defines `__all__` as a list/tuple/set of string literals, **that list is
   authoritative** — exactly those functions are ingested.
2. Otherwise, every top-level function whose name does **not** start with `_`.
3. `--include-private` overrides both and ingests every top-level function.

Only module-level `def` / `async def` are ingested; methods inside `class` bodies are skipped.

## Post-ingestion workflow

Each emitted record is schema-valid but carries placeholder values to be filled in:

```bash
# 1. Ingest into a staging file
python3 nl_ingest.py --pretty --module mylib src/mylib.py > /tmp/draft-records.jsonl

# 2. Validate / verify each record with the reference validator
VAL=../validator/target/release/nl-validator
SCHEMA=../../spec/function-record.schema.json
while IFS= read -r record; do
    printf '%s' "$record" > /tmp/rec.json
    "$VAL" validate "$SCHEMA" /tmp/rec.json && "$VAL" verify /tmp/rec.json
done < /tmp/draft-records.jsonl

# 3. Edit drafts: fill in examples, effects, properties, intent_tags, terminates
# 4. Re-validate and re-verify (verify recomputes the hash, catching any drift)
```

## What nl-ingest-py populates

| Field | How populated |
|-------|---------------|
| `hash` | Real `fn_` BLAKE3 content-address computed from the record itself (JCS → BLAKE3-256) |
| `name_hints` | Sanitized bare function name; `<module>_<fn>` form if `--module` given |
| `signature.type` | Nova-Lingua-flavored type string built from Python annotations (see table below) |
| `body_hash` | Synthetic `expr_` BLAKE3 of the body's normalised source (`ast.unparse`) — changes when the body changes; **not** a Nova Lingua body AST |
| `examples` | One placeholder per function: `args` = `[null, …]` (correct arity), `result` = `null` |
| `signature.effects` | `[]` (conservative; fill in after review) |
| `signature.terminates` | `"unknown"` (fill in after analysis) |
| `properties`, `intent_tags`, `refinements` | `[]` (fill in after review) |
| `derived_from`, `supersedes` | `null` |

## Type mapping

Python type annotations are rendered into a Nova Lingua v0.1 surface **type string**. Where a
Python type has a Nova Lingua builtin (per [`type-expression.schema.json`](../../spec/type-expression.schema.json))
it maps to that builtin; otherwise it is kept verbatim as a hint.

| Python annotation | Rendered type |
|-------------------|---------------|
| `int` | `int` |
| `bool` | `bool` |
| `float` | `float` |
| `str` | `string` |
| `bytes`, `bytearray` | `bytes` |
| `None` / `-> None` | `unit` |
| `Any`, `object` | `unknown` |
| *no annotation* | `unknown` |
| `list[T]`, `Sequence[T]` | `List T` |
| `set[T]`, `frozenset[T]` | `Set T` |
| `dict[K, V]`, `Mapping[K, V]` | `Map K V` |
| `tuple[A, B]` | `(A, B)` |
| `Optional[T]`, `T \| None` | `Maybe T` |
| `Union[A, B]` (no `None`) | `A \| B` |
| `Callable[[A], R]` | `(A) -> R` |
| a `TypeVar` / PEP 695 `def f[T]` param | a lowercased type variable, bound by a leading `forall` |
| a user/class name | kept verbatim |

A function that references type variables is rendered with a `forall` prefix, e.g.
`def first[T](xs: list[T]) -> T | None` → `forall t. (List t) -> Maybe t`.

## Limitations (all addressable in future iterations)

- `signature.type` is a v0.1 **string**, not the structured type AST. It is Nova-Lingua-flavored
  but `unknown` (used for unannotated/`Any` positions) is a placeholder, not a Nova builtin, so a
  record may need its type filled in before `nl-validator parse-type`/v0.2 use. This mirrors the
  Rust tool, whose strings are Rust-flavored.
- `body_hash` is a synthetic address over the body's normalised source, not a proper Nova Lingua
  body-expression hash. Translating the `ast` body to a body-expression AST and hashing that is
  future work (the Rust tool has the identical limitation over its token stream).
- `*args` / `**kwargs` are omitted from both the type string and the example arity.
- `effects`, `terminates`, `properties`, and real `examples` are not inferred; `async def` is
  ingested but not specially marked. Fill these in after ingestion.

## Tests

```bash
python3 -m unittest discover -s tests
```

The suite (`tests/test_nl_ingest.py`) covers:

- **BLAKE3** against the official reference vectors (empty, 1, 64, 1024, 1025, 2048, 3072 bytes —
  single-block, single-chunk, and multi-chunk tree paths).
- **JCS** against the worked example in `spec/canonical-serialization.md`, plus key-ordering and
  whitespace rules.
- **End-to-end** `JCS → BLAKE3` reproducing the hashes the project already pins on `map.json` and
  `double.v0.2.json`.
- **Type mapping**, **visibility** (`__all__` / underscore / `--include-private`), and record shape.
- **Cross-validation**: every record emitted from `tests/sample.py` passes `nl-validator validate`
  and `verify`; the validator's computed hash equals the Python-computed hash; and a forced
  >1 KB record confirms multi-chunk hashing agrees with the Rust implementation. These tests skip
  automatically if the `nl-validator` release binary has not been built.

## License

Dual-licensed under Apache-2.0 OR MIT, same as the rest of *Novae Linguae*.
