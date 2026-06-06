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
use serde_json::{json, Value};
use sha2::{Digest, Sha256, Sha512};
use x25519_dalek::{PublicKey, StaticSecret};

const ENVELOPE_VERSION: &str = "0.2";
const ENC_ALG: &str = "xchacha20poly1305";
const KDF_ALG: &str = "hkdf-sha256";
const KEX_ALG: &str = "x25519-ed25519";
const KEYWRAP_INFO: &[u8] = b"novae-linguae/v0.2/xchacha20poly1305/key-wrap";

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

/// Seal `plaintext` to one or more did:nova recipients, returning an envelope JSON value matching
/// encrypted-envelope.schema.json. `rng` is the byte source (use `Rng::Os` for real use; `Rng::seeded`
/// reproduces conformance vectors). The RNG draw order — cek, esk, nonce, then each wrap_nonce — is
/// part of the byte-for-byte contract.
pub fn seal(plaintext: &[u8], recipient_dids: &[String], aad: &[u8], rng: &mut Rng) -> Result<Value> {
    if recipient_dids.is_empty() {
        return Err(anyhow!("at least one recipient DID is required"));
    }
    let cek = to_arr32(&rng.fill(32))?;
    let esk = to_arr32(&rng.fill(32))?;
    let esk_secret = StaticSecret::from(esk);
    let epk = PublicKey::from(&esk_secret).to_bytes();
    let nonce = rng.fill(24);
    let ciphertext = xchacha_seal(&cek, &nonce, plaintext, aad)?;

    let mut recipients = Vec::new();
    for did in recipient_dids {
        let rxpub = xpub_from_did(did)?;
        let shared = esk_secret.diffie_hellman(&PublicKey::from(rxpub)).to_bytes();
        let kek = derive_kek(&shared, &epk, &rxpub);
        let wrap_nonce = rng.fill(24);
        let wrapped = xchacha_seal(&kek, &wrap_nonce, &cek, did.as_bytes())?;
        recipients.push(json!({
            "to": did,
            "wrap_nonce": b64(&wrap_nonce),
            "wrapped_key": b64(&wrapped),
        }));
    }

    let mut envelope = json!({
        "v": ENVELOPE_VERSION,
        "enc": ENC_ALG,
        "kdf": KDF_ALG,
        "kex": KEX_ALG,
        "epk": b64(&epk),
        "nonce": b64(&nonce),
        "ciphertext": b64(&ciphertext),
        "recipients": recipients,
    });
    if !aad.is_empty() {
        envelope["aad"] = json!(b64(aad));
    }
    Ok(envelope)
}

/// Recover the plaintext from `envelope` for the recipient holding `x25519_secret`.
pub fn open(envelope: &Value, recipient_did: &str, x25519_secret: &[u8; 32]) -> Result<Vec<u8>> {
    let alg_ok = envelope.get("enc").and_then(|v| v.as_str()) == Some(ENC_ALG)
        && envelope.get("kdf").and_then(|v| v.as_str()) == Some(KDF_ALG)
        && envelope.get("kex").and_then(|v| v.as_str()) == Some(KEX_ALG);
    if !alg_ok {
        return Err(anyhow!("unsupported envelope algorithms"));
    }
    let recipients = envelope
        .get("recipients")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow!("envelope missing recipients"))?;
    let entry = recipients
        .iter()
        .find(|r| r.get("to").and_then(|v| v.as_str()) == Some(recipient_did))
        .ok_or_else(|| anyhow!("envelope is not addressed to {recipient_did}"))?;

    let epk = to_arr32(&unb64(envelope["epk"].as_str().context("epk")?)?)?;
    let aad = match envelope.get("aad").and_then(|v| v.as_str()) {
        Some(s) => unb64(s)?,
        None => Vec::new(),
    };
    let secret = StaticSecret::from(*x25519_secret);
    let rxpub = PublicKey::from(&secret).to_bytes();
    let shared = secret.diffie_hellman(&PublicKey::from(epk)).to_bytes();
    let kek = derive_kek(&shared, &epk, &rxpub);
    let cek_vec = xchacha_open(
        &kek,
        &unb64(entry["wrap_nonce"].as_str().context("wrap_nonce")?)?,
        &unb64(entry["wrapped_key"].as_str().context("wrapped_key")?)?,
        recipient_did.as_bytes(),
    )?;
    let cek = to_arr32(&cek_vec)?;
    xchacha_open(
        &cek,
        &unb64(envelope["nonce"].as_str().context("nonce")?)?,
        &unb64(envelope["ciphertext"].as_str().context("ciphertext")?)?,
        &aad,
    )
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

    // Deterministic envelope: reseal byte-for-byte, then open and recover the plaintext.
    {
        let env = vectors.get("envelope").ok_or_else(|| anyhow!("no envelope vector"))?;
        let seed = hex(field(env, "rng_seed_hex")?)?;
        let aad = hex(field(env, "aad_hex")?)?;
        let plaintext = hex(field(env, "plaintext_hex")?)?;
        let did = field(env, "recipient_did")?.to_string();
        let expected = env.get("envelope").ok_or_else(|| anyhow!("no expected envelope"))?;

        let mut rng = Rng::seeded(seed);
        let resealed = seal(&plaintext, &[did.clone()], &aad, &mut rng)?;
        if &resealed != expected {
            return Err(anyhow!(
                "envelope reseal mismatch:\n got: {resealed}\nwant: {expected}"
            ));
        }
        let xsk = x25519_secret_from_user_seed(field(env, "recipient_seed")?);
        let recovered = open(expected, &did, &xsk)?;
        if recovered != plaintext {
            return Err(anyhow!("opening the vector envelope did not recover the plaintext"));
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
        let env = seal(b"hello nova", &[did.clone()], b"label", &mut rng).unwrap();
        assert_eq!(open(&env, &did, &xsk).unwrap(), b"hello nova");

        // A wrong secret fails authentication.
        let wrong = x25519_secret_from_user_seed("not-the-recipient");
        assert!(open(&env, &did, &wrong).is_err());
    }
}
