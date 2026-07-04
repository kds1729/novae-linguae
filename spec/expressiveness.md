# Expressiveness v0.4 — from proof of concept to practical functionality

**Status: adopted plan (2026-07-04).** The build-out arcs — verification, prover, trust,
commons, agent loop, and the model arc — are complete or at documented frontiers. The wall
between *demonstration* and *tool* is now the expressible fragment itself: the languages can
verify, certify, discover, and apply functions end to end, but the functions a practical agent
needs mostly cannot be written yet. This document records what grows, why, in what order, and
what each addition must pay.

Priority decision: expressiveness first; deeper ingestion fidelity follows it (the adapters can
only lift what the language can express); adoption-telling follows working practical
functionality. Crypto audit is out of scope.

## Method: pull, don't push

Two standing principles govern every addition:

1. **Workflow-pulled.** A primitive enters the language only when a *golden workflow* — a
   concrete, end-to-end task a practical agent would run — demands it. No feature lists, no
   speculative generality. This is the discipline that added `last`/`init` (a model's correct
   algorithm demanded them) and declined `error`/`^`/`!!` (totality by design).
2. **The verified-by-default tax is the gate.** Every primitive lands across the *whole*
   semantic core or does not land at all: evaluator, typechecker, effect inference, termination,
   complexity/cost classes, and an honest answer in the prover (in-fragment, or UNSUPPORTED —
   never silently unprovable). Plus surface syntax, the eval conventions prompt, and eventually
   corpus families and ingestion mappings. If an addition can't pay the tax, it waits.

Totality is preserved throughout: partial operations return `Maybe`/`Result`, never trap.
Determinism is preserved throughout: every new operation's semantics is exact and
platform-independent (principle 5).

## Where the fragment stands (verified 2026-07-04)

Already in place, and further along than the "Int/Bool/List research core" reputation suggests:

- **Carrier types**: string values exist end to end (value schema, surface literals, evaluator
  `Val::Str`, typechecker `string`, JCS canonicalization) — and the *type* vocabulary already
  reserves `string`, `bytes`, `Map`, `Set` (closed enum in
  [`type-expression.schema.json`](type-expression.schema.json)), so growth needs **no schema
  version bump**.
- **Real effects**: `fs.read`/`fs.write`, TLS-capable `net.read`/`net.write`,
  `process.spawn`, all capability-gated, traced, and replayable — the I/O side of practical
  functionality already works, and its builtins already consume and produce strings.
- **Sum types** (`Maybe`/`Result` with computed payloads), records (construction + field
  projection), pairs, higher-order composition, `fn_ref` assembly from the commons.

Missing — the actual wall:

- **String *operations*: none.** A string can be created, passed, printed, written, compared —
  not built, measured, searched, or taken apart. (`concat` in the builtin table is a list-append
  alias.) Nearly every practical agent task touches text; today text is opaque.
- **Dynamic key–value data: none.** Records are static-shaped; there is no map/dict.
- **JSON-as-data: none.** The language's canonical form *is* JSON, but a JSON payload (an HTTP
  response, a config) cannot be inspected from inside the language.
- `bytes` and `Set` are reserved words with no operations; no workflow has pulled them yet.

## Golden workflows

Three concrete tasks define "practical". Each must eventually run **end to end through the
existing machinery**: written as records, typechecked, effect-checked, certified, published,
discovered by intent, trust-ranked, applied, and re-verified — nothing bespoke.

- **GW1 — fetch → extract → transform.** `http_get` a URL, isolate a field from the textual
  body (split/contains), parse it (`parse_int`), transform it numerically, return the result.
  The canonical "agent retrieves and uses external data".
- **GW2 — compute → format → report.** Fold over a list of numbers, render a human- or
  agent-readable summary string (`to_string`, `str_concat`/`str_join`), `write_file` or
  `print` it. The canonical "agent produces an artifact".
- **GW3 — dispatch on message content.** Split a small command string, compare its head
  against known commands (string equality in `case`), apply the matching `fn_ref`. The
  canonical "agent routes work based on text" — and the bridge toward Nova Locutio payloads
  that are themselves manipulable.

