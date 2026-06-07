# Crypto conformance contract

**Status:** v0.2 / v0.3. Normative for any implementation of the Nova Locutio encrypted envelope
([`encryption.md`](encryption.md), [`encrypted-envelope.schema.json`](encrypted-envelope.schema.json)),
including the v0.3 post-quantum hybrid `kex`.

Confidentiality in Nova Locutio is defined by **test vectors, not by a reference codebase**. An
implementation is conformant iff it reproduces the bytes in
[`conformance/encryption.json`](conformance/encryption.json) exactly. This lets the ecosystem swap
the pure-Python reference for a vetted constant-time library without anyone re-deciding what
"correct" means — the vectors are the spec.

## Why this exists

The reference implementation [`tooling/crypto-python/nl_crypto.py`](../tooling/crypto-python/) is a
clear, vector-verified teaching implementation. Its own module docstring is blunt: it is **not
constant-time and not side-channel resistant — do not use it to protect real secrets on a hostile
host.** Production agents MUST use a vetted library. To make that swap safe, "vetted library X is a
valid Nova Locutio crypto backend" has to be a *checkable* property, and this document defines the
check: reproduce every vector below, byte for byte.

Two conformant implementations ship in this repo and are tested against each other:

- **Reference (pure-Python, stdlib-only):** `tooling/crypto-python/nl_crypto.py`. Every primitive is
  hand-written and checked against its RFC/draft vector.
- **Hardened (Rust, vetted crates):** [`tooling/validator/src/seal.rs`](../tooling/validator/src/seal.rs)
  and the `nl-seal` binary, built on `x25519-dalek`, `curve25519-dalek`, `chacha20poly1305`,
  `hkdf`+`sha2`, `ed25519-dalek`, and (for the post-quantum hybrid) the NIST-validated `ml-kem` crate.

Because both reproduce the same vectors, an envelope sealed by either opens with the other.

## The construction (what a backend must implement)

A hybrid multi-recipient sealed box (full prose in [`encryption.md`](encryption.md)):

| Element | Primitive | Detail |
|---|---|---|
| Payload AEAD | XChaCha20-Poly1305 | 24-byte nonce, 16-byte tag appended to ciphertext |
| Key agreement | X25519 ECDH | recipient public key = Ed25519→Curve25519 map of the `did:nova` key, `u = (1+y)/(1-y)` |
| KEK derivation | HKDF-SHA-256 | `ikm = ECDH(esk, rxpub)`, `salt = epk ‖ rxpub`, `info = "novae-linguae/v0.2/xchacha20poly1305/key-wrap"`, 32-byte output |
| CEK wrap | XChaCha20-Poly1305 | key = KEK, AAD = the recipient DID bytes |

**RNG draw order is part of the contract** (so deterministic vectors are reproducible): `cek` (32) →
`esk` (32) → payload `nonce` (24) → then, per recipient in list order, an ML-KEM `m` (32, **hybrid kex
only**) before each `wrap_nonce` (24). The ephemeral public key is `epk = X25519_base(esk)`. Real
implementations draw these from a CSPRNG; the vectors use a deterministic source
`BLAKE3(seed ‖ counter_le64)` (NOT for production).

A recipient's X25519 secret is derived from its user seed exactly as the validator derives its
signing identity: `ed25519_seed = BLAKE3(user_seed)`, then
`x25519_secret = clamp(SHA-512(ed25519_seed)[..32])`.

### Post-quantum hybrid (`kex: x25519-mlkem768`, v0.3)

The additive variant keeps the payload path identical and changes only key agreement (full prose in
[`encryption.md`](encryption.md)):

| Element | Primitive | Detail |
|---|---|---|
| Key agreement | X25519 ECDH **+** ML-KEM-768 (FIPS 203) | recipient ML-KEM key published in its DID document ([`did-document.md`](did-document.md)); each recipient entry adds `kem_ct` (1088 bytes) |
| KEK derivation | HKDF-SHA-256 | `ikm = ecdh_ss ‖ mlkem_ss`, `salt = epk ‖ rxpub`, `info = "novae-linguae/v0.3/x25519-mlkem768/key-wrap"` |

A recipient's ML-KEM keypair is derived from its user seed: the 64-byte FIPS 203 seed is
`BLAKE3(user_seed ‖ "novae-linguae/v0.3/ml-kem-768/keygen" ‖ 0x00) ‖ BLAKE3(… ‖ 0x01)`, then
`ML-KEM.KeyGen_internal`. The envelope version is `0.3`.

## The vectors

[`conformance/encryption.json`](conformance/encryption.json) has these sections:

- `primitives` — one case each for X25519 (RFC 7748), HChaCha20 + ChaCha20-Poly1305 (RFC 8439),
  XChaCha20-Poly1305 (draft-irtf-cfrg-xchacha), HKDF-SHA-256 (RFC 5869), the Ed25519→X25519
  conversion against a real signer DID, and `ml_kem` — a FIPS-203-final ML-KEM-768 known-answer test
  (`d,z → ek`; `ek,m → ct,K`; decaps recovers `K`) from the NIST-validated `ml-kem` crate. These pin
  each primitive to its published / cross-validated test vector.
- `envelope`, `stealth_envelope` — deterministic v0.2 seals (direct and stealth) to one recipient.
- `mlkem768_envelope`, `mlkem768_stealth_envelope` — deterministic **v0.3 post-quantum hybrid** seals
  (direct and stealth). The recipient's ML-KEM key is derived from `recipient_seed`.

For every envelope vector — given `rng_seed_hex`, `plaintext_hex`, `aad_hex`, `recipient_did`, and (for
the hybrid) the recipient's seed-derived ML-KEM key — a conformant impl **reseals to identical bytes**
and **opens it back to the plaintext** using the recipient seed.

## Running the check

- Hardened Rust: `nl-seal conformance [path]`, or `cargo test --lib seal` in `tooling/validator/`.
- Reference Python: the `TestConformance` cases in `tooling/crypto-python/tests/`.

Both replay the same file; a new backend is conformant when it passes the same replay. Adding a new
algorithm follows the additive pattern the post-quantum hybrid `kex` already demonstrated: a new
envelope-format version (`0.3`) and a new vector set (`mlkem768_*`), with the existing vectors never
changing.
