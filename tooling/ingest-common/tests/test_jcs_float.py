"""JCS float serialization (nl_core._es_number) — known values + byte-for-byte conformance with the
Rust nl-validator's canonicalizer (the cross-implementation contract for canonical form)."""

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

_HERE = Path(__file__).resolve()
sys.path.insert(0, str(_HERE.parents[1]))                 # tooling/ingest-common
import nl_core                                            # noqa: E402

VALIDATOR = _HERE.parents[2] / "validator" / "target" / "release" / "nl-validator"

EXPECTED = {
    2.8: "2.8", 100.0: "100", 1e21: "1e+21", 1e-7: "1e-7", 0.1: "0.1", -0.0: "0",
    1e-6: "0.000001", 5.0: "5", 0.0: "0", -2.8: "-2.8", 1e20: "100000000000000000000",
    6.022e23: "6.022e+23", 1234.5678: "1234.5678", 1e22: "1e+22", 1e-21: "1e-21",
    0.5: "0.5", -100.0: "-100",
}


class EsNumberTests(unittest.TestCase):
    def test_known_values(self):
        for x, exp in EXPECTED.items():
            self.assertEqual(nl_core._es_number(x), exp, f"{x!r}")

    def test_nonfinite_raises(self):
        for bad in (float("nan"), float("inf"), float("-inf")):
            with self.assertRaises(ValueError):
                nl_core._es_number(bad)

    def test_canonicalize_emits_float(self):
        self.assertEqual(nl_core.canonicalize({"v": 2.8}), b'{"v":2.8}')


@unittest.skipUnless(VALIDATOR.exists(), "nl-validator release binary not built")
class FloatConformanceTests(unittest.TestCase):
    def test_canonical_bytes_match_validator(self):
        battery = [2.8, 100.0, 1e21, 1e-7, 0.1, -0.0, 1e-6, 5.0, 3.14159, 2.5, 0.3, 1e20, 9.999e20,
                   1.5e-10, 123456789.0, 1234.5678, 1e22, 1e-21, -0.000125, 6.022e23, 0.0001,
                   1000000.0, 2.999999999, 0.0]
        doc = {f"k{i}": x for i, x in enumerate(battery)}
        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
            json.dump(doc, f)
            path = f.name
        rust = subprocess.run([str(VALIDATOR), "canonicalize", path], capture_output=True).stdout
        self.assertEqual(nl_core.canonicalize(doc), rust)   # numbers + key order + structure all agree


class ManifestVectorReplayTests(unittest.TestCase):
    """Replay the language-neutral canonicalization_vectors from spec/conformance/manifest.json (the
    Rust validator replays the same section in tooling/validator/tests/conformance.rs)."""

    def test_replay(self):
        manifest = json.loads((_HERE.parents[3] / "spec" / "conformance" / "manifest.json").read_text())
        vectors = manifest["canonicalization_vectors"]["vectors"]
        self.assertTrue(vectors)
        for v in vectors:
            got = nl_core.canonicalize(v["input_inline"]).decode("utf-8")
            self.assertEqual(got, v["expected_canonical"], v["name"])


if __name__ == "__main__":
    unittest.main()

