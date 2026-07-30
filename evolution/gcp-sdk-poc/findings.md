# Findings — Google Cloud Storage v1 through `nl-ingest-openapi`

Companion to [`README.md`](README.md). Per [`evolution/README.md`](../README.md), measurements and
inferences are kept separate: each finding states what was observed, then what the author concludes
from it. A reader may reject every conclusion here and keep every number.

## Provenance

| input | pinned value |
|---|---|
| Discovery document | `storage:v1`, revision **20260719**, from `https://storage.googleapis.com/$discovery/rest?version=v1` |
| Discovery → Swagger 2.0 | `google-discovery-to-swagger` **2.1.0** (npm) |
| Swagger 2.0 → OpenAPI 3 | `swagger2openapi` **7.0.8** (npm) |
| upstream commit | `76fc6ba` (2026-07-15) |
| run date | 2026-07-26 |

Google publishes Discovery documents rather than OpenAPI 3, so the pipeline is
`Discovery → Swagger 2.0 → OpenAPI 3 → normalize → nl-ingest-openapi → commons node`.

## What was measured

| quantity | value |
|---|---|
| operations in the converted description | 81 |
| operations compiled | 81 |
| operations refused or skipped | 0 |
| records that certified | 81 |
| operations with a `requestBody` | 31 (30 `application/json`, 1 `application/octet-stream`) |
| 2xx responses declaring a body | 69 |
| distinct 2xx response content types across the spec | 1 — `*/*` |
| operations passing the projection constructibility filter | 1 (`storage.buckets.list`) |
| notes emitted by the run | 877, all "optional query param … omitted" |
| notes about the 69 declined response bodies | 0 |
| records with non-empty `refinements` | 0 |
| modifications to this repository required | 0 |

Live provisioning: bucket `insert` → 200, `get` → 200, `delete` → 204, each asserted by the
executed record's example, effects granted from `signature.effects`, bearer token supplied as
`--secret`.

---

## 1. The request `Content-Type` is dropped

**Measured.** A hand-authored OpenAPI 3 description with one `POST` whose `requestBody` declares
`application/json` compiles to a body AST whose `http` header argument is
`{"kind": "var", "name": "map_empty"}`. Neither generated artifact contains the string
`content-type`. The run reports `certify=OK`. 31 of the 81 Cloud Storage operations are affected —
every write operation in the API. No converter is involved in the reproduction.

Multipart bodies are unaffected: they have always emitted a `Content-Type` carrying the spec-time
boundary. Only the non-multipart path is missing it.

**Concluded.** The record cannot perform its own documented call, and nothing in the run report says
so, which makes this a faithfulness defect rather than a missing feature. The repair must be
data-driven from each operation's own declared media type — a blanket `application/json` would be
wrong for `storage.objects.insert`, which declares `application/octet-stream`. See
[proposal 01](proposals/01-request-content-type.md).

## 2. A non-JSON 2xx response body was declined silently

**Measured.** Two descriptions differing in exactly one key — the response content type — produce
different amounts of explanation. With `application/json`, four notes naming the pending projections
(`getStatusBody`, `getStatusState`, `getStatusHealthy`) and why each was not materialised. With
`*/*`, the entire output is:

```
getstatus        body=expr_09e7efcc93b0153…  certify=OK
```

The body hash is identical in both runs, so only the projections diverge. At scale: 877 notes
emitted, none about the 69 declined response bodies.

The trigger is not vendor-specific. `google-discovery-to-swagger` emits Swagger 2.0 with **no
`produces`** — not global, not per-operation — while still declaring response schemas.
`swagger2openapi` then has no media type to key the response content by. Confirmed on a hand-written
four-line Swagger 2.0 document with nothing Google about it:

```
no `produces`                    -> content keys: ["*/*"]
`produces: [application/json]`   -> content keys: ["application/json"]
```

**Concluded.** Declining to project on `*/*` is *correct* — a media range promises any type, so it
cannot license the parses-as-JSON promise `parse_json` needs. Declining **silently** is the defect,
because it contradicts the adapter's own contract that what it cannot carry gets a printed reason.
`produces` is optional in Swagger 2.0 and routinely omitted, so any description arriving through
that conversion path loses every body projection without a word — a large class, and Swagger 2.0
conversion is a common way OpenAPI 3 descriptions come into existence.

Only the note is proposed as a fix. Whether `*/*` accompanied by a JSON Schema should be *treated*
as JSON is a separate judgement, deliberately not taken here.

Worth noting for whoever reviews it: the existing suite had locked the silence in.
`test_suffixed_json_content_type_licenses_schema` asserted `text/html` → `pending == []` and never
checked whether anything was said.

## 3. The constructibility rule admits 1 of 81 operations

**Measured.** A body projection requires a bodyless `GET` with no path parameters.

| filter | operations |
|---|---|
| total | 81 |
| 2xx response declaring a body | 69 |
| `GET`, no path parameters, no request body | **1** (`storage.buckets.list`) |

Everything under `/b/{bucket}` — `buckets.get`, `objects.get` — is excluded by the path parameter, and
every mutating verb by the verb.

