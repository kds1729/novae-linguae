//! Production-hardened encrypted-envelope seal/open, built on vetted constant-time crates
//! (x25519-dalek, curve25519-dalek, chacha20poly1305, hkdf+sha2). This is the hardened counterpart
//! to the pure-Python reference in tooling/crypto-python/nl_crypto.py, and it reproduces the same
//! bytes: conformance is defined by spec/conformance/encryption.json, NOT by either implementation.
//! See spec/encryption.md and spec/crypto-conformance.md.
//!
//! Construction (spec/encryption.md): a hybrid multi-recipient sealed box. The payload is encrypted
//! once under a random CEK with XChaCha20-Poly1305; the CEK is wrapped per recipient under a KEK
//! derived by HKDF-SHA-256 from an X25519 ECDH against the recipient's did:nova key (mapped from
//! Ed25519 to Curve25519), with the recipient DID as wrap AAD.

use anyhow::{anyhow, Context, Result};
use base64::Engine;
use chacha20poly1305::aead::{Aead, Payload};
use chacha20poly1305::{KeyInit, XChaCha20Poly1305, XNonce};
use curve25519_dalek::edwards::CompressedEdwardsY;
use hkdf::Hkdf;
use ml_kem::kem::Decapsulate;
use ml_kem::{B32, Ciphertext, DecapsulationKey, EncapsulationKey, KeyExport, MlKem768, Seed, TryKeyInit};
use serde_json::{json, Value};
use sha2::{Digest, Sha256, Sha512};
use std::collections::BTreeMap;
use x25519_dalek::{PublicKey, StaticSecret};

const ENVELOPE_VERSION: &str = "0.2";
const ENVELOPE_VERSION_PQ: &str = "0.3";
const ENC_ALG: &str = "xchacha20poly1305";
const KDF_ALG: &str = "hkdf-sha256";
const KEX_ALG: &str = "x25519-ed25519";
// Post-quantum hybrid key agreement (v0.3, spec/encryption.md): X25519 ECDH + an ML-KEM-768
// encapsulation against the recipient's published key, with the KEK derived from both shared secrets.
const KEX_MLKEM: &str = "x25519-mlkem768";
const KEYWRAP_INFO: &[u8] = b"novae-linguae/v0.2/xchacha20poly1305/key-wrap";
const HYBRID_WRAP_INFO: &[u8] = b"novae-linguae/v0.3/x25519-mlkem768/key-wrap";
const MLKEM_KEYGEN_LABEL: &[u8] = b"novae-linguae/v0.3/ml-kem-768/keygen";
// Stealth addressing (v0.3): wrap bound to a fixed label instead of the recipient DID, so the
// recipient set can be omitted and recovered by trial-decryption.
const STEALTH_WRAP_AAD: &[u8] = b"novae-linguae/v0.3/stealth/key-wrap";

fn b64(b: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(b)
}
fn unb64(s: &str) -> Result<Vec<u8>> {
    base64::engine::general_purpose::STANDARD
        .decode(s)
        .map_err(|e| anyhow!("base64 decode: {e}"))
}

/// A byte source. `random()` uses the OS CSPRNG; `seeded()` is the BLAKE3(seed‖counter) deterministic
/// source the conformance vectors are built with (reproducible, NOT for production).
pub enum Rng {
    Os,
    Seeded { seed: Vec<u8>, n: u64 },
}

impl Rng {
    pub fn seeded(seed: Vec<u8>) -> Self {
        Rng::Seeded { seed, n: 0 }
    }
    fn fill(&mut self, len: usize) -> Vec<u8> {
        match self {
            Rng::Os => {
                use rand_core::RngCore;
                let mut buf = vec![0u8; len];
                rand_core::OsRng.fill_bytes(&mut buf);
                buf
            }
            Rng::Seeded { seed, n } => {
                let mut out = Vec::with_capacity(len);
                while out.len() < len {
                    let mut block = seed.clone();
                    block.extend_from_slice(&n.to_le_bytes());
                    out.extend_from_slice(blake3::hash(&block).as_bytes());
                    *n += 1;
                }
                out.truncate(len);
                out
            }
        }
    }
}

