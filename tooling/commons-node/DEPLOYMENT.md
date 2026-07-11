# Production deployment & cost strategy (Milestone 2)

**Status: built.** The production stack is [`docker-compose.prod.yml`](docker-compose.prod.yml) +
[`Dockerfile`](Dockerfile): containerized Django under gunicorn, a Celery worker (replication + async
embedding backfill), Postgres+pgvector, Redis, and the TEI embeddings model server. The egress-budget
governor ([`commons/egress.py`](commons/egress.py)), gzip, and the Postgres GIN typed-query pushdown
([`commons/query.py`](commons/query.py) + migration `0004`) are in. TLS / ingress is intentionally left
to the host's edge (see "Running it" below) so the node coexists on a shared host. The dev PoC stack
([`docker-compose.yml`](docker-compose.yml), Django on the host) is unchanged.

## Principle: cheap PoC, no ceiling

Our node is a cost-minimized proof of concept, but **nothing in the code or protocol caps a future
operator from going industrial.** Every scaling axis is a seam or config, never hardcoded (principle 7 —
engine-agnostic; our node is one implementation, never the authority).

| Axis | Industrial upgrade — no code change |
|---|---|
| Embedding model | `COMMONS_EMBEDDER` + model-server URL → a GPU model (TEI ships CPU *and* GPU images, same API) or a hosted API |
| Database | `COMMONS_PG_*` → managed/clustered Postgres + read replicas; pgvector HNSW scales to millions |
| CDN | `resolve` is immutable + content-addressed (`Cache-Control: immutable`) → front it with any CDN at ~100% hit rate |
| Horizontal scale | the app is stateless (all state in Postgres/Redis) → N web replicas behind a load balancer |
| Cost guards | rate limits / egress budget are local config (a principle-7 endpoint policy), never protocol gates |

## Production container stack

| Container | Image / software | Role | Cost profile |
|---|---|---|---|
| **proxy** | Caddy or nginx | TLS, rate-limit (`limit_req`/`limit_conn`/`limit_rate`), gzip/brotli, cache headers | flat |
| **web** | Django under gunicorn/uvicorn | the commons node app (stateless → scale to N replicas) | flat (CPU) |
| **db** | `pgvector/pgvector:pg16` | records (JSONB) + `vector` column + HNSW ANN index | flat (disk/RAM) |
| **embeddings** | HF Text-Embeddings-Inference (CPU now; GPU image later) | text → vectors | flat |
| **redis** | redis | cache, async job broker, sync pub/sub | flat |
| **worker** | Celery / RQ / Dramatiq | async embed / verify / replicate (publish doesn't block on inference) | flat |

Local dev runs the *same elements* (minus the proxy/worker) so dev mirrors prod; Django stays on the
host for autoreload. Production containerizes Django and adds the proxy + worker.

## The bill: egress is the only variable cost

The VM fee is flat; **metered egress** (data out to the internet, ~$0.08–0.12/GB on most clouds) is the
only thing that can run away. The content-addressed design makes egress unusually easy to bound:

1. **Self-host the embedding model.** A hosted embedding *API* bills per token on every publish *and*
   every search, plus egress — the runaway path. A self-hosted model has **zero per-call network cost**;
   inference is on the flat-fee CPU. (This is the cost-decisive choice and matches the local ethos.)
2. **`sync` is the biggest egress risk, not search.** If you become a public origin, other nodes mirroring
   your whole corpus serve your data ×N. Search returns *hashes* (tiny) by default; `include=record` is
   opt-in; **embeddings never leave the node.** Bound `sync` with pagination (have) + per-peer
   bandwidth caps.
3. **Immutable CDN caching of `resolve`.** Records never change, so `Cache-Control: immutable` (already
   emitted) lets an **off-box CDN** serve repeat `resolve`s at ~100% hit rate and absorb spikes. (Note: a
   cache *on the same VM* cuts CPU/DB load but **not** metered egress — only an off-box CDN does.)
4. **Egress-budget governor.** A middleware/proxy counter that throttles or returns `503` once a
   daily/monthly byte budget is hit — a hard ceiling on the bill, degrading gracefully instead of
   surprising you.
5. **Compression.** gzip/brotli on JSON responses — typically 60–80% fewer egress bytes, nearly free.
6. **Federation = you're not obligated to be the world's origin.** Every record is self-verifying, so
   other nodes mirror and serve *their* clients. Run a deliberately modest node, cap your throughput, and
   let the mesh absorb growth — no node is forced to carry global load.

## Milestone 2 build list (status)

- ✅ `Dockerfile` (multi-stage: builds the `nl-validator` verification binary, then a slim gunicorn
  image) + `docker-compose.prod.yml` (web, Celery `worker`, db, redis, embeddings). TLS/rate-limit live
  at the host edge rather than a bundled proxy, so the node shares a host without owning :443.
- ✅ **Egress-budget governor** ([`commons/egress.py`](commons/egress.py)): a monthly byte budget in the
  cache → `503` past the cap; usage is advertised in `/v0/info` and `X-Egress-Used`. gzip is on.
- ✅ Postgres **GIN-indexed** typed query: migration `0004` adds `gin (… jsonb_path_ops)` indexes and
  `query.py` pushes `@>` containment into the DB on Postgres (the Python post-filter stays authoritative;
  SQLite is unchanged).
- ✅ **Replication worker** + **async embedding** ([`commons/tasks.py`](commons/tasks.py)): Celery beat
  runs `replicate_all` (mirror verified records from `COMMONS_PEERS`) and `embed_pending` (backfill
  embeddings, so publish never blocks on the model server). **Demonstrated against production**: a
  fresh node with `COMMONS_PEERS=<the live node>` mirrored the full store (597 records across all
  seven kinds) through the untrusted-peer gate, and both an `observed` claim (`verify-claim <msg>
  --node <replica>`) and an effectful record's trace-carried examples then verified against the
  REPLICA — a claim survives its origin node. Failure semantics (fixed by that first real mirror,
  which silently missed 12 records): an *unverifiable* record is a permanent skip (a bad peer can't
  wedge the cursor), but a *transient fetch failure* stops the run without committing the durable
  cursor past its page — the next interval retries, so the mirror converges to complete.
- CDN for `resolve`: `resolve` already emits `Cache-Control: public, max-age=…, immutable`; front it with
  any CDN at ~100% hit rate (off-box, so it also absorbs metered egress). No code change.

## Running it

On a **dedicated** host, terminate TLS in front of `web` (`127.0.0.1:8001`) with Caddy/nginx. On a
**shared** host, add a vhost to the existing edge that reverse-proxies `nl.<domain>` → `127.0.0.1:8001`.

```sh
cd tooling/commons-node
cp .env.prod.example .env            # set COMMONS_SECRET_KEY, COMMONS_PG_PASSWORD, COMMONS_ALLOWED_HOSTS
docker compose -f docker-compose.prod.yml up -d --build   # migrate runs on web start
docker compose -f docker-compose.prod.yml exec -T web \
  sh -c 'python ../ingest-common/... | python manage.py loadrecords -'   # optional: seed
curl -s http://127.0.0.1:8001/v0/info | python3 -m json.tool             # health + egress posture
```

See also [`../../spec/resilience.md`](../../spec/resilience.md) for the availability/anti-sabotage design
(Arca, seed bundles, the `.nlb` bundle format, censorship-resistant bootstrap).
