"""ml_kem — a pure-Python, standard-library-only ML-KEM-768 reference (FIPS 203 final).

ML-KEM (Module-Lattice-based Key-Encapsulation Mechanism, formerly Kyber) is the
NIST post-quantum KEM standardized in FIPS 203. This module implements the **768**
parameter set (k=3, η1=η2=2, du=10, dv=4) used by Nova Locutio's post-quantum hybrid
key agreement (``kex: "x25519-mlkem768"``; see ``spec/encryption.md``).

Like the rest of ``tooling/crypto-python``, this is a reference implementation chosen for
clarity and verifiability, **not** for constant-time / side-channel resistance — do not use
it to protect real secrets on a hostile host. Conformance is byte-for-byte: the public API
(``keygen_derand`` / ``encaps_derand`` / ``decaps``) reproduces the NIST-validated RustCrypto
``ml-kem`` crate exactly, and is checked against a FIPS-203-final known-answer vector in
``tests/test_ml_kem.py``. A second implementation must reproduce those same bytes.

Only ``hashlib`` is used (SHA3-256/512 and SHAKE-128/256 are the only symmetric primitives
ML-KEM needs — no Keccak to vendor). The three deterministic entry points take the raw
randomness (``d``, ``z``, ``m``) so envelopes and conformance vectors are reproducible; a
real caller supplies fresh ``os.urandom`` bytes for each.
"""

from __future__ import annotations

import hashlib

# ---------------------------------------------------------------------------
# Parameters (ML-KEM-768, FIPS 203 §8 / Table 2).
# ---------------------------------------------------------------------------

N = 256
Q = 3329
K = 3
ETA1 = 2
ETA2 = 2
DU = 10
DV = 4

# Serialized sizes (bytes).
EK_SIZE = 384 * K + 32          # 1184: k encoded NTT vectors (12-bit) + rho
DK_SIZE = 768 * K + 96          # 2400: dk_pke + ek + H(ek) + z
CT_SIZE = 32 * (DU * K + DV)    # 1088: c1 (k * du-bit) + c2 (dv-bit)
SS_SIZE = 32
SEED_SIZE = 64                  # d || z


# ---------------------------------------------------------------------------
# NTT machinery. zetas[i] = 17^BitRev7(i) mod q; gammas[i] = 17^(2*BitRev7(i)+1) mod q.
# ---------------------------------------------------------------------------

def _bitrev7(i: int) -> int:
    return int(f"{i:07b}"[::-1], 2)


_ZETAS = [pow(17, _bitrev7(i), Q) for i in range(128)]
_GAMMAS = [pow(17, 2 * _bitrev7(i) + 1, Q) for i in range(128)]


def _ntt(f):
    """NTT (FIPS 203 Algorithm 9). f: 256 coefficients in [0,q). Returns the NTT representation."""
    fh = list(f)
    i = 1
    length = 128
    while length >= 2:
        start = 0
        while start < 256:
            zeta = _ZETAS[i]
            i += 1
            for j in range(start, start + length):
                t = (zeta * fh[j + length]) % Q
                fh[j + length] = (fh[j] - t) % Q
                fh[j] = (fh[j] + t) % Q
            start += 2 * length
        length //= 2
    return fh


def _intt(fh):
    """Inverse NTT (FIPS 203 Algorithm 10)."""
    f = list(fh)
    i = 127
    length = 2
    while length <= 128:
        start = 0
        while start < 256:
            zeta = _ZETAS[i]
            i -= 1
            for j in range(start, start + length):
                t = f[j]
                f[j] = (t + f[j + length]) % Q
                f[j + length] = (zeta * (f[j + length] - t)) % Q
            start += 2 * length
        length *= 2
    return [(x * 3303) % Q for x in f]   # 3303 = 128^{-1} mod q


def _multiply_ntts(f, g):
    """Multiply two NTT-domain polynomials (FIPS 203 Algorithm 11/12)."""
    h = [0] * 256
    for i in range(128):
        a0, a1 = f[2 * i], f[2 * i + 1]
        b0, b1 = g[2 * i], g[2 * i + 1]
        gamma = _GAMMAS[i]
        h[2 * i] = (a0 * b0 + a1 * b1 * gamma) % Q
        h[2 * i + 1] = (a0 * b1 + a1 * b0) % Q
    return h


def _poly_add(a, b):
    return [(x + y) % Q for x, y in zip(a, b)]


def _poly_sub(a, b):
    return [(x - y) % Q for x, y in zip(a, b)]


# ---------------------------------------------------------------------------
# Compression and byte (de)serialization (FIPS 203 §4.2.1).
# ---------------------------------------------------------------------------

