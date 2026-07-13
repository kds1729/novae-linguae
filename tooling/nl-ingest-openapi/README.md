# nl-ingest-openapi — API descriptions → Nova Lingua records

`openapi_ingest.py` reads an [OpenAPI 3](https://spec.openapis.org/oas/v3.0.0) JSON description
and emits one **verified** Nova Lingua function record per operation. Unlike the source-code
adapters (`nl-ingest`, `nl-ingest-py/-hs/-ts`), which *lift* a function's body from code, this one
*compiles* a client function from a machine-readable API description: an operation's semantic
content — base URL, HTTP verb, path parameters, request body, auth scheme, documented statuses —
is exactly the surface of a client call, and it maps to a record over the general `http` builtin
([`spec/expressiveness.md`](../../spec/expressiveness.md) GW6) with no hand-authoring.

This is the high-leverage ingestion path for real SDKs: the large service SDKs are themselves
generated from machine-readable service definitions, so the semantic content that matters is the
description, not the generated client plumbing.

## Mapping

| OpenAPI | Nova Lingua |
|---|---|
| `operationId` | record `name_hints` |
| HTTP verb | the `http` method literal, and the effect (`net.read` for GET/HEAD, `net.write` otherwise) |
| `servers[0].url` | the `base` parameter (records stay host-portable; the server url is the example base) |
| path template `/items/{name}` | a `str_concat` URL builder over `base` and the path-parameter variables |
| path parameters | `string` parameters, in template order |
| **required query parameters** (GW10) | record parameters: a string value rides through the **`url_encode`** builtin (RFC 3986 strict — raw concatenation of caller data into a URL is unsound); an `integer` schema becomes an `int` parameter through `to_string`; parameter *names* are spec-time literals, percent-encoded at generation time |
| **required header parameters** (GW10) | `string` parameters, `map_put` into the header map by literal name |
| `requestBody` | a `body` `string` parameter (omitted for bodyless verbs) |
| **multipart-only `requestBody`** | a deterministic form: the boundary is a **spec-time constant** riding in the `Content-Type` literal, part names are description data, and each *required* `string` part (incl. `format: binary`) becomes a caller parameter — framing is literal, only part values are caller data (the `url_encode` split, applied to RFC 2046). Optional parts are omitted with a note |
| **relative-file `$ref`s** (`schemas.json#/…`) | resolved against the spec's own directory; the referenced document's refs resolve against *that* document and the imported subtree comes back fully inlined — pure factoring, byte-identical to the inlined description |
| `security` (Bearer) | an `Authorization: Bearer {{secret:NAME}}` header; operation-level `security: []` = no auth |
| `security` (**apiKey in header**, GW10) | a `<name>: {{secret:NAME}}` header (placeholder name defaults to the scheme key) |
| `security` (**oauth2 clientCredentials**, GW13) | an `Authorization: Bearer {{oauth:NAME}}` header — the record names the identity symbolically; at run time the *validator's* `--oauth NAME=token_url\|client_id\|client_secret` exchanges credentials at the description's token endpoint inside the live effect (token never in the record or trace; replay needs no identity). The adapter's live gate supplies this itself: pass `--oauth-client id:secret` — the tokenUrl comes from the description, never the operator. Every other oauth2 flow refuses — interactive flows need a principal the effect boundary cannot supply |
| **local `$ref`s** (GW10) | resolved (parameters, requestBodies, responses, security schemes, path-item-level shared parameters; cycle-bounded) |
| documented `responses` | the status code the worked example asserts |
| documented 2xx JSON **example** (GW11; `application/json` or any RFC 6839 `+json` subtype — `ld+json`/`geo+json`/`hal+json`, the NWS finding, ingestion-sweep increment 5) | a second **body-projection record** `<opId>Body : … -> Maybe Json` — `parse_json` over the response body, worked example = `Just(<the documented payload>)` |
| documented response **header with an example** (GW16) | a **header-projection record** `<opId><Header> : … -> Maybe string` over `http_full` — the call bound once, status-guarded to the documented response, `map_get` of the lowercase name |
| declared 2xx JSON **schema, no example** (ingestion-sweep increment 2; any `+json` content type, increment 5) | **schema-derived projections**: `<opId>Body : … -> Maybe Json` plus one **typed field projection** per declared property that narrows soundly (`string` -> `Maybe string`, `boolean` -> `Maybe bool`, object/array/untyped -> `Maybe Json`; numeric properties noted, never projected). Materialized only through the **live observation gate** — see below |

Each status record **returns the response `.status`** (an `int`) — the always-deterministic part of
a response. A **body projection** is emitted only where the description itself documents the payload
(a response `example`) *and* a deterministic success example is constructible from the spec alone (a
bodyless `GET` with no path parameters — path parameters name server state the description cannot
promise); anything else gets a printed note. Field access composes in-language via the certified
`json_get`/`json_path` commons records (principle 4 — the adapter exposes the payload as data, it
does not enumerate fields). Applied under grants, a projection's assert is an **`observed` claim**
(trace-conditioned, spec/trace.schema.json): a third party replays it against the recorded trace —
no effect grants, no secrets — which is what makes a verifiable claim about a response body possible
at all (see agent-loop.md §Scope).

**Schema-derived depth** (the real-world case: production descriptions overwhelmingly declare
response *schemas*, not examples — the Frankfurter finding) splits the promise from the value: the
declared schema **licenses** the projections and says what shape the answer must have; it does not
supply a value, so without `--verify-against` nothing is emitted (a printed note, never an invented
example). Under the gate, each projection body runs **once** — the observation becomes its worked
example, trace-attached and offline-replayable — and the observed document is **held to the declared
shape**: required properties present, every declared-type property that is present carries its
declared type (exactly what the projections promise; enum/minProperties/nested constraints are
deliberately out of scope). A description the service does not honor **fails the gate and publishes
nothing**. Numeric properties are noted, never projected: `JNum` carries an int *or* a float, so a
typed numeric promise cannot be narrowed soundly by pattern alone. A response documenting both an
example and a schema takes the example path.

## Honest refusals

What the language (or determinism) can't carry refuses the operation with a printed reason rather
than generating something subtly wrong: a **URL `$ref`** (no network at ingestion time — the
description must be locally complete), an **absolute-path or directory-escaping file `$ref`** (the
description is the unit of trust; it does not get to read the rest of the filesystem), a
**dangling/cyclic `$ref`**, a **multipart body without declared part properties / without required
parts / with a non-string part** (no spec-time part names, no minimal documented call, or a value
the form cannot carry), **apiKey in query/cookie** (a secret placeholder substitutes only inside a
*header* value at the effect boundary — in a query string the credential would enter the URL, hence
the record and the trace), **HTTP basic** (no base64 builtin), **oauth2/openIdConnect** interactive
flows, and **cookie parameters**. An *optional* query/header parameter — and an *optional*
multipart part — is omitted with a note: the record is the minimal documented call, never a silent
truncation. A compiled multipart form carries one honest caveat, printed as a note: the boundary is
a spec-time constant, and a part value containing the boundary delimiter line would break framing
(there is no escaping builtin — that contract is the caller's).
[`examples/search-service.openapi.json`](examples/search-service.openapi.json) is the
GW10 reference description exercising all of it (`$ref`-factored components, `?q=&limit=` query
building, a header parameter, apiKey auth, and the compiled multipart upload — live-gated against
the fake service's `POST /upload`, which really parses the form).

## Verified by default

Every generated record is gated through `nl-validator certify` (typecheck / effects / termination /
complexity). With `--verify-against <base-url>` the worked examples are additionally **run** against
a live service — the "gate = examples vs an emulator" step — so a generated record is
verified-by-default exactly like a hand-authored one. The in-repo
[`tooling/fake-service`](../fake-service/) is a reference service to verify against.

For an **effectful** record the live gate is also the trace capture (GW12, replay-checkable
examples): each example runs exactly once — grants and secrets supplied by the operator — must
reproduce its documented result, and the observed effect trace is attached to the example by
`trc_…` content-address (the trace artifact is written alongside the record, re-addressing it).
The adapter then re-checks the examples by **replay with no secrets**: the check any commons
consumer can perform offline — no credentials, no reachable service — with the usual honest scope
(the trace is the publisher's testimony; replay proves the documented result follows from it).

**Large expected values ride by address.** Above `--blob-threshold` JCS-canonical bytes (default
64 KiB) an example's expected value — an observed multi-MB document (the NWS glossary), or an
equally large *documented* example — leaves the record: the example carries a
`result_blob` `{sha256, bytes}` pointer (function-record v0.2) and the value's canonical bytes are
written as a `blob-<sha256>.json` sidecar destined for a node's gate-free `/v0/blobs` store
(`manage.py addblob`), so the record never blows a node's record-size cap. The run report marks such
records `example=BY-ADDRESS(<n> bytes)`, and offline replay resolves the sidecar from the records
directory, sha256-verified.

```
python3 openapi_ingest.py examples/item-store.openapi.json --out /tmp/recs \
    --secret-name api_token --verify-against http://127.0.0.1:8878 --token test-token
```

`--token` binds credentials **per security scheme**: `--token NAME=VALUE` (repeatable, NAME = the
scheme key) gives each scheme its own credential, and a bare `--token VALUE` (at most one) is the
default for every unbound scheme — so a description mixing schemes (apiKey + bearer) live-gates in
one run, each operation authenticating with its own scheme's value. Binding a scheme the
description doesn't declare refuses up front. (This closes the former known limit — one shared
token for all schemes; the old single-value invocation still means what it always did.)

## Faithfulness

The generator's output is not merely *valid*, it is *the same records a human would author*: run
against [`examples/item-store.openapi.json`](examples/item-store.openapi.json) (the description of
the reference fake service), the generated `getItemStatus` and `deleteItem` bodies are
**byte-identical** (same `expr_` content-address) to the hand-authored GW6 records
[`item-status`](../../spec/examples/item-status.v0.2.json) /
[`delete-item`](../../spec/examples/delete-item.v0.2.json) — the description contains their full
semantic content. (`putItem` differs only in the request-body parameter name; `healthCheck` — an
unauthenticated liveness probe — is net-new, certified, and published to the commons.)

Reuses [`ingest-common`](../ingest-common/) (the shared BLAKE3+JCS core and body-AST builders), so
its records agree byte-for-byte with every other adapter on canonical form and content-hash.
Requires only `python3` (3.10+) and the built `nl-validator` on the sibling `target/release` path.
