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
*(GW4 — the sorted-report workflow, below — later pulled exactly two: `str_lt`, code-point
order sidestepping collation entirely, and `str_lower`, the untailored default mapping. GW5 —
the numeric report — later pulled the rendering half of float formatting: `to_float`, numeric
`div`/`mod`, and numeric `to_string` emitting the JCS canonical rendering. Regex, slicing,
`parse_float`, and format control remain excluded.)*

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

**Nested projection (2026-07-05): real APIs nest objects, and the path idiom is now in the
commons — as ordinary certified functions, not new builtins.**
[`examples/json-get.v0.2.json`](examples/json-get.v0.2.json) (`json_get : string → Json →
Maybe Json`, one `case` on `JObj` + `map_get`) and
[`examples/json-path.v0.2.json`](examples/json-path.v0.2.json) (`json_path : List string →
Json → Maybe Json`, structural recursion over the key path; any miss — absent key, non-object,
leftover path — is `None`, the empty path is `Just`) both **certify fully SOUND**
(typecheck / effects / termination / complexity `O(n)` resp. `O(n²)`). Their signatures are the
first to *mention* `Json`: the type-expression vocabulary gained a nominal, nullary `Json`
builtin (the subject of `parse_json`/`render_json`; it erases to the opaque `Sum` at the HM
level like every sum type). Demonstrated against a live API: `http_get` (traced, replayable) →
`parse_json` → `json_path ["owner", "login"]` projects a nested field out of a real GitHub
response, and the replay reproduces the live run byte-for-byte with no network grant.
Both are published to the live commons with signed certifications, and the verified remote
agent loop closes over them: `orchestrate --node … --intent json --verify --require-certified`
discovers both, disambiguates **by signature** (the coarse argument-fit filter learned the
string/float/Map/Json sorts en route — before that it proposed the wrong candidate and the
responder rejected at apply time), certifies, applies, publishes the assert, and an
independent `verify-claim` re-confirms it from the address alone.

**GW4 — the sorted report (2026-07-05): the first tier-2 pull.** A real workflow — *fetch a
contributors endpoint, case-fold the logins, sort them, render a report* — pulled exactly two of
the deliberately-excluded phase-1 builtins and no more: **`str_lt : (string, string) → bool`**
(strict lexicographic order over Unicode scalar values — **the same order canonical map keys
already use**, so the core has one ordering; explicitly not a collation; maps onto SMT-LIB
`str.<`, so ordering laws prove) and **`str_lower : string → string`** (the Unicode *default*,
untailored lowercase — deterministic, locale-independent; out of the prover fragment, no theory
counterpart). Regex, slicing, and float formatting stay unpulled — the workflow didn't need them.
Everything above the two builtins is **in-language certified records**
([`examples/insert-sorted.v0.2.json`](examples/insert-sorted.v0.2.json) — the one genuinely
recursive piece; [`examples/sort-strings.v0.2.json`](examples/sort-strings.v0.2.json) —
insertion sort as a `foldr` over `insert_sorted` by `fn_ref`, *assemble don't write*;
[`examples/logins-of.v0.2.json`](examples/logins-of.v0.2.json);
[`examples/contributors-report.v0.2.json`](examples/contributors-report.v0.2.json) — the pure
parse→project→lower→sort→join pipeline; and the one effectful leg
[`examples/fetch-contributors-report.v0.2.json`](examples/fetch-contributors-report.v0.2.json),
declared `net.read`). All five **certify** and are published to Arca with signed certifications.
The end-to-end run exercised the whole recent arc at once: the responder **refused** the
effectful function without a grant (`effect not granted: [net.read]`), fulfilled it under
`--grant net.read` against the live GitHub API, a grantless `verify-claim` correctly reported
the claim **undecidable** (an effectful assert is testimony) while a granted one re-fetched and
**CONFIRMED**, and the remote loop
(`orchestrate --node … --intent io --grant net.read --verify --require-certified --publish`)
discovered, certified, applied, and published the live sorted report.