fn to_arr32(b: &[u8]) -> Result<[u8; 32]> {
    b.try_into().map_err(|_| anyhow!("expected 32 bytes, got {}", b.len()))
}

/// Map a did:nova:<64-hex> Ed25519 public key to its X25519 public key (the Edwards→Montgomery
/// birational map u = (1+y)/(1-y)).
pub fn xpub_from_did(did: &str) -> Result<[u8; 32]> {
    let hex = did
        .strip_prefix("did:nova:")
        .ok_or_else(|| anyhow!("not a did:nova DID: {did}"))?;
    let ed_pub = decode_hex32(hex)?;
    let comp = CompressedEdwardsY::from_slice(&ed_pub).map_err(|e| anyhow!("bad Edwards point: {e}"))?;
    let point = comp
        .decompress()
        .ok_or_else(|| anyhow!("Ed25519 public key does not decompress"))?;
    Ok(point.to_montgomery().to_bytes())
}

fn decode_hex32(hex: &str) -> Result<[u8; 32]> {
    if hex.len() != 64 {
        return Err(anyhow!("expected 64 hex chars, got {}", hex.len()));
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16)
            .map_err(|e| anyhow!("bad hex: {e}"))?;
    }
    Ok(out)
}

/// The recipient's X25519 secret derived from its user seed, matching the reference:
/// ed25519_seed = BLAKE3(user_seed); x25519_secret = clamp(SHA-512(ed25519_seed)[..32]).
pub fn x25519_secret_from_user_seed(user_seed: &str) -> [u8; 32] {
    let ed_seed = blake3::hash(user_seed.as_bytes());
    let mut h = [0u8; 32];
    h.copy_from_slice(&Sha512::digest(ed_seed.as_bytes())[..32]);
    h[0] &= 248;
    h[31] &= 127;
    h[31] |= 64;
    h
}

fn xchacha_seal(key: &[u8; 32], nonce: &[u8], pt: &[u8], aad: &[u8]) -> Result<Vec<u8>> {
    let cipher = XChaCha20Poly1305::new(key.into());
    cipher
        .encrypt(XNonce::from_slice(nonce), Payload { msg: pt, aad })
        .map_err(|e| anyhow!("xchacha20poly1305 seal: {e}"))
}

fn xchacha_open(key: &[u8; 32], nonce: &[u8], ct: &[u8], aad: &[u8]) -> Result<Vec<u8>> {
    let cipher = XChaCha20Poly1305::new(key.into());
    cipher
        .decrypt(XNonce::from_slice(nonce), Payload { msg: ct, aad })
        .map_err(|e| anyhow!("xchacha20poly1305 open (auth failed): {e}"))
}

fn derive_kek(shared: &[u8; 32], epk: &[u8; 32], rxpub: &[u8; 32]) -> [u8; 32] {
    let mut salt = Vec::with_capacity(64);
    salt.extend_from_slice(epk);
    salt.extend_from_slice(rxpub);
    let hk = Hkdf::<Sha256>::new(Some(&salt), shared);
    let mut okm = [0u8; 32];
    hk.expand(KEYWRAP_INFO, &mut okm).expect("32 is a valid HKDF length");
    okm
}

/// Hybrid KEK (spec/encryption.md): HKDF over the X25519 and ML-KEM shared secrets concatenated, with
/// the same `epk ‖ recipient_xpub` salt as v0.2 and a distinct info label. Secure if either holds.
fn derive_kek_hybrid(ecdh_ss: &[u8; 32], mlkem_ss: &[u8], epk: &[u8; 32], rxpub: &[u8; 32]) -> [u8; 32] {
    let mut ikm = Vec::with_capacity(32 + mlkem_ss.len());
    ikm.extend_from_slice(ecdh_ss);
    ikm.extend_from_slice(mlkem_ss);
    let mut salt = Vec::with_capacity(64);
    salt.extend_from_slice(epk);
    salt.extend_from_slice(rxpub);
    let hk = Hkdf::<Sha256>::new(Some(&salt), &ikm);
    let mut okm = [0u8; 32];
    hk.expand(HYBRID_WRAP_INFO, &mut okm).expect("32 is a valid HKDF length");
    okm
}

