# Intent-tag vocabulary (v0.1)

## Purpose

`intent_tags` is a free-form, queryable field on every function record. Two agents that independently derive a function with the same semantic role SHOULD tag it identically so the commons stays searchable. This document is the v0.1 controlled vocabulary — the set of tags blessed for routine use. Tags outside this list are not invalid (the schema accepts any slash-separated lowercase kebab-case path), but they will not benefit from cross-agent agreement.

## Format

```
tag = segment ( "/" segment )*
segment = [a-z] [a-z0-9-]*
```

- Lowercase only, kebab-case (hyphens between words, never underscores).
- Slash-separated path forms a hierarchy: `transform/list` is more specific than `transform`.
- Hierarchical tags imply their prefixes: a record tagged `transform/list/elementwise` is also `transform/list` and `transform` for query purposes.
- Modifier tags (e.g. `pure`, `elementwise`, `idempotent`) live at the top level without a category prefix.

## Top-level categories

### `transform/<…>`
Produces output of the same conceptual shape as the input, just modified.

- `transform/list` — operates over a list, returns a list
- `transform/string` — string → string
- `transform/scalar` — single value → single value
- `transform/record` — record → record (e.g. field projection, renaming)
- `transform/map` — map → map
- `transform/set` — set → set

### `predicate/<…>`
Returns a boolean about the input.

- `predicate/scalar` — single value → bool
- `predicate/list` — list → bool (e.g. `all`, `any`, `contains`)
- `predicate/string` — string → bool (e.g. `is-empty`, `matches`)
- `predicate/record` — record → bool

### `aggregate/<…>`
Collapses a collection into a single value.

- `aggregate/sum`
- `aggregate/count`
- `aggregate/min`
- `aggregate/max`
- `aggregate/fold` — generic fold/reduce
- `aggregate/concat`

### `filter/<…>`
Produces a subset of the input.

- `filter/list`
- `filter/map`
- `filter/set`

### `query/<…>`
Reads from a data structure.

- `query/lookup` — by key
- `query/search` — by predicate
- `query/index`

### `parse/<…>`
Converts a textual representation into a structured value.

- `parse/integer`
- `parse/float`
- `parse/json`
- `parse/csv`
- `parse/url`
- `parse/datetime`
- `parse/uuid`

### `serialize/<…>`
The inverse of `parse/<…>`.

- `serialize/json`
- `serialize/csv`
- `serialize/binary`
- `serialize/url`
- `serialize/datetime`

### `io/<…>`
Performs an I/O effect. Records tagged here MUST also declare the corresponding `signature.effects`.

- `io/file/read`
- `io/file/write`
- `io/network/http`
- `io/network/socket`
- `io/console/read`
- `io/console/write`
- `io/random` — reads from a random source

### `arithmetic/<…>`
Numeric operations.

- `arithmetic` — generic
- `arithmetic/integer`
- `arithmetic/nat`
- `arithmetic/float`
- `arithmetic/bigint`
- `arithmetic/rational`

### `math/<…>`
Higher math beyond basic arithmetic.

- `math/trig`
- `math/logarithm`
- `math/exponential`
- `math/statistic`
- `math/linear-algebra`

### `logical/<…>`
Boolean logic and comparison.

- `logical` — generic
- `logical/connective` — and / or / not / implies / iff
- `logical/comparison` — eq / neq / lt / le / gt / ge

### `string/<…>`
String manipulation.

- `string/case` — to-upper / to-lower / title-case
- `string/replace`
- `string/match`
- `string/split`
- `string/join`
- `string/trim`
- `string/format`

### `concurrent/<…>`
Concurrency primitives. MUST declare the `process.spawn` or `time` effect as applicable.

- `concurrent/parallel`
- `concurrent/serial`
- `concurrent/lock`
- `concurrent/channel`
- `concurrent/atomic`

### `crypto/<…>`
Cryptographic operations. MUST declare any relevant effects (e.g. `random` for keygen).

- `crypto/hash`
- `crypto/sign`
- `crypto/verify`
- `crypto/encrypt`
- `crypto/decrypt`
- `crypto/keygen`
- `crypto/kdf`

### `time/<…>`
Temporal operations. MUST declare the `time` effect as applicable.

- `time/now`
- `time/delta`
- `time/format`
- `time/parse`
- `time/sleep`

### `coll/<…>`
Generic collection operations not covered by transform / filter / aggregate.

- `coll/empty`
- `coll/length`
- `coll/reverse`
- `coll/sort`
- `coll/unique`
- `coll/zip`
- `coll/unzip`

## Modifier tags (no category)

These describe a *property* a function has rather than a category it belongs to. Stackable freely with category tags.

- `pure` — no declared effects (redundant with `effects: []`, but useful for fast filtering)
- `elementwise` — for transforms / predicates over collections: applies to each element independently
- `idempotent` — `f(f(x)) == f(x)`
- `monotonic` — preserves the ordering of inputs in outputs
- `commutative` — argument order does not matter
- `associative` — `f(f(a, b), c) == f(a, f(b, c))`
- `total` — defined on every input of its declared type (no precondition needed)
- `partial` — has a meaningful precondition; results are undefined when violated
- `lossless` — `parse(serialize(x)) == x`
- `non-deterministic` — declared `random` or `time` effect; output varies between calls

## Extending the vocabulary

Anyone may use a tag that is not in this list. Doing so does not break validation; it only forfeits cross-agent agreement. To propose a new tag for blessing:

1. Open a PR that adds the tag under an existing category (or proposes a new category if needed) with a one-line description.
2. The PR should cite at least one existing function record where the tag is in use.
3. Once merged, the tag is part of the controlled vocabulary from that revision forward.

The pace of vocabulary growth should be governed by actual use, not anticipated needs. A tag with no records using it is not yet earned.

## What v0.1 deliberately defers

- A machine-readable form of this vocabulary (e.g. a JSON enum) for tooling that wants to soft-warn on out-of-vocab tags. Markdown-only is sufficient for human readers in v0.1.
- Synonym / equivalence relations between tags. If `transform/list` and `coll/map` end up meaning the same thing in practice, the canonical form should be chosen and the other deprecated; that policy is v0.2+.
- Localized tag names. The vocabulary is English-only in v0.1.
