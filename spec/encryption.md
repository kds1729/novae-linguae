# Nova Locutio payload encryption (v0.2)

## Purpose

Signing (see [`canonical-serialization.md`](canonical-serialization.md) and the message schema)
gives **integrity and provenance** — a receiver knows who sent a message and that it was not
altered. It does **not** give **confidentiality**: a signed message is readable by anyone who sees
it. Principle 6 of the project README places confidentiality in the same layer as signing, and
principle 7 makes the *capability* load-bearing: without encryption being available and
un-suppressible, "open communication" can be chilled by whoever can observe the wire. An adversary
who cannot stop a message can still chill it by reading it. Encryption removes that leverage.

This document specifies the v0.2 **encrypted envelope**: how an agent seals a payload so that only
the intended recipients can read it, using the identities agents already have.

## Optional by design

**Signing is mandatory; sealing is not.** Every message that crosses an agent boundary is signed
(principle 6) — that is the baseline. Encryption is a separate, **opt-in** layer applied *per
conversation or per message*, only when the channel warrants it. A plaintext **signed** message is
the normal, cheaper default; an encrypted envelope is what you reach for when an observer could read
the wire.

When sealing is **not** worth it (send signed plaintext):

- Agents co-located on the **same host** (loopback, Unix sockets, shared memory, in-process).
- Agents on a **trusted private subnet** or inside one operator's trust boundary.
- Any channel the operator already secures at a lower layer (a mutually-authenticated TLS tunnel, a
  VPN, a WireGuard mesh) — double-encrypting buys nothing but CPU.

When sealing **is** worth it (seal before sending):

- Messages crossing an **untrusted network**, or **relayed/mirrored through third parties** that
  could read them.
- Any path where a passive observer reading the content would let it be selectively suppressed —
  exactly the principle-7 case.

The cost is real and is why this is opt-in: ECDH + an AEAD pass, plus ~`32 + 16` bytes of wrapped
key per recipient and an ephemeral public key per envelope, plus key-management state. On a fast
local channel that overhead dominates; on a hostile one it is mandatory.

**How a receiver knows which to expect.** A received payload is *either* a cleartext signed message
*or* an encrypted envelope; the two are distinguishable by shape (an envelope has `enc`/`kex`/
`recipients`; a message has `kind`/`signature`). Whether to *accept* plaintext on a given channel is
a **local-policy** decision (see [`trust-model.md`](trust-model.md)): an agent MAY require that peers
on an untrusted link seal their messages and reject unencrypted ones, or MAY accept plaintext from
peers it reaches over a channel it already trusts. Nothing in the protocol forces encryption on, and
— per principle 7 — nothing lets a third party force it *off* or decrypt what was sealed. **The
endpoints choose.** That choice being free, in both directions, is the load-bearing property.

## What it protects, and what it does not

- **Protected:** the confidentiality and integrity of the payload. Only a holder of a recipient's
  secret key can decrypt; tampering with any ciphertext byte is detected.
- **Not protected (v0.2):** *metadata*. The envelope's `recipients` list is cleartext — who can
  read an envelope is visible. Hiding the recipient set (anonymous/stealth addressing) is deferred.
  Traffic analysis (timing, size, who-talks-to-whom) is out of scope for this layer.

Confidentiality is layered **over** signing, not instead of it. The recommended construction is
**sign-then-seal**: build and sign an ordinary Nova Locutio message, then seal the signed message as
the envelope's payload. Decrypting yields a fully verifiable inner message, so the receiver gets
provenance *and* confidentiality. (Sealing alone authenticates the *envelope* to a recipient via the
AEAD, but the inner signature is what binds content to a sender identity.)

## Identities: reuse the signing DID, no new keys

An agent's v0.1 identity is `did:nova:<64-hex>`, where the hex is its **Ed25519 public key** (used
for signing). Key agreement needs an **X25519** (Curve25519 ECDH) key, not an Ed25519 one — but the
two are related by a standard birational map, so the *same* DID serves both:

- **Public:** an Ed25519 public key maps to its X25519 public key by `u = (1 + y) / (1 - y) mod p`,
  where `y` is the Edwards `y`-coordinate (the low 255 bits of the key) and `p = 2^255 − 19`. Only
  `y` is needed; no point decompression.
- **Secret:** the X25519 secret is `clamp(SHA-512(ed25519_seed)[0..32])`, the same scalar Ed25519
  signing uses internally. Since `nl-validator` derives the Ed25519 seed as `BLAKE3(user_seed)`, an
  agent's single seed yields a matching signing identity and key-agreement key.

This is the same construction libsodium exposes as `crypto_sign_ed25519_*_to_curve25519`. The
reference implementation cross-checks it against the real example DIDs: the X25519 public key
obtained by converting a DID equals the one derived from that DID's seed.

> **Caveat (acknowledged, not glossed):** reusing one keypair for both signing and key agreement is
> a deliberate simplicity/identity-reuse trade-off, standard in practice (Signal, age, libsodium)
> but not free — it couples the two uses. v0.3+ may let a DID document advertise a *separate* X25519
> key; the `kex` field is versioned so that change is additive.

## The scheme

A **hybrid, multi-recipient sealed box**:

1. **Content-encryption key (CEK):** a random 32-byte symmetric key. This is the *per-conversation
   symmetric key* of the README — a sender MAY reuse one CEK across every message in a conversation
   (recipients cache it), or mint a fresh one per message. The envelope is self-contained either way.
2. **Payload encryption:** `ciphertext = XChaCha20-Poly1305(CEK, nonce, payload, aad)` with a random
   24-byte `nonce`. XChaCha20's 192-bit nonce makes random nonces safe even when the CEK is reused
   across many messages.
3. **Key wrapping (per recipient):** the sender generates one **ephemeral** X25519 keypair
   `(esk, epk)` for the envelope. For each recipient with X25519 public key `rpk`:
   - `shared = X25519(esk, rpk)`
   - `kek = HKDF-SHA-256(ikm = shared, salt = epk ‖ rpk, info = "novae-linguae/v0.2/xchacha20poly1305/key-wrap", L = 32)`
   - `wrapped_key = XChaCha20-Poly1305(kek, wrap_nonce, CEK, aad = recipient_DID)`
   The ephemeral sender key gives forward secrecy for the wrap: compromising the sender's long-term
   key later does not reveal past CEKs. Binding the recipient DID as the wrap's AAD prevents a
   recipient entry from being lifted onto a different identity.

### Encryption algorithm

```
seal(payload, recipient_dids[], aad = ""):
    CEK   = random(32)                       # or a reused per-conversation key
    esk   = random(32);  epk = X25519(esk, 9)
    nonce = random(24)
    ciphertext = XChaCha20-Poly1305_seal(CEK, nonce, payload, aad)
    recipients = []
    for did in recipient_dids:
        rpk    = ed25519_pub_to_x25519(ed25519_key_of(did))
        kek    = HKDF-SHA256(X25519(esk, rpk), epk ‖ rpk, KEYWRAP_INFO, 32)
        wn     = random(24)
        wk     = XChaCha20-Poly1305_seal(kek, wn, CEK, aad = utf8(did))
        recipients += { to: did, wrap_nonce: b64(wn), wrapped_key: b64(wk) }
    return { v:"0.2", enc:"xchacha20poly1305", kdf:"hkdf-sha256", kex:"x25519-ed25519",
             epk: b64(epk), nonce: b64(nonce), ciphertext: b64(ciphertext),
             recipients, aad?: b64(aad) }
```

### Decryption algorithm

```
open(envelope, my_did, my_x25519_secret):
    entry = envelope.recipients where to == my_did            # else: not a recipient
    rpk   = X25519(my_x25519_secret, 9)
    kek   = HKDF-SHA256(X25519(my_x25519_secret, b64d(envelope.epk)),
                        b64d(envelope.epk) ‖ rpk, KEYWRAP_INFO, 32)
    CEK   = XChaCha20-Poly1305_open(kek, b64d(entry.wrap_nonce), b64d(entry.wrapped_key), aad = utf8(my_did))
    return XChaCha20-Poly1305_open(CEK, b64d(envelope.nonce), b64d(envelope.ciphertext),
                                   aad = b64d(envelope.aad) or "")
```