## Phase 1 — strings (the minimal total set)

Seven builtins, all pure, all total, all deterministic. Names are `str_`-prefixed (except the
two conversions) to keep the vocabulary orthogonal to the list ops:

| builtin | type | semantics decisions |
|---|---|---|
| `str_concat` | `(string, string) → string` | |
| `str_length` | `string → nat` | counts **Unicode scalar values** (not bytes, not graphemes) — exact and platform-independent |
| `str_contains` | `(needle: string, s: string) → bool` | substring test, **pattern-first** (like `str_split`/`str_join` — so `str_contains(x)` partially applies to a predicate usable with `filter`); empty needle → `true` |
| `str_split` | `(sep: string, s: string) → List string` | **separator-first** (Haskell `splitOn`); splits keeping empties (`str_split(",", "a,,b")` → `["a","","b"]`); separator absent → `[s]`; empty separator → one singleton per scalar value; total |
| `str_join` | `(sep: string, xs: List string) → string` | separator-interleaved concatenation (Haskell `intercalate`); `str_join(sep, str_split(sep, s)) = s` for non-empty `sep` |
| `to_string` | `int → string` | canonical decimal (the JCS integer rendering) |
| `parse_int` | `string → Maybe int` | accepts exactly canonical decimal with optional leading `-` (no leading zeros, no `-0`, no whitespace/`+`); anything else, or overflow past the evaluator's integer range (i128) → `None`. **Totality via Maybe** — the pattern that replaces `error`. Round-trips: `parse_int(to_string(n)) = Just(n)`, and `to_string(m) = s` whenever `parse_int(s) = Just(m)` |

Deliberately excluded from phase 1 (each waits for a workflow to pull it): string ordering
(`lt` on strings — locale/collation rabbit hole; `eq`/`neq` already work structurally),
case conversion (Unicode tailoring), regex (determinism is satisfiable but the vocabulary is
huge), slicing/indexing (partial or Maybe-heavy; split covers the workflows), float formatting.

**The tax, itemized** (template: the `last`/`init` commit `6a5cbc3`):

- `interp.rs` — 7 builtins, pure (auto-covered by effect inference via `builtin_arity`).
- `typecheck.rs` — monomorphic signatures above; `parse_int` returns the existing `Maybe` type.
- `terminate.rs` — first-order terminating ops (no recursion introduced).
- `complexity.rs` — cost classes: all classified **O(n)** (conservative; `str_concat`,
  `str_join` are output-linear, which also feeds `output_size` soundly).
- `prove.rs` — recognized as out-of-Int-fragment (like list ops): a law over them reads
  UNSUPPORTED, never mis-typed. Inductive string lemmas are **explicitly deferred** (strings
  are morally `List char`; if a workflow ever needs proved string laws, that is the encoding).
  *(Superseded same-day: the prover gained a **string fragment** — `str_concat`/`str_length`/
  `str_contains` map onto the solver's native string theory, `self` parameter sorts are inferred
  from body usage, and string laws PROVE over the unbounded domain; `equiv` decides
  string-function equivalence and `check-refinement` proves `string → nat` results ≥ 0.
  `str_split`/`str_join` stay out (no theory counterpart), and `to_string`/`parse_int`
  deliberately do NOT map onto `str.from_int`/`str.to_int` — the solver's negative-number
  semantics differ from ours, so the mapping would be unsound.)*
- Surface syntax — string literals already lex/parse/unparse; only the builtin name set grows.
- `spec/evaluation.md` + the eval harness conventions prompt — document the seven.
- Tests: per-builtin eval, typecheck, the split/join inverse property, parse_int edge cases
  (empty, `-`, overflow), plus a GW-shaped integration test.

**Phase-1 exit gate:** all existing tests green; GW1 and GW2 authored as real records that
typecheck, effect-check, run with granted effects (and replay), and **certify**; GW1
discovered and applied via `orchestrate --verify`.

