# The commons (v0.2)

## Purpose

The commons is the shared, content-addressed store the whole project hangs from. Principle 4 — *the
author's job is to assemble, not to write* — is only real if there is somewhere to **publish**
artifacts and somewhere to **discover** them. The ingestion adapters
([`tooling/ingest-*`](../tooling/)) produce function records; without a commons those records have
nowhere to go. This document specifies what a commons node does and the protocol it speaks, so that
**any** node — backed by Postgres, Redis, a flat directory, or IPFS — interoperates with any other.

This is a **specification of a protocol, not of a server.** A node's storage engine is its own
private business. What is normative is the content-addressed, self-verifying, federatable interface
below.

## What the commons is — and is not

The commons **is** a content-addressed object store with a discovery layer over it:

- **Publish** a record → it is stored under its hash.
- **Resolve** a hash → get the record back.
- **Discover** records by structured query (exact) or semantic similarity (best-effort).
- **Replicate** between nodes so no single node is the commons.

The commons **is not** a ledger, a database of record, or an authority:

- **No consensus, no global order.** Content-addressing already gives immutability and dedup; nodes
  never need to agree on an ordering of records. (This is the core reason a blockchain is the wrong
  primitive here — you would pay for consensus you do not need.)
- **No naming authority.** Identity is the hash (principle 2); `name_hints` are hints with no
  semantic weight and no uniqueness.
- **No central authority and no gatekeeping** (principle 7). There is no approval queue, no
  identity-based exclusion, and no content moderation *in the protocol*. The only thing a node
  checks on ingest is that a record verifies (below). Filtering is a local, endpoint decision.

## Design properties (inherited from the principles)

| Property | Source | Consequence |
|----------|--------|-------------|
| Content-addressed | P2 | The store is a map from `<prefix>_<hash>` to bytes. Get/put by hash. |
| Self-verifying | P1, P3 | A client recomputes the hash and checks the schema and signatures **locally**; it never has to trust the node. |
| Monotonic | P2 | Append-only. Records are never overwritten or deleted; a new version is a new hash. |
| Open | P7 | Anyone may publish; anyone may run a node; anyone may mirror. No upstream party can interpose. |
| Federatable | P7 | Nodes replicate by pulling hashes and records from one another. Absence on one node never means non-existence. |

## Data model — what is stored

Every object is identified by its content-address `(fn|expr|type|proof|msg)_<64-hex>` (see
[`canonical-serialization.md`](canonical-serialization.md)). Two broad classes, with different
durability expectations:

- **Nova Lingua artifacts** — `fn_` function records, `expr_` bodies, `type_` types, `proof_`
  certificates. These are the **durable** commons: small, immutable, kept forever (the corpus only
  grows).
- **Nova Locutio messages** — `msg_` speech acts. Mostly **ephemeral** coordination traffic. A node
  MAY keep them only transiently (e.g. for delivery, with a TTL) and persist a message durably only
  when a conversation needs durable history. They are content-addressed and verifiable the same way;
  they simply need not be retained forever.

Records are small and scannable by design; **bodies** (`expr_`) are stored and fetched separately
(a function record carries `body_hash`, not the body), so discovery can scan metadata without
pulling code.

## The store is untrusted: verify-on-read

The single most important property: **a commons node is untrusted infrastructure.** It cannot forge
a record (the bytes would not hash to the requested address) and cannot fake provenance (the
signature would not verify). It can only *serve*, *omit*, or *decline to mirror* — none of which lets
it lie about content.

Therefore every client MUST verify what it resolves:

1. Recompute the content-address from the returned bytes per `canonical-serialization.md`; it MUST
   equal the requested hash.
2. Validate the record against its pinned `schema_version`.
3. For messages, verify the Ed25519 signature against the key in the `from` DID.

A node returns the record bytes in any formatting; the client re-canonicalizes before hashing, so
verification is formatting-independent.

## Operations (the protocol)

| Operation | Purpose |
|-----------|---------|
| **publish** | Store a record under its hash, after verifying it. Idempotent. |
| **resolve** | Fetch a record by hash. |
| **exists** | Cheap presence check by hash. |
| **query** | Exact, structured discovery over record fields (effects, intent tags, signature shape, …). |
| **search** | Best-effort semantic discovery by embedding similarity. |
| **sync** | Replication feed: hashes added since a cursor, for mirroring. |
| **info** | Node metadata: protocol version, accepted schema versions, embedding model, peers. |

