# DID documents (v0.3)

*Status: v0.3, implemented. Schema: [`did-document.schema.json`](did-document.schema.json). Reference
impl: [`tooling/crypto-python/nl_crypto.py`](../tooling/crypto-python/nl_crypto.py)
(`build_did_document` / `verify_did_document` / `mlkem_pub_from_did_document`); hardened impl in
[`tooling/validator/src/seal.rs`](../tooling/validator/src/seal.rs).*

## Why this exists

A `did:nova:<64-hex>` identifier *is* an Ed25519 public key. That single key is enough for everything
v0.2 needs: it verifies signatures, and the standard Edwards→Montgomery birational map turns it into
an X25519 key for Diffie-Hellman. So `did:nova` has a frictionless property — **"I have your DID"
already means "I can encrypt to you."** No lookup, no registry, no second key.

Post-quantum key agreement (`kex: "x25519-mlkem768"`, see [`encryption.md`](encryption.md)) breaks that
property in exactly one place. An ML-KEM public key is lattice material, not a curve point; there is no
map from an Ed25519 key to an ML-KEM key, and the key is large (1184 bytes). It cannot be carried in
the DID string and cannot be derived by a recipient on the fly. It has to be **generated and
published**.

A DID document is that publication, and nothing more. It is the minimum needed to keep the agent
experience close to the original: `have DID → resolve its document → verify → read the ML-KEM key →
encrypt`. Crucially, the document is **self-verifying** — it is signed by the very identity it
describes — so a sender never has to trust whoever served it (principle 7, untrusted-by-design).

## Shape

```json
{
  "id": "did:nova:<64-hex>",
  "keyAgreement": [
    { "id": "<did>#mlkem768", "type": "ML-KEM-768", "publicKeyBase64": "<1184 bytes, base64>" }
  ],
  "signature": "ed25519:<base64>"
}
```

- **`id`** — the subject identity. The 64 hex chars are the Ed25519 public key that signs this document.
- **`keyAgreement`** — published key-agreement keys. v0.3 defines one type, `ML-KEM-768` (FIPS 203); the
  `publicKeyBase64` is the raw 1184-byte encapsulation key. The list is open so future algorithms (or a
  separately-advertised X25519 key, [`encryption.md`](encryption.md) open question 1) are additive.
- **`signature`** — Ed25519 over the **JCS-canonical document with the `signature` field removed**, the
  same construction as bundle-manifest signing ([`resilience.md`](resilience.md)). It is verifiable
  against the key embedded in `id`, so the binding "this ML-KEM key belongs to this identity" needs no
  third party.

A worked example is [`examples/did-document.json`](examples/did-document.json) (the document for the
fixed example identity `did:nova:ea9b…505e`).

## Deterministic key derivation

The ML-KEM keypair is **not** new key material to store. It is derived from the agent's existing user
seed, so one seed still regenerates the whole identity (signing + X25519 + ML-KEM):

```
mlkem_seed = BLAKE3(user_seed ‖ "novae-linguae/v0.3/ml-kem-768/keygen" ‖ 0x00)   // d (32 bytes)
           ‖ BLAKE3(user_seed ‖ "novae-linguae/v0.3/ml-kem-768/keygen" ‖ 0x01)   // z (32 bytes)
(ek, dk)   = ML-KEM.KeyGen_internal(d, z)
```

`d` and `z` are the two FIPS 203 keygen seeds. Both the reference and the hardened implementation
derive them this way, so an agent's published ML-KEM key is stable and reproducible from its seed.

## Resolution and trust

This spec defines the **artifact and its verification**, not a transport. A DID document is just another
self-verifying record, so it is served the same untrusted ways everything else is — the commons
([`commons.md`](commons.md)), a bundle, or any bootstrap channel ([`resilience.md`](resilience.md)). A
recipient of a document MUST run `verify_did_document` before using a key from it; an `invalid` or
`unsigned` document MUST be rejected. Because verification is against the DID itself, a malicious server
can withhold a document but cannot substitute a key.

## Open questions

- **Key rotation / multiple keys.** The `keyAgreement` array already allows several keys; a published
  ordering or `created`/`expires` metadata for rotation is deferred until there is a concrete need.
- **Separate X25519 key.** v0.2 reuses the identity key for X25519. A document could also advertise a
  distinct X25519 key (decoupling signing from key agreement); see [`encryption.md`](encryption.md)
  open question 1. Additive — a new `keyAgreement` entry type.
