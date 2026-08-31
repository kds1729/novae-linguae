# nl-ingest-graphql — GraphQL schemas → Nova Lingua records

`graphql_ingest.py` reads a GraphQL schema **as the service itself describes it** — the result of
the standard introspection query (`__schema`), saved to a local file — and emits one **verified**
Nova Lingua projection record per root `Query` field (plus one typed projection per scalar leaf of
an object-valued result). It is the second description-layer adapter after
[`nl-ingest-openapi`](../nl-ingest-openapi/) and shares its doctrine: a machine-readable
description *is* the semantic content of a client call, so the record compiles from it with no
hand-authoring; every record is gated through `nl-validator certify`; every worked example is a
recorded observation that replays offline.

What makes it a *different* adapter rather than a second front-end is what a GraphQL description
does and does not say — three things that change the mapping's shape:

1. **The request is a document, and caller data are variables.** An OpenAPI operation's caller
   data lands in a URL path, a query string, or a body — three wire encodings, one builtin
   (`url_encode`) pulled for the unsound one. A GraphQL call is one *document* — a spec-time
   literal the adapter derives (`query Q($code: ID!) { country(code: $code) { code name … } }`) —
   plus a *variables* object. The variables are built as a `Json` **value**
   (`JObj (map_put "code" (JStr code) map_empty)`) and serialized by the existing `render_json`
   builtin: caller data never touches a string concatenation, and **no new builtin was needed**
   — the language already had the sound encoder (a zero-pull, like GW15's Link-header pagination).
2. **The transport is undeclared.** GraphQL-over-HTTP serves a query on `GET` (document and
   variables in the request target) or `POST` (a JSON body); the schema says nothing about which
   the server accepts. `--transport get|post` is the operator's fact. Under `GET` the effect is
   `net.read`; under `POST` the validator's method rule infers `net.write` for what the
   description calls a *query* — a measured tax, not a hidden one (see the module).
3. **Nothing is spec-derivable.** The transport status is `200` for a successful query, for an
   absent name (`data.field = null` — absence is a *value*), and for a validation failure
   (`errors`, no `data`); the schema documents no example values. There is therefore no leaf
   "status" record and no spec-derived worked example: **every record is observation-gated**
   (`--verify-against`) and, without the gate, the adapter prints a licensing report and writes
   nothing. A schema licenses shapes; only an observation supplies a value.

## Mapping

| GraphQL (introspection) | Nova Lingua |
|---|---|
| root `Query` field `f` | the **whole-value projection** `f : base, args… → Maybe Json` — `data.f` out of the parsed 200 response, `None` if the envelope is not the declared one |
| its object-valued result's scalar leaves | one **typed projection** per leaf, `f<Leaf> : … → Maybe string` (`String`, `ID`, any enum) / `Maybe bool` (`Boolean`) / `Maybe Json` (custom scalar); `Int`/`Float` leaves are **noted, never projected** (`JNum` carries an int *or* a float — a typed numeric promise cannot be narrowed soundly by pattern; the OpenAPI adapter's rule) |
| the field's return type | the **selection set**: every argument-free `SCALAR`/`ENUM` field of the result type; object-valued fields recurse to `--select-depth` (default 1 — scalars of the result only); a field with a required argument is never selected; a list result selects its element type's leaves (and mints no typed projections — the value is the list) |
| required argument (`NON_NULL`, no default) | a record parameter, in declared order after `base`: `String`/`ID` → `string`, `Int` → `int`, `Float` → `float`, `Boolean` → `bool`, an enum → `string` (a value outside the set is the service's to refuse), anything else (input object, list, custom scalar) → `Json` — placed in the variables as-is |
| a list / input-object / custom-scalar argument at observation time | bound as JSON text: `--observe-arg 'charactersByIds.ids=["1","2"]'` — parsed into the `Json` value the record's variables carry |
| optional argument | **omitted with a note** (the record is the minimal documented call) — unless the operator binds it with `--observe-arg`, in which case it is *included* as a parameter: the minimal call widened by exactly what the operator named |
| the endpoint URL | the `base` parameter (records stay host-portable; the introspection result carries no URL) |
| `--transport get` (default) | `http "GET" (str_concat base "?query=<pct(document)>&variables=" ++ url_encode (render_json <variables>)) headers ""` — the document percent-encoded at generation time (spec-time literal), the variables through `url_encode` at run time; effect `net.read` |
| `--transport post` | `http "POST" base {Content-Type: application/json} (render_json (JObj {query, variables}))`; effect `net.write` by the method rule |
| auth | introspection declares none. `--auth-bearer NAME` adds `Authorization: Bearer {{secret:NAME}}` — the operator's fact, substituted only at the effect boundary |
| `Mutation` / `Subscription` root fields | **refused** — read-only by rule |

Files are named by the record's first name hint — the field name lowercased (`countryCapital` →
`countrycapital.v0.2.json`, `body-countrycapital.json`); the run report prints the file next to
each materialized record. Each record's `intent_tags` are `io`, `io/network/http`, `query/lookup`, `parse`, plus one
extending tag (`query/lookup/<field>` for a whole-value projection, `parse/<field-leaf>` for a
typed one; omitted, never truncated, past 64 characters).

