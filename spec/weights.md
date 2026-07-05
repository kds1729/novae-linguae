# Model weights in the commons — pointer records + eval attestations

**Status: proposed design (v0.1 draft) — not yet implemented.** No `wgt_` artifact kind exists in the
validator or the node yet; this records the agreed design so implementation has a spec to land against.
The normative store/discovery protocol is [`commons.md`](commons.md); this document will merge into it
(plus a `weights.schema.json`) when the kind is built.

## Why

The project's reference checkpoints — LoRA adapters that make a small open-weights model *speak Nova
Lingua* (`tooling/eval/REFERENCE_CHECKPOINT.md`) — are pinned by content hash and fully reproducible,
but live nowhere durable: the weights are gitignored (regenerable, wrong shape for a source tree) and
the checkpoint doc already says "pin/host them in the commons, not the source tree." The commons is the
right home for their *identity, provenance, and verification* — but the wrong home for their *bytes*.

A trained adapter is also the purest example of a **derived artifact of commons content**: the training
corpus is generated from verified records, the split is content-hashed, the recipe is deterministic
(fixed seed, no RNG in the data path). The weights are a function of content the commons already
addresses — the record below makes that derivation explicit and checkable.

## The design decision: pointer record, not blob store

The commons node stores small, JCS-canonicalized JSON artifacts behind a verify-then-store gate, with
peer replication and token-budgeted discovery. A 100–300 MB opaque binary through that gate would need
multipart upload, range serving, and storage/egress quotas — a CDN's job, not the reference node's, and
none of it improves verifiability. So:

- **The commons stores a small JSON *pointer record*** (`wgt_…`, content-addressed like every artifact:
  JCS + BLAKE3-256 over the record with its kind-appropriate fields stripped).
- **The blob lives on ordinary static hosting**, fetched by plain HTTPS `GET` and verified against the
  hash in the record. The blob host is **untrusted by construction** — the same verify-then-use boundary
  as everything else in the commons; a URL is advisory, the hash is the truth.

Consumer-agent rationale (the design tie-breaker): a static `GET` by content hash is the same primitive
an agent already uses for every artifact, and serving blobs from the same origin as the node keeps the
agent's `net.read` capability grant at **one domain** — a second host (a mirror) widens the grant, so
mirrors are listed but not required. No third-party registry API enters the loop.

## The weights record (`wgt_` kind)

| field | meaning |
|---|---|
| `schema_version` | as elsewhere |
| `hash` | `wgt_<blake3-256-hex>` content address of this record |
| `name_hints` | non-normative names, as in function records |
| `base` | the base model the weights apply to (e.g. `Qwen/Qwen2.5-Coder-7B-Instruct`) and its license |
| `format` | what the bytes are (e.g. `lora-peft-safetensors`) — a closed enum, grown by schema bump |
| `files[]` | the blob manifest: `{name, sha256, bytes}` per file. **`sha256`**, not BLAKE3 — blobs are not JCS-canonicalizable JSON, so any collision-resistant hash serves; sha256 is the ML-ecosystem convention (safetensors, HF) and matches the pins already recorded in `REFERENCE_CHECKPOINT.md` |
| `recipe` | the deterministic derivation: training-corpus content identity (the corpus jsonl sha + the generator provenance), train-split sha, trainer + version, seed, epochs, hyperparameters. This is the **reproducibility claim**: same base + recipe ⇒ bit-identical `files[].sha256` |
| `measured` | *optional, self-reported* eval summary (harness settings + scores). Normatively **untrusted** — consumers rely on signed attestations (below), never on this field |
| `urls[]` | advisory fetch locations, primary first (convention: `<node-origin>/blobs/<sha256>`); any mirror may serve, the hash decides |
| `derived_from` / `supersedes` | lineage, as in function records — versioning is a supersedes chain, not mutable tags |

## Serving blobs

Static file serving keyed by content hash: `GET /blobs/<sha256>` — no gate, no schema, no judgment; the
node's edge (or any mirror, including a plain web server or an object store) can serve it. Ingest of the
*pointer record* goes through the normal verify-then-store gate (schema + `wgt_` hash check); the node
does not fetch or verify the blobs it points at. Blob egress is metered like everything else on a public
node; mirrors exist precisely so one node's budget is not the artifact's availability.

## Verification — three rungs

1. **Integrity** (mechanical, free): after download, the consumer hashes the blob and compares to
   `files[].sha256`. A mismatch is a hard reject. This alone makes any host — including a hostile
   mirror — safe to fetch from.
2. **Reproducibility** (mechanical, expensive): the `recipe` is deterministic, so a certifier can
   retrain from the content-addressed corpus and confirm the byte-identical sha. This is the weights
   analogue of re-running a function body: nobody has to be believed, only re-executed.
3. **Eval attestation** (the rung consumers actually use): a signed record — the weights analogue of a
   function `certification` — in which a certifier states *measured capability*:
   `{subject: wgt_…, eval: {harness settings, task-set content identity}, results, signature}`.
   Cheap to produce (minutes of GPU for a full held-out eval), cheap to check (hash + signature), and
   accountable (an attestation is only as good as its certifier). The attestation graph ingests it as
   an edge — the same trust machinery that answers "is this function certified by a certifier I trust?"
   answers "is this model's measured score attested by one?" — and discovery can then **rank weights by
   trusted measured capability**, not by name.

For an opaque binary, rung 3 *is* the artifact's value: a function record's meaning is re-executable in
place, but weights are bytes — recipe + attestation + signature is what distinguishes a commons weights
record from a download link.

## Security boundary & non-goals

- The node stores and serves pointer records and attestations **mechanically** (principle 7); whether to
  load any weights is the consumer's decision under its **own** policy over the attestation graph.
- Weights are **data, not language**: nothing in Nova Lingua executes them; no evaluation semantics,
  effect, or builtin refers to them. (An inference *effect* is conceivable much later; it is explicitly
  out of scope here.)
- Not a model registry: no mutable tags, no "latest" — new weights are a new record, lineage is
  `supersedes`, and a consumer pins the `wgt_` address exactly as it pins a `fn_` address.
- Licensing rides along, not enforced: `base` carries the base model's license; the record cannot
  launder it.
