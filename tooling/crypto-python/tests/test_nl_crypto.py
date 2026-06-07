"""Tests for nl_crypto — Nova Locutio payload-encryption primitives and envelope.

    python3 -m unittest discover -s tooling/crypto-python/tests

Every primitive is checked against its official RFC/draft test vector. The Ed25519->X25519
conversion is checked for consistency against the real example DIDs in spec/examples (whose
generating seeds are fixed), so key agreement interoperates with the nl-validator signing identities.
"""

import json
import sys
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
TOOL_DIR = HERE.parent
REPO_ROOT = TOOL_DIR.parent.parent
EXAMPLES = REPO_ROOT / "spec" / "examples"
CONFORMANCE = REPO_ROOT / "spec" / "conformance" / "encryption.json"

sys.path.insert(0, str(TOOL_DIR))
import nl_crypto as x  # noqa: E402

# Example DID <- seed pairs (the committed example messages are signed with these seeds).
DID_SEEDS = {
    "request.json": "novae-linguae-example-claude",
    "assert.json": "novae-linguae-example-verifier",
}


def _h(s):
    return bytes.fromhex(s)


class TestPrimitiveVectors(unittest.TestCase):
    def test_x25519_rfc7748(self):
        self.assertEqual(
            x.x25519(_h("a546e36bf0527c9d3b16154b82465edd62144c0ac1fc5a18506a2244ba449ac4"),
                     _h("e6db6867583030db3594c1a424b15f7c726624ec26b3353b10a903a6d0ab1c4c")).hex(),
            "c3da55379de9c6908e94ea4df28d084f32eccf03491c71f754b4075577a28552")
        self.assertEqual(
            x.x25519(_h("4b66e9d4d1b4673c5ad22691957d6af5c11b6421e0ea01d42ca4169e7918ba0d"),
                     _h("e5210f12786811d3f4b7959d0538ae2c31dbe7106fc03c3efc4cd549c715a493")).hex(),
            "95cbde9476e8907d7aade45cb4b873f88b595a68799fa152e6f8f7647aac7957")

    def test_chacha20_block_rfc8439(self):
        self.assertEqual(
            x._chacha20_block(bytes(range(32)), 1, _h("000000090000004a00000000")).hex(),
            "10f1e7e4d13b5915500fdd1fa32071c4c7d1f4c733c068030422aa9ac3d46c4e"
            "d2826446079faa0914c2d705d98b02a2b5129cd1de164eb9cbd083e8a2503c4e")

    def test_poly1305_rfc8439(self):
        otk = _h("85d6be7857556d337f4452fe42d506a80103808afb0db2fd4abff6af4149f51b")
        self.assertEqual(x._poly1305(otk, b"Cryptographic Forum Research Group").hex(),
                         "a8061dc1305136c6c22b8baf0c0127a9")

    def test_chacha20poly1305_rfc8439(self):
        key = _h("808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9f")
        nonce = _h("070000004041424344454647")
        aad = _h("50515253c0c1c2c3c4c5c6c7")
        pt = (b"Ladies and Gentlemen of the class of '99: If I could offer you only "
              b"one tip for the future, sunscreen would be it.")
        expected = _h(
            "d31a8d34648e60db7b86afbc53ef7ec2a4aded51296e08fea9e2b5a736ee62d6"
            "3dbea45e8ca9671282fafb69da92728b1a71de0a9e060b2905d6a5b67ecd3b36"
            "92ddbd7f2d778b8c9803aee328091b58fab324e4fad675945585808b4831d7bc"
            "3ff4def08e4b7a9de576d26586cec64b6116" "1ae10b594f09e26a7e902ecbd0600691")
        self.assertEqual(x.chacha20poly1305_seal(key, nonce, pt, aad), expected)
        self.assertEqual(x.chacha20poly1305_open(key, nonce, expected, aad), pt)

    def test_xchacha20poly1305_draft(self):
        key = _h("808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9f")
        nonce = _h("404142434445464748494a4b4c4d4e4f5051525354555657")
        aad = _h("50515253c0c1c2c3c4c5c6c7")
        pt = (b"Ladies and Gentlemen of the class of '99: If I could offer you only "
              b"one tip for the future, sunscreen would be it.")
        expected = _h(
            "bd6d179d3e83d43b9576579493c0e939572a1700252bfaccbed2902c21396cbb"
            "731c7f1b0b4aa6440bf3a82f4eda7e39ae64c6708c54c216cb96b72e1213b452"
            "2f8c9ba40db5d945b11b69b982c1bb9e3f3fac2bc369488f76b2383565d3fff9"
            "21f9664c97637da9768812f615c68b13b52e" "c0875924c1c7987947deafd8780acf49")
        self.assertEqual(x.xchacha20poly1305_seal(key, nonce, pt, aad), expected)
        self.assertEqual(x.xchacha20poly1305_open(key, nonce, expected, aad), pt)

    def test_hkdf_sha256_rfc5869(self):
        self.assertEqual(
            x.hkdf_sha256(_h("0b" * 22), _h("000102030405060708090a0b0c"),
                          _h("f0f1f2f3f4f5f6f7f8f9"), 42).hex(),
            "3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf34007208d5b887185865")


