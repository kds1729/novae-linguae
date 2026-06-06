# commons-node — reference node for the commons protocol (MVP)

A local, dependency-light Django implementation of [`spec/commons.md`](../../spec/commons.md): the
content-addressed, self-verifying store where ingested records finally go. This MVP runs **100%
local** — SQLite, no Postgres, no Redis, no public endpoint — and is fully exercised by an
in-process test suite.

It is one *implementation* of the protocol, not the commons. Because records are content-addressed
and signed, this node is untrusted infrastructure: clients verify what they fetch (hash + signature)
and can run their own node and mirror. The storage engine here (SQLite) is a private detail; the
[protocol](../../spec/commons.md) is what is normative.

## What the MVP implements

| Endpoint | Status |
|----------|--------|
| `POST /v0/records` — publish (verify-then-store, idempotent) | ✅ |
| `GET /v0/records/{hash}` — resolve · `HEAD` — exists | ✅ |
| `POST /v0/query` — typed (exact) discovery | ✅ |
| `GET /v0/sync` — replication feed (cursor) | ✅ |
| `GET /v0/info` — node metadata | ✅ |
| `POST /v0/search` — semantic discovery | ⏳ deferred (needs the embedding tier) |

**Verification reuses the reference validator.** On ingest, the node shells out to `nl-validator`
to (1) `validate` the record against the schema named by its `(kind, schema_version)` and (2)
`verify` its content-address (and Ed25519 signature, for messages). This is the *only* admission
gate — mechanical, not editorial (principle 7). Build it first: `cd ../validator && cargo build --release`.

## Run it

```bash
cd tooling/commons-node
python3 -m venv .venv && source .venv/bin/activate
pip install -r requirements.txt

python manage.py migrate           # creates ./db.sqlite3 (gitignored)
python manage.py runserver         # http://127.0.0.1:8000/

# publish a record, then resolve it
curl -X POST http://127.0.0.1:8000/v0/records \
     -H 'content-type: application/json' --data @../../spec/examples/map.json
curl http://127.0.0.1:8000/v0/records/$(python3 -c "import json;print(json.load(open('../../spec/examples/map.json'))['hash'])")

# typed query
curl -X POST http://127.0.0.1:8000/v0/query -H 'content-type: application/json' \
     -d '{"intent_tags":{"any":["elementwise"]},"effects":{"none":true}}'
```

### The adapter → commons pipeline

The ingestion adapters emit one record per line, so they load straight in via the `loadrecords`
management command (same verify-then-store path as `POST /v0/records`, in-process):

```bash
python3 ../ingest-python/nl_ingest.py --module mypkg mypkg.py > /tmp/recs.jsonl
python3 manage.py loadrecords /tmp/recs.jsonl
# -> stored=N skipped=M failed=K   (rejects are the records that do not verify)
```

This is the point of the whole node: records produced by `nl-ingest{,-py,-hs,-ts}` finally have a
destination and become discoverable.

## Configuration (env vars)

| Var | Default | Purpose |
|-----|---------|---------|
| `COMMONS_VALIDATOR` | repo's `nl-validator` release binary | verifier path |
| `COMMONS_SPEC_DIR` | repo's `spec/` | schema directory |
| `COMMONS_DB_PATH` | `./db.sqlite3` | SQLite file |
| `COMMONS_MAX_RECORD_BYTES` | 1 MiB | local size cap (a permitted endpoint policy) |
| `COMMONS_PEERS` | empty | comma-separated peer hints for future replication |

## Tests

```bash
python3 manage.py test commons      # 16 tests; skips if nl-validator isn't built
```

Covers publish/idempotency, resolve, `HEAD` exists, `404` for absent, message
signature verification, tamper and malformed-input rejection, typed query (intent tags, effects,
name-hint prefix, non-match exclusion, `include=record`), the `sync` feed, `info`, and the
`loadrecords` pipeline.

## Deliberately deferred (later phases of `spec/commons.md`)

- **Semantic search** (`POST /v0/search`) — needs an embedding model + a vector index. On the
  planned Postgres backend this is `pgvector`; the node reports its model in `/v0/info`.
- **Redis tier** — read-through cache, ephemeral `msg_` delivery with TTL, a job broker for async
  embedding/verification/replication, and pub/sub for `sync` notifications.
- **Replication worker** — a background task that polls peers' `GET /v0/sync` and mirrors verified
  records, making the node a true mirror rather than a silo.
- **Postgres backend** — JSONB + GIN-indexed array columns for in-database typed query (the MVP
  applies array predicates in Python over a bounded scan). Swap is a settings change; see
  `commons_node/settings.py`.
- **In-process verification** — porting the `nl-validator` checks into Python (nl_core hashing +
  jsonschema + PyNaCl) to avoid a subprocess per publish, as long as it agrees byte-for-byte.

## License

Dual-licensed under Apache-2.0 OR MIT, same as the rest of *Novae Linguae*.
