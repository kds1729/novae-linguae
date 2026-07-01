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
| `POST /v0/records` — publish (verify-then-store, idempotent) — records, messages, **and signed certifications** | ✅ |
| `GET /v0/records/{hash}` — resolve · `HEAD` — exists | ✅ |
| `GET /v0/records/{hash}/certifications` — the signed certifications about a function | ✅ |
| `POST /v0/query` — typed (exact) discovery | ✅ |
| `GET /v0/sync` — replication feed (cursor) | ✅ |
| `GET /v0/info` — node metadata | ✅ |
| `POST /v0/search` — semantic discovery (stdlib lexical embedder) | ✅ |
| `POST /v0/prove` — prove a record's properties (best-effort, SMT-backed) | ✅ |
| `POST /v0/equiv` — prove two functions equivalent (best-effort, SMT-backed) | ✅ |

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

# semantic search (free text, or "more like this" by hash)
curl -X POST http://127.0.0.1:8000/v0/search -H 'content-type: application/json' \
     -d '{"query":"map a function over each element of a list","k":5}'
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

## Semantic search (`POST /v0/search`)

Where `query` is exact/typed, `search` ranks records by *meaning* via embedding cosine similarity —
the discovery aid that makes principle 4 ("assemble, don't write") usable. It is **best-effort and
node-local** (per [`spec/commons.md`](../../spec/commons.md)): the node advertises its embedding model
and two nodes MAY rank differently. The content-addressed guarantee applies only after you resolve
and verify a result.

```bash
# free-text query, optional typed filter (same filter language as /v0/query)
curl -X POST http://127.0.0.1:8000/v0/search -H 'content-type: application/json' \
     -d '{"query":"encode and decode bytes","k":5,"filter":{"kind":"function-record"}}'

# "more like this" by hash
curl -X POST http://127.0.0.1:8000/v0/search -H 'content-type: application/json' \
     -d '{"like":"fn_…","k":10}'
# -> {"results":[{"hash":"fn_…","score":0.87}, …], "model":"lexical-hashing-v0"}
```

**Embedding model.** This node ships a stdlib-only, **deterministic lexical** embedder
(`lexical-hashing-v0`, in [`commons/embedding.py`](commons/embedding.py)): it builds an L2-normalized
vector from a record's own tokens (`name_hints`, type string, `intent_tags`, refinement/property
expressions; speech-act kind + body for messages) via the hashing trick — no model download, no
network, reproducible byte-for-byte. It captures lexical/structural overlap, not deep meaning; that
is the explicit trade for zero dependencies and determinism. The active model id is reported in
`/v0/info` (`embedding_model`) and in every search response (`model`).

**Pluggable.** `get_embedder()` is the seam: a neural backend (sentence-transformers, an API client,
…) implements the `Embedder` interface and is selected by the `COMMONS_EMBEDDER` env var, with **no
protocol change**. Vectors are only ranked against others from the same model id, so mixed-model
corpora are safe.

**Backfill.** Records ingested before search (or after a model change) get vectors via:

```bash
python3 manage.py embedrecords          # embed rows missing the current model's vector
python3 manage.py embedrecords --all    # re-embed everything
```

## Proof service (`POST /v0/prove`)

Proves a record's `forall` `properties[]` over the **unbounded** domain by shelling out to the
reference validator's `prove` (the same SMT + structural-induction + lemma-discovery engine the CLI
uses). Each property comes back `PROVED` / `REFUTED` (with a counterexample) / `UNKNOWN` / `NOT-PROVED`
/ `UNSUPPORTED`. Like search this is **best-effort and node-local** — it is *not* part of the admission
decision (principle 7): proving says nothing about whether a record is stored.

Target it two ways — a record stored on this node, or an inline record (plus an optional `body` AST,
needed only for properties that reference `self`, since bodies are not themselves stored):

```bash
# prove a stored record's properties by content-address
curl -X POST http://127.0.0.1:8000/v0/prove -H 'content-type: application/json' \
  -d '{"hash": "fn_…"}'

# prove an inline record (first-order law: holds for all integers)
curl -X POST http://127.0.0.1:8000/v0/prove -H 'content-type: application/json' -d '{"record": {
  "schema_version": "0.2.0",
  "properties": [{"name": "doubling", "expr": {"kind": "forall", "vars": ["n"], "body":
    {"kind": "app", "op": "eq", "args": [
      {"kind": "app", "op": "add", "args": [{"kind": "var", "name": "n"}, {"kind": "var", "name": "n"}]},
      {"kind": "app", "op": "mul", "args": [{"kind": "lit", "value": {"kind": "int", "value": 2}}, {"kind": "var", "name": "n"}]}]}}}]}}'
# → {"solver": "z3", "results": [{"name": "doubling", "status": "PROVED", "detail": "…"}], "summary": {"proved": 1}}
```

