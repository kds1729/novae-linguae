# Resilience & availability — Arca, seed bundles, and censorship-resistant bootstrap

**Status: proposed design — not yet implemented.** This records the resilience strategy for the
public commons service and the distribution formats that make it robust. It is forward-looking design,
not a normative protocol spec; the normative store/discovery protocol is [`commons.md`](commons.md).

## Why

A public commons service is a target. There is a small but non-zero chance that an effort is made to
sabotage nodes, take the service down, or block access to it. The goal here is that **no such effort
can destroy the commons or sever access to it** — only inconvenience it.

## The load-bearing property: transport is untrusted

Every artifact is **content-addressed and self-verifying** (principle 2): a record's identity *is* the
hash of its semantic content, and messages carry signatures. A consumer verifies anything it receives
by recomputing the hash (and checking signatures) — so whoever supplies a record (a public node, a
mirror, a torrent, a USB stick, even an attacker) is **untrusted**. A tampered or substituted record
simply fails verification.

The consequence is the foundation of all resilience here:

> The transport is irrelevant to correctness. The public service is only ever the *most convenient*
> transport — never a trust anchor and never a single point of failure. The commons survives as long
> as **one verified copy and one verifier exist anywhere**.

Everything below just adds more transports and more ways to find them.

## Arca

**Arca** (Latin: *ark / vault*) is the name of the reference public commons service/network — the
vessel that carries the records, and the seed bundles, through any flood. Arca runs the
[commons protocol](commons.md); it is an *implementation and a convenience*, not an authority
(principle 7). Anyone may run their own node and mirror; Arca being unavailable degrades convenience,
not survival.

## The resilience stack

| Layer | Status | What it gives |
|---|---|---|
| Content-addressed verification | **have** | every record/message verified locally; supplier untrusted |
| Federation via `sync` | **have** | nodes mirror each other; no node is authoritative |
| Seed bundles | **have** (node) | offline / out-of-band redistribution; cold-start & disaster recovery |
| Standard `.nlb` bundle format (`nlb/1`) | **have** (format + node export/import) | any project publishes a commons-ready release artifact |
| Pluggable censorship-resistant bootstrap | **have** (HTTPS · IPNS · DNS-over-HTTPS · Nostr · chain-anchor) | find live data when the usual entry points are blocked |

### Seed bundles

A **seed bundle** is a portable, self-verifying archive of records. Because verification is intrinsic,
the distributor is untrusted: a bundle can be *withheld* but not *poisoned*.

- **Implemented (node)**: `exportbundle <out.nlb> [--filter <json>]` dumps records to a bundle;
  `loadbundle <in.nlb>` ingests one through the same verify-then-store gate as `POST /v0/records`
  (`commons/bundle.py` + the two management commands). Filtered exports reuse the typed `query`
  language; incremental **delta** exports use `--since <cursor>` (the `/v0/sync` id cursor), and
  `exportbundle` prints the next cursor for chaining.
- **Distribute over anything**: HTTP mirrors, IPFS, BitTorrent, a git repo, email, physical media. If
  Arca is down or blocked, a node bootstraps from a bundle obtained by any means — verified records
  exported from a Postgres node restore cleanly into a fresh zero-dependency SQLite node.

### The standard commons-bundle format (`.nlb`)

Elevate the bundle from a fallback into a **first-class interchange format**, so that — instead of a
central crawler lifting libraries — **any open-source project ships its own commons-ready bundle as a
release artifact**, the way it already ships wheels, crates, or jars. This is the decentralized answer
to "bootstrapping the commons."

A `.nlb` ("Nova Lingua Bundle") is a **gzipped tar** (format id `nlb/1`, implemented in
`tooling/commons-node/commons/bundle.py`) containing exactly:

```
records.jsonl     # the content-addressed records (exactly what the nl-ingest-* adapters emit),
                  #   one per line, sorted by hash
manifest.json     # { format_version, count, schema_versions[], bundle_digest, source?, producer? }
```

It is **deterministic** — the same record set always produces identical bytes (sorted records, sorted
manifest keys, fixed tar mtime, gzip mtime=0) — so bundles dedupe and diff cleanly. `bundle_digest`
(a blake2b fingerprint of the record set) is a cheap whole-payload integrity pre-check; it is not the
security boundary (per-record hash verification on ingest is).