class TestKeyConversion(unittest.TestCase):
    """Ed25519->X25519 conversion must agree with the real nl-validator signing identities."""

    def _did(self, filename):
        return json.loads((EXAMPLES / filename).read_text())["from"]

    def test_did_pub_matches_seed_secret(self):
        for filename, seed in DID_SEEDS.items():
            did = self._did(filename)
            xpub_from_did = x.x25519_pub_from_did(did)
            _, xpub_from_seed = x.x25519_keypair_from_user_seed(seed)
            self.assertEqual(xpub_from_did, xpub_from_seed,
                             f"{seed}: DID-derived X25519 pubkey != seed-derived one")

    def test_ecdh_agreement(self):
        items = list(DID_SEEDS.items())
        (fa, sa), (fb, sb) = items[0], items[1]
        xa, _ = x.x25519_keypair_from_user_seed(sa)
        xb, _ = x.x25519_keypair_from_user_seed(sb)
        shared_a = x.x25519(xa, x.x25519_pub_from_did(self._did(fb)))
        shared_b = x.x25519(xb, x.x25519_pub_from_did(self._did(fa)))
        self.assertEqual(shared_a, shared_b)


class TestEnvelope(unittest.TestCase):
    def setUp(self):
        self.did_a = json.loads((EXAMPLES / "request.json").read_text())["from"]
        self.seed_a = DID_SEEDS["request.json"]
        self.did_b = json.loads((EXAMPLES / "assert.json").read_text())["from"]
        self.seed_b = DID_SEEDS["assert.json"]

    def test_roundtrip_single_recipient(self):
        pt = b'{"hello":"nova locutio"}'
        env = x.seal(pt, [self.did_a])
        self.assertEqual(x.open_with_seed(env, self.did_a, self.seed_a), pt)

    def test_multi_recipient_each_opens(self):
        pt = b"shared secret payload"
        env = x.seal(pt, [self.did_a, self.did_b])
        self.assertEqual(x.open_with_seed(env, self.did_a, self.seed_a), pt)
        self.assertEqual(x.open_with_seed(env, self.did_b, self.seed_b), pt)

    def test_non_recipient_cannot_open(self):
        env = x.seal(b"for A only", [self.did_a])
        with self.assertRaises(ValueError):
            x.open_with_seed(env, self.did_b, self.seed_b)

    def test_wrong_key_fails_auth(self):
        env = x.seal(b"secret", [self.did_a])
        wrong, _ = x.x25519_keypair_from_user_seed("not-the-right-seed")
        with self.assertRaises(ValueError):
            x.open_envelope(env, self.did_a, wrong)

    def test_tamper_detected(self):
        env = x.seal(b"integrity matters", [self.did_a])
        ct = bytearray(x._unb64(env["ciphertext"]))
        ct[0] ^= 0x01
        env["ciphertext"] = x._b64(bytes(ct))
        with self.assertRaises(ValueError):
            x.open_with_seed(env, self.did_a, self.seed_a)

    def test_aad_binding(self):
        env = x.seal(b"bound", [self.did_a], aad=b"context-42")
        xsk, _ = x.x25519_keypair_from_user_seed(self.seed_a)
        self.assertEqual(x.open_envelope(env, self.did_a, xsk), b"bound")
        env["aad"] = x._b64(b"context-99")  # different AAD must fail
        with self.assertRaises(ValueError):
            x.open_envelope(env, self.did_a, xsk)

    def test_per_conversation_cek_reuse(self):
        cek = x.random_bytes(32)
        e1 = x.seal(b"message one", [self.did_a], cek=cek)
        e2 = x.seal(b"message two", [self.did_a], cek=cek)
        self.assertEqual(x.open_with_seed(e1, self.did_a, self.seed_a), b"message one")
        self.assertEqual(x.open_with_seed(e2, self.did_a, self.seed_a), b"message two")

    def test_deterministic_is_reproducible(self):
        rng1 = x.seeded_rng(b"vector-seed")
        rng2 = x.seeded_rng(b"vector-seed")
        e1 = x.seal(b"deterministic", [self.did_a], rng=rng1)
        e2 = x.seal(b"deterministic", [self.did_a], rng=rng2)
        self.assertEqual(e1, e2)
        self.assertEqual(x.open_with_seed(e1, self.did_a, self.seed_a), b"deterministic")

    def test_stealth_hides_recipients_and_round_trips(self):
        env = x.seal(b"who can read this?", [self.did_a, self.did_b], stealth=True)
        # No cleartext recipient identities leak.
        self.assertEqual(env["addressing"], "stealth")
        self.assertTrue(all("to" not in r for r in env["recipients"]))
        # Each true recipient recovers the plaintext by trial-decryption (recipient_did ignored).
        self.assertEqual(x.open_with_seed(env, None, self.seed_a), b"who can read this?")
        self.assertEqual(x.open_with_seed(env, None, self.seed_b), b"who can read this?")

    def test_stealth_non_recipient_cannot_open(self):
        env = x.seal(b"secret", [self.did_a], stealth=True)
        wrong, _ = x.x25519_keypair_from_user_seed("not-a-recipient")
        with self.assertRaises(ValueError):
            x.open_envelope(env, None, wrong)