**Needs a solver.** It invokes `COMMONS_SOLVER` (default `z3`); without one on PATH every property
reports `NO-SOLVER`. `/v0/info` advertises `prove.solver` and `prove.available` so a client can tell
before asking. Work is bounded by `COMMONS_PROVE_TIMEOUT` (default 60 s) and `COMMONS_PROVE_MAX_PROPERTIES`
(default 32). The production image installs `z3`. In the production stack the public Caddy edge also
**rate-limits** the solver-backed endpoints (`/v0/prove`, `/v0/equiv`) strictly per client IP (default
10/min, vs. 300/min for everything else), via the [`caddy-ratelimit`](Caddy.Dockerfile) plugin — see the
`rate_limit` zones in [`Caddyfile`](Caddyfile), tunable with `ARCA_PROVE_RATE` / `ARCA_GENERAL_RATE`.

## Equivalence service (`POST /v0/equiv`)

Decides whether two functions are **semantically equivalent** — `∀x. f(x) = g(x)` over the unbounded
domain — via the validator's `equiv` (reusing the prove engine). The operable form of "semantic
equivalence vs hash equivalence": two records can be hash-different yet behaviorally identical. Takes two
**inline** body-expression ASTs (bodies aren't stored, so there's no by-hash form); returns
`{verdict: equivalent|distinct|unknown|unsupported, detail, solver}`. Scope follows the validator's
`equiv`: any arity ≥ 1 with one side non-recursive, plus both-recursive pairs of arity ≤ 2 (induction over
the leading list parameter, drawing on the list-algebra lemma catalog when a step needs it).

```bash
# \n -> add(n,n)  ≡  \m -> mul(2,m)   → {"verdict": "equivalent", ...}
curl -X POST http://127.0.0.1:8000/v0/equiv -H 'content-type: application/json' -d '{
  "f": {"kind":"lambda","params":[{"name":"n"}],"body":{"kind":"app","fn":{"kind":"var","name":"add"},"args":[{"kind":"var","name":"n"},{"kind":"var","name":"n"}]}},
  "g": {"kind":"lambda","params":[{"name":"m"}],"body":{"kind":"app","fn":{"kind":"var","name":"mul"},"args":[{"kind":"lit","value":{"kind":"int","value":2}},{"kind":"var","name":"m"}]}}}'
```

## Certifications (`POST /v0/records`, `GET /v0/records/{hash}/certifications`)

A **certification** ([`spec/certification.schema.json`](../../spec/certification.schema.json), produced by
`nl-validator certify --sign`) is a signed, content-addressed record (`cert_…`) attesting that a function
passed every "verified by default" check. The node treats it as a first-class artifact: it ingests through
the **same verify-then-store gate** as everything else (`nl-validator verify` checks the `cert_` hash and the
Ed25519 signature; `nl-validator validate` checks the schema), resolves back byte-for-byte, and — the point —
serves the certifications **about a function** by its address:

```bash
# a certifier publishes a signed certification (same endpoint as any record)
nl-validator certify spec/examples/reverse.json --body spec/examples/body-reverse.json \
    --sign "$CERTIFIER_SEED" > cert.json
curl -X POST http://127.0.0.1:8000/v0/records -H 'content-type: application/json' --data @cert.json

# a consumer that resolved a function fetches its certifications, then decides under ITS OWN policy
curl http://127.0.0.1:8000/v0/records/<fn-hash>/certifications          # all
curl http://127.0.0.1:8000/v0/records/<fn-hash>/certifications?certified=true
# -> {"subject":"fn_…","certifications":[ <signed cert>, … ],"count":1}
```

This is the network face of **trust-delegation**: the node stores and serves signed certifications but
**does not judge** them (principle 7 — mechanical, not editorial). A client verifies each returned
certification (hash + signature) and decides whether any certifier is trusted under its *local* policy —
`nl-validator certified --policy … --attestations cert.json --subject <fn-hash>` — so it can rely on a
trusted third party's certification instead of re-running every check itself. Nothing about certification
gates *admission*: a function is stored on its own merits, and a `certified: false` record is served too
(transparency), with the trust decision left entirely to the consumer.

## Seed bundles (`.nlb`)

A portable, self-verifying archive of records for out-of-band distribution — cold-start, disaster
recovery, or shipping a project's records as a release artifact (see
[`spec/resilience.md`](../../spec/resilience.md)). An `.nlb` is a deterministic gzipped tar of
`manifest.json` + `records.jsonl`.

