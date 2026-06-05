"""nl_crypto — cryptographic primitives for Nova Locutio payload encryption (v0.2).

Pure-Python, standard-library-only reference implementations of every primitive the
encrypted-envelope format (``spec/encryption.md``) needs:

  - X25519 (RFC 7748) — ECDH on Curve25519.
  - ChaCha20 and ChaCha20-Poly1305 AEAD (RFC 8439), HChaCha20 and XChaCha20-Poly1305
    (draft-irtf-cfrg-xchacha) — authenticated encryption with a 192-bit nonce.
  - HKDF-SHA-256 (RFC 5869) — key derivation.
  - Ed25519 → X25519 key conversion (the standard birational map), so the *same*
    ``did:nova:<ed25519-pubkey>`` identity used for signing also serves key agreement —
    no new key material, matching "key exchange via DID-resolved public keys".

Like the project's vendored BLAKE3, these are reference implementations chosen for clarity
and verifiability, not for constant-time/side-channel resistance. **Do not** use this module
to protect real secrets on a hostile host; use a vetted library (libsodium, ring, the Rust
``x25519-dalek``/``chacha20poly1305`` crates) that produces the *same bytes*. Every primitive
here is checked against its official test vector by ``tests/test_nl_crypto.py``; that is what a
second implementation must reproduce.

This module has one intra-repo dependency: it imports the vendored BLAKE3 from
``tooling/ingest-common/nl_core`` for seed derivation, so an agent's single seed yields the same
identity the Rust ``nl-validator`` derives for signing (``ed25519_seed = BLAKE3(user_seed)``).
"""

from __future__ import annotations

import base64
import hashlib
import hmac
import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "ingest-common"))
from nl_core import blake3_256  # noqa: E402  (vendored BLAKE3, matches nl-validator seed derivation)

_P25519 = (1 << 255) - 19


# ---------------------------------------------------------------------------
# X25519 (RFC 7748).
# ---------------------------------------------------------------------------

def _decode_u(u: bytes) -> int:
    arr = bytearray(u[:32])
    arr[31] &= 0x7F
    return int.from_bytes(arr, "little")


def _decode_scalar(k: bytes) -> int:
    arr = bytearray(k[:32])
    arr[0] &= 248
    arr[31] &= 127
    arr[31] |= 64
    return int.from_bytes(arr, "little")


def x25519(scalar: bytes, u: bytes) -> bytes:
    """X25519 scalar multiplication per RFC 7748 §5. Returns the 32-byte u-coordinate."""
    k = _decode_scalar(scalar)
    x1 = _decode_u(u)
    x2, z2, x3, z3 = 1, 0, x1, 1
    swap = 0
    p = _P25519
    a24 = 121665
    for t in reversed(range(255)):
        kt = (k >> t) & 1
        swap ^= kt
        if swap:
            x2, x3 = x3, x2
            z2, z3 = z3, z2
        swap = kt
        a = (x2 + z2) % p
        aa = (a * a) % p
        b = (x2 - z2) % p
        bb = (b * b) % p
        e = (aa - bb) % p
        c = (x3 + z3) % p
        d = (x3 - z3) % p
        da = (d * a) % p
        cb = (c * b) % p
        x3 = pow(da + cb, 2, p)
        z3 = (x1 * pow(da - cb, 2, p)) % p
        x2 = (aa * bb) % p
        z2 = (e * (aa + a24 * e)) % p
    if swap:
        x2, x3 = x3, x2
        z2, z3 = z3, z2
    res = (x2 * pow(z2, p - 2, p)) % p
    return res.to_bytes(32, "little")


_X25519_BASE = b"\x09" + b"\x00" * 31


def x25519_base(scalar: bytes) -> bytes:
    """Public key for an X25519 secret scalar: X25519(scalar, 9)."""
    return x25519(scalar, _X25519_BASE)


# ---------------------------------------------------------------------------
# Ed25519 <-> X25519 conversion (matches libsodium crypto_sign_ed25519_*_to_curve25519).
# ---------------------------------------------------------------------------

def ed25519_pub_to_x25519(ed_pub: bytes) -> bytes:
    """Convert an Ed25519 public key to the X25519 public key u = (1+y)/(1-y) mod p.

    Only the Edwards y-coordinate is needed (the high bit is the x sign, masked off), so no point
    decompression / square root is required."""
    y = int.from_bytes(ed_pub, "little") & ((1 << 255) - 1)
    p = _P25519
    u = ((1 + y) * pow((1 - y) % p, p - 2, p)) % p
    return u.to_bytes(32, "little")