class TestCLI(unittest.TestCase):
    """End-to-end CLI round-trip via subprocess."""

    CLI = str(TOOL_DIR / "nl_encrypt.py")

    def setUp(self):
        self.did_a = json.loads((EXAMPLES / "request.json").read_text())["from"]
        self.seed_a = DID_SEEDS["request.json"]

    def _run(self, args, stdin=None):
        import subprocess
        return subprocess.run([sys.executable, self.CLI, *args], input=stdin,
                              capture_output=True)

    def test_seal_then_open(self):
        pt = b'{"msg":"hi"}'
        sealed = self._run(["seal", "--to", self.did_a], stdin=pt)
        self.assertEqual(sealed.returncode, 0, sealed.stderr)
        opened = self._run(["open", "--did", self.did_a, "--seed", self.seed_a], stdin=sealed.stdout)
        self.assertEqual(opened.returncode, 0, opened.stderr)
        self.assertEqual(opened.stdout, pt)

    def test_pubkey_did_matches_seed(self):
        a = self._run(["pubkey", "--did", self.did_a])
        b = self._run(["pubkey", "--seed", self.seed_a])
        self.assertEqual(a.stdout.strip(), b.stdout.strip())


@unittest.skipUnless(CONFORMANCE.exists(), "encryption conformance vectors not generated yet")
class TestConformance(unittest.TestCase):
    def setUp(self):
        self.vectors = json.loads(CONFORMANCE.read_text())

    def test_envelope_vector_opens(self):
        v = self.vectors["envelope"]
        plaintext = x.open_with_seed(v["envelope"], v["recipient_did"], v["recipient_seed"])
        self.assertEqual(plaintext, bytes.fromhex(v["plaintext_hex"]))

    def test_envelope_vector_reseals_identically(self):
        v = self.vectors["envelope"]
        rng = x.seeded_rng(bytes.fromhex(v["rng_seed_hex"]))
        env = x.seal(bytes.fromhex(v["plaintext_hex"]), v["recipients"],
                     aad=bytes.fromhex(v["aad_hex"]) if v.get("aad_hex") else b"", rng=rng)
        self.assertEqual(env, v["envelope"])

    def test_stealth_envelope_vector_reseals_and_opens(self):
        v = self.vectors.get("stealth_envelope")
        self.assertIsNotNone(v, "stealth_envelope vector missing")
        rng = x.seeded_rng(bytes.fromhex(v["rng_seed_hex"]))
        env = x.seal(bytes.fromhex(v["plaintext_hex"]), [v["recipient_did"]],
                     aad=bytes.fromhex(v["aad_hex"]) if v.get("aad_hex") else b"",
                     rng=rng, stealth=True)
        self.assertEqual(env, v["envelope"])
        self.assertTrue(all("to" not in r for r in v["envelope"]["recipients"]))
        opened = x.open_with_seed(v["envelope"], None, v["recipient_seed"])
        self.assertEqual(opened, bytes.fromhex(v["plaintext_hex"]))

    def test_ml_kem_primitive_vector(self):
        v = self.vectors["primitives"]["ml_kem"]
        ek, dk = x.ml_kem.keygen_derand(bytes.fromhex(v["d"]), bytes.fromhex(v["z"]))
        self.assertEqual(ek.hex(), v["ek"])
        K, ct = x.ml_kem.encaps_derand(ek, bytes.fromhex(v["m"]))
        self.assertEqual(ct.hex(), v["ct"])
        self.assertEqual(K.hex(), v["K"])
        self.assertEqual(x.ml_kem.decaps(dk, ct).hex(), v["K"])

    def test_mlkem768_envelope_reseals_and_opens(self):
        v = self.vectors["mlkem768_envelope"]
        keys = {v["recipient_did"]: x.mlkem_keypair_from_user_seed(v["recipient_seed"])[1]}
        rng = x.seeded_rng(bytes.fromhex(v["rng_seed_hex"]))
        env = x.seal(bytes.fromhex(v["plaintext_hex"]), [v["recipient_did"]],
                     aad=bytes.fromhex(v["aad_hex"]), rng=rng, recipient_mlkem_keys=keys)
        self.assertEqual(env, v["envelope"])
        self.assertEqual(env["v"], "0.3")
        self.assertEqual(env["kex"], "x25519-mlkem768")
        opened = x.open_with_seed(v["envelope"], v["recipient_did"], v["recipient_seed"])
        self.assertEqual(opened, bytes.fromhex(v["plaintext_hex"]))

    def test_mlkem768_stealth_envelope_reseals_and_opens(self):
        v = self.vectors["mlkem768_stealth_envelope"]
        keys = {v["recipient_did"]: x.mlkem_keypair_from_user_seed(v["recipient_seed"])[1]}
        rng = x.seeded_rng(bytes.fromhex(v["rng_seed_hex"]))
        env = x.seal(bytes.fromhex(v["plaintext_hex"]), [v["recipient_did"]],
                     aad=bytes.fromhex(v["aad_hex"]), rng=rng, stealth=True, recipient_mlkem_keys=keys)
        self.assertEqual(env, v["envelope"])
        self.assertTrue(all("to" not in r for r in v["envelope"]["recipients"]))
        opened = x.open_with_seed(v["envelope"], None, v["recipient_seed"])
        self.assertEqual(opened, bytes.fromhex(v["plaintext_hex"]))


