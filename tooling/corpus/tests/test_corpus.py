"""Tests for the synthetic training-corpus generator.

The fast test checks the *committed* corpus artifact's integrity (every example carries passing
verification verdicts and all the views). The slow test actually re-runs the generator — which gates
every example through `nl-validator` — and confirms it reproduces the committed corpus byte-for-byte
(determinism + live re-verification). The slow test skips if the validator isn't built.
"""

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

_HERE = Path(__file__).resolve()
_CORPUS_DIR = _HERE.parents[1]
_CORPUS = _CORPUS_DIR / "corpus.jsonl"
_GEN = _CORPUS_DIR / "gen_corpus.py"
_VALIDATOR = _HERE.parents[2] / "validator" / "target" / "release" / "nl-validator"


def _load(path):
    with open(path, encoding="utf-8") as fh:
        return [json.loads(line) for line in fh if line.strip()]


class CommittedCorpusTests(unittest.TestCase):
    def test_every_example_is_fully_verified(self):
        examples = _load(_CORPUS)
        self.assertGreaterEqual(len(examples), 12)
        for ex in examples:
            v = ex["verification"]
            self.assertTrue(v["schema_valid"], f"{ex['id']} not schema-valid")
            self.assertTrue(v["well_typed"], f"{ex['id']} not well-typed")
            self.assertNotEqual(v["examples_passed"], "FAILED", f"{ex['id']} examples failed")
            for p in v["proofs"]:
                self.assertEqual(p["verdict"], "PROVED", f"{ex['id']} property {p['name']} not proved")

    def test_every_example_has_all_views(self):
        for ex in _load(_CORPUS):
            for key in ("intent", "summary", "tags"):
                self.assertTrue(ex.get(key), f"{ex['id']} missing {key}")
            views = ex["views"]
            for key in ("surface_type", "surface_body", "record", "body", "examples"):
                self.assertIsNotNone(views.get(key), f"{ex['id']} missing view {key}")
            # The record's content-address is a real fn_ hash and its body a real expr_ address.
            self.assertTrue(views["record"]["hash"].startswith("fn_"))
            self.assertTrue(views["record"]["body_hash"].startswith("expr_"))


class RegenerationTests(unittest.TestCase):
    def test_generator_reproduces_corpus_and_reverifies(self):
        if not _VALIDATOR.exists():
            self.skipTest("nl-validator not built")
        with tempfile.TemporaryDirectory() as d:
            out = Path(d) / "corpus.jsonl"
            proc = subprocess.run([sys.executable, str(_GEN), "--out", str(out)],
                                  capture_output=True, text=True)
            self.assertEqual(proc.returncode, 0, f"generator dropped an unverified example:\n{proc.stderr}")
            self.assertEqual(_load(out), _load(_CORPUS), "regenerated corpus differs from committed (non-deterministic?)")


if __name__ == "__main__":
    unittest.main()