**Phase 1: DONE (2026-07-04).** The seven builtins are live across the whole semantic core
(evaluator, typechecker, effects, termination, complexity, prover fragment guard, surface
syntax, eval conventions). The exit gate ran end to end:

- Records [`examples/csv-second-int.v0.2.json`](examples/csv-second-int.v0.2.json) and
  [`examples/double-second-field.v0.2.json`](examples/double-second-field.v0.2.json) (GW1) and
  [`examples/render-csv.v0.2.json`](examples/render-csv.v0.2.json) (GW2) validate, run all
  their examples, and **certify** — the two first-order ones fully SOUND including termination
  and `O(n)` complexity, `render_csv` with the honest higher-order UNVERIFIABLEs.
- The effectful GW1 leg ran against a live HTTP server: `\url -> case parse_int (head (tail
  (str_split "," (http_get url)))) of { Just(n) => n + n; None => int(0) }` under
  `--grant net.read` fetched `"id,21,ok"` and returned 42, recorded a trace, and **replayed
  to the same 42 with no grant and no I/O**.
- The agent loop closed over it: `orchestrate --intent parse` discovered
  `double_second_field` by intent, and `--verify --require-certified` certified it before
  applying — query → ack → certify → propose → commit → assert 42 → CONFIRMED.

En-route fix the gate surfaced: the termination/complexity analyzers only recognized builtins
at the head of *flat* applications, so surface-parsed (curried) bodies read as "opaque callee"
— UNVERIFIABLE for any juxtaposed multi-argument application. Both analyzers now flatten the
curried spine (`((f a) b)` → `f(a, b)`, including curried `self`-calls), which is what turned
the GW1 records' termination/complexity from UNVERIFIABLE into SOUND. Surface-authored and
AST-authored bodies now analyze identically.

## Phase 2 — maps (dynamic key–value data)

Pulled by the config/lookup halves of GW1/GW3 and by JSON (phase 3). Minimal set over
`Map string a` (string keys only until something pulls polymorphic keys):
`map_empty`, `map_put`, `map_get : … → Maybe a`, `map_del`, `map_keys : … → List string`,
`map_size`. Representation is an ordered map (the evaluator already uses `BTreeMap` for
records), so canonical serialization and deterministic iteration come for free — the same
choice that makes records canonical. The `Map` type constructor already exists in the schema
vocabulary. Tax as phase 1, plus a canonical JSON value encoding for map values.