class TestDidDocument(unittest.TestCase):
    """DID documents publish the ML-KEM key-agreement key the did:nova string cannot carry."""

    SEED = "novae-linguae-example-claude"

    def test_build_verify_extract(self):
        doc = x.build_did_document(self.SEED)
        self.assertEqual(x.verify_did_document(doc), ("valid", doc["id"]))
        ek = x.mlkem_pub_from_did_document(doc)
        _dk, ek_direct = x.mlkem_keypair_from_user_seed(self.SEED)
        self.assertEqual(ek, ek_direct)
        self.assertEqual(len(ek), 1184)

    def test_deterministic_from_seed(self):
        # One seed regenerates the same key + the same document.
        self.assertEqual(x.build_did_document(self.SEED), x.build_did_document(self.SEED))

    def test_id_matches_signing_identity(self):
        doc = x.build_did_document(self.SEED)
        _seed32, _pub, did = x.signing_keypair_from_user_seed(self.SEED)
        self.assertEqual(doc["id"], did)

    def test_tamper_is_rejected(self):
        doc = x.build_did_document(self.SEED)
        bad = json.loads(json.dumps(doc))
        b64 = bad["keyAgreement"][0]["publicKeyBase64"]
        bad["keyAgreement"][0]["publicKeyBase64"] = ("A" if b64[0] != "A" else "B") + b64[1:]
        self.assertEqual(x.verify_did_document(bad)[0], "invalid")
        with self.assertRaises(ValueError):
            x.mlkem_pub_from_did_document(bad)

    def test_unsigned_document(self):
        doc = x.build_did_document(self.SEED)
        del doc["signature"]
        self.assertEqual(x.verify_did_document(doc)[0], "unsigned")