**GW5 — the numeric report (2026-07-06): the smallest tier-2 pull.** A real workflow — *take a
numeric series, compute count/mean/max, render a stats line* (the float-precise half of GW2) —
pulled **one new builtin and two signature generalizations**, and no more:
**`to_float : int → float`** (total; IEEE-754 nearest-even for magnitudes beyond 2⁵³ — a
deterministic rounding, documented rather than hidden), **`div`/`mod` lifted to
numeric-polymorphic** (the evaluator could already divide floats — the surface the type system
refused; the lift adds the missing **zero-divisor guard on the float path**, so `Infinity`/`NaN`
— unrepresentable in canonical JCS — cannot be produced; float `div`/`mod` stay partial-at-zero
exactly like their int forms), and **`to_string` lifted to numeric-polymorphic** (the float arm
emits the **JCS / ECMAScript Number-to-String canonical rendering** the hashing layer already
uses — `to_string(3.0) = "3"`, `to_string(3.25) = "3.25"` — one rendering everywhere; non-finite
inputs are refused, not rendered). `parse_float`, rounding, and formatting *control* (precision,
padding) stay unpulled — the workflow didn't need them. The pull also **paid down a latent
soundness gap as its prover-tax line item**: `prove`/`check-refinement`/`equiv` inferred SMT
sorts from body usage with an Int default and never read the declared type, so a float-typed
record carrying an arithmetic law (e.g. associativity — true over ℤ, false over IEEE floats)
would have been "PROVED" over the wrong domain; all three now **guard on `float` in the declared
signature** and report UNSUPPORTED/UNVERIFIABLE — honest, never mis-proved. Everything above the
primitives is in-language certified records:
[`examples/mean-of.v0.2.json`](examples/mean-of.v0.2.json) (`List float → Maybe float` —
**totality via Maybe**: the empty series has no mean, so the division-by-zero case is
unrepresentable rather than guarded), [`examples/stat-line.v0.2.json`](examples/stat-line.v0.2.json)
(`(string, float) → string`, the `label=value` renderer), and
[`examples/stats-report.v0.2.json`](examples/stats-report.v0.2.json) (`List float → string` —
total: the empty series reports `count=0`; `count` renders integrally through `to_float` because
the canonical rendering of a whole float has no fraction). All three certify and are published
to Arca with signed certifications; the corpus grows curated rows + combinatorial family #45 so
the new operations have training shapes from day one (the pinned every-builtin-needs-a-shape
lesson, applied preemptively like #43).

**GW6 — the authed mutating call (2026-07-07): the general HTTP core.** A real workflow —
*create a resource on an authenticated service, verify it exists, delete it, verify it's gone* —
pulled **one builtin and two effect-boundary mechanisms**, and no more. The builtin is
**`http : (string, string, Map string string, string) → {status: int, body: string}`**
(method, url, headers, body): one general request covering the whole verb surface, whose
**effect is decided by the method** — `net.read` for GET/HEAD, `net.write` for every other verb —
so a mutating call is gated by the mutating grant even through the one builtin (the effects
walker refines a literal method to exactly the side performed; a dynamic method is
conservatively both), and whose **record result carries the status** — the thing the workflow
verifies against, which `http_get`'s body-only result could never express. The two mechanisms:
**host-scoped grants** (`--grant net.write@api.example.com` — enforced at the effect boundary
where the URL is known; a bare grant still means any host, a scoped grant refuses every other
host by name) and **secret placeholders** (`{{secret:NAME}}` in a header value, substituted from
operator-supplied `--secret NAME=VALUE` only inside the live effect — records, asserts, and
traces are public content-addressed artifacts, so a credential never exists as a language value:
the wire sees it, the trace keeps the placeholder, and **replay needs no secrets at all**; a
verifier re-running an authenticated claim authenticates with its *own* secrets). The exit gate
runs against an in-repo **reference fake service**
([`tooling/fake-service/fake_service.py`](../tooling/fake-service/fake_service.py) — stdlib-only,
in-memory, client-chosen names so nothing is server-assigned, Bearer-auth required so the gate
exercises the secret path). Everything above the builtin is in-language certified records:
[`examples/put-item.v0.2.json`](examples/put-item.v0.2.json) /
[`examples/item-status.v0.2.json`](examples/item-status.v0.2.json) /
[`examples/delete-item.v0.2.json`](examples/delete-item.v0.2.json) (the three verbs, each
declaring exactly its side of the net split — certify shows `effects SOUND` per-verb), and
[`examples/item-roundtrip.v0.2.json`](examples/item-roundtrip.v0.2.json) (`(string, string,
string) → bool` — the whole create→verify→delete→verify-gone cycle as one **total, self-cleaning
predicate**, which is what makes an effectful claim about it re-runnable at all). All four
certify and are published with signed certifications. Demonstrated end to end: the responder
**refused** the roundtrip without grants; the remote loop (`orchestrate --node … --intent
predicate --verify --require-certified --publish --grant net.read@127.0.0.1 --grant
net.write@127.0.0.1 --secret api_token=…`) discovered it among twenty candidates, disambiguated
by signature, certified, applied it live → `true`, published the assert; a grantless
`verify-claim` correctly reported the claim **undecidable** (an effectful assert is testimony);
and a granted one **re-ran the whole cycle live and CONFIRMED**. `run` needed no new grant flag —
it already grants exactly the record's declared effects (its examples are its own tests) — only
`--secret`. Wire-format details deliberately unpulled: response headers, redirects,
query-parameter encoding, multipart bodies — no workflow has needed them.

**GW7 — records from an API description (2026-07-07): ingestion at the description layer.** With
the general `http` core in place, a **machine-readable API description is an ingestion source**:
each operation (a path × method) has a well-defined surface — base URL, verb, path parameters,
request body, auth scheme, documented statuses — which is exactly the semantic content of a
client function, so it compiles to a record over `http` with no hand-authoring. The new adapter
[`tooling/nl-ingest-openapi`](../tooling/nl-ingest-openapi/) reads an OpenAPI 3 description (the
neutral exemplar) and emits one **verified** record per operation: `operationId → name_hints`,
verb → the method literal and the effect, `servers[0].url → base` parameter, the path template →
a `str_concat` URL builder, path params → string parameters, `requestBody` → a `body` parameter,
Bearer `security` → an `{{secret:NAME}}` auth header, documented `responses` → the status the
worked example asserts. Each record returns the response `.status` (the deterministic part;
body projection waits for observed-claims). Every one is gated through `certify`, and with
`--verify-against <base-url>` its examples are **run** against a live service (the "gate =
examples vs an emulator" step) — verified-by-default exactly like a hand-authored record. The
exit gate runs against the in-repo reference fake service, whose description is
[`examples/item-store.openapi.json`](../tooling/nl-ingest-openapi/examples/item-store.openapi.json).
**The faithfulness result:** the generated `getItemStatus` and `deleteItem` bodies are
**byte-identical** (same `expr_` address) to the hand-authored GW6 `item_status` / `delete_item`
records — the description contains their full semantic content, so machine generation reproduces
what a human wrote. The net-new `health_check` (an unauthenticated liveness probe, the no-auth /
no-params / no-body case) certifies, is published to Arca, and closes the remote loop:
`orchestrate --node … --intent io/network/http --verify --require-certified --publish --grant
net.read@127.0.0.1` discovered it among five candidates, disambiguated **by signature** (the only
arity-1 one), applied it live → status 200, published the assert; a grantless `verify-claim`
reported it undecidable (testimony) and a granted one re-ran it live and CONFIRMED. Wire-format
depth (response headers, `$ref` schema resolution, query/multipart encoding, non-Bearer auth)
stays unpulled — the description-to-record path, not full OpenAPI coverage, is the pull.

**GW3 — dispatch on message content (2026-07-07): the zero-pull workflow.** The last of the
three original golden workflows — *split a command string, compare its head against known
commands, apply the matching function* — pulled **no new builtins at all**: `str_split` (phase
1), string equality as a `case` over **string-literal patterns** (the `lit` pattern kind the
schema always had), `parse_int`'s `Maybe` for the argument, and direct **application of an
`fn_ref` literal** (the assemble-don't-write primitive that `sort_strings` passes to `foldr`,
here in function position). [`examples/dispatch-command.v0.2.json`](examples/dispatch-command.v0.2.json)
(`dispatch_command : string → Maybe int`) routes `"double 21"` / `"negate 7"` / `"square 6"` to
the commons functions [`double`](examples/double.v0.2.json), [`negate`](examples/negate.v0.2.json),
and [`square`](examples/square.v0.2.json) **by content-address**, and is total: an unknown
command, a missing argument, or an unparseable argument is `None`, never an error. It
**certifies** (typecheck/effects sound; termination/complexity conservatively UNVERIFIABLE
through the opaque `fn_ref` callees, exactly like `sort_strings`) and is published to Arca with
a signed certification. The intent-tag vocabulary gained a blessed **`dispatch/<…>`** category
(`dispatch/command`, `dispatch/variant`) so routers stay discoverable by intent. Demonstrated
end to end against the live node: `orchestrate --node … --intent dispatch --verify
--require-certified --publish` discovered the router, hash-verified the four-record closure
(router + all three callees + bodies), certified it, applied it to `"square 6"` → `Just 36`,
published the assert, and an independent `verify-claim` re-confirmed the claim from the message
address alone. This is the bridge the workflow was designed to be: the dispatch table is
ordinary data (a `case` over strings) and the dispatched-to behavior is ordinary commons
content — the shape a Nova Locutio payload router takes when the payload itself picks the
function. The corpus follow-through landed the same day (curated rows `toggle_on` /
`signal_step` / `cmd_of` / `arg_of` / `run_command` — eval 380 → 390, oracle 390/390 — plus
combinatorial family #46, the string-scrutinee/literal-pattern shapes nothing else taught), and
**paid down a latent round-trip hole as its tax line item**: the pretty-printer emits a
non-negative `int`-kind literal *pattern* as `int(N)`, but the pattern parser rejected that
form (the expression-position twin was fixed long ago), so any `case` over typed int literals
unparsed to a surface its own parser refused — caught by the oracle gate on the new rows, fixed
in the parser with a regression test.

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
  gate rather than shipping wrong. *TypeScript too:* a `: string` parameter annotation roots the
  same inference through the shared expression translator, with the TS-native spellings mapped —
  `s.split(sep)`, the array-order `xs.join(sep)` (separator still lands first), `s.includes(x)` →
  `str_contains`, `String(n)` → `to_string`, and `+`-concatenation.
  *Dicts: the TOTAL subset is DONE for Python (2026-07-04).* A `dict`/`dict[...]`/`Mapping[...]`
  annotation roots a known-dict inference: `d.get(k, default)` → `case map_get(k, d) of {Just(v)
  => v; None => default}`, `k in d` → the has-key case, `len(d)` → `map_size`,
  `sorted(d)`/`sorted(d.keys())` → `map_keys` (sound precisely because `map_keys` is sorted).
  Dict example values encode as the `map` value kind when the signature expects `Map …` (or the
  keys aren't identifier-shaped); identifier-keyed dicts without a Map expectation keep the
  historical record encoding, so existing ingested hashes are stable. Demonstrated: a 4-function
  config module runs 8/8 doctest examples over real map values. **Deliberately out of subset:**
  the bare 1-arg `d.get(k)` — an `Optional` at the record boundary whose `None`↔`Maybe` example
  mapping is a design decision not yet taken — and `d[k]` (raises). The Haskell body subset
  (flat token applications, no method calls) has no string/dict surface to map yet.
  *Rust too (2026-07-05):* the Rust adapter's own lifter roots a known-string inference in
  `&str`/`String` parameter types — `+` → `str_concat`, `s.len()` → `str_length` (bytes vs
  scalars agree on ASCII; a non-ASCII doc-test fails the gate), `s.contains(x)` →
  `str_contains(x, s)`, `s.is_empty()` → `str_length = 0`, `n.to_string()` → `to_string` (and
  the identity on an already-string receiver). `format!` (a macro) and iterator-returning
  `.split()` stay out of subset. Verified by hash equality: four doc-tested Rust functions emit
  exactly the expected translated body addresses, and the lifted bodies evaluate correctly.
  *Statement subset widened (2026-07-07):* the honest 9/57-stdlib finding is a control-flow gap
  (real library code is multi-statement, not single-expression), and the two most common loop
  shapes the single-statement translator couldn't reach are now in: a **guarded accumulator**
  `for x in xs: if c: acc <op>= e` → a `foldl` whose step is `case c of true => acc <op> e; false
  => acc` (sum-of-positives, count-matching), and a **list-building loop** `out = []; for x in
  src: out.append(e)` → `out = map(\x -> e, src)` (with a guard, `map` over `filter`; append onto
  the prior accumulator, so `append(nil, L) = L` makes a `[]`-seed collapse cleanly), plus **list
  literals** (`[]` → `nil`) as the seed. All reuse existing builtins (`foldl`/`map`/`filter`/
  `append`/`cons`), no evaluator change, and — a load-bearing invariant — the unguarded
  single-statement fold's output is unchanged, so no previously-ingested hash moves. Ten sample
  functions (the original five + `sum_positives`/`count_evens`/`doubled`/`keep_positive`/
  `squares_of_evens`) ingest and run against their doctests. Honest residuals still out of subset:
  multi-accumulator loops (a tuple accumulator), early-`return`-in-a-loop (search/`any` — a fold
  can't short-circuit), `while`, tuple-unpacking `for`, nested loops.
  *Search loops (2026-07-09):* early-`return`-in-a-loop — the top residual, and the ubiquitous
  find/`any` idiom — is now in. A fold indeed can't short-circuit, but in a **pure total**
  language the short-circuit is unobservable, so find-first *is* `head` of the guarded sublist:
  `for x in xs: if c: return e` (+ the block after the loop as the not-found branch) →
  `let hits = filter(\x -> c, xs) in case null(hits) of true => <after-loop>; false =>
  let x = head(hits) in e`. Existing builtins only; the `hits` binder is freshened past any
  colliding name; a tail that *reads* the loop variable after the loop (bound to the last
  element in Python, unbound here) is refused rather than silently mistranslated. Three more
  sample functions (`first_negative`/`contains`/`double_first_even`, thirteen total) ingest and
  run against their doctests. Residuals now: multi-accumulator loops (a tuple accumulator),
  `while` (non-structural), tuple-unpacking `for`, nested loops.
  *Multi-accumulator loops (2026-07-09):* the "needs a tuple accumulator" residual falls to the
  same purity argument — the body language has no record/tuple *construction*, but **independent**
  accumulator statements don't need one: `for x in xs: s += e1; c += e2` → one `foldl` per
  accumulator (`let s = foldl(…s…) in let c = foldl(…c…) in …`), since re-walking the list N
  times is unobservable in a pure total language. Exactness is guarded, not assumed: an update or
  the loop guard reading *another* accumulator (which in Python sees a mid-loop value no separate
  fold reproduces), or the same accumulator twice, is refused. Two more sample functions
  (`sum_minus_count`/`even_sum_and_count`, fifteen total) ingest and run against their doctests.
  Residuals now: *dependent* multi-accumulator loops (those genuinely need in-language record
  construction), `while` (non-structural), tuple-unpacking `for`, nested loops.
  *Nested list-building loops (2026-07-09):* the flatten/flatMap idiom
  `for x in xss: for i in <inner(x)>: out.append(e)` → a `foldl` of per-row appends,
  `out = foldl(\out x -> append(out, map(\i -> e, [filter] inner)), out, xss)` — inner guard
  filters the row's batch, outer guard filters the outer source, seed is the accumulator's prior
  value so the `[]` seed still collapses. Refused: the element/guards *reading* the accumulator
  mid-loop (a fold step sees only its own batch, not Python's growing list). The
  read-the-loop-variable-after-the-loop honesty guard was also hoisted to cover **all** loop
  shapes (previously only the search loop refused it; the accumulator/append shapes silently
  dropped Python's last-element binding). Two more sample functions (`flatten`/`evens_of_rows`,
  seventeen total) ingest and run against their doctests. Residuals now: *dependent*
  multi-accumulator loops, `while` (non-structural), tuple-unpacking `for`, deeper loop nesting
  (three levels) and nested loops whose inner statement is not an append.
- **Commons**: publish the golden-workflow records and their certifications to Arca; they are
  the first *practical* inhabitants of the commons.

## Sequencing and non-goals

Order: **1 → 2 → 3**, with phase 4 items interleaved opportunistically (corpus families can
trail each phase). Each phase merges only through its exit gate (tests + golden workflow(s)
end to end + certify).

Non-goals for v0.4: bytes/Set operations, string *collation* (code-point `str_lt` was pulled by
GW4; locale collation stays out), regex, Unicode case *tailoring* (untailored `str_lower` was
pulled by GW4), float *parsing* and format *control* (canonical float *rendering* was pulled by
GW5 — `to_string` emits the one JCS rendering; precision/padding/`parse_float` stay out),
polymorphic map keys, mutation of any kind, and any primitive no
golden workflow demands. The tie-breaker remains AI-efficiency, not human ergonomics.