## HTTP API (reference binding)

A node SHOULD expose the protocol over HTTP as below. Paths are under a version prefix (`/v0/`). All
request and response bodies are UTF-8 JSON. A non-HTTP binding (e.g. over Nova Locutio messages
themselves) MAY exist; the operations are what is normative.

### `POST /v0/records` — publish

Body: a single record (function record, body, type, proof, or message) as JSON.

The node verifies (§"Verification on ingest"). On success it stores the record under its hash and
returns the hash. Publishing the same content again is a no-op (idempotent).

**Bare body expressions are self-addressing.** A body expression carries no embedded `hash` field —
the whole expression *is* the hashed content (spec/canonical-serialization.md) — so a node MUST accept
a hashless artifact whose top-level `kind` is a body-expression kind (`var`/`lit`/`app`/`let`/
`lambda`/`case`/`field`), compute its `expr_…` content address itself after schema validation, store
it under that address, and return the computed address. This is what lets a record's `body_hash`
resolve on the same node that serves the record — the precondition for a *remote* agent loop.

```
201 Created   { "hash": "fn_3a9b…", "stored": true }      # newly stored
200 OK        { "hash": "fn_3a9b…", "stored": false }     # already present
422 Unprocessable Entity  { "error": "hash_mismatch" | "schema_invalid" | "signature_invalid",
                            "detail": "…" }
400 Bad Request           { "error": "malformed_json", "detail": "…" }
```

A node MUST NOT reject a verifying record on the basis of who published it or what it says
(principle 7). A node MAY apply a **local** mirroring policy (e.g. decline to store payloads above a
size, or rate-limit a noisy peer); such filtering is an endpoint choice and MUST NOT be presented as
the record being invalid.

### `GET /v0/records/{hash}` — resolve

Returns the record JSON. The client re-hashes and verifies (§"verify-on-read").

```
200 OK   <record JSON>
404 Not Found   { "error": "absent" }    # absent *here* — may exist on another node
```

### `HEAD /v0/records/{hash}` — exists

`200` if present, `404` if absent. No body.

### `GET /v0/records/{hash}/certifications` — certifications about a function

Returns the signed **certification** records (`spec/certification.schema.json`, from `certify --sign`) whose
`subject` is this function's address — the network face of trust-delegation. Certifications are published
through the ordinary `POST /v0/records` verify-then-store gate (their `cert_` hash and Ed25519 signature are
checked like any signed artifact) and are content-addressed like any record. The node **stores and serves,
but does not judge** (principle 7): the client verifies each certification and decides, under its *own* local
policy, whether any certifier is trusted (`nl-validator certified`). `?certified=true` returns only positive
certifications; by default all are returned (a `certified: false` record is served too — transparency).

```
200 OK   { "subject": "fn_…", "certifications": [ <signed cert>, … ], "count": 1 }
```

### `GET /v0/records/{hash}/attestations` — eval attestations about a weights record

The weights counterpart of `certifications` ([`weights.md`](weights.md) rung 3): returns the signed
**eval attestation** records ([`eval-attestation.schema.json`](eval-attestation.schema.json), from
`attest-weights --sign`) whose `subject` is this `wgt_…` address — a certifier's measured-capability
statement about weights whose bytes the commons does not hold. Published through the ordinary
verify-then-store gate (`evl_` hash + Ed25519 signature); the node stores and serves, but does not
judge — the client verifies each attestation and decides under its own policy
(`nl-validator certified --subject wgt_…`).

```
200 OK   { "subject": "wgt_…", "attestations": [ <signed attestation>, … ], "count": 1 }
```

### `GET /v0/records/{hash}/equivalences` — equivalence claims about a function

Returns the signed `assert` messages whose claim (kind `equivalent`,
[`claim-expression.schema.json`](claim-expression.schema.json)) names this `fn_…` address as either
endpoint — the discovery face of "semantic equivalence vs hash equivalence": which *other*
addresses are claimed to compute the same function. Each claim is **objective and re-checkable**
(`nl-validator verify-claim <msg> --node <url>` resolves both bodies and re-proves — equal normal
forms, else the equivalence prover), so the node stores and serves but does not judge; a consumer
re-proves locally or prices the signer's testimony under its own policy. `assert-equivalent`
creates and publishes the claims; the verified agent loop uses the resulting `equivalent-to`
attestation edges (and its own local normal-form recomputation) to collapse equivalent discovery
candidates into one class ([`agent-loop.md`](agent-loop.md), the `collapse` step).