/// An agent's ML-KEM-768 keypair from its user seed (matching tooling/crypto-python): the 64-byte
/// FIPS 203 seed is two domain-separated BLAKE3 draws over `user_seed ‖ label`. Returns the
/// decapsulation key and the serialized 1184-byte encapsulation (public) key.
pub fn mlkem_keypair_from_user_seed(user_seed: &str) -> (DecapsulationKey<MlKem768>, Vec<u8>) {
    let mut base = Vec::from(user_seed.as_bytes());
    base.extend_from_slice(MLKEM_KEYGEN_LABEL);
    let mut b0 = base.clone();
    b0.push(0x00);
    let mut b1 = base;
    b1.push(0x01);
    let mut seed = Seed::default();
    seed[..32].copy_from_slice(blake3::hash(&b0).as_bytes());
    seed[32..].copy_from_slice(blake3::hash(&b1).as_bytes());
    let dk = DecapsulationKey::<MlKem768>::from_seed(seed);
    let ek = dk.encapsulation_key().to_bytes()[..].to_vec();
    (dk, ek)
}

/// Deterministically encapsulate to a recipient's serialized ML-KEM-768 public key with message `m`.
/// Returns (shared_secret, ciphertext).
fn mlkem_encaps(ek_bytes: &[u8], m: &[u8; 32]) -> Result<(Vec<u8>, Vec<u8>)> {
    let ek = EncapsulationKey::<MlKem768>::new_from_slice(ek_bytes)
        .map_err(|_| anyhow!("invalid ML-KEM-768 encapsulation key"))?;
    let (ct, k) = ek.encapsulate_deterministic(&B32::from(*m));
    Ok((k[..].to_vec(), ct[..].to_vec()))
}

/// Decapsulate an ML-KEM-768 ciphertext to its shared secret (implicit rejection on failure).
fn mlkem_decaps(dk: &DecapsulationKey<MlKem768>, ct_bytes: &[u8]) -> Result<Vec<u8>> {
    let ct = Ciphertext::<MlKem768>::try_from(ct_bytes)
        .map_err(|_| anyhow!("malformed ML-KEM-768 ciphertext"))?;
    Ok(dk.decapsulate(&ct)[..].to_vec())
}

/// Verify a DID document and return (id, serialized ML-KEM-768 public key). The Ed25519 signature must
/// be valid over the JCS-canonical document minus `signature`, against the key embedded in `id`
/// (spec/did-document.md). Mirrors the Python reference's `mlkem_pub_from_did_document`.
pub fn mlkem_pub_from_did_document(doc: &Value) -> Result<(String, Vec<u8>)> {
    let id = doc.get("id").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("DID document missing id"))?;
    let sig_str = doc.get("signature").and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("DID document is unsigned"))?;
    let mut body = doc.clone();
    body.as_object_mut().ok_or_else(|| anyhow!("DID document must be a JSON object"))?.remove("signature");
    let msg = crate::canonicalize(&body)?;
    let vk = crate::pubkey_from_did_nova(id)?;
    let sig = crate::parse_signature(sig_str)?;
    vk.verify_strict(&msg, &sig).map_err(|_| anyhow!("DID document signature is invalid"))?;

    let entries = doc.get("keyAgreement").and_then(|v| v.as_array())
        .ok_or_else(|| anyhow!("DID document has no keyAgreement"))?;
    for e in entries {
        if e.get("type").and_then(|v| v.as_str()) == Some("ML-KEM-768") {
            let pk = unb64(e.get("publicKeyBase64").and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("keyAgreement entry missing publicKeyBase64"))?)?;
            if pk.len() != 1184 {
                return Err(anyhow!("ML-KEM-768 public key has wrong length ({})", pk.len()));
            }
            return Ok((id.to_string(), pk));
        }
    }
    Err(anyhow!("DID document has no ML-KEM-768 key-agreement key"))
}

