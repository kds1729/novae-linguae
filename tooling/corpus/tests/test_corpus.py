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
        self.assertGreaterEqual(len(examples), 20)
        self.assertTrue(any(e["modality"] == "nova_lingua" for e in examples))
        self.assertTrue(any(e["modality"] == "nova_locutio" for e in examples))
        self.assertTrue(any(e["polarity"] == "negative" for e in examples))
        for ex in examples:
            v = ex["verification"]
            # A NEGATIVE example is "verified to be rejected": the reference verifier must have rejected it.
            if ex["polarity"] == "negative":
                self.assertTrue(v["rejected"], f"{ex['id']} (negative) was not rejected by {v['check']}")
                continue
            # A positive COMPOSITION example: the pipeline composes (checked by `nl-validator compose`).
            if ex["category"] == "composition":
                self.assertTrue(v["composable"], f"{ex['id']} composition is not composable")
                continue
            # A positive multi-turn TRANSCRIPT: all messages schema-valid, threaded, non-failure outcome.
            if ex["category"] == "transcript":
                self.assertTrue(v["all_schema_valid"], f"{ex['id']} transcript not all schema-valid")
                self.assertTrue(v["threaded"], f"{ex['id']} transcript not threaded")
                self.assertFalse(v["outcome"].startswith("NOT"), f"{ex['id']} transcript outcome {v['outcome']}")
                continue
            if ex["modality"] == "nova_lingua":
                self.assertTrue(v["schema_valid"], f"{ex['id']} not schema-valid")
                self.assertTrue(v["well_typed"], f"{ex['id']} not well-typed")
                self.assertNotEqual(v["examples_passed"], "FAILED", f"{ex['id']} examples failed")
                for p in v["proofs"]:
                    self.assertEqual(p["verdict"], "PROVED", f"{ex['id']} property {p['name']} not proved")
            else:  # nova_locutio — a signed agent-loop exchange
                self.assertTrue(v["request_schema_valid"], f"{ex['id']} request not schema-valid")
                self.assertTrue(v["reply_schema_valid"], f"{ex['id']} reply not schema-valid")
                self.assertTrue(v["threaded"], f"{ex['id']} reply not threaded to request")
                # request/apply exchanges carry a claim re-run to CONFIRMED; other acts succeed differently.
                if v["outcome"].startswith("NOT") or v["outcome"] in ("NO-REPLY", "REJECT"):
                    self.fail(f"{ex['id']} positive exchange has failure outcome {v['outcome']}")

    def test_every_example_has_all_views(self):
        for ex in _load(_CORPUS):
            for key in ("intent", "summary", "tags", "modality", "polarity", "category"):
                self.assertTrue(ex.get(key), f"{ex['id']} missing {key}")
            views = ex["views"]
            if ex["category"] == "composition":
                for k in ("pipeline", "stages", "composite"):
                    self.assertIn(k, views, f"{ex['id']} missing view {k}")
                continue
            if ex["category"] == "transcript":
                # A multi-turn transcript: a chain of >= 3 real signed messages, threaded by in_reply_to.
                self.assertIn("transcript", views, f"{ex['id']} missing transcript")
                msgs = views["transcript"]
                self.assertGreaterEqual(len(msgs), 3, f"{ex['id']} transcript too short")
                for m in msgs:
                    self.assertTrue(m["hash"].startswith("msg_"))
                    self.assertTrue(m["signature"].startswith("ed25519:"))
                    self.assertTrue(m["from"].startswith("did:nova:"))
                for i in range(1, len(msgs)):
                    self.assertEqual(msgs[i].get("in_reply_to"), msgs[i - 1]["hash"],
                                     f"{ex['id']} turn {i} not threaded to the previous message")
                continue
            if ex["polarity"] == "negative":
                # Negatives carry the offending artifact: a record+body (lingua) or a message (locutio).
                if ex["modality"] == "nova_lingua":
                    self.assertIn("record", views, f"{ex['id']} missing record")
                    self.assertIn("body", views, f"{ex['id']} missing body")
                else:  # nova_locutio negative: a single offending message, or a rejected exchange.
                    self.assertTrue("message" in views or "request" in views, f"{ex['id']} missing message/request")
                continue
            if ex["modality"] == "nova_lingua":
                for key in ("surface_type", "surface_body", "record", "body", "examples"):
                    self.assertIsNotNone(views.get(key), f"{ex['id']} missing view {key}")
                self.assertTrue(views["record"]["hash"].startswith("fn_"))
                self.assertTrue(views["record"]["body_hash"].startswith("expr_"))
            else:  # nova_locutio
                for key in ("speech_act", "request", "reply"):
                    self.assertIsNotNone(views.get(key), f"{ex['id']} missing view {key}")
                # Both messages are real signed Nova Locutio messages.
                for m in (views["request"], views["reply"]):
                    self.assertTrue(m["hash"].startswith("msg_"))
                    self.assertTrue(m["signature"].startswith("ed25519:"))
                    self.assertTrue(m["from"].startswith("did:nova:"))


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