Decryption recomputes the recipient's own X25519 public key from its secret to rebuild the HKDF
salt; this equals the `rpk` the sender used (the conversion is consistent), so the same `kek` falls
out. Any AEAD authentication failure (wrong key, tampering, wrong AAD) raises an error.

## Envelope format

Defined by [`encrypted-envelope.schema.json`](encrypted-envelope.schema.json). Fields: `v`, `enc`,
`kdf`, `kex`, `epk`, `nonce`, `ciphertext`, `recipients[] = {to, wrap_nonce, wrapped_key}`, optional
`aad`. All binary values are standard base64. A worked example is
[`examples/encrypted-envelope.json`](examples/encrypted-envelope.json).

The envelope is a **transport** artifact: it is *not* content-addressed and carries no hash. (The
inner payload, once decrypted, may be a content-addressed, signed message that is.)

## Algorithm choices

| Layer | v0.2 choice | Why |
|------|-------------|-----|
| Key agreement | X25519 (RFC 7748) over keys mapped from the Ed25519 DID | Reuses the existing identity; ubiquitous, fast, misuse-resistant. |
| KDF | HKDF-SHA-256 (RFC 5869) | The standard ECIES KDF with ubiquitous test vectors. SHA-2 is *already* in the stack (the Ed25519→X25519 secret uses SHA-512), so this adds no new primitive family. BLAKE3's `derive_key` was considered for principle-8 minimality but HKDF was chosen for verifiability and interop. |
| AEAD | XChaCha20-Poly1305 | 192-bit nonce → random nonces are safe under per-conversation CEK reuse; no AES hardware assumption. |
| Encoding | base64 (RFC 4648) | JSON-native, like the rest of v0.1/v0.2. A binary wire format (CBOR) can map over it later without changing the construction. |

Each primitive is pinned by official RFC/draft test vectors and an end-to-end envelope vector in
[`conformance/encryption.json`](conformance/encryption.json); see the reference implementation at
[`tooling/crypto-python/`](../tooling/crypto-python/README.md).

## Security considerations

- **Reference implementation is not hardened.** `tooling/crypto-python/nl_crypto.py` is a clear,
  vector-verified reference, *not* constant-time and not side-channel resistant. Production agents
  MUST use a vetted library (libsodium, `ring`, `x25519-dalek` + `chacha20poly1305`) that reproduces
  the same bytes. Conformance is defined by the test vectors, not by this code.
- **Nonces.** Every `nonce`/`wrap_nonce` MUST be unique per key. The 24-byte XChaCha20 nonce makes
  random generation safe; never use a counter that can reset, and never reuse a (key, nonce) pair.
- **Forward secrecy** applies to the *key-wrap* (ephemeral sender key per envelope), not to the CEK
  if a sender deliberately reuses it across a conversation — that is the explicit cost of the
  per-conversation-key convenience. Mint fresh CEKs when forward secrecy of the payload matters.
- **Metadata.** The recipient list is visible (§"What it protects"). Treat it as public.
- **Key compromise.** Because signing and key-agreement share a key, compromise of an agent's seed
  breaks both its signatures and its confidentiality. Rotate by minting a new DID; old envelopes
  remain decryptable by the old key (content-addressing means nothing is rewritten).
- **No sender authentication from the envelope alone.** The wrap authenticates *to* a recipient but
  does not prove *who sealed it* — that is the inner signature's job (sign-then-seal).

## Open questions (v0.3+, not blockers)

1. **Separate encryption key in a DID document** — decouple signing from key agreement.
2. **Metadata privacy** — stealth/anonymous recipient addressing so the recipient set is hidden.
3. **Post-quantum** — a hybrid X25519 + ML-KEM `kex` once the wire cost is justified.
4. **Sender authentication / deniability** — an authenticated-but-deniable mode (e.g. a sender-static
   ECDH variant) for agents that want sender binding without a non-repudiable signature.
5. **Group/conversation key management** — rekeying, membership changes, and forward secrecy for
   long-lived multi-party conversations.