/// Seal `plaintext` to one or more did:nova recipients, returning an envelope JSON value matching
/// encrypted-envelope.schema.json. `rng` is the byte source (use `Rng::Os` for real use; `Rng::seeded`
/// reproduces conformance vectors).
///
/// When `mlkem_keys` (a `{did: serialized ML-KEM public key}` map) is supplied, key agreement is the
/// post-quantum hybrid `kex: x25519-mlkem768`: each recipient also gets an ML-KEM-768 encapsulation
/// (carried as `kem_ct`) and the KEK mixes both shared secrets; the envelope version becomes `0.3`.
///
/// RNG draw order (part of the byte-for-byte contract): cek, esk, nonce, then per recipient — for the
/// hybrid kex an ML-KEM `m` (32 bytes) precedes each `wrap_nonce` (24 bytes); for v0.2 just `wrap_nonce`.
pub fn seal(
    plaintext: &[u8],
    recipient_dids: &[String],
    aad: &[u8],
    rng: &mut Rng,
    stealth: bool,
    mlkem_keys: Option<&BTreeMap<String, Vec<u8>>>,
) -> Result<Value> {
    if recipient_dids.is_empty() {
        return Err(anyhow!("at least one recipient DID is required"));
    }
    let hybrid = mlkem_keys.is_some();
    let cek = to_arr32(&rng.fill(32))?;
    let esk = to_arr32(&rng.fill(32))?;
    let esk_secret = StaticSecret::from(esk);
    let epk = PublicKey::from(&esk_secret).to_bytes();
    let nonce = rng.fill(24);
    let ciphertext = xchacha_seal(&cek, &nonce, plaintext, aad)?;

    let mut recipients = Vec::new();
    for did in recipient_dids {
        let rxpub = xpub_from_did(did)?;
        let ecdh = esk_secret.diffie_hellman(&PublicKey::from(rxpub)).to_bytes();
        let (kek, kem_ct) = match mlkem_keys {
            Some(keys) => {
                let ek = keys.get(did).ok_or_else(|| anyhow!("no ML-KEM key for recipient {did}"))?;
                let m = to_arr32(&rng.fill(32))?;
                let (mlkem_ss, ct) = mlkem_encaps(ek, &m)?;
                (derive_kek_hybrid(&ecdh, &mlkem_ss, &epk, &rxpub), Some(ct))
            }
            None => (derive_kek(&ecdh, &epk, &rxpub), None),
        };
        let wrap_nonce = rng.fill(24);
        let wrap_aad: &[u8] = if stealth { STEALTH_WRAP_AAD } else { did.as_bytes() };
        let wrapped = xchacha_seal(&kek, &wrap_nonce, &cek, wrap_aad)?;
        let mut entry = serde_json::Map::new();
        if !stealth {
            entry.insert("to".into(), json!(did));
        }
        if let Some(ct) = &kem_ct {
            entry.insert("kem_ct".into(), json!(b64(ct)));
        }
        entry.insert("wrap_nonce".into(), json!(b64(&wrap_nonce)));
        entry.insert("wrapped_key".into(), json!(b64(&wrapped)));
        recipients.push(Value::Object(entry));
    }

    let mut envelope = json!({
        "v": if hybrid { ENVELOPE_VERSION_PQ } else { ENVELOPE_VERSION },
        "enc": ENC_ALG,
        "kdf": KDF_ALG,
        "kex": if hybrid { KEX_MLKEM } else { KEX_ALG },
        "epk": b64(&epk),
        "nonce": b64(&nonce),
        "ciphertext": b64(&ciphertext),
        "recipients": recipients,
    });
    if stealth {
        envelope["addressing"] = json!("stealth");
    }
    if !aad.is_empty() {
        envelope["aad"] = json!(b64(aad));
    }
    Ok(envelope)
}