## The observation gate

With `--verify-against <endpoint>` each licensed projection runs **once**: `nl-validator eval
--trace-out` under the record's effect (and `--auth-bearer`'s secret), the observed value is held
to the **declared type** — a non-null position is not `null`; a list position is a list whose
every element conforms; an object position carries **every selected leaf** (the GraphQL spec
guarantees a selected field appears in the data — absence is a protocol violation, not a value),
each leaf its declared constructor, an enum leaf a declared value; custom scalars unconstrained —
and only then is the record minted, with the observation as its worked example, the trace
attached by `trc_…` address. Each projection is judged on its own recorded call (the trace's real
status and body): a non-200, a non-JSON body, a response with no `data`, or **a response
carrying `errors` alongside `data`** (a partial document is not the declared one) fails the
gate and publishes nothing.

**One request per document.** Sibling projections of the same root field issue the byte-identical
request, so the first one observes live and the rest run by `eval --replay` of its trace — the
same observation, the same `trc_` address, one call on the service per root field however many
records it licenses (`--pace SECONDS` spaces the live calls; a public service's rate limit is the
operator's to respect). A failed observation is shared the same way: the siblings inherit the
verdict without a call.

**Absence is a value.** `--observe-arg country.code=ZZ` at an absent name observes
`Just JNull` for the whole value and `None` for each typed leaf — legitimate observations of an
obtained document (finding-7 doctrine), and a satisfiable absent-name example where the
OpenAPI adapter's (gcp finding 11) was not. Whether a *server* spells absence that way is its
own choice: AniList answers `404` instead, and the gate mints nothing there.

**Large observed values ride by address** above `--blob-threshold` JCS bytes (default 64 KiB):
the example carries a `result_blob` pointer and the value is written as a `blob-<sha256>.json`
sidecar for a node's `/v0/blobs` store — the 249-country `countries` list (338 KB) is the
first GraphQL example to do so.

After the gate every record is certified (`nl-validator certify`) and **replayed with no
secrets** (`nl-validator run`) — the offline check any commons consumer can perform.

## Honest refusals

A `Mutation` or `Subscription` root field (read-only by rule — an observation must not create
state during ingestion; a subscription has no request/response shape); a result type with **no
argument-free scalar leaf** (an empty selection set is not a legal document — `SiteStatistics`
on AniList at depth 1, `characters`/`locations`/`episodes` on Rick & Morty at depth 1, all
compile at depth 2); a **union** result (no common field; `__typename` alone projects nothing);
a parameter named `base`; an `--observe-arg` naming a field or argument the schema does not
declare — refused **before any artifact is written or any call is made**. A root field with
required arguments and no binding is licensed but *not observed* (a note names the parameters).

```
python3 graphql_ingest.py examples/item-store.graphql.json --out /tmp/recs \
    --verify-against http://127.0.0.1:8878/graphql --observe-arg item.name=gw18-widget \
    --auth-bearer api_token --token test-token
```

[`examples/item-store.graphql.json`](examples/item-store.graphql.json) is the introspection
result of the in-repo [fake service](../fake-service/)'s `/graphql` (a nullable `item(name:
ID!)` with an enum, a boolean, a nullable leaf and a nested object; a non-null list `items`; an
auth-only `secret`; a `Mutation` to refuse), served on both transports. `tests/` gates against
it: 25 tests, `python3 -m unittest discover -s tests` (any
python3 ≥ 3.10 works for this adapter).

## At production scale

[`evolution/graphql-poc`](../../evolution/graphql-poc/) reports this adapter against three
public GraphQL services on the day it was written (2026-08-27): **Countries** (6 root fields →
21 records from 6 live calls, all certified and replayed, published to Arca), **Rick & Morty**
(9 → 27 from 9 calls at depth 2; 3 of 9 refused at depth 1) and **AniList** (POST-only, 196
types, 27 query root fields + 29 mutations → 23 compiled, 142 projections licensed, 37
materialized from 20 paced calls; every remaining failure is description-level — a schema whose
all-nullable arguments the service nevertheless requires, auth-only fields, a 404 at an absent
id). The module records the five findings that
run surfaced and the two design decisions it settled.

Reuses [`ingest-common`](../ingest-common/) (the shared BLAKE3+JCS core and body constructors).
Requires only `python3` and the built `nl-validator` on the sibling `target/release` path.