```bash
python3 manage.py exportbundle commons.nlb                          # all records
python3 manage.py exportbundle fns.nlb --filter '{"kind":"function-record"}'
python3 manage.py exportbundle delta.nlb --since 1200                # only records newer than cursor 1200
python3 manage.py exportbundle - --source-repo https://github.com/org/lib > lib.nlb   # to stdout

python3 manage.py exportbundle signed.nlb --sign-seed "$PUBLISHER_SEED"   # advisory provenance

python3 manage.py loadbundle commons.nlb        # verify-then-store each record (same gate as publish)
python3 manage.py loadbundle --require-signed signed.nlb   # refuse unless a VALID signature
curl -s https://mirror.example.org/lib.nlb | python3 manage.py loadbundle -
```

The producer is **untrusted**: `loadbundle` re-verifies every record by hash (and signature), so a
bundle can be withheld but not poisoned. Bundles are deterministic (same records → identical bytes),
and portable across backends — records exported from a Postgres node restore into a fresh SQLite node.

`--sign-seed` signs the manifest with the `did:nova` derived from the seed (Ed25519 over the canonical
manifest, which carries `bundle_digest`). This is **advisory provenance** — record-level verification
is still the admission gate — reported by `loadbundle` as `signed by <did> (verified)` and enforceable
with `--require-signed`.

## Censorship-resistant bootstrap

When a node can't reach the usual peers, it discovers *where the data is* from a small **signed
bootstrap descriptor** published to a "dead-drop" (see [`spec/resilience.md`](../../spec/resilience.md)).
The first channel is a signed descriptor fetched over HTTPS (or `file://`); the resolver is pluggable.

```bash
# Publish a signed descriptor pointing at peers + the latest seed bundle (host it at a well-known URL):
python3 manage.py exportbundle commons.nlb --sign-seed "$SEED"     # note the printed digest=blake2b:…
python3 manage.py makebootstrap bootstrap.json \
    --peer https://node-a.example.org \
    --bundle-hash blake2b:… --bundle-url https://mirror.example.org/commons.nlb \
    --sign-seed "$SEED"

# A stranded node recovers the commons from it (trust-but-verify), then pulls + ingests the bundle:
python3 manage.py bootstrap --from https://a.example.org/.well-known/nlb-bootstrap.json \
    --trust did:nova:… --pull
```

`--from` URLs are tried in order (fallback). `--trust` requires a valid signature by a trusted
`did:nova`; otherwise provenance is reported but advisory. `--pull` verifies the fetched bundle's
digest against the signed descriptor, then ingests through the normal verify-then-store gate — so the
whole chain is verified end to end.

