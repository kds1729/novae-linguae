"""Generate spec/conformance/encryption.json — the language-neutral encryption conformance vectors.

Run:  python3 tooling/crypto-python/tests/gen_vectors.py

Writes primitive vectors (X25519, ChaCha20-Poly1305, XChaCha20-Poly1305, HKDF, HChaCha20, the
Ed25519->X25519 conversion) plus one deterministic encrypted-envelope vector. A second
implementation replays these to prove byte-for-byte agreement. Regenerate and review the diff after
any deliberate change to the format or primitives.
"""

import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO_ROOT = HERE.parent.parent.parent
EXAMPLES = REPO_ROOT / "spec" / "examples"
OUT = REPO_ROOT / "spec" / "conformance" / "encryption.json"

sys.path.insert(0, str(HERE.parent))
import nl_crypto as x


def main():
    did_a = json.loads((EXAMPLES / "request.json").read_text())["from"]
    seed_a = "novae-linguae-example-claude"

    plaintext = b'{"kind":"assert","claim":"confidential"}'
    rng_seed = bytes.fromhex("00112233445566778899aabbccddeeff")
    aad = b"novae-linguae/example"
    envelope = x.seal(plaintext, [did_a], aad=aad, rng=x.seeded_rng(rng_seed))
    # Stealth-addressing vector (v0.3): same recipient, recipient set hidden (no cleartext `to`).
    stealth_seed = bytes.fromhex("0f0e0d0c0b0a09080706050403020100")
    stealth_envelope = x.seal(plaintext, [did_a], aad=aad, rng=x.seeded_rng(stealth_seed), stealth=True)

    vectors = {
        "description": (
            "Conformance vectors for Nova Locutio payload encryption (spec/encryption.md). "
            "Primitive vectors are the official RFC/draft test vectors; the envelope vector is a "
            "deterministic seal (BLAKE3-seeded RNG) that any implementation must reproduce and open."
        ),
        "primitives": {
            "x25519": [
                {"scalar": "a546e36bf0527c9d3b16154b82465edd62144c0ac1fc5a18506a2244ba449ac4",
                 "u": "e6db6867583030db3594c1a424b15f7c726624ec26b3353b10a903a6d0ab1c4c",
                 "out": "c3da55379de9c6908e94ea4df28d084f32eccf03491c71f754b4075577a28552"},
                {"scalar": "4b66e9d4d1b4673c5ad22691957d6af5c11b6421e0ea01d42ca4169e7918ba0d",
                 "u": "e5210f12786811d3f4b7959d0538ae2c31dbe7106fc03c3efc4cd549c715a493",
                 "out": "95cbde9476e8907d7aade45cb4b873f88b595a68799fa152e6f8f7647aac7957"},
            ],
            "hchacha20": {
                "key": "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
                "nonce": "000000090000004a0000000031415927",
                "out": x.hchacha20(
                    bytes.fromhex("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f"),
                    bytes.fromhex("000000090000004a0000000031415927")).hex(),
            },
            "chacha20poly1305": {
                "key": "808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9f",
                "nonce": "070000004041424344454647",
                "aad": "50515253c0c1c2c3c4c5c6c7",
                "plaintext_hex": (b"Ladies and Gentlemen of the class of '99: If I could offer you "
                                  b"only one tip for the future, sunscreen would be it.").hex(),
                "ciphertext_and_tag":
                    "d31a8d34648e60db7b86afbc53ef7ec2a4aded51296e08fea9e2b5a736ee62d6"
                    "3dbea45e8ca9671282fafb69da92728b1a71de0a9e060b2905d6a5b67ecd3b36"
                    "92ddbd7f2d778b8c9803aee328091b58fab324e4fad675945585808b4831d7bc"
                    "3ff4def08e4b7a9de576d26586cec64b61161ae10b594f09e26a7e902ecbd0600691",
            },
            "xchacha20poly1305": {
                "key": "808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9f",
                "nonce": "404142434445464748494a4b4c4d4e4f5051525354555657",
                "aad": "50515253c0c1c2c3c4c5c6c7",
                "plaintext_hex": (b"Ladies and Gentlemen of the class of '99: If I could offer you "
                                  b"only one tip for the future, sunscreen would be it.").hex(),
                "ciphertext_and_tag": x.xchacha20poly1305_seal(
                    bytes.fromhex("808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9f"),
                    bytes.fromhex("404142434445464748494a4b4c4d4e4f5051525354555657"),
                    (b"Ladies and Gentlemen of the class of '99: If I could offer you "
                     b"only one tip for the future, sunscreen would be it."),
                    bytes.fromhex("50515253c0c1c2c3c4c5c6c7")).hex(),
            },
            "hkdf_sha256": {
                "ikm": "0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b",
                "salt": "000102030405060708090a0b0c",
                "info": "f0f1f2f3f4f5f6f7f8f9",
                "length": 42,
                "okm": "3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf34007208d5b887185865",
            },
            "ed25519_to_x25519": {
                "comment": "X25519 public key derived from the example request.json signer's did:nova.",
                "did": did_a,
                "x25519_pub": x.x25519_pub_from_did(did_a).hex(),
                "from_seed": seed_a,
            },
        },
        "envelope": {
            "comment": "Deterministic seal to one recipient; reproduce with the BLAKE3-seeded RNG.",
            "rng_seed_hex": rng_seed.hex(),
            "recipients": [did_a],
            "recipient_did": did_a,
            "recipient_seed": seed_a,
            "aad_hex": aad.hex(),
            "plaintext_hex": plaintext.hex(),
            "envelope": envelope,
        },
        "stealth_envelope": {
            "comment": ("Stealth addressing (v0.3): recipient set hidden — no cleartext `to`, wrap "
                        "bound to a fixed label, recovered by trial-decryption. Reproduce with the "
                        "BLAKE3-seeded RNG and stealth=True."),
            "rng_seed_hex": stealth_seed.hex(),
            "recipient_did": did_a,
            "recipient_seed": seed_a,
            "aad_hex": aad.hex(),
            "plaintext_hex": plaintext.hex(),
            "envelope": stealth_envelope,
        },
    }

    OUT.write_text(json.dumps(vectors, indent=2) + "\n")
    print(f"wrote {OUT} ({OUT.stat().st_size} bytes)")

    # The same envelope, written as the standalone worked example.
    example = EXAMPLES / "encrypted-envelope.json"
    example.write_text(json.dumps(envelope, indent=2) + "\n")
    print(f"wrote {example}")

    # Self-check.
    recovered = x.open_with_seed(envelope, did_a, seed_a)
    assert recovered == plaintext, "self-check failed"
    assert x.open_with_seed(stealth_envelope, None, seed_a) == plaintext, "stealth self-check failed"
    print("self-check: direct and stealth envelopes open to the expected plaintext")


if __name__ == "__main__":
    main()