def _compress(x: int, d: int) -> int:
    return (((x << d) + (Q >> 1)) // Q) & ((1 << d) - 1)   # round(2^d/q * x) mod 2^d


def _decompress(y: int, d: int) -> int:
    return (y * Q + (1 << (d - 1))) >> d                   # round(q/2^d * y)


def _bits_to_bytes(bits):
    out = bytearray(len(bits) // 8)
    for i, bit in enumerate(bits):
        out[i // 8] |= bit << (i % 8)
    return bytes(out)


def _bytes_to_bits(data):
    bits = []
    for byte in data:
        for j in range(8):
            bits.append((byte >> j) & 1)
    return bits


def _byte_encode(coeffs, d):
    bits = []
    for c in coeffs:
        for b in range(d):
            bits.append((c >> b) & 1)
    return _bits_to_bytes(bits)


def _byte_decode(data, d):
    bits = _bytes_to_bits(data)
    m = (1 << d) if d < 12 else Q
    coeffs = []
    for i in range(256):
        c = 0
        for b in range(d):
            c |= bits[i * d + b] << b
        coeffs.append(c % m)
    return coeffs


# ---------------------------------------------------------------------------
# Sampling and hashing (FIPS 203 §4.1 / §4.2.2).
# ---------------------------------------------------------------------------

def _G(data: bytes):
    h = hashlib.sha3_512(data).digest()
    return h[:32], h[32:]


def _H(data: bytes) -> bytes:
    return hashlib.sha3_256(data).digest()


def _J(data: bytes) -> bytes:
    return hashlib.shake_256(data).digest(32)


def _prf(eta: int, s: bytes, b: int) -> bytes:
    return hashlib.shake_256(s + bytes([b])).digest(64 * eta)


def _xof_reader(seed: bytes):
    """A growable SHAKE-128 squeeze. Returns read(k) yielding successive output bytes.

    hashlib's SHAKE has no streaming squeeze, so we re-`digest()` an increasing length (the
    prefix is stable). SampleNTT normally needs ~3*256 bytes; the rare "unlucky" case just
    grows the buffer further."""
    h = hashlib.shake_128(seed)
    state = {"buf": b"", "have": 0, "pos": 0}

    def read(k: int) -> bytes:
        need = state["pos"] + k
        if need > state["have"]:
            new = max(need, state["have"] + 168)
            state["buf"] = h.digest(new)
            state["have"] = new
        out = state["buf"][state["pos"]:state["pos"] + k]
        state["pos"] += k
        return out

    return read


def _sample_ntt(seed: bytes):
    """SampleNTT (FIPS 203 Algorithm 7): 256 NTT-domain coeffs by rejection from SHAKE-128(seed)."""
    read = _xof_reader(seed)
    a = []
    while len(a) < 256:
        c = read(3)
        d1 = c[0] + 256 * (c[1] & 0xF)
        d2 = (c[1] >> 4) + 16 * c[2]
        if d1 < Q:
            a.append(d1)
        if d2 < Q and len(a) < 256:
            a.append(d2)
    return a


def _sample_poly_cbd(data: bytes, eta: int):
    """SamplePolyCBD_eta (FIPS 203 Algorithm 8): centered binomial from 64*eta bytes."""
    bits = _bytes_to_bits(data)
    f = []
    for i in range(256):
        x = sum(bits[2 * i * eta + j] for j in range(eta))
        y = sum(bits[2 * i * eta + eta + j] for j in range(eta))
        f.append((x - y) % Q)
    return f


def _gen_matrix(rho: bytes):
    """Generate the k*k matrix A-hat. A_hat[i][j] = SampleNTT(rho || j || i) (FIPS 203 final order)."""
    return [[_sample_ntt(rho + bytes([j, i])) for j in range(K)] for i in range(K)]


# ---------------------------------------------------------------------------
# K-PKE (FIPS 203 §5): the IND-CPA public-key scheme underneath ML-KEM.
# ---------------------------------------------------------------------------

def _kpke_keygen(d: bytes):
    rho, sigma = _G(d + bytes([K]))
    a_hat = _gen_matrix(rho)
    nonce = 0
    s = []
    for _ in range(K):
        s.append(_sample_poly_cbd(_prf(ETA1, sigma, nonce), ETA1))
        nonce += 1
    e = []
    for _ in range(K):
        e.append(_sample_poly_cbd(_prf(ETA1, sigma, nonce), ETA1))
        nonce += 1
    s_hat = [_ntt(p) for p in s]
    e_hat = [_ntt(p) for p in e]
    t_hat = []
    for i in range(K):
        acc = [0] * 256
        for j in range(K):
            acc = _poly_add(acc, _multiply_ntts(a_hat[i][j], s_hat[j]))
        t_hat.append(_poly_add(acc, e_hat[i]))
    ek_pke = b"".join(_byte_encode(t_hat[i], 12) for i in range(K)) + rho
    dk_pke = b"".join(_byte_encode(s_hat[i], 12) for i in range(K))
    return ek_pke, dk_pke


def _kpke_encrypt(ek_pke: bytes, m: bytes, rand: bytes) -> bytes:
    t_hat = [_byte_decode(ek_pke[384 * i:384 * (i + 1)], 12) for i in range(K)]
    rho = ek_pke[384 * K:384 * K + 32]
    a_hat = _gen_matrix(rho)
    nonce = 0
    y = []
    for _ in range(K):
        y.append(_sample_poly_cbd(_prf(ETA1, rand, nonce), ETA1))
        nonce += 1
    e1 = []
    for _ in range(K):
        e1.append(_sample_poly_cbd(_prf(ETA2, rand, nonce), ETA2))
        nonce += 1
    e2 = _sample_poly_cbd(_prf(ETA2, rand, nonce), ETA2)
    y_hat = [_ntt(p) for p in y]
    u = []
    for i in range(K):
        acc = [0] * 256
        for j in range(K):
            acc = _poly_add(acc, _multiply_ntts(a_hat[j][i], y_hat[j]))   # A-hat^T
        u.append(_poly_add(_intt(acc), e1[i]))
    mu = [_decompress(b, 1) for b in _byte_decode(m, 1)]
    acc = [0] * 256
    for j in range(K):
        acc = _poly_add(acc, _multiply_ntts(t_hat[j], y_hat[j]))
    v = _poly_add(_poly_add(_intt(acc), e2), mu)
    c1 = b"".join(_byte_encode([_compress(x, DU) for x in u[i]], DU) for i in range(K))
    c2 = _byte_encode([_compress(x, DV) for x in v], DV)
    return c1 + c2


def _kpke_decrypt(dk_pke: bytes, c: bytes) -> bytes:
    c1, c2 = c[:32 * DU * K], c[32 * DU * K:]
    u = [[_decompress(x, DU) for x in _byte_decode(c1[32 * DU * i:32 * DU * (i + 1)], DU)]
         for i in range(K)]
    v = [_decompress(x, DV) for x in _byte_decode(c2, DV)]
    s_hat = [_byte_decode(dk_pke[384 * i:384 * (i + 1)], 12) for i in range(K)]
    acc = [0] * 256
    u_hat = [_ntt(ui) for ui in u]
    for j in range(K):
        acc = _poly_add(acc, _multiply_ntts(s_hat[j], u_hat[j]))
    w = _poly_sub(v, _intt(acc))
    return _byte_encode([_compress(x, 1) for x in w], 1)


# ---------------------------------------------------------------------------
# ML-KEM (FIPS 203 §6): the IND-CCA2 KEM. Deterministic ("internal") entry points.
# ---------------------------------------------------------------------------

def keygen_derand(d: bytes, z: bytes):
    """ML-KEM.KeyGen_internal (FIPS 203 Algorithm 16). Returns (ek, dk)."""
    if len(d) != 32 or len(z) != 32:
        raise ValueError("d and z must be 32 bytes each")
    ek_pke, dk_pke = _kpke_keygen(d)
    dk = dk_pke + ek_pke + _H(ek_pke) + z
    return ek_pke, dk


def encaps_derand(ek: bytes, m: bytes):
    """ML-KEM.Encaps_internal (FIPS 203 Algorithm 17). Returns (shared_key, ciphertext)."""
    if len(ek) != EK_SIZE:
        raise ValueError(f"encapsulation key must be {EK_SIZE} bytes")
    if len(m) != 32:
        raise ValueError("m must be 32 bytes")
    shared, rand = _G(m + _H(ek))
    c = _kpke_encrypt(ek, m, rand)
    return shared, c


def decaps(dk: bytes, c: bytes) -> bytes:
    """ML-KEM.Decaps_internal (FIPS 203 Algorithm 18) with implicit rejection. Returns shared_key."""
    if len(dk) != DK_SIZE:
        raise ValueError(f"decapsulation key must be {DK_SIZE} bytes")
    if len(c) != CT_SIZE:
        raise ValueError(f"ciphertext must be {CT_SIZE} bytes")
    dk_pke = dk[:384 * K]
    ek = dk[384 * K:768 * K + 32]
    h = dk[768 * K + 32:768 * K + 64]
    z = dk[768 * K + 64:768 * K + 96]
    m2 = _kpke_decrypt(dk_pke, c)
    shared, rand = _G(m2 + h)
    k_bar = _J(z + c)
    c2 = _kpke_encrypt(ek, m2, rand)
    if c != c2:
        shared = k_bar   # implicit rejection (constant-time in a real impl; not here)
    return shared