**Concluded.** The rule's reasoning is sound: a path parameter names server state the description
cannot promise, so no worked example is derivable from the spec alone. But the consequence is not
visible from the in-repo reference descriptions, and it is severe. Every generated leaf record
projects `.status` and discards `.body`, so the corpus is a set of operations returning `int`. Values
cannot flow between calls, which means the corpus **cannot express a dataflow plan** — nodes without
edges — no matter how many operations are ingested.

This is a constraint on where a worked example may come from, not on runtime capability: `http`
already returns `{status, body}`, and a record that parsed `.body` would execute correctly. It simply
could not be certify-gated the same way. The live gate already sources examples from observations
elsewhere, which is why open question 1 in the README is phrased as it is.

## 4. The live observation gate is unusable for mutating verbs

**Measured.** Schema-derived projections materialise only under `--verify-against`, which executes
each example once. The pipeline withheld the flag entirely, so no schema-derived projection could
materialise for this corpus.

**Concluded.** For `storage.buckets.insert` the gate would create a real bucket during *ingestion*,
which is not an acceptable side effect of building a corpus. `GET`/`HEAD` are `net.read` and create
nothing, so a read-only gate would be safe and would materialise projections wherever finding 3
permits them. Whether that belongs behind a separate flag is a maintainer's call.

## 5. What the description layer cannot supply

**Measured.** All 81 records carry `refinements: []`, `derived_from: null`, and `supersedes: null`.

**Concluded.** An OpenAPI description carries no pre/postconditions and no lineage. Google's document
says a subnet takes a `network` field; it never says a network must exist first, because that is not
a fact about the API surface — it is a fact about the domain. **Dependency and ordering knowledge is
not manufacturable from descriptions at any scale.** A corpus grown this way is a vocabulary, and the
knowledge of how to assemble it has to be authored, as composite records or refinements or both.

This is scoping, not criticism: the adapter does exactly what it claims. It does mean that the value
of a commons for assembling systems rests on a layer this pipeline cannot produce.

## 6. Resolution needed to be exact, not semantic

**Measured.** Operations were resolved for execution with `POST /v0/query {name_hint_prefix}`.
`/v0/search` was tried first and was not reliable for selecting one operation out of 81 sharing four
near-identical intent tags.

**Concluded.** The node's stdlib embedder is lexical — it ranks by token overlap, not meaning — and
the extending `<lead>/<own-hyphenated-name>` tag helps but does not make ranking dependable at this
density. Exact prefix query is the right tool when the caller knows the operation's name, which is
the case for execution. It is unavailable to a caller searching for a *design* it cannot name, which
is the case for finding prior art.

## 7. The two description transforms, and why each is faithful

**OAuth2 → a plain `Bearer` HTTP scheme.** GCP Discovery declares OAuth2 (implicit /
clientCredentials), and `resolve_auth` refuses interactive flows because a browser/redirect principal
cannot cross the effect boundary. That refusal is correct. But every GCP call is, on the wire,
`Authorization: Bearer <token>`, and `gcloud auth print-access-token` yields such a token out of
band. Rewriting the scheme leaves the wire request byte-identical and changes only the *description*
of how the token is obtained.

**Concluded — deliberately not proposed upstream.** As a general transform it would let any
description launder past a boundary this project chose on principle. If it were ever wanted, the
honest shape is narrower: an explicit operator-supplied bearer credential for an oauth2-declared
operation, recorded as operator-supplied.

**`*/*` → `application/json` on responses.** Downstream compensation for the conversion artifact in
finding 2, applied only because Cloud Storage is independently known to serve JSON. It is not
proposed upstream either — the adapter's correct fix is the note, and a description that means
`application/json` should say so.

## Reproducing

Findings 1–3 need no credentials and no network.

```bash
# finding 1 — hand-authored OpenAPI 3, no converter, no vendor
python3 openapi_ingest.py <fixture-with-requestBody>.json --out /tmp/a
grep -ci content-type /tmp/a/*                     # 0

# finding 2 — two fixtures differing only in the response content-type key
python3 openapi_ingest.py <fixture-application-json>.json --out /tmp/b1   # 4 notes
python3 openapi_ingest.py <fixture-any-star>.json      --out /tmp/b2   # 0 notes before the fix

# the */* provenance — swagger2openapi alone, no Google input
node -e "require('swagger2openapi').convertObj({swagger:'2.0',info:{title:'p',version:'1'},
  paths:{'/s':{get:{operationId:'g',responses:{200:{description:'ok',
  schema:{type:'object',properties:{state:{type:'string'}}}}}}}}},{patch:true,warnOnly:true},
  (e,o)=>console.log(Object.keys(o.openapi.paths['/s'].get.responses['200'].content)))"
# -> [ '*/*' ]

# findings 2, 3, 5 at scale — needs the converted Cloud Storage description
python3 openapi_ingest.py storage.v1.openapi3.bearer.json --out /tmp/gcs 2>&1 | tee /tmp/gcs.log
grep -c "optional query param" /tmp/gcs.log         # 877
grep -c "response body not projected" /tmp/gcs.log  #  69   (after the fix; 0 before)
ls /tmp/gcs/*.v0.2.json | wc -l                     #  81
```

The live provisioning step needs a GCP project and `gcloud auth print-access-token`; it is not
required for any finding above.