A descriptor/bundle URL list can **mix channels** (blocking one doesn't sever bootstrap), chosen by
scheme — `https://`/`file://`, `ipns://<name>` (IPFS gateway), `dns://<name>` (DNS-over-HTTPS TXT,
base64 descriptor), `nostr://<relay>/<author>` (newest event), `chain://<read-endpoint>[#json.path]`
(read an on-chain pointer, then follow it). Each channel is untrusted transport — the descriptor
signature and bundle hash are the real checks.

## Configuration (env vars)

| Var | Default | Purpose |
|-----|---------|---------|
| `COMMONS_VALIDATOR` | repo's `nl-validator` release binary | verifier path |
| `COMMONS_SPEC_DIR` | repo's `spec/` | schema directory |
| `COMMONS_SOLVER` | `z3` | SMT solver for `/v0/prove` (must read SMT-LIB 2 on stdin via `-in`) |
| `COMMONS_PROVE_TIMEOUT` | `60` | wall-clock seconds per `/v0/prove` request |
| `COMMONS_PROVE_MAX_PROPERTIES` | `32` | per-call cap on properties to prove |
| `COMMONS_DB_PATH` | `./db.sqlite3` | SQLite file |
| `COMMONS_MAX_RECORD_BYTES` | 1 MiB | local size cap (a permitted endpoint policy) |
| `COMMONS_PEERS` | empty | comma-separated peer hints for future replication |
| `COMMONS_EMBEDDER` | `lexical-hashing-v0` | embedding model id; a non-lexical id selects the neural backend |
| `COMMONS_EMBEDDING_DIM` | `256` | vector dimensionality (256 lexical / 384 bge-small) — part of model identity |
| `COMMONS_EMBEDDINGS_URL` | empty | neural model server (`/embed`-compatible, e.g. TEI); required for a neural embedder |
| `COMMONS_EMBEDDINGS_TIMEOUT` | `30` | seconds for a model-server request |
| `COMMONS_EMBEDDINGS_BATCH` | `32` | inputs per model-server request (TEI's default client-batch cap) |
| `COMMONS_PG_NAME` / `_USER` / `_PASSWORD` / `_HOST` / `_PORT` | — | set any to use the Postgres backend (+ pgvector ANN) instead of SQLite |
| `COMMONS_REDIS_URL` | empty | Redis cache (`redis://…`); falls back to in-process locmem |

## Tests

```bash
python3 manage.py test commons      # 95 tests (94 on SQLite + 1 Postgres-only, auto-skipped)
```

Covers publish/idempotency, resolve, `HEAD` exists, `404` for absent, message
signature verification, **signed certifications** (publish/resolve, serve-by-subject, `?certified=true`,
tamper rejection), tamper and malformed-input rejection, typed query (intent tags, effects,
name-hint prefix, non-match exclusion, `include=record`), the `sync` feed, `info`, and the
`loadrecords` pipeline. The verify-gated tests skip if `nl-validator` isn't built; the **semantic
search** tests run regardless — embedding determinism / L2-normalization / relevance ordering,
`search` query + `like` + `filter` composition + `k` cap + error cases, `/v0/info` model id, and the
`embedrecords` backfill.

## Production-shaped stack (Postgres + pgvector + Redis + a model server)

Local dev can run the **same compute elements** as production via [`docker-compose.yml`](docker-compose.yml):
Postgres+pgvector, Redis, and an embeddings model server — with **Django on the host** for autoreload.

```bash
docker compose up -d db redis embeddings     # bring up the elements (first start downloads the model)
curl http://127.0.0.1:8080/health            # TEI ready?
cp .env.example .env                          # Postgres/Redis/neural-model env
set -a; source .env; set +a

.venv/bin/python manage.py migrate            # 0003 builds the pgvector column + HNSW index (Postgres only)
nl-ingest-py … | .venv/bin/python manage.py loadrecords    # ingest (embeds on store)
.venv/bin/python manage.py embedrecords --all              # (re)embed any existing rows
.venv/bin/python manage.py runserver
```

On Postgres, `search` ranks via pgvector's `<=>` cosine distance over an **HNSW index**
([`commons/vectorindex.py`](commons/vectorindex.py)); on SQLite it falls back to the Python cosine
scan. The portable JSON `embedding` is the source of truth on both; `embedding_vec` is the
Postgres-only ANN column kept in sync on ingest/backfill.

**This node is the cost-minimized proof of concept, not a ceiling.** Every scaling axis is a seam or
config — a future operator goes industrial with **no code change**:

| Axis | Industrial upgrade |
|---|---|
| Embedding model | `COMMONS_EMBEDDER` + the model-server URL → GPU model (TEI ships CPU *and* GPU images, same API) or a hosted API |
| Database | `COMMONS_PG_*` → managed/clustered Postgres + read replicas; HNSW scales to millions |
| CDN | `resolve` is immutable + content-addressed (`Cache-Control: immutable`) → front it with any CDN at ~100% hit rate |
| Horizontal scale | the app is stateless (state in Postgres/Redis) → N web replicas behind a load balancer |

**Egress is the only variable cost** (the VM fee is flat). Self-hosting the model means *zero
per-call network cost*; immutable caching + a CDN keep `resolve`/`sync` (the heavy endpoints) off the
origin; and rate-limit / egress-budget guards are local config (principle-7 endpoint policy), never
protocol gates. The full production layer (gunicorn web + Celery worker + a rate-limiting/egress
proxy, see below) is the next milestone.

## Deliberately deferred (later phases of `spec/commons.md`)

- **Production hardening (Milestone 2)** — a `Dockerfile` + `docker-compose.prod.yml` (Django under
  gunicorn, a Celery worker for async embed/verify/replicate, a Caddy/nginx proxy for TLS + gzip +
  rate limits), and an **egress-budget governor** (a hard ceiling on the bill). The neural embedder +
  pgvector ANN search and the Postgres/Redis backends landed in Milestone 1 (above). Full topology +
  cost-control strategy: [`DEPLOYMENT.md`](DEPLOYMENT.md).
- **Postgres typed query** — JSONB + GIN-indexed array columns so `/v0/query` array predicates run
  in-database (today they apply in Python over a bounded scan, on either backend).
- **Replication worker** — a background task that polls peers' `GET /v0/sync` and mirrors verified
  records, making the node a true mirror rather than a silo.
- **In-process verification** — porting the `nl-validator` checks into Python (nl_core hashing +
  jsonschema + PyNaCl) to avoid a subprocess per publish, as long as it agrees byte-for-byte.

## License

Dual-licensed under Apache-2.0 OR MIT, same as the rest of *Novae Linguae*.
