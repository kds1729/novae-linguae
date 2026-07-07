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
| `requestBody` | a `body` `string` parameter (omitted for bodyless verbs) |
| `security` (Bearer) | an `Authorization: Bearer {{secret:NAME}}` header; operation-level `security: []` = no auth |
| documented `responses` | the status code the worked example asserts |

Each record **returns the response `.status`** (an `int`) — the deterministic, verifiable part of a
response. Projecting the body (often server-assigned and nondeterministic) waits for observed-claims
(see agent-loop.md §Scope).

## Verified by default

Every generated record is gated through `nl-validator certify` (typecheck / effects / termination /
complexity). With `--verify-against <base-url>` the worked examples are additionally **run** against
a live service — the "gate = examples vs an emulator" step — so a generated record is
verified-by-default exactly like a hand-authored one. The in-repo
[`tooling/fake-service`](../fake-service/) is a reference service to verify against.

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
