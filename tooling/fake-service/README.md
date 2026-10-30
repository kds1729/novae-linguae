# fake-service — the reference sandbox for effectful gates

`fake_service.py` is a stdlib-only, deterministic, in-memory HTTP service: the smallest thing an
effectful record's live gate can run against without touching any real, billable or
mutable-in-the-world system. The adapters' tests use it; so can you, as the `--verify-against`
target for a first OpenAPI (or GraphQL, or Smithy) run.

```
python3 tooling/fake-service/fake_service.py --port 18878 --log
```

| option | default | meaning |
|---|---|---|
| `--port` | 8878 | listens on `127.0.0.1:<port>` only |
| `--token` | `test-token` | the bearer token `/items/*` requires (`Authorization: Bearer …`) |
| `--api-key` | same as `--token` | the `X-Api-Key` value the keyed read endpoints accept |
| `--oauth-client` | `gw13-client:gw13-secret` | the client-credentials pair `/token` exchanges |
| `--cc-polls`, `--cc-retry-after` | 2, 1.0 | the Cloud Control stand-in's pacing (the Smithy adapter) |
| `--log` | off | one line per request on stderr, so you can watch a gate or an `orchestrate` arrive |

State is in memory and names are client-chosen, so a run replays byte-identically; restart the
process for a clean slate (a live gate that creates an item will find it already there next time
— gate against a fresh instance).

## What it serves, and which description describes it

| surface | endpoints | description |
|---|---|---|
| the item store (bearer auth) | `PUT/GET/DELETE /items/{name}`, `PUT /items/{name}/note` (404 unless the item exists), `GET /health` | [`../nl-ingest-openapi/examples/item-store.openapi.json`](../nl-ingest-openapi/examples/item-store.openapi.json) |
| query/header/api-key reads | `GET /search?q=&limit=`, `GET /version` (needs `X-Client-Id`), `POST /upload` (multipart) | [`../nl-ingest-openapi/examples/search-service.openapi.json`](../nl-ingest-openapi/examples/search-service.openapi.json) |
| OAuth2 client credentials | `POST /token`, `GET /reports/summary` | the adapter tests' inline descriptions |
| GraphQL | `POST`/`GET /graphql`, introspection included | [`../nl-ingest-graphql/examples/item-store.graphql.json`](../nl-ingest-graphql/examples/item-store.graphql.json) |
| JSON-RPC (awsJson) | `POST /rpc` with `X-Amz-Target`, plus a Cloud Control stand-in | the Smithy adapter's tests |

The one-minute OpenAPI run, from the repository root:

```
python3 tooling/fake-service/fake_service.py --port 18878 --log &
python3 tooling/nl-ingest-openapi/openapi_ingest.py tooling/nl-ingest-openapi/examples/item-store.openapi.json \
    --out out --secret-name api_token --verify-against http://127.0.0.1:18878 --token test-token
```

Every record certifies, runs live once (the trace is attached), and replays offline; the
closing `summary:` line counts what is ready to publish. Point `--verify-against` at a port where
nothing listens and every record fails its gate and moves to `out/failed/` — the same run shows
both halves of the gate. The module docstring at the top of `fake_service.py` has the full
endpoint-by-endpoint contract, including the deliberate 400s on unencoded request targets.
