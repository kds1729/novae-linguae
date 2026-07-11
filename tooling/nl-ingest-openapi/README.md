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
| `security` (Bearer) | an `Authorization: Bearer {{secret:NAME}}` header; operation-level `security: []` = no auth |
| `security` (**apiKey in header**, GW10) | a `<name>: {{secret:NAME}}` header (placeholder name defaults to the scheme key) |
| **local `$ref`s** (GW10) | resolved (parameters, requestBodies, responses, security schemes, path-item-level shared parameters; cycle-bounded) |
| documented `responses` | the status code the worked example asserts |
| documented 2xx `application/json` **example** (GW11) | a second **body-projection record** `<opId>Body : … -> Maybe Json` — `parse_json` over the response body, worked example = `Just(<the documented payload>)` |

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

## Honest refusals

What the language (or determinism) can't carry refuses the operation with a printed reason rather
than generating something subtly wrong: an **external or dangling `$ref`**, a **multipart-only
request body** (no deterministic boundary construction), **apiKey in query/cookie** (a secret
placeholder substitutes only inside a *header* value at the effect boundary — in a query string the
credential would enter the URL, hence the record and the trace), **HTTP basic** (no base64
builtin), **oauth2/openIdConnect** flows, and **cookie parameters**. An *optional* query/header
parameter is omitted with a note — the record is the minimal documented call, never a silent
truncation. [`examples/search-service.openapi.json`](examples/search-service.openapi.json) is the
GW10 reference description exercising all of it (`$ref`-factored components, `?q=&limit=` query
building, a header parameter, apiKey auth, and a refused multipart upload).

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

```
python3 openapi_ingest.py examples/item-store.openapi.json --out /tmp/recs \
    --secret-name api_token --verify-against http://127.0.0.1:8878 --token test-token
```

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