```
200 OK   { "subject": "fn_…", "equivalences": [ <signed assert>, … ], "count": 1 }
```

### `GET /v0/blobs/{sha256}` — binary blobs, by content hash

Serves opaque binary bytes keyed by their sha256 — adapter weights ([`weights.md`](weights.md)) and
large example values (a function record's `examples[].result_blob`: the JCS-canonical value-expression
bytes of an expected result too big to inline, e.g. a multi-MB observed document).
**Deliberately gate-free and outside the record store**: blobs are not records — the referencing
record's sha256 (a weights record's `files[].sha256`, an example's `result_blob.sha256`) is the
security boundary, so any host (including a hostile mirror) is safe to fetch
from; the client hashes the download and rejects a mismatch. The URL shape is the contract, not the
implementation — a static file server or CDN may front or replace the node's view; `urls[]` entries in
a weights record are advisory hints, primary-first by convention. Content-addressed and immutable
(cache-forever). A node populates its blob store out-of-band (the reference node: `manage.py addblob`).

```
200 OK   <application/octet-stream>          404  { "error": "absent" }
```

### `POST /v0/query` — typed discovery (exact, portable)

Body: a structured filter. All fields are optional and combine with AND.

```json
{
  "kind": "function-record",
  "schema_version": "0.2.0",
  "effects": { "subset_of": ["alloc"] },
  "capabilities": { "none": true },
  "intent_tags": { "all": ["transform"], "any": ["elementwise"] },
  "terminates": ["always", "conditional"],
  "name_hint_prefix": "map",
  "type_contains": "List",
  "type_pattern": { "kind": "fn",
                    "params": [ { "kind": "builtin", "name": "string" } ],
                    "result": { "kind": "head", "names": ["Maybe"] } },
  "limit": 100,
  "cursor": "…",
  "token_budget": 4000
}
```

Returns content-addresses (the client resolves and verifies the ones it wants). `cursor` paginates.

```json
{ "results": ["fn_3a9b…", "fn_048a…"], "cursor": "…", "complete": false }
```

Typed query is **exact and node-portable**: it is computed from fields that are part of the record,
so any correct node returns the same set for the same corpus (modulo what each node holds). Use
`?include=record` to inline the records in the response as a fetch optimization (still verify them).

**Structured type matching (`type_pattern`).** `type_contains` is a substring hint over the type's
*rendering*; `type_pattern` matches the structured v0.2 type AST
([`type-expression.schema.json`](type-expression.schema.json)) by **unification** — a Hoogle-style
search that is exact and node-portable like the rest of typed query. The pattern grammar is the
type-expression grammar plus three pattern-only forms:

- `{"kind": "any"}` — wildcard, matches any type;
- `{"kind": "any_of", "types": […]}` — disjunction (e.g. a caller who accepts `int` or `nat` says so);
- `{"kind": "head", "names": […]}` — the type's head constructor is one of the named builtins
  (matches a bare `builtin` or the ctor of an `apply` — "returns *some* `Maybe`").

