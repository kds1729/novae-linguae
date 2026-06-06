"""Ed25519 signing vectors — the pure-Python signer must match nl-validator (ed25519-dalek)
byte-for-byte, validated against the repo's own signed message examples, plus manifest sign/verify."""

import json
import sys
import unittest
from pathlib import Path

_HERE = Path(__file__).resolve()
sys.path.insert(0, str(_HERE.parents[1]))                       # tooling/crypto-python
sys.path.insert(0, str(_HERE.parents[2] / "ingest-common"))    # for nl_core

import nl_crypto as C       # noqa: E402
from nl_core import canonicalize  # noqa: E402

_SPEC = _HERE.parents[3] / "spec"

# (user seed, example message, expected did:nova) — the seeds nl-validator used to sign these.
_VECTORS = [
    ("novae-linguae-example-claude", "request.json",
     "did:nova:ea9b49af638db86bff90080c8f1800535182ae30564132fb24276f06904e505e"),
    ("novae-linguae-example-verifier", "assert.json",
     "did:nova:896a2e2c0578ec83132584d629abaef1a6eefdbec326c803a9a62c1f1fc1e054"),
]


class Ed25519VectorTests(unittest.TestCase):
    def test_reproduces_repo_signatures(self):
        for seed, example, expect_did in _VECTORS:
            seed32, pub, did = C.signing_keypair_from_user_seed(seed)
            self.assertEqual(did, expect_did)

            rec = json.loads((_SPEC / "examples" / example).read_text())
            preimage = canonicalize({k: v for k, v in rec.items() if k != "signature"})
            self.assertEqual(C.format_signature(C.ed25519_sign(seed32, preimage)), rec["signature"])
            self.assertTrue(C.ed25519_verify(pub, preimage, C.parse_signature(rec["signature"])))
            self.assertFalse(C.ed25519_verify(pub, preimage + b"x", C.parse_signature(rec["signature"])))


class ManifestSigningTests(unittest.TestCase):
    def test_sign_then_verify(self):
        m = {"format_version": "nlb/1", "count": 1, "schema_versions": ["0.1.0"],
             "bundle_digest": "blake2b:" + "0" * 64}
        signed = C.sign_manifest(m, "publisher-seed")
        status, producer = C.verify_manifest(signed)
        self.assertEqual(status, "valid")
        self.assertEqual(producer, signed["producer"])

    def test_tamper_is_invalid(self):
        m = {"format_version": "nlb/1", "count": 1, "schema_versions": ["0.1.0"],
             "bundle_digest": "blake2b:" + "0" * 64}
        signed = C.sign_manifest(m, "publisher-seed")
        signed["count"] = 999                          # change a signed field
        self.assertEqual(C.verify_manifest(signed)[0], "invalid")

    def test_unsigned(self):
        self.assertEqual(C.verify_manifest({"format_version": "nlb/1"})[0], "unsigned")


if __name__ == "__main__":
    unittest.main()