def ed25519_seed_to_x25519_secret(ed_seed: bytes) -> bytes:
    """Derive the X25519 secret from a 32-byte Ed25519 seed: clamp(SHA-512(seed)[:32])."""
    h = bytearray(hashlib.sha512(ed_seed).digest()[:32])
    h[0] &= 248
    h[31] &= 127
    h[31] |= 64
    return bytes(h)


def x25519_keypair_from_user_seed(user_seed: str):
    """An agent's X25519 keypair from its user seed, consistent with nl-validator's signing key.

    nl-validator derives the Ed25519 seed as BLAKE3(user_seed); we map that to X25519. Returns
    (x25519_secret_bytes, x25519_public_bytes)."""
    ed_seed = blake3_256(user_seed.encode("utf-8"))
    xsk = ed25519_seed_to_x25519_secret(ed_seed)
    return xsk, x25519_base(xsk)


# ---------------------------------------------------------------------------
# ChaCha20 / HChaCha20 (RFC 8439 + draft-irtf-cfrg-xchacha).
# ---------------------------------------------------------------------------

_SIGMA = (0x61707865, 0x3320646E, 0x79622D32, 0x6B206574)
_M32 = 0xFFFFFFFF


def _rotl32(x: int, n: int) -> int:
    return ((x << n) | (x >> (32 - n))) & _M32


def _quarterround(s, a, b, c, d):
    s[a] = (s[a] + s[b]) & _M32
    s[d] = _rotl32(s[d] ^ s[a], 16)
    s[c] = (s[c] + s[d]) & _M32
    s[b] = _rotl32(s[b] ^ s[c], 12)
    s[a] = (s[a] + s[b]) & _M32
    s[d] = _rotl32(s[d] ^ s[a], 8)
    s[c] = (s[c] + s[d]) & _M32
    s[b] = _rotl32(s[b] ^ s[c], 7)


def _double_rounds(w):
    for _ in range(10):
        _quarterround(w, 0, 4, 8, 12)
        _quarterround(w, 1, 5, 9, 13)
        _quarterround(w, 2, 6, 10, 14)
        _quarterround(w, 3, 7, 11, 15)
        _quarterround(w, 0, 5, 10, 15)
        _quarterround(w, 1, 6, 11, 12)
        _quarterround(w, 2, 7, 8, 13)
        _quarterround(w, 3, 4, 9, 14)


def _words_le(b: bytes):
    return [int.from_bytes(b[i:i + 4], "little") for i in range(0, len(b), 4)]


def _chacha20_block(key: bytes, counter: int, nonce12: bytes) -> bytes:
    state = list(_SIGMA) + _words_le(key) + [counter & _M32] + _words_le(nonce12)
    w = state[:]
    _double_rounds(w)
    out = bytearray()
    for i in range(16):
        out += ((w[i] + state[i]) & _M32).to_bytes(4, "little")
    return bytes(out)


def _chacha20_xor(key: bytes, counter: int, nonce12: bytes, data: bytes) -> bytes:
    out = bytearray()
    for off in range(0, len(data), 64):
        ks = _chacha20_block(key, counter, nonce12)
        counter = (counter + 1) & _M32
        chunk = data[off:off + 64]
        out += bytes(b ^ ks[i] for i, b in enumerate(chunk))
    return bytes(out)


def hchacha20(key: bytes, nonce16: bytes) -> bytes:
    """HChaCha20 (draft-irtf-cfrg-xchacha §2.2): 32-byte subkey from key + 16-byte nonce."""
    w = list(_SIGMA) + _words_le(key) + _words_le(nonce16)
    _double_rounds(w)
    words = w[0:4] + w[12:16]
    return b"".join(x.to_bytes(4, "little") for x in words)


# ---------------------------------------------------------------------------
# Poly1305 + ChaCha20-Poly1305 / XChaCha20-Poly1305 AEAD (RFC 8439).
# ---------------------------------------------------------------------------

def _poly1305(otk: bytes, msg: bytes) -> bytes:
    r = int.from_bytes(otk[0:16], "little") & 0x0FFFFFFC0FFFFFFC0FFFFFFC0FFFFFFF
    s = int.from_bytes(otk[16:32], "little")
    p = (1 << 130) - 5
    acc = 0
    for off in range(0, len(msg), 16):
        block = msg[off:off + 16]
        n = int.from_bytes(block, "little") + (1 << (8 * len(block)))
        acc = ((acc + n) * r) % p
    acc = (acc + s) % (1 << 128)
    return acc.to_bytes(16, "little")