/// Recover the plaintext from `envelope` for the recipient holding `x25519_secret`. For a
/// `kex: x25519-mlkem768` envelope the recipient's ML-KEM-768 decapsulation key (`mlkem_dk`) is also
/// required; for v0.2 envelopes it is ignored.
pub fn open(
    envelope: &Value,
    recipient_did: &str,
    x25519_secret: &[u8; 32],
    mlkem_dk: Option<&DecapsulationKey<MlKem768>>,
) -> Result<Vec<u8>> {
    let kex = envelope.get("kex").and_then(|v| v.as_str());
    let alg_ok = envelope.get("enc").and_then(|v| v.as_str()) == Some(ENC_ALG)
        && envelope.get("kdf").and_then(|v| v.as_str()) == Some(KDF_ALG)
        && (kex == Some(KEX_ALG) || kex == Some(KEX_MLKEM));
    if !alg_ok {
        return Err(anyhow!("unsupported envelope algorithms"));
    }
    let hybrid = kex == Some(KEX_MLKEM);
    if hybrid && mlkem_dk.is_none() {
        return Err(anyhow!("this envelope uses x25519-mlkem768; the recipient ML-KEM secret is required"));
    }
    let recipients = envelope
        .get("recipients")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow!("envelope missing recipients"))?;

    let epk = to_arr32(&unb64(envelope["epk"].as_str().context("epk")?)?)?;
    let aad = match envelope.get("aad").and_then(|v| v.as_str()) {
        Some(s) => unb64(s)?,
        None => Vec::new(),
    };
    let secret = StaticSecret::from(*x25519_secret);
    let rxpub = PublicKey::from(&secret).to_bytes();
    let ecdh = secret.diffie_hellman(&PublicKey::from(epk)).to_bytes();
    let payload_nonce = unb64(envelope["nonce"].as_str().context("nonce")?)?;
    let payload_ct = unb64(envelope["ciphertext"].as_str().context("ciphertext")?)?;

    // The KEK for an entry: hybrid mixes the ML-KEM shared secret decapsulated from that entry's kem_ct.
    let kek_for = |entry: &Value| -> Result<[u8; 32]> {
        if hybrid {
            let ct = unb64(entry["kem_ct"].as_str().context("kem_ct")?)?;
            let mlkem_ss = mlkem_decaps(mlkem_dk.unwrap(), &ct)?;
            Ok(derive_kek_hybrid(&ecdh, &mlkem_ss, &epk, &rxpub))
        } else {
            Ok(derive_kek(&ecdh, &epk, &rxpub))
        }
    };

    let stealth = envelope.get("addressing").and_then(|v| v.as_str()) == Some("stealth");
    if stealth {
        // The recipient set is hidden: trial-decrypt each entry's wrap with our KEK and the fixed
        // stealth label; the one that authenticates yields the CEK.
        for entry in recipients {
            let kek = match kek_for(entry) {
                Ok(k) => k,
                Err(_) => continue,
            };
            let wn = unb64(entry["wrap_nonce"].as_str().context("wrap_nonce")?)?;
            let wk = unb64(entry["wrapped_key"].as_str().context("wrapped_key")?)?;
            if let Ok(cek_vec) = xchacha_open(&kek, &wn, &wk, STEALTH_WRAP_AAD) {
                let cek = to_arr32(&cek_vec)?;
                return xchacha_open(&cek, &payload_nonce, &payload_ct, &aad);
            }
        }
        return Err(anyhow!("envelope is not addressed to this recipient (stealth)"));
    }

    let entry = recipients
        .iter()
        .find(|r| r.get("to").and_then(|v| v.as_str()) == Some(recipient_did))
        .ok_or_else(|| anyhow!("envelope is not addressed to {recipient_did}"))?;
    let cek_vec = xchacha_open(
        &kek_for(entry)?,
        &unb64(entry["wrap_nonce"].as_str().context("wrap_nonce")?)?,
        &unb64(entry["wrapped_key"].as_str().context("wrapped_key")?)?,
        recipient_did.as_bytes(),
    )?;
    let cek = to_arr32(&cek_vec)?;
    xchacha_open(&cek, &payload_nonce, &payload_ct, &aad)
}

// ---- portable conformance harness -------------------------------------------------------------

fn hex(s: &str) -> Result<Vec<u8>> {
    if s.len() % 2 != 0 {
        return Err(anyhow!("odd-length hex"));
    }
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).map_err(|e| anyhow!("hex: {e}")))
        .collect()
}

