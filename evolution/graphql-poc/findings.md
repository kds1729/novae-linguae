# Findings — graphql-poc

Each finding: what was observed (with the command or probe), then what is inferred, kept apart.
Run date 2026-08-27; the schemas are the introspection results fetched that day (README →
Provenance). `gi` = `tooling/nl-ingest-graphql/graphql_ingest.py`.

## 1. The effect follows the HTTP method, not the description's operation type

**Observed.** `tooling/validator/src/effects.rs` refines a general request builtin's effect by
its *literal method*: `GET`/`HEAD` → `net.read`, anything else → `net.write`, a non-literal
method → both. A GraphQL query sent over POST (`gi … --transport post`) therefore certifies with
`effects: ["net.write"]`; AniList, which answers GET with 404 `"Use POST request to access
graphql subdomain"`, yields 37 records all declaring `net.write` for what its schema calls
`Query` fields. Over GET (Countries, Rick & Morty) the same records declare `net.read`.

**Inferred.** The rule is correct as a *lower bound the validator can check*: the evaluator will
perform a POST, and a POST may write; the description's "this is a query" is testimony about
the server that no static check can confirm. Changing it would let a description under-declare.
Settled by precedent (the GW6/GW14 method rule and the `check-effects` doctrine that inferred
effects are a lower bound, never a purity certificate) — **declined to change**, recorded here.
The cost is real and bounded: on a POST-only server a read-only corpus needs a `net.write@host`
grant to run, and pure-only policies exclude it. The mitigation is the transport itself: where
the server accepts GET, the adapter's default produces honest `net.read` records.

## 2. The transport is a server property the description does not declare

**Observed.** Probe `GET <endpoint>?query={ __typename }`: Countries 200, Rick & Morty 200,
AniList 404. The introspection schema has no field, directive or type that could say which.

**Inferred.** Like AWS moving auth and endpoint from data to computation (aws finding 5), GraphQL
moves *transport* out of the description entirely: `--transport` is an operator fact, as the
endpoint URL already is (`base`). The adapter records the fact in the artifact — the trace
carries the method — and the effect declaration follows it (finding 1).

## 3. Nothing is spec-derivable: the transport status carries no verdict, and absence is a value — on some servers

**Observed.** Countries: `{ country(code:"ZZ") { code name } }` → 200
`{"data":{"country":null}}`; `{ nope }` → 200 `{"errors":[…GRAPHQL_VALIDATION_FAILED…]}`;
`{ country(code:"DE") … }` → 200 with data. AniList: `User(id: 1)` → **404**; `Media` with no
arguments → **400** `"The Media query requires at least 1 argument"`. `gi` without
`--verify-against` writes zero records for every schema (`summary: N projections licensed, 0
materialized`), because `function-record.v0.2.schema.json` requires ≥ 1 example and no example
can be derived from a GraphQL description.