def _pad16(x: bytes) -> bytes:
    rem = len(x) % 16
    return b"" if rem == 0 else b"\x00" * (16 - rem)


def _aead_mac_data(aad: bytes, ct: bytes) -> bytes:
    return (aad + _pad16(aad) + ct + _pad16(ct)
            + len(aad).to_bytes(8, "little") + len(ct).to_bytes(8, "little"))


def chacha20poly1305_seal(key: bytes, nonce12: bytes, plaintext: bytes, aad: bytes = b"") -> bytes:
    otk = _chacha20_block(key, 0, nonce12)[:32]
    ct = _chacha20_xor(key, 1, nonce12, plaintext)
    tag = _poly1305(otk, _aead_mac_data(aad, ct))
    return ct + tag


def chacha20poly1305_open(key: bytes, nonce12: bytes, ct_tag: bytes, aad: bytes = b"") -> bytes:
    if len(ct_tag) < 16:
        raise ValueError("ciphertext too short")
    ct, tag = ct_tag[:-16], ct_tag[-16:]
    otk = _chacha20_block(key, 0, nonce12)[:32]
    if not hmac.compare_digest(_poly1305(otk, _aead_mac_data(aad, ct)), tag):
        raise ValueError("AEAD authentication failed")
    return _chacha20_xor(key, 1, nonce12, ct)


def xchacha20poly1305_seal(key: bytes, nonce24: bytes, plaintext: bytes, aad: bytes = b"") -> bytes:
    """XChaCha20-Poly1305 seal. nonce24 is 24 bytes. Returns ciphertext || 16-byte tag."""
    if len(nonce24) != 24:
        raise ValueError("XChaCha20 nonce must be 24 bytes")
    subkey = hchacha20(key, nonce24[:16])
    n12 = b"\x00\x00\x00\x00" + nonce24[16:24]
    return chacha20poly1305_seal(subkey, n12, plaintext, aad)


def xchacha20poly1305_open(key: bytes, nonce24: bytes, ct_tag: bytes, aad: bytes = b"") -> bytes:
    if len(nonce24) != 24:
        raise ValueError("XChaCha20 nonce must be 24 bytes")
    subkey = hchacha20(key, nonce24[:16])
    n12 = b"\x00\x00\x00\x00" + nonce24[16:24]
    return chacha20poly1305_open(subkey, n12, ct_tag, aad)


# ---------------------------------------------------------------------------
# HKDF-SHA-256 (RFC 5869).
# ---------------------------------------------------------------------------

def hkdf_sha256(ikm: bytes, salt: bytes, info: bytes, length: int) -> bytes:
    if salt == b"":
        salt = b"\x00" * hashlib.sha256().digest_size
    prk = hmac.new(salt, ikm, hashlib.sha256).digest()
    okm = b""
    t = b""
    counter = 1
    while len(okm) < length:
        t = hmac.new(prk, t + info + bytes([counter]), hashlib.sha256).digest()
        okm += t
        counter += 1
    return okm[:length]


# ---------------------------------------------------------------------------
# did:nova helpers.
# ---------------------------------------------------------------------------

_DID_PREFIX = "did:nova:"


def ed25519_pub_from_did(did: str) -> bytes:
    """Extract the 32-byte Ed25519 public key from a did:nova:<64-hex> identifier."""
    if not did.startswith(_DID_PREFIX):
        raise ValueError(f"not a did:nova identifier: {did!r}")
    hexpart = did[len(_DID_PREFIX):]
    if len(hexpart) != 64 or any(c not in "0123456789abcdef" for c in hexpart):
        raise ValueError(f"did:nova key must be 64 lowercase hex chars: {did!r}")
    return bytes.fromhex(hexpart)


def x25519_pub_from_did(did: str) -> bytes:
    """The X25519 public key for a did:nova identity (its Ed25519 key, mapped to Curve25519)."""
    return ed25519_pub_to_x25519(ed25519_pub_from_did(did))


def random_bytes(n: int) -> bytes:
    return os.urandom(n)


def seeded_rng(seed: bytes):
    """A deterministic byte source (BLAKE3(seed || counter)) for reproducible envelopes/vectors.

    NOT for production use — real envelopes must use os.urandom (``random_bytes``). This exists so the
    conformance vectors are byte-reproducible."""
    state = {"n": 0}

    def rng(n: int) -> bytes:
        out = b""
        while len(out) < n:
            out += blake3_256(seed + state["n"].to_bytes(8, "little"))
            state["n"] += 1
        return out[:n]

    return rng


