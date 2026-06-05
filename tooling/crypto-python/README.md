# nl-encrypt / nl_crypto — Nova Locutio payload encryption (v0.2)

Reference implementation of the Nova Locutio **encrypted envelope** ([`spec/encryption.md`](../../spec/encryption.md)):
a hybrid, multi-recipient sealed box that gives payloads **confidentiality** to complement the
integrity/provenance that signing already provides. This is the load-bearing piece of principle 7 —
without it, "open communication" can be selectively suppressed by anyone who can read the wire.

- **`nl_crypto.py`** — the library: X25519, ChaCha20/HChaCha20/XChaCha20-Poly1305, HKDF-SHA-256, the
  Ed25519↔X25519 conversion, and the envelope `seal`/`open`.
- **`nl_encrypt.py`** — the CLI: `seal`, `open`, `pubkey`.

## Zero dependencies

Runs with only `python3` (3.10+). Every primitive is a pure-Python, standard-library-only reference
implementation; it imports the vendored BLAKE3 from [`ingest-common`](../ingest-common/) only so an
agent's seed derives the **same** identity `nl-validator` uses for signing.

> ⚠️ **Not for protecting real secrets.** These are clear, *verifiable* reference implementations,
> chosen for readability — they are **not** constant-time or side-channel resistant. Production
> agents must use a vetted library (libsodium, `ring`, the Rust `x25519-dalek` + `chacha20poly1305`
> crates) that reproduces the **same bytes**. Conformance is defined by the test vectors, not this code.

## How it works (one paragraph)

The payload is encrypted once under a random 32-byte content-encryption key (CEK) with
XChaCha20-Poly1305. The CEK is wrapped for each recipient via X25519 ECDH — using the recipient's
`did:nova` Ed25519 key mapped to Curve25519 — through HKDF-SHA-256 to a key-encryption key, then
XChaCha20-Poly1305. The sender uses a fresh ephemeral X25519 key per envelope (forward secrecy for
the wrap). The CEK may be reused across a conversation (the "per-conversation symmetric key") or
minted per message. Identity reuse means **no new keys**: the signing DID is the encryption identity.
Full design and rationale: [`spec/encryption.md`](../../spec/encryption.md).

## Usage

```bash
# The X25519 public key a DID (or seed) resolves to
./nl_encrypt.py pubkey --did did:nova:<64-hex>
./nl_encrypt.py pubkey --seed my-seed

# Seal stdin to one or more recipients (pretty JSON envelope on stdout)
echo -n '{"secret":1}' | ./nl_encrypt.py seal --to did:nova:<hex> --to did:nova:<hex> --pretty

# Open an envelope addressed to you (raw plaintext to stdout)
./nl_encrypt.py open --did did:nova:<hex> --seed my-seed envelope.json
```

The recommended pattern is **sign-then-seal**: sign a Nova Locutio message with `nl-validator sign`,
then seal the signed message as the envelope payload — decrypting yields a verifiable inner message.

## Tests

```bash
python3 -m unittest discover -s tests
```

20 tests covering: every primitive against its official RFC/draft vector (X25519 RFC 7748,
ChaCha20/Poly1305/ChaCha20-Poly1305 RFC 8439, XChaCha20-Poly1305 + HChaCha20 draft-irtf-cfrg-xchacha,
HKDF-SHA-256 RFC 5869); the Ed25519→X25519 conversion checked for **consistency against the real
`nl-validator` example DIDs** (the X25519 key from a DID equals the one from its seed) plus ECDH
agreement; envelope round-trips (single/multi-recipient, non-recipient rejection, wrong-key and
tamper detection, AAD binding, per-conversation CEK reuse, deterministic reproducibility); a CLI
seal→open round-trip; and replay of the conformance vectors.

## Conformance vectors

[`spec/conformance/encryption.json`](../../spec/conformance/encryption.json) pins the primitive
vectors and one deterministic envelope (reproducible with a BLAKE3-seeded RNG). Regenerate with:

```bash
python3 tests/gen_vectors.py
```

This also rewrites the worked example [`spec/examples/encrypted-envelope.json`](../../spec/examples/encrypted-envelope.json),
which validates against [`spec/encrypted-envelope.schema.json`](../../spec/encrypted-envelope.schema.json)
via `nl-validator validate`.

## License

Dual-licensed under Apache-2.0 OR MIT, same as the rest of *Novae Linguae*.