**Inferred.** The OpenAPI adapter's leaf record — "the response `.status`, the always-
deterministic part" — has no counterpart here: the status is constant on a conforming server
and meaningless as a verdict, and the verdict lives in the body (`data` vs `errors`). So the
adapter's *only* record kinds are projections, all observation-gated. gcp-sdk-poc measured the
constructibility rule admitting 1 of 81 operations; GraphQL is 0 of everything by construction,
which makes `--observe-arg` (gcp OQ1) the whole value channel. On absence: GraphQL's own
semantics spell an absent entity as `null` — so, unlike gcp finding 11 (absent-name examples
unsatisfiable, no documented 404), the absent-name example is satisfiable and observed:
`Just JNull` for the whole value, `None` for each typed leaf (13 records at `country.code=ZZ`).
But a server may choose otherwise (AniList's 404), and the gate observes what *this* server does
rather than assuming the spec's spelling.

## 4. The observation gate is a flood: one request per projection trips a public service's rate limit

**Observed.** First AniList run (`--transport post`, no pacing): 142 projections licensed, the
gate issued **one live request per projection**; the service answered 429 from early on; 3
records materialized. Second run (`--pace 1.5`): still 429s, and a *failed* observation was not
shared with its siblings, so byte-identical requests were re-issued — 118 live calls for 3
records. Countries had already shown the shape without the pain: 21 records carrying only 6
distinct `trc_` addresses, because sibling projections' requests are byte-identical and traces
are content-addressed.

**Fix (in the adapter).** A live call is made once per distinct (document, arguments); every
sibling projection runs by `nl-validator eval --replay <that trace>` — same observation, same
`trc_` address, no request — and a failed observation is shared the same way (siblings inherit
the verdict, no call). `--pace SECONDS` spaces the live calls. Third run (`--pace 2.5`, 6
optional bindings): **37 records from 20 live calls**, 0 rate-limit failures. Countries: 21
records from 6 calls, addresses byte-identical to the unshared run's.

**Inferred.** An emulator can never show this defect — it does not count requests. It is the
one property of a *public* exit gate that the fake-service gate structurally cannot rehearse,
and it argues for the same one-request-per-document rule in the OpenAPI adapter's
schema-projection path (each field projection there is also a separate live execution of the
same request; `aws-sdk-poc`'s 63 observed projections were 63 calls). Not changed there —
no driver has hit a limit — but named.

## 5. A schema whose arguments are all nullable can still require one: "at least one of" is inexpressible

**Observed.** AniList `Media`, `User`, `MediaTrend`, `AiringSchedule`, `MediaList`,
`MediaListCollection`, `Review`, `ActivityReply`, `Thread`, `ThreadComment`, `Recommendation`,
`Like` (10 distinct requests of 20) declare every argument nullable; the adapter's minimal call
(no arguments) draws 400 `"The X query requires at least 1 argument."`. The gate minted nothing
for any of them (`the call answered 400, not 200`). With `--observe-arg Media.id=1` (an
*optional* argument bound by the operator) `Media` observes and materializes 18 records.

**Inferred.** OpenAPI's `required` list and GraphQL's `NON_NULL` both state per-argument
necessity; neither can state a disjunction across arguments. The description is therefore
*wider* than the service, the mirror image of aws finding 3 (a synthesized example narrower than
the description's `required`). Design decision, settled by precedent (gcp OQ1: the operator
names what the description cannot): `--observe-arg` on an **optional** argument *includes* it
as a record parameter — the minimal call widened by exactly what the operator named, printed as
a note, visible in the example's arguments. The record then honestly says "this call, with this
argument", which is what the service actually accepts.

## 6. Zero-pull: `render_json` over a `Json` value is the sound encoder for variables — no builtin needed

**Observed.** The generated body for `country(code)`:
`http "GET" (str_concat base (str_concat "?query=<pct(document)>" (str_concat "&variables="
(url_encode (render_json (JObj (map_put "code" (JStr code) map_empty))))))) map_empty ""`.
The trace's URL:
`…?query=query%20Q%28%24code%3A%20ID%21%29%20%7B%20country%28code%3A%20%24code%29…&variables=%7B%22code%22%3A%22DE%22%7D`.
Every generated record certifies; the `Json` sum's `JStr`/`JNum`/`JBool` constructors carry the
five built-in scalars, an enum rides as `JStr`, and an input object / list / custom scalar rides
as a `Json` parameter placed in the variables as-is (`charactersByIds.ids=["1","2"]` observed
on Rick & Morty).

**Inferred.** GW10 pulled `url_encode` because building a URL by concatenating caller data is
unsound and the language lacks per-character access. The same argument applies verbatim to
splicing caller data into a GraphQL document or a JSON body — and this time the language
already had the answer: keep the data as a *value* and let `render_json` serialize it. The
document is a spec-time literal (percent-encoded at generation time, exactly as a query
parameter *name* is in the OpenAPI adapter); only the variables are run-time data, and they
never meet a `str_concat`. This is the strongest single piece of evidence in the module that
the language's primitives are at the right altitude.

## 7. Selection depth is a design choice with a measurable cost

**Observed.** Rick & Morty at `--select-depth 1`: 3 of 9 root fields refused (`characters`,
`locations`, `episodes` return `{ info {…} results [{…}] }` — no scalar leaf at depth 1); at
depth 2: 0 refused, 27 projections. AniList at depth 1: `Page` and `SiteStatistics` refused for
the same reason. Nested object leaves ride inside the whole-value projection; no typed
projection is minted for them (the typed projections are the result's *own* scalar leaves).

**Inferred.** A GraphQL client composes its own response shape, so "the record" is
under-determined by the description in a way an OpenAPI response never is. The adapter fixes
the shape deterministically (all argument-free scalar leaves, objects to a depth) and makes the
depth an explicit operator parameter, so two records for the same field at different depths
are different records with different documents — honest, addressable, and reproducible. The
default of 1 is conservative (the smallest legal document); a paginated API needs 2.

## 8. Unions refuse; interfaces select

**Observed.** AniList `Notification: NotificationUnion` and `Activity: ActivityUnion` refuse
(`UNION … no legal selection set`). Interface-typed fields select like objects (the interface's
own fields); none of the three schemas exercised one at a root.

**Inferred.** A union has no common field; selecting only `__typename` would mint a record that
projects nothing. Inline fragments per possible type are the correct next rung (README → Open
questions); refusing loudly is the right interim.

## 9. Sibling projections dedup to one trace, and the artifact says so

**Observed.** Countries: 21 records, 6 distinct `trc_` addresses; the ten `country*` records all
carry `trc_ed6e87c8…`, the five `language*` records `trc_ee40cdd5…`. This was true *before* the
finding-4 fix (byte-identical requests → byte-identical traces → one address) and after it
(one request, replayed); the records' `fn_` addresses are identical across the two runs.

**Inferred.** Content-addressing did the deduplication for free at the artifact level; finding 4
only stopped the *service* from paying for it. A consumer verifying `countryCapital` fetches the
same trace as one verifying `countryName` — the evidence is shared exactly as widely as it is
identical.