class TestHybridEnvelope(unittest.TestCase):
    """Post-quantum hybrid kex (x25519-mlkem768): X25519 + ML-KEM-768, KEK mixes both secrets."""

    SEED_A = "novae-linguae-example-claude"
    SEED_B = "novae-linguae-example-verifier"

    def setUp(self):
        self.doc_a = x.build_did_document(self.SEED_A)
        self.doc_b = x.build_did_document(self.SEED_B)
        self.pt = b'{"kind":"assert","claim":"post-quantum-confidential"}'

    def test_shape_and_roundtrip_single(self):
        env = x.seal_to_did(self.pt, [self.doc_a], aad=b"label")
        self.assertEqual(env["v"], "0.3")
        self.assertEqual(env["kex"], "x25519-mlkem768")
        import base64 as _b
        entry = env["recipients"][0]
        self.assertEqual(len(_b.b64decode(entry["kem_ct"])), 1088)   # ML-KEM-768 ciphertext
        self.assertEqual(x.open_with_seed(env, self.doc_a["id"], self.SEED_A), self.pt)

    def test_deterministic_reseal(self):
        keys = {self.doc_a["id"]: x.mlkem_pub_from_did_document(self.doc_a)}
        rng1 = x.seeded_rng(b"hybrid-vector-seed")
        rng2 = x.seeded_rng(b"hybrid-vector-seed")
        e1 = x.seal(self.pt, [self.doc_a["id"]], aad=b"a", rng=rng1, recipient_mlkem_keys=keys)
        e2 = x.seal(self.pt, [self.doc_a["id"]], aad=b"a", rng=rng2, recipient_mlkem_keys=keys)
        self.assertEqual(e1, e2)

    def test_multi_recipient(self):
        env = x.seal_to_did(self.pt, [self.doc_a, self.doc_b])
        self.assertEqual(len(env["recipients"]), 2)
        self.assertEqual(x.open_with_seed(env, self.doc_a["id"], self.SEED_A), self.pt)
        self.assertEqual(x.open_with_seed(env, self.doc_b["id"], self.SEED_B), self.pt)

    def test_non_recipient_rejected(self):
        env = x.seal_to_did(self.pt, [self.doc_a])
        with self.assertRaises(Exception):
            x.open_with_seed(env, self.doc_b["id"], self.SEED_B)

    def test_stealth_hybrid(self):
        env = x.seal_to_did(self.pt, [self.doc_a], stealth=True)
        self.assertEqual(env["addressing"], "stealth")
        self.assertTrue(all("to" not in r for r in env["recipients"]))
        self.assertTrue(all("kem_ct" in r for r in env["recipients"]))
        self.assertEqual(x.open_with_seed(env, None, self.SEED_A), self.pt)
        with self.assertRaises(Exception):
            x.open_with_seed(env, None, self.SEED_B)

    def test_missing_mlkem_secret_raises(self):
        env = x.seal_to_did(self.pt, [self.doc_a])
        xsk, _ = x.x25519_keypair_from_user_seed(self.SEED_A)
        with self.assertRaises(ValueError):
            x.open_envelope(env, self.doc_a["id"], xsk)   # no mlkem_secret supplied

    def test_tampered_kem_ct_fails(self):
        env = x.seal_to_did(self.pt, [self.doc_a])
        import base64 as _b
        ct = bytearray(_b.b64decode(env["recipients"][0]["kem_ct"]))
        ct[0] ^= 0xFF
        env["recipients"][0]["kem_ct"] = _b.b64encode(bytes(ct)).decode()
        with self.assertRaises(Exception):
            x.open_with_seed(env, self.doc_a["id"], self.SEED_A)

    def test_unsigned_did_doc_refused(self):
        bad = json.loads(json.dumps(self.doc_a))
        del bad["signature"]
        with self.assertRaises(ValueError):
            x.seal_to_did(self.pt, [bad])


if __name__ == "__main__":
    unittest.main(verbosity=2)