**Phase 2: DONE (2026-07-04).** The six operations are live across the whole core (all six of
the phase-1 layers, plus the pieces strings didn't need):

- A **`map` value kind** in `value-expression.schema.json` (and the `ve_map` mirror inlined in
  `function-record.v0.2.schema.json`; the message schema picks it up by cross-file `$ref`):
  `{kind: "map", entries: [{key, value}…]}` with **canonical form = unique keys sorted in
  code-point order**, enforced by the well-formedness checker (an out-of-order entry list is
  *ill-formed*, so equal maps always hash equal). No schema version bump — `Map` was already
  in the type vocabulary.
- Surface value syntax `map {"k" => v, …}` / `map {}` (the keyword prefix keeps `{}`
  unambiguously a record), round-tripping canonically; `Map string int` already parsed as a
  type. Bodies need no map literal — construction is `map_put` chains from `map_empty`.
- Totality: `map_get` returns `Maybe`, absent-key `map_del` is a no-op — no error path
  anywhere, same discipline as `parse_int`.
- Exit-gate record [`examples/config-port.v0.2.json`](examples/config-port.v0.2.json)
  (`config_port : Map string int → int` — `case map_get "port" m of {Just(p) => p;
  None => int(8080)}`, the GW3 config-lookup idiom): validates, runs its examples over real
  map values (incl. the empty map), and **certifies fully SOUND** — typecheck, effects,
  termination, `O(n)` complexity — through the curried-spine analyzers phase 1 fixed.

## Phase 3 — JSON-as-data

The thesis-aligned capstone: the language's own canonical form becomes manipulable *from
inside*. One sum type `Json` (`JNull | JBool bool | JNum int/float | JStr string |
JList (List Json) | JObj (Map string Json)`) — expressible in the *existing* variant system
once maps exist — plus two builtins: `parse_json : string → Maybe Json` and
`render_json : Json → string` (JCS-canonical, so `render ∘ parse` is canonicalization).
This turns GW1 from "split the body text" into "parse the body, project the field" — the
practical form of the workflow. No new type-system machinery: it is a library-shaped sum type
with two total conversions.

**Phase 3: DONE (2026-07-04).** `parse_json : string → Maybe Json` and `render_json : Json →
string` are live across the core (same six layers), with `Json` exactly the planned sum over
the existing variants — nothing new in the type system, patterns, or serialization:

- `parse_json` is total via `Maybe` (malformed text → `None`; duplicate object keys last-wins);
  `render_json` emits **JCS-canonical** text via the validator's own canonicalizer, so
  `render_json ∘ parse_json` *is* canonicalization — verified in-tree
  (`{ "b" : [1, true, null] , "a": "x" }` renders `{"a":"x","b":[1,true,null]}`).
- Field projection is ordinary nested `case`: exit-gate record
  [`examples/json-port.v0.2.json`](examples/json-port.v0.2.json) (`json_port : string → int` —
  `case parse_json s of { Just(JObj(m)) => case map_get "port" m of { Just(JNum(p)) => p;
  _ => int(8080) }; _ => int(8080) }`) validates, runs 4/4 examples (real JSON, missing field,
  mistyped field, garbage — all total), and **certifies fully SOUND**, authored entirely in
  surface syntax with a canonical round-trip.
- GW1's practical form now composes end to end from existing parts: `http_get` (phase 0) →
  `parse_json` (phase 3) → `map_get` (phase 2) → `case` — an agent fetches a JSON API response
  and uses a field, verified by default at every layer.

## Phase 4 — follow-through (already-proven loops, re-run)

- **Corpus/model arc**: string (then map, then Json) combinatorial families through the verify
  gate; retrain the reference tiers; the broaden→retrain→measure loop is documented and cheap.
- **Ingestion**: map source-language string/dict idioms onto the new builtins in
  `nl_body.py` (+ the Rust/Haskell/TS adapters) — this is where "deeper ingestion fidelity"
  resumes, now with somewhere to land.
  *Strings: DONE for the Python adapter (2026-07-04).* A known-string inference rooted in
  `str`-annotated parameters drives the type-dependent translations a syntactic adapter can't
  otherwise decide: `+` → `str_concat`, `len` → `str_length`, `s.split(sep)` → `str_split(sep,
  s)` (receiver/argument swap onto the separator-first builtin), `sep.join(xs)` → `str_join`,
  `in` → `str_contains`, `str(n)` → `to_string`, and **f-strings** (`f"n={n}"` →
  `str_concat("n=", to_string(n))`; conversions/format specs honestly out of subset).
  Demonstrated end to end: a 6-function module (concat, split-count, rejoin, membership,
  labeling) plus an f-string function ingest to executable bodies that run 14/14 doctest-mined
  examples. Unannotated code keeps its numeric/list reading; a wrong guess fails the example
  gate rather than shipping wrong. Dict/JSON idioms (`d.get(k)` → `map_get` with the
  `Maybe`/`None` value-mapping question) and the Rust/Haskell/TS adapters remain.
- **Commons**: publish the golden-workflow records and their certifications to Arca; they are
  the first *practical* inhabitants of the commons.

## Sequencing and non-goals

Order: **1 → 2 → 3**, with phase 4 items interleaved opportunistically (corpus families can
trail each phase). Each phase merges only through its exit gate (tests + golden workflow(s)
end to end + certify).

Non-goals for v0.4: bytes/Set operations, string ordering/collation, regex, Unicode case ops,
float↔string formatting, polymorphic map keys, mutation of any kind, and any primitive no
golden workflow demands. The tie-breaker remains AI-efficiency, not human ergonomics.
