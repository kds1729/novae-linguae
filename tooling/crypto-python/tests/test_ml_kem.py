"""Tests for ml_kem — the pure-Python ML-KEM-768 reference (FIPS 203 final).

    python3 -m unittest discover -s tooling/crypto-python/tests

The known-answer vector (mlkem768_kat.json) was generated from the NIST-validated RustCrypto
``ml-kem`` crate's deterministic API. Reproducing it byte-for-byte is what makes the pure-Python
reference and the hardened Rust impl interoperate.
"""

import json
import sys
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
TOOL_DIR = HERE.parent
KAT = HERE / "mlkem768_kat.json"

sys.path.insert(0, str(TOOL_DIR))
import ml_kem as M  # noqa: E402


def _h(s):
    return bytes.fromhex(s)


class TestKnownAnswer(unittest.TestCase):
    """FIPS-203-final byte-for-byte agreement with the NIST-validated ml-kem crate."""

    @classmethod
    def setUpClass(cls):
        cls.v = json.loads(KAT.read_text())

    def test_keygen_derand_matches_vector(self):
        ek, dk = M.keygen_derand(_h(self.v["d"]), _h(self.v["z"]))
        self.assertEqual(ek.hex(), self.v["ek"])
        self.assertEqual(len(ek), M.EK_SIZE)
        self.assertEqual(len(dk), M.DK_SIZE)
        # The decapsulation key embeds ek, H(ek) and z.
        self.assertEqual(dk[384 * M.K:768 * M.K + 32], ek)
        self.assertEqual(dk[768 * M.K + 64:], _h(self.v["z"]))

    def test_encaps_derand_matches_vector(self):
        ek = _h(self.v["ek"])
        shared, ct = M.encaps_derand(ek, _h(self.v["m"]))
        self.assertEqual(ct.hex(), self.v["ct"])
        self.assertEqual(shared.hex(), self.v["K"])
        self.assertEqual(len(ct), M.CT_SIZE)
        self.assertEqual(len(shared), M.SS_SIZE)

    def test_decaps_recovers_shared_key(self):
        ek, dk = M.keygen_derand(_h(self.v["d"]), _h(self.v["z"]))
        shared, ct = M.encaps_derand(ek, _h(self.v["m"]))
        self.assertEqual(M.decaps(dk, ct), shared)
        self.assertEqual(M.decaps(dk, ct).hex(), self.v["K"])


class TestRoundTrips(unittest.TestCase):
    def test_distinct_seeds_distinct_keys_and_round_trip(self):
        ek1, dk1 = M.keygen_derand(b"\x01" * 32, b"\x02" * 32)
        ek2, dk2 = M.keygen_derand(b"\x03" * 32, b"\x04" * 32)
        self.assertNotEqual(ek1, ek2)
        s1, c1 = M.encaps_derand(ek1, b"\x05" * 32)
        self.assertEqual(M.decaps(dk1, c1), s1)
        # The wrong key holder decapsulates to a *different* (implicitly-rejected) secret, not s1.
        self.assertNotEqual(M.decaps(dk2, c1), s1)

    def test_implicit_rejection_is_deterministic(self):
        ek, dk = M.keygen_derand(b"\x07" * 32, b"\x08" * 32)
        _shared, ct = M.encaps_derand(ek, b"\x09" * 32)
        tampered = bytearray(ct)
        tampered[0] ^= 0xFF
        tampered = bytes(tampered)
        r1 = M.decaps(dk, tampered)
        r2 = M.decaps(dk, tampered)
        self.assertEqual(r1, r2)            # deterministic implicit rejection
        self.assertEqual(len(r1), M.SS_SIZE)
        self.assertNotEqual(r1, _shared)    # and not the real shared key

    def test_input_validation(self):
        with self.assertRaises(ValueError):
            M.keygen_derand(b"short", b"\x00" * 32)
        with self.assertRaises(ValueError):
            M.encaps_derand(b"\x00" * 10, b"\x00" * 32)
        with self.assertRaises(ValueError):
            M.decaps(b"\x00" * 10, b"\x00" * M.CT_SIZE)


if __name__ == "__main__":
    unittest.main()
