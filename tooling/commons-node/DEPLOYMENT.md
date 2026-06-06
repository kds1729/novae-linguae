# Production deployment & cost strategy (Milestone 2 design)

**Status: design — not yet built.** The runnable PoC stack is [`docker-compose.yml`](docker-compose.yml)
(Postgres+pgvector, Redis, an embeddings model server; Django on the host). This document records the
*production* topology and — importantly — how to keep the bill bounded. It is the plan for Milestone 2.

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

## Milestone 2 build list

- `Dockerfile` + `docker-compose.prod.yml`: web (gunicorn), worker (Celery on Redis), proxy (Caddy/nginx
  with TLS + gzip + rate limits).
- **Egress-budget governor** middleware (config-driven byte budget → throttle/`503`) and per-peer `sync`
  bandwidth caps.
- Postgres **GIN-indexed** in-database typed query (today `/v0/query` array predicates apply in Python
  over a bounded scan).
- **Replication worker** (polls peers' `GET /v0/sync` and mirrors verified records).
- CDN deployment guide for `resolve`.

See also [`../../spec/resilience.md`](../../spec/resilience.md) for the availability/anti-sabotage design
(Arca, seed bundles, the `.nlb` bundle format, censorship-resistant bootstrap).