**Manifest signing (implemented).** `exportbundle`/`nl_bundle --sign-seed <seed>` add a `producer`
(the signer's `did:nova`) and an Ed25519 `signature` over the canonical manifest (minus the signature
field). Since the manifest carries `bundle_digest`, the signature transitively attests to the record
set. It is **advisory provenance, not a trust gate** — a node still re-verifies every record by hash on
ingest. `loadbundle` reports the provenance (`signed by <did> (verified)` / `unsigned` / an `INVALID`
warning) and `--require-signed` refuses anything but a valid signature. The Ed25519 signer is a pure-
Python reference impl ([`tooling/crypto-python/nl_crypto.py`](../tooling/crypto-python/nl_crypto.py))
that matches `nl-validator` (ed25519-dalek) byte-for-byte, so a stdlib-only publisher can sign.

Properties (all of which fall out of the existing design):

- **Self-verifying on ingest.** A node re-verifies every record by hash (+ message signatures). The
  producer is therefore **untrusted**; the manifest signature is *advisory provenance* ("this came from
  `github.com/org/lib@v1.2.3`"), never a trust gate (principle 7).
- **Produced by the existing adapters.** `nl-ingest-py mylib/ | nl-bundle --repo … --release … >
  mylib-1.2.3.nlb`, attached to a GitHub Release by CI.
- **One format, two jobs.** The same artifact is both the publishing/interchange format *and* the
  seed/disaster-recovery bundle — nothing extra to maintain.

A future `bundle.schema.json` + a section in [`commons.md`](commons.md) would make this normative.

### Pluggable censorship-resistant bootstrap ("dead drops")

When a fresh node cannot reach Arca or any known peer, it needs to discover *where the data is*. This is
a small amount of information — a "dead drop" — and it should be publishable to **several independent
channels**, so blocking one does not sever bootstrap:

- a **blockchain anchor** (`OP_RETURN` / calldata on a chain that exists anyway),
- **Nostr** relays, **IPNS**, a **DNS `TXT`** record, a **BitTorrent DHT** key, or a hardcoded fallback list.

What gets published is tiny and pointer-only: the **hash of the latest seed bundle**, a **signed list of
live node endpoints**, or a periodic **checkpoint**. Whatever a stranded node fetches is verified by
hash, so every channel is safe to trust-but-verify.

**Implemented: a pluggable multi-channel resolver.** A **bootstrap descriptor**
(`nlb-bootstrap/1`: `{peers[], latest_bundle:{hash, urls[]}, producer, signature}`) is published with
`makebootstrap` (signed by a `did:nova`, reusing the manifest signer) and resolved with `bootstrap`
([`commons/bootstrap.py`](../tooling/commons-node/commons/bootstrap.py)): it fetches the descriptor
from one or more URLs (trying each in turn, so blocking one channel doesn't sever bootstrap), verifies
the signature (`--trust <did>` requires a trusted signer), and with `--pull` fetches the latest bundle
— checking its digest matches the signed descriptor — and ingests it through the normal
verify-then-store gate. A descriptor's (and bundle's) URL list can **mix channels**, chosen by scheme:

| Scheme | Channel | Transport |
|---|---|---|
| `https://` `http://` `file://` | direct | urllib |
| `ipns://<name>[?gateway=]` | IPFS/IPNS | GET an IPFS gateway |
| `dns://<name>[?doh=]` | DNS-over-HTTPS | DoH `TXT` query; value is base64(descriptor) |
| `nostr://<relay>/<author>[?kind=]` | Nostr | minimal WebSocket read of the newest event |
| `chain://<read-endpoint>[#json.path]` | blockchain anchor | read a pointer the chain anchored, then follow it |

Each channel is **pure untrusted transport** — whatever bytes come back are still verified by the
descriptor signature (and the bundle by hash). Adding a channel = adding a fetcher to the registry;
the verify/ingest path is unchanged. The live transports are reference implementations (the scheme
dispatch + response parsing are what the test suite pins). **On blockchains**: the chain holds only a
tiny pointer (a URL/CID), never the bytes — consistent with rejecting blockchain as the substrate
while using it as a thin anchor.

**On blockchains specifically.** A blockchain is the *wrong primitive for the substrate* and is rejected
as such ([`commons.md`](commons.md)): the commons needs no global consensus or total ordering, and
on-chain storage is slow, expensive, and public. But a blockchain is a *fine thin anchor*: **write
rarely** (only tiny pointers), **read freely** (via any public RPC/explorer), and it **points to
off-chain bytes — it never stores them**. It is one dead-drop option among several; the bootstrap layer
must not depend on any single one (the depended-upon chain could be the thing targeted).

## Alignment

This strategy is a direct consequence of the project's principles — content-addressed identity
(principle 2) and open communication with local-only filtering (principle 7) — and it operationalizes
the "bootstrapping the commons" open problem. Nothing here requires Arca, or any specific transport, to
be available.
