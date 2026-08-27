# A second description format that is not OpenAPI-shaped: GraphQL, through a new adapter

- **Status:** absorbed (2026-08-27). The module's conclusions are in the tree as the adapter it
  motivated — [`tooling/nl-ingest-graphql`](../../tooling/nl-ingest-graphql/) — and its two
  in-adapter design decisions ([finding 4](findings.md#4-the-observation-gate-is-a-flood-one-request-per-projection-trips-a-public-services-rate-limit),
  [finding 5](findings.md#5-a-schema-whose-arguments-are-all-nullable-can-still-require-one-at-least-one-of-is-inexpressible)).
  The one language-level question it raised ([finding 1](findings.md#1-the-effect-follows-the-http-method-not-the-descriptions-operation-type))
  is settled by precedent and recorded as declined-to-change, not as a proposal.
- **Author:** Keith Sprochi <kds1729@gmail.com>
- **Dates:** first published 2026-08-27, last updated 2026-08-27
- **Scope:** `tooling/nl-ingest-graphql` (new), `tooling/fake-service` (a `/graphql` surface),
  `spec/expressiveness.md` (GW18); read against `tooling/nl-ingest-openapi`'s doctrine and
  `evolution/gcp-sdk-poc` / `evolution/aws-sdk-poc`'s findings 3, 7, 8 and 11.
- **Provenance:** upstream commit `1306ac5` (the tree the adapter was written against);
  introspection results of three public services fetched 2026-08-27T14:25Z by the standard
  introspection query (`repro/introspection_query.json`): **Countries**
  (`https://countries.trevorblades.com/graphql`, 23 types, no `Mutation`), **Rick & Morty**
  (`https://rickandmortyapi.com/graphql`, 25 types), **AniList** (`https://graphql.anilist.co`,
  196 types, `Mutation` with 29 fields); live runs the same day. The saved schemas are in
  `/home/claude/sandbox/graphql-poc/` (not committed — regenerate with the query; a live schema
  drifts, so a re-run may differ in counts).
- **Resolution:** the adapter, its tests (25), the fake-service surface and this module landed
  together on `main`; the Countries production close (21 records + 6 traces + 21 signed
  certifications to Arca) is the named next step below — its artifacts are generated and
  gate-verified locally, publication awaits the operator.

## Summary

Every earlier description-layer result went through **one** adapter: Google Discovery→Swagger→OpenAPI
(gcp) and Smithy→OpenAPI (aws) both landed in `nl-ingest-openapi` unchanged, and the project
concluded that "the description layer is genuinely format-plural". That conclusion had only ever
been tested on formats that *are* OpenAPI after a projection. This module tests it on one that is
not: a GraphQL introspection schema describes **types**, not operations — no verbs, no paths, no
statuses, no documented values — and the request is a *document* the client composes.

The result is a second adapter of about 800 lines that **needed nothing from the language**: the
one place a new builtin looked necessary — encoding caller data into a GraphQL document/JSON
variables object, the exact unsoundness `url_encode` was pulled for in GW10 — is closed by
composing what exists (`render_json (JObj (map_put "code" (JStr code) map_empty))`), a zero-pull
([finding 6](findings.md#6-zero-pull-render_json-over-a-json-value-is-the-sound-encoder-for-variables-no-builtin-needed)).
Against three public services on the day it was written: **Countries** — 6 root fields → 21
projection records from 6 live calls, all certified and offline-replayed, the 249-country list
riding by address (338 KB); **Rick & Morty** — 9 → 27 records from 9 calls (at selection depth 2;
depth 1 refuses 3 of 9); **AniList** (POST-only) — 27 query root fields → 23 compiled, 142
projections licensed, **37 materialized from 20 paced live calls**, 29 mutations refused.

The uncomfortable parts are the ones that make it a different adapter rather than a front-end:

- **Nothing in a GraphQL description is spec-derivable.** The transport status is 200 for
  success, for an absent name and for a validation failure; the schema documents no values. So
  the adapter has no leaf status record and no spec-derived worked example at all — the entire
  corpus is observation-gated ([finding 3](findings.md#3-nothing-is-spec-derivable-the-transport-status-carries-no-verdict-and-absence-is-a-value--on-some-servers)).
  Where gcp-sdk-poc measured "1 of 81 constructible", GraphQL measures **0 of everything**, by
  construction. The `--observe-arg` mechanism gcp open question 1 produced is the *only* route to
  a value for any root field with a required argument.
- **The effect follows the wire, not the description.** A GraphQL *query* over POST is inferred
  `net.write` by the validator's method rule. This is measured (AniList is POST-only: all 37
  records declare `net.write` for reads), examined, and **not changed**: the method rule is what
  the validator can check; "query" is the description's testimony
  ([finding 1](findings.md#1-the-effect-follows-the-http-method-not-the-descriptions-operation-type)).
- **The gate as first written was a flood.** One request per projection is invisible against an
  emulator and fatal against a public service: 142 projections were 142 requests, and AniList
  answered 429 (3 records from the first run). The fix — one live call per byte-identical
  document, siblings by `eval --replay` of its trace, failures shared — is the module's main
  design contribution back to the gate doctrine
  ([finding 4](findings.md#4-the-observation-gate-is-a-flood-one-request-per-projection-trips-a-public-services-rate-limit)).
- **A schema can declare every argument nullable and the service can still demand one.**
  AniList's `Media`, `User`, `MediaList`, … answer 400 "requires at least 1 argument" to the
  minimal call their schema licenses — an *at-least-one-of* the description cannot say. The gate
  caught all 10; the operator resolves it by binding an optional argument, which the adapter
  therefore admits ([finding 5](findings.md#5-a-schema-whose-arguments-are-all-nullable-can-still-require-one-at-least-one-of-is-inexpressible)).

## Contents

| file | what |
|---|---|
| [`findings.md`](findings.md) | Nine findings, measurements separated from conclusions |
| [`repro/`](repro/) | The introspection query, and a hermetic reproduction of the gate (fake service, no network, no credentials) |

## What was measured

Commands: `graphql_ingest.py <schema> --out <dir> [--verify-against <url> …]`, run 2026-08-27
against the saved introspection results; the `report …` lines are the adapter's own.

| service | transport | query root fields | compiled / refused | projections licensed | materialized | live calls | by-address |
|---|---|---|---|---|---|---|---|
| Countries | GET | 6 | 6 / 0 | 21 | **21** (3 bindings: `country.code=DE`, `continent.code=EU`, `language.code=de`) | 6 | 1 (338,134 B) |
| Countries, absent | GET | 6 | 6 / 0 | 21 | 13 (`country.code=ZZ`: `Just JNull` + 9 × `None`) | 4 | — |
| Rick & Morty, depth 1 | GET | 9 | 6 / **3** | 24 | — | — | — |
| Rick & Morty, depth 2 | GET | 9 | 9 / 0 | 27 | **27** (6 bindings) | 9 | 0 |
| AniList, first run | POST | 27 (+29 mutations) | 23 / 4 | 142 | 3 | 118 | — |
| AniList, paced 2.5 s, 6 optional bindings | POST | 27 (+29 mutations) | 23 / 4 | 142 | **37** | 20 | 1 |

AniList's 20 live calls: 6 observed OK (`Media`, `Character`, `Studio`, `GenreCollection`,
`MediaTagCollection`, `ExternalLinkSourceCollection`); 10 × 400 ("requires at least 1
argument"); 2 × 401 (`Viewer`, `AniChartUser` — auth-only); 2 × 404 (`Staff`/`User` at id 1 —
absent); 24 projections not observed (required arguments unbound). Refusals: `Page` and
`SiteStatistics` (no argument-free scalar leaf at depth 1), `Notification` and `Activity`
(union results). Transport probe: GET `{ __typename }` → 200 on Countries and Rick & Morty;
404 `"Use POST request"` on AniList. Absent-name probe: Countries `country(code:"ZZ")` → 200
`{"data":{"country":null}}`; AniList `User(id:1)` → 404.

Hermetic: the in-repo fake service's `/graphql` (4 query fields + 1 mutation) → 10 projections,
10 materialized from 4 live calls on GET with `--auth-bearer`; 8 of 10 on POST without it (the
auth-only field fails 401 and its sibling inherits the verdict); a schema altered to declare
`health.status: Int` fails the gate for both `health` records and publishes nothing.
`tests/`: 25 tests pass; `nl-ingest-openapi` (75) and `ingest-common` (93) unchanged.

## What is argued

- **The description layer is format-plural beyond OpenAPI-shaped formats — but not adapter-plural
  for free.** The *doctrine* transferred intact (spec-time literal framing vs caller data; the
  observation gate; read-only by rule; refuse loudly; by-address values; replay offline). The
  *code* did not: what a GraphQL description says is different enough (types, not operations;
  a client-composed selection; no statuses) that the mapping had to be re-derived. The right
  unit of reuse is the doctrine and `ingest-common`, not `openapi_ingest.py`.
- **"Absence is a value" is a description-level fact only on servers that honor it.** Countries
  and Rick & Morty answer 200 + `null`; AniList answers 404. The adapter must not assume either;
  the gate observes what *this* server does, and the records say so.
- **A public service is a different exit gate from an emulator in one respect the emulator can
  never show: it counts your requests.** The one-request-per-document rule is not an
  optimisation; it is what makes the gate usable at all against anything with a rate limit.

## What worked well

- `--observe-arg` (gcp OQ1) carried over unchanged in spirit and turned out to be the *only*
  route to any value for a parameterised root field — the GraphQL case is the one it was built
  for, taken to its limit.
- `eval --replay` already existed; sharing one observation across sibling projections was a
  30-line change with no new artifact kind.
- `render_json` + the `Json` sum: variables are a value, not a string. No builtin, no splicing.
- The fake service took its `/graphql` surface in one function; the same introspection document
  both seeds the adapter's example and is what the service answers, so the two cannot drift
  (a test asserts it).

## Defects reported

None in existing code. The adapter's own first-draft defect (finding 4) is fixed in the same
tree the adapter landed in.

## Proposals

None. The one candidate — declare a `net.read` for a query over POST on the description's word
(finding 1) — is settled by precedent against: the effect is what the evaluator will *perform*,
inferred from what it can see; a description's operation type is testimony the validator cannot
check. Recorded here so the next author does not re-open it without new evidence.

## Open questions

- **Unions and interfaces.** A union result refuses (2 of 27 AniList root fields). GraphQL
  offers inline fragments (`… on Type { … }`) that would let the adapter select each possible
  type's scalar leaves and project `__typename`; the observed value would be held to *one of*
  the declared shapes. A design rung, not started — driver: a corpus where unions carry the
  data (AniList's `Notification`/`Activity` feeds do).
- **Mutations as world state.** GraphQL mutations are the natural site for `requires`/`ensures`
  contracts (a `putItem` ensures `item` exists) and `check-plan`; the adapter refuses them by
  rule today, exactly as the OpenAPI adapter refuses mutating verbs. The world-state thread
  closed on REST; whether a GraphQL mutation corpus adds anything is unmeasured.
- **`errors` alongside `data`.** The gate refuses a partial document (settled by preference:
  conservative). A field-level error on a leaf the record does *not* project is arguably
  irrelevant to that record; the stricter reading was kept because the trace is the publisher's
  testimony and a partial document under-states what the service said.

## Reproducing

Hermetic (no network, no credentials): `repro/run_hermetic.sh` — starts the fake service on a
private port, ingests `tooling/nl-ingest-graphql/examples/item-store.graphql.json` on both
transports, certifies and replays every record. Public-service runs need network and the saved
introspection results (`repro/introspection_query.json` regenerates them); AniList needs
`--transport post --pace 2.5`.