fn field<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v.get(k).and_then(|x| x.as_str()).ok_or_else(|| anyhow!("missing field {k}"))
}

/// Run the encryption conformance vectors (spec/conformance/encryption.json), checking each primitive
/// and the deterministic envelope (reseal byte-for-byte + open). This is the language-neutral contract
/// a hardened implementation must satisfy; see spec/crypto-conformance.md.
pub fn run_conformance(vectors: &Value) -> Result<()> {
    let prim = vectors.get("primitives").ok_or_else(|| anyhow!("no primitives"))?;

    // X25519 (RFC 7748): dalek bare scalar mult.
    {
        let v = &prim["x25519"][0];
        let out = x25519_dalek::x25519(to_arr32(&hex(field(v, "scalar")?)?)?, to_arr32(&hex(field(v, "u")?)?)?);
        if out.to_vec() != hex(field(v, "out")?)? {
            return Err(anyhow!("x25519 vector mismatch"));
        }
    }
    // ChaCha20-Poly1305 (RFC 8439, 12-byte nonce).
    {
        use chacha20poly1305::ChaCha20Poly1305;
        let v = &prim["chacha20poly1305"];
        let cipher = ChaCha20Poly1305::new(to_arr32(&hex(field(v, "key")?)?)?.as_slice().into());
        let nonce = hex(field(v, "nonce")?)?;
        let ct = cipher
            .encrypt(
                nonce.as_slice().into(),
                Payload { msg: &hex(field(v, "plaintext_hex")?)?, aad: &hex(field(v, "aad")?)? },
            )
            .map_err(|e| anyhow!("chacha20poly1305: {e}"))?;
        if ct != hex(field(v, "ciphertext_and_tag")?)? {
            return Err(anyhow!("chacha20poly1305 vector mismatch"));
        }
    }
    // XChaCha20-Poly1305 (draft-irtf-cfrg-xchacha, 24-byte nonce) — exercises HChaCha20 internally.
    {
        let v = &prim["xchacha20poly1305"];
        let ct = xchacha_seal(
            &to_arr32(&hex(field(v, "key")?)?)?,
            &hex(field(v, "nonce")?)?,
            &hex(field(v, "plaintext_hex")?)?,
            &hex(field(v, "aad")?)?,
        )?;
        if ct != hex(field(v, "ciphertext_and_tag")?)? {
            return Err(anyhow!("xchacha20poly1305 vector mismatch"));
        }
    }
    // HKDF-SHA-256 (RFC 5869).
    {
        let v = &prim["hkdf_sha256"];
        let hk = Hkdf::<Sha256>::new(Some(&hex(field(v, "salt")?)?), &hex(field(v, "ikm")?)?);
        let len = v["length"].as_u64().ok_or_else(|| anyhow!("hkdf length"))? as usize;
        let mut okm = vec![0u8; len];
        hk.expand(&hex(field(v, "info")?)?, &mut okm).map_err(|e| anyhow!("hkdf: {e}"))?;
        if okm != hex(field(v, "okm")?)? {
            return Err(anyhow!("hkdf vector mismatch"));
        }
    }
    // Ed25519 -> X25519 public-key conversion against a real signer DID.
    {
        let v = &prim["ed25519_to_x25519"];
        if xpub_from_did(field(v, "did")?)?.to_vec() != hex(field(v, "x25519_pub")?)? {
            return Err(anyhow!("ed25519->x25519 vector mismatch"));
        }
    }
    // ML-KEM-768 (FIPS 203 final): keygen(d,z)->ek, encaps(ek,m)->(ct,K), decaps recovers K.
    if let Some(v) = prim.get("ml_kem") {
        let mut seed = Seed::default();
        seed[..32].copy_from_slice(&hex(field(v, "d")?)?);
        seed[32..].copy_from_slice(&hex(field(v, "z")?)?);
        let dk = DecapsulationKey::<MlKem768>::from_seed(seed);
        let ek = dk.encapsulation_key();
        if ek.to_bytes()[..] != hex(field(v, "ek")?)?[..] {
            return Err(anyhow!("ml_kem keygen (ek) vector mismatch"));
        }
        let m = to_arr32(&hex(field(v, "m")?)?)?;
        let (ct, k) = ek.encapsulate_deterministic(&B32::from(m));
        if ct[..] != hex(field(v, "ct")?)?[..] || k[..] != hex(field(v, "K")?)?[..] {
            return Err(anyhow!("ml_kem encaps (ct/K) vector mismatch"));
        }
        if dk.decapsulate(&ct)[..] != hex(field(v, "K")?)?[..] {
            return Err(anyhow!("ml_kem decaps did not recover K"));
        }
    }

    // Deterministic envelope: reseal byte-for-byte, then open and recover the plaintext.
    {
        let env = vectors.get("envelope").ok_or_else(|| anyhow!("no envelope vector"))?;
        let seed = hex(field(env, "rng_seed_hex")?)?;
        let aad = hex(field(env, "aad_hex")?)?;
        let plaintext = hex(field(env, "plaintext_hex")?)?;
        let did = field(env, "recipient_did")?.to_string();
        let expected = env.get("envelope").ok_or_else(|| anyhow!("no expected envelope"))?;

        let mut rng = Rng::seeded(seed);
        let resealed = seal(&plaintext, &[did.clone()], &aad, &mut rng, false, None)?;
        if &resealed != expected {
            return Err(anyhow!(
                "envelope reseal mismatch:\n got: {resealed}\nwant: {expected}"
            ));
        }
        let xsk = x25519_secret_from_user_seed(field(env, "recipient_seed")?);
        if open(expected, &did, &xsk, None)? != plaintext {
            return Err(anyhow!("opening the vector envelope did not recover the plaintext"));
        }
    }

    // Stealth-addressing envelope (v0.3): reseal byte-for-byte, confirm no cleartext recipient, open.
    if let Some(env) = vectors.get("stealth_envelope") {
        let seed = hex(field(env, "rng_seed_hex")?)?;
        let aad = hex(field(env, "aad_hex")?)?;
        let plaintext = hex(field(env, "plaintext_hex")?)?;
        let did = field(env, "recipient_did")?.to_string();
        let expected = env.get("envelope").ok_or_else(|| anyhow!("no expected stealth envelope"))?;

        let mut rng = Rng::seeded(seed);
        let resealed = seal(&plaintext, &[did.clone()], &aad, &mut rng, true, None)?;
        if &resealed != expected {
            return Err(anyhow!("stealth envelope reseal mismatch:\n got: {resealed}\nwant: {expected}"));
        }
        if expected["recipients"].as_array().map_or(false, |rs| rs.iter().any(|r| r.get("to").is_some())) {
            return Err(anyhow!("stealth envelope leaks a cleartext recipient `to`"));
        }
        let xsk = x25519_secret_from_user_seed(field(env, "recipient_seed")?);
        if open(expected, "", &xsk, None)? != plaintext {
            return Err(anyhow!("opening the stealth vector envelope did not recover the plaintext"));
        }
    }

    // Post-quantum hybrid envelope (v0.3, kex x25519-mlkem768): reseal byte-for-byte, then open with
    // the recipient's seed-derived ML-KEM key. Both the direct and stealth variants are exercised.
    for (name, stealth) in [("mlkem768_envelope", false), ("mlkem768_stealth_envelope", true)] {
        let env = match vectors.get(name) {
            Some(e) => e,
            None => continue,
        };
        let seed = hex(field(env, "rng_seed_hex")?)?;
        let aad = hex(field(env, "aad_hex")?)?;
        let plaintext = hex(field(env, "plaintext_hex")?)?;
        let did = field(env, "recipient_did")?.to_string();
        let recipient_seed = field(env, "recipient_seed")?;
        let expected = env.get("envelope").ok_or_else(|| anyhow!("no expected {name}"))?;

        let (mlkem_dk, mlkem_ek) = mlkem_keypair_from_user_seed(recipient_seed);
        let mut keys = BTreeMap::new();
        keys.insert(did.clone(), mlkem_ek);

        let mut rng = Rng::seeded(seed);
        let resealed = seal(&plaintext, &[did.clone()], &aad, &mut rng, stealth, Some(&keys))?;
        if &resealed != expected {
            return Err(anyhow!("{name} reseal mismatch:\n got: {resealed}\nwant: {expected}"));
        }
        let xsk = x25519_secret_from_user_seed(recipient_seed);
        let opened_did = if stealth { "" } else { did.as_str() };
        if open(expected, opened_did, &xsk, Some(&mlkem_dk))? != plaintext {
            return Err(anyhow!("opening the {name} vector did not recover the plaintext"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn vectors() -> Value {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spec/conformance/encryption.json");
        serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
    }

    #[test]
    fn conformance_vectors_reproduce_byte_for_byte() {
        run_conformance(&vectors()).expect("hardened impl must reproduce the conformance vectors");
    }

    #[test]
    fn seal_open_round_trip_and_wrong_key_fails() {
        // Use the vector's recipient (a known did:nova, so xpub_from_did succeeds).
        let v = vectors();
        let did = v["envelope"]["recipient_did"].as_str().unwrap().to_string();
        let xsk = x25519_secret_from_user_seed(v["envelope"]["recipient_seed"].as_str().unwrap());

        let mut rng = Rng::Os; // real OS randomness
        let env = seal(b"hello nova", &[did.clone()], b"label", &mut rng, false, None).unwrap();
        assert_eq!(open(&env, &did, &xsk, None).unwrap(), b"hello nova");

        // A wrong secret fails authentication.
        let wrong = x25519_secret_from_user_seed("not-the-recipient");
        assert!(open(&env, &did, &wrong, None).is_err());
    }

    #[test]
    fn stealth_hides_recipient_and_round_trips() {
        let v = vectors();
        let did = v["envelope"]["recipient_did"].as_str().unwrap().to_string();
        let xsk = x25519_secret_from_user_seed(v["envelope"]["recipient_seed"].as_str().unwrap());

        let mut rng = Rng::Os;
        let env = seal(b"secret", &[did.clone()], b"", &mut rng, true, None).unwrap();
        // No cleartext recipient is present.
        assert_eq!(env["addressing"], "stealth");
        assert!(env["recipients"].as_array().unwrap().iter().all(|r| r.get("to").is_none()));
        // The true recipient still opens it (by trial-decryption; recipient_did is ignored).
        assert_eq!(open(&env, "", &xsk, None).unwrap(), b"secret");
        // A non-recipient cannot.
        assert!(open(&env, "", &x25519_secret_from_user_seed("nope"), None).is_err());
    }

    #[test]
    fn hybrid_mlkem_round_trip_and_wrong_key_fails() {
        // The recipient's ML-KEM key (and secret) come from its user seed, like the Python reference.
        let v = vectors();
        let did = v["envelope"]["recipient_did"].as_str().unwrap().to_string();
        let seed = v["envelope"]["recipient_seed"].as_str().unwrap();
        let xsk = x25519_secret_from_user_seed(seed);
        let (dk, ek) = mlkem_keypair_from_user_seed(seed);
        let mut keys = BTreeMap::new();
        keys.insert(did.clone(), ek);

        let mut rng = Rng::Os;
        let env = seal(b"pq secret", &[did.clone()], b"label", &mut rng, false, Some(&keys)).unwrap();
        assert_eq!(env["v"], "0.3");
        assert_eq!(env["kex"], "x25519-mlkem768");
        assert!(env["recipients"][0].get("kem_ct").is_some());
        assert_eq!(open(&env, &did, &xsk, Some(&dk)).unwrap(), b"pq secret");

        // Missing the ML-KEM secret is an error; a wrong ML-KEM secret fails to authenticate.
        assert!(open(&env, &did, &xsk, None).is_err());
        let (wrong_dk, _) = mlkem_keypair_from_user_seed("not-the-recipient");
        assert!(open(&env, &did, &xsk, Some(&wrong_dk)).is_err());
    }
}