# ---------------------------------------------------------------------------
# Encrypted envelope (spec/encryption.md): hybrid multi-recipient seal.
#
#   CEK            random 32-byte content-encryption key (the per-conversation symmetric key).
#   payload        XChaCha20-Poly1305(CEK, nonce, plaintext, aad).
#   per recipient  ECDH(ephemeral_sk, recipient_x25519_pub) -> HKDF -> KEK; wrap CEK under KEK
#                  with XChaCha20-Poly1305 and the recipient DID as AAD.
# ---------------------------------------------------------------------------

ENVELOPE_VERSION = "0.2"
ENC_ALG = "xchacha20poly1305"
KDF_ALG = "hkdf-sha256"
KEX_ALG = "x25519-ed25519"
_KEYWRAP_INFO = b"novae-linguae/v0.2/xchacha20poly1305/key-wrap"


def _b64(b: bytes) -> str:
    return base64.b64encode(b).decode("ascii")


def _unb64(s: str) -> bytes:
    return base64.b64decode(s)


def _derive_kek(shared: bytes, epk: bytes, recipient_xpub: bytes) -> bytes:
    return hkdf_sha256(shared, epk + recipient_xpub, _KEYWRAP_INFO, 32)


def seal(plaintext: bytes, recipient_dids, aad: bytes = b"", *, rng=random_bytes,
         cek: bytes | None = None, ephemeral_secret: bytes | None = None) -> dict:
    """Seal ``plaintext`` to one or more ``did:nova`` recipients. Returns an envelope dict.

    ``cek`` (content-encryption key) may be supplied to reuse a per-conversation symmetric key
    across messages; otherwise a fresh random one is generated. ``rng`` and ``ephemeral_secret``
    are injection points for deterministic vectors; real use leaves them at their random defaults.
    """
    dids = list(recipient_dids)
    if not dids:
        raise ValueError("at least one recipient DID is required")
    cek = cek if cek is not None else rng(32)
    esk = ephemeral_secret if ephemeral_secret is not None else rng(32)
    epk = x25519_base(esk)
    nonce = rng(24)
    ciphertext = xchacha20poly1305_seal(cek, nonce, plaintext, aad)

    recipients = []
    for did in dids:
        rxpub = x25519_pub_from_did(did)
        kek = _derive_kek(x25519(esk, rxpub), epk, rxpub)
        wrap_nonce = rng(24)
        wrapped = xchacha20poly1305_seal(kek, wrap_nonce, cek, did.encode("utf-8"))
        recipients.append({
            "to": did,
            "wrap_nonce": _b64(wrap_nonce),
            "wrapped_key": _b64(wrapped),
        })

    envelope = {
        "v": ENVELOPE_VERSION,
        "enc": ENC_ALG,
        "kdf": KDF_ALG,
        "kex": KEX_ALG,
        "epk": _b64(epk),
        "nonce": _b64(nonce),
        "ciphertext": _b64(ciphertext),
        "recipients": recipients,
    }
    if aad:
        envelope["aad"] = _b64(aad)
    return envelope


def open_envelope(envelope: dict, recipient_did: str, x25519_secret: bytes) -> bytes:
    """Recover the plaintext from ``envelope`` for the recipient holding ``x25519_secret``."""
    if envelope.get("enc") != ENC_ALG or envelope.get("kdf") != KDF_ALG or envelope.get("kex") != KEX_ALG:
        raise ValueError("unsupported envelope algorithms")
    entry = next((r for r in envelope.get("recipients", []) if r.get("to") == recipient_did), None)
    if entry is None:
        raise ValueError(f"envelope is not addressed to {recipient_did}")
    epk = _unb64(envelope["epk"])
    aad = _unb64(envelope["aad"]) if envelope.get("aad") else b""
    rxpub = x25519_base(x25519_secret)
    kek = _derive_kek(x25519(x25519_secret, epk), epk, rxpub)
    cek = xchacha20poly1305_open(kek, _unb64(entry["wrap_nonce"]),
                                 _unb64(entry["wrapped_key"]), recipient_did.encode("utf-8"))
    return xchacha20poly1305_open(cek, _unb64(envelope["nonce"]), _unb64(envelope["ciphertext"]), aad)


def open_with_seed(envelope: dict, recipient_did: str, user_seed: str) -> bytes:
    """Convenience: derive the recipient's X25519 secret from its user seed, then open."""
    xsk, _ = x25519_keypair_from_user_seed(user_seed)
    return open_envelope(envelope, recipient_did, xsk)