A `var` in the *pattern* is a named wildcard with consistency (`{a} -> {a}` finds
`forall b. b -> b`, not `int -> string`); a `var` in the *record's* type is a unification variable
(the record's own polymorphism — it matches any pattern subtree, consistently). `forall` is
stripped on both sides (rank-1). Builtin names match exactly (`int` does not match `nat`);
structural forms (`fn`/`apply`/`tuple`/`record`/`sum`/`ref`) match structurally. Only records with
a structured type participate: a v0.1 string-typed record never matches a `type_pattern`.

This is the **discovery-precision** lever found at ingestion scale: an agent loop's application
carries its argument and result sorts *in the query* (`orchestrate --verify --node` sends them
automatically), so the node's page is already argument-shaped BEFORE the page cap or token budget
trims it — instead of a broad intent page that may have truncated the right candidate away. The
receiver's local signature filter still confirms every fetched candidate; the pattern narrows,
it never decides.

**Discovery cost.** Resolving every candidate to a full record — body, examples, properties, proof
certificates — just to read its signature is the dominant context cost of "assemble, don't write" at
scale. `?include=summary` returns a **compact projection** instead: the decision fields only — `type`,
`effects`, `capabilities`, `intent_tags`, `terminates`, `complexity`, `certified`, `name_hints`,
`body_hash` — so a client ranks and prunes a whole candidate set in one round-trip, then resolves only
the finalists. The summary is derived from record fields (not heuristic), and each item carries its
`hash`, so a client still resolves + verifies before use.

```json
{ "results": [ { "hash": "fn_3a9b…", "kind": "function-record",
                 "type": "forall a b. (a -> b) -> List a -> List b",
                 "effects": [], "intent_tags": ["transform", "elementwise"],
                 "terminates": "always", "complexity": "O(n)" }, … ],
  "cursor": "…", "complete": false }
```

A `"token_budget": N` in the filter caps the summary response by **estimated token cost** rather than by
count — the honest discovery-cost cap, since a client's constraint is its context window, not a result
count, and summaries vary in size (a long type string and many intent tags cost more than a bare scalar).
The node greedily keeps summaries (in `id` or, with `?rank=relevance`, in ranked order) until the next
would overrun the budget, and reports the spend:

```json
{ "results": [ … ], "cursor": "…", "complete": false,
  "budget": { "token_budget": 4000, "tokens_estimated": 3970, "returned": 42, "more": true } }
```

`more` is true when more results matched than fit, and the `cursor` continues past the last *included*
record, so the next page resumes exactly where the budget cut off. The top result is always returned even
if it alone exceeds the budget (so a small budget still yields the best candidate; `tokens_estimated` then
reports the overrun). The estimate is tokenizer-free (canonical-JSON length over a fixed chars-per-token
factor) and node-local, so it is a budgeting aid, not an exact count; it applies only to `?include=summary`
(uniform-size hashes and heavy full records are not what a context window is spent on).

**Relevance ranking.** Every hit satisfies the filter equally, so the default `id` order discards a real
signal: how well each hit fits the filter's *soft* preferences. `?rank=relevance` orders the matched set
by a node-local score — the count of requested `intent_tags.any` a record carries (an on-target match
dominates), the primacy of a `name_hint_prefix` match (a record's primary name outranks an alias), and a
small boost for a `certified` record — so the best candidates surface first. Ranking re-orders the exact
set (it never changes membership), and because it re-orders it returns the single best-`limit` page rather
than an `id`-cursor feed; it is heuristic and node-local (like `search`), not part of the portable
guarantee. Combine with `?include=summary` to rank *and* project in one round-trip.

### `POST /v0/search` — semantic discovery (best-effort, node-local)

Body: a free-text query or a "more like this" target, an optional typed `filter`, and `k`.

```json
{ "query": "map a function over a list preserving length", "k": 20,
  "filter": { "effects": { "none": true } } }
```

or

```json
{ "like": "fn_3a9b…", "k": 20 }
```

Returns ranked addresses with scores:

```json
{ "results": [ { "hash": "fn_048a…", "score": 0.91 }, … ],
  "model": "<embedding-model-id>" }
```

`?include=summary` folds the same compact projection as `query` into each ranked hit (the `score` is
preserved), so a client ranks *and* judges candidates in a single round-trip. A body `"token_budget": N`
caps those summaries by estimated token cost exactly as on `query` — the highest-similarity hits that fit
the budget are kept, and the same `budget` report is returned (there is no `cursor`: `search` is a ranked
view, not a paged feed).

Unlike `query`, `search` is **heuristic and node-local**: it depends on the node's embedding model
(reported in `model` and in `/v0/info`). Two nodes MAY rank differently. Semantic search is a
discovery aid; the content-addressed guarantee applies only after you resolve and verify a result.

### `GET /v0/sync?since={cursor}&limit={n}` — replication feed

Returns the content-addresses stored since an opaque, node-local, monotonic `cursor` (a sequence
position), plus the next cursor. A mirror polls this and resolves any hashes it lacks. This is how
the commons federates without any node being authoritative.

```json
{ "hashes": ["fn_3a9b…", "msg_e7a2…"], "cursor": "…", "complete": false }
```

### Seed bundles (`.nlb`) — out-of-band federation

Where `sync` federates node-to-node over HTTP, a **seed bundle** federates over *anything* — an HTTP
mirror, IPFS, BitTorrent, a git repo, email, physical media — for cold-start, disaster recovery, and
publishing. A `.nlb` ("Nova Lingua Bundle", format id `nlb/1`) is a **gzipped tar** containing exactly:

```
manifest.json    { format_version, count, schema_versions[], bundle_digest, source?, producer?, signature? }
records.jsonl    one content-addressed record per line, sorted by hash
```

The manifest is specified by [`bundle.schema.json`](bundle.schema.json). A bundle is **deterministic**
(records sorted by hash, manifest keys sorted, fixed tar/gzip mtime) so the same record set always
produces identical bytes and bundles dedupe and diff cleanly. `bundle_digest` (BLAKE2b-256 over the
sorted record-hash set) is a cheap whole-payload integrity pre-check on read.

A bundle is ingested through **exactly the same verify-then-store gate as `POST /v0/records`** — every
record is re-checked by hash (and messages by signature) — so the **producer is untrusted**: a bundle can
be *withheld* but never *poisoned*, and a verified export from a Postgres node restores cleanly into a
fresh zero-dependency SQLite node. The manifest MAY carry an Ed25519 `signature` (with the signer's
`producer` DID) over the canonical manifest minus the `signature` field; since the manifest carries
`bundle_digest`, that signature transitively attests to the record set. It is **advisory provenance
only** ("this bundle came from `github.com/org/lib@v1.2.3`"), never an admission gate (principle 7) —
`bundle_digest` and the manifest signature are integrity/provenance, and per-record hash verification on
ingest is the sole security boundary. One format serves two jobs: the publishing/interchange artifact any
project ships like a wheel or crate, and the seed/disaster-recovery archive. The reference node
implements export/import (`exportbundle`/`loadbundle`); the full resilience strategy — bundles plus a
pluggable censorship-resistant bootstrap — is in [`resilience.md`](resilience.md).

### `GET /v0/info` — node metadata

```json
{
  "protocol": "v0",
  "schema_versions": ["0.1.0", "0.2.0"],
  "kinds": ["function-record", "body", "type", "proof", "message"],
  "embedding_model": "<id or null>",
  "record_count": 1234567,
  "peers": ["https://commons.example.org", "…"],
  "retains_messages": "ttl:86400"
}
```

`peers` is a hint list for replication/bootstrap; it carries no authority.

## Verification on ingest (the only gate)

On `publish`, a node MUST, and on `resolve` a client MUST:

1. **Hash check** — recompute `(fn|expr|type|proof|msg)_<hash>` per `canonical-serialization.md`
   (strip `hash`; for messages also strip `signature`); it MUST equal the address.
2. **Schema check** — the record MUST validate against the schema named by its `schema_version`.
   `additionalProperties: false` means an invalid record cannot produce a meaningful hash, so an
   invalid record is rejected, not stored.
3. **Signature check (messages)** — the Ed25519 signature MUST verify against the `from` DID's key.

The reference validator [`tooling/validator/`](../tooling/validator/) performs exactly these checks
(`nl-validator verify`); a node SHOULD reuse it (or an equivalent that agrees byte-for-byte).

This is the *entire* admission policy. It is mechanical, not editorial. There is no step where a
node decides a record is unwelcome on grounds other than "it does not verify."

## Federation: mirrors, replication, no authority

- A node replicates from a peer by polling `GET /v0/sync` and resolving unknown hashes, verifying
  each. Because records are self-verifying, **you can replicate from anyone** — a malicious peer can
  withhold records but cannot inject false ones.
- **Blobs replicate alongside records.** A mirrored record whose content lives partly in the blob
  store — a by-address example value (`examples[].result_blob`), a weights manifest (`files[]`) —
  is only *checkable* if the replica also holds those bytes, so a replicating node SHOULD pull the
  blobs its mirrored records reference from the peer's `/v0/blobs` store, verifying each download
  against the sha256 it was requested by (same untrusted-peer boundary: lying bytes are discarded,
  never stored under the address). The reference node does this self-healingly — no cursor;
  anything still missing is re-counted next run.
- There is no canonical node. A bootstrap peer list (in `/v0/info` or out-of-band) seeds discovery;
  it is convenience, not authority. The commons is the *union* of what nodes hold.
- **Absence is not non-existence.** A `404` means "not here"; the record may live on another node.
  Clients SHOULD be able to consult more than one node.
- Trust *groups* and curated subsets (federations that mirror only vetted records) are expected and
  happen **above** the protocol (per [`trust-model.md`](trust-model.md)), never within it. Curation
  is a local filter, not a protocol gate.

## Bodies vs records

Function records carry `body_hash`, not the body. A node stores bodies (`expr_`) as ordinary
content-addressed objects, and MAY keep them in cheaper blob storage than the metadata index since
they are not scanned by `query`. `resolve` works identically for an `expr_` address.

## Principle 7: what the protocol forbids

To keep the guarantee structural rather than aspirational, a conforming node MUST NOT:

- maintain a protocol-level allowlist/denylist of publisher identities;
- require approval, payment, or registration to publish a verifying record;
- refuse to *serve* a verifying record it holds on the basis of its content or author;
- present a local mirroring decision as the record being invalid.

A node MAY (these are local, endpoint choices, and cannot be imposed by any upstream party):

- decline to *mirror* particular records or peers;
- rate-limit or size-cap to protect itself;
- serve a curated subset to its own clients.

The binding line, as in principle 7: **no central authority can interpose itself; endpoints decide
for themselves.** A single node you run is fine precisely because it is not the only possible node
and cannot suppress content that another node will serve.

## Security and abuse considerations

- **Spam / junk.** The commons accepts any *well-formed, verifying* record, so an adversary can
  publish large volumes of valid-but-useless records. This is by design (no editorial gate); the
  defenses are local and above-protocol: per-peer rate limits, size caps, and **quality/reputation
  filtering at the endpoint** (trust-model.md). Quality is a discovery-layer concern, not an
  admission gate.
- **Confidentiality.** Encrypted Nova Locutio payloads ([`encryption.md`](encryption.md)) are
  opaque envelopes to the commons — a node stores and serves ciphertext it cannot read. The
  `recipients` list is visible metadata (a known v0.2 limitation).
- **Untrusted node.** Covered above — a node cannot forge or alter records; the worst it can do is
  omit, which federation routes around.
- **Hash collisions.** BLAKE3-256; second-preimage resistance makes address collisions infeasible.
- **Resource exhaustion.** Blobs dominate storage on any node that hosts them (a single adapter
  weights file outweighs the whole record index); bodies and embeddings dominate the record store
  itself. A node bounds its own footprint via local policy and tiering. None of this is
  protocol-visible.

## Engine-agnostic: the reference node

The first reference node is a Django service (built and deployed — **Arca**, live at
https://nl.1105software.com; see [`../tooling/commons-node/`](../tooling/commons-node/)):

- **Postgres** as the durable system of record — JSONB for the raw record plus extracted, indexed
  columns (effects, capabilities, intent_tags, terminates, complexity, normalized signature) for
  `query`, and **pgvector** for `search`. Disk-first, so it scales past RAM as the corpus grows.
- **Redis** as the hot/ephemeral tier — read-through cache of hot records, in-flight `msg_` delivery
  with TTL, a fast "exists?" set, a job broker for async embedding/verification/replication, and
  pub/sub for `sync` notifications.

None of that is normative. A node backed entirely by Redis, by flat files, or by IPFS is equally
conformant if it speaks the protocol above. The engine choice MUST NOT leak into the wire contract.

## Open questions (v0.3+, not blockers)

1. **Authenticated `sync`/anti-entropy** — efficient set reconciliation (e.g. Merkle/IBLT) instead
   of cursor polling, for large mirrors.
2. **Provenance anchoring** — *optionally* publishing periodic Merkle roots of a node's corpus to an
   external append-only log (including, if desired, a public blockchain) as a tamper-evident
   timestamp. This is an add-on for auditability, never the store itself.
3. **Embedding portability** — a recommended embedding model (or a way to publish embeddings as
   `proof`-like derived artifacts) so semantic search is more comparable across nodes.
4. **Body storage tiering** — the blob/CDN layer itself now exists (`GET /v0/blobs/{sha256}`, above:
   gate-free, CDN-frontable, carrying weights and by-address example values). What remains open is
   the narrower question of routing large `expr_` *bodies* through it — bodies are still ordinary
   records in the metadata index.
5. ~~**Query over structured ASTs** — richer `type_contains` matching against the v0.2 type AST
   (unification, subtyping) rather than substring hints.~~ **RESOLVED** — `type_pattern` (above)
   matches the structured type AST by unification, with pattern wildcards, disjunction, and
   head-constructor sets; `orchestrate --verify --node` sends the application's argument/result
   sorts as a pattern automatically. Subtyping is not part of the type system, so unification is
   the whole of the question; what remains open is only matching *through* a `ref` into the
   commons (the node matches the reference by address, it does not resolve and match its
   definition).
