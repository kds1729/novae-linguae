"""Tests for the curated law catalog (property_catalog) + matcher, and an end-to-end check that
catalog laws attached by nl-ingest-py are CONSISTENT under nl-validator check-properties."""

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

_HERE = Path(__file__).resolve()
sys.path.insert(0, str(_HERE.parents[1]))  # tooling/ingest-common
from property_catalog import match_catalog  # noqa: E402

VALIDATOR = _HERE.parents[2] / "validator" / "target" / "release" / "nl-validator"
PY_INGEST = _HERE.parents[2] / "ingest-python" / "nl_ingest.py"


class CatalogMatchTests(unittest.TestCase):
    def test_identity_matches_by_name_and_arity(self):
        props, tags = match_catalog(["id"], 1)
        self.assertEqual([p["name"] for p in props], ["identity"])
        self.assertIn("idempotent", tags)

    def test_arity_must_match(self):
        # `map` law requires arity 2; a 1-ary "map" does not match.
        self.assertEqual(match_catalog(["map"], 1), ([], []))
        props, _ = match_catalog(["map"], 2)
        self.assertEqual([p["name"] for p in props], ["length_preserving"])

    def test_no_match(self):
        self.assertEqual(match_catalog(["frobnicate"], 3), ([], []))

    def test_emitted_property_uses_predicate_ast(self):
        props, _ = match_catalog(["reverse"], 1)
        expr = props[0]["expr"]
        self.assertEqual(expr["kind"], "app")
        self.assertEqual(expr["op"], "eq")


@unittest.skipUnless(VALIDATOR.exists(), "nl-validator release binary not built")
class EndToEndTests(unittest.TestCase):
    def _ingest(self, src):
        with tempfile.NamedTemporaryFile("w", suffix=".py", delete=False) as fh:
            fh.write(src)
            path = fh.name
        out = subprocess.run([sys.executable, str(PY_INGEST), "--properties", path],
                             capture_output=True, text=True)
        self.assertEqual(out.returncode, 0, out.stderr)
        return [json.loads(l) for l in out.stdout.splitlines() if l.strip()]

    def _check_properties(self, record):
        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as fh:
            json.dump(record, fh)
            path = fh.name
        return subprocess.run([str(VALIDATOR), "check-properties", path],
                              capture_output=True, text=True)

    def test_reverse_gets_length_law_and_is_consistent(self):
        src = (
            "def reverse(xs):\n"
            '    """Reverse a list.\n\n'
            "    >>> reverse([1, 2, 3])\n"
            "    [3, 2, 1]\n"
            '    """\n'
            "    return xs[::-1]\n"
        )
        recs = self._ingest(src)
        rev = next(r for r in recs if r["name_hints"][0] == "reverse")
        self.assertEqual([p["name"] for p in rev.get("properties", [])], ["length_preserving"])
        self.assertIn("lossless", rev["intent_tags"])
        res = self._check_properties(rev)
        self.assertEqual(res.returncode, 0, res.stderr)
        self.assertIn("length_preserving: CONSISTENT", res.stdout)

    def test_contradicting_example_is_caught(self):
        # A function named `reverse` whose example does NOT preserve length -> the catalog law is
        # CONTRADICTED and check-properties fails (exit 1). This is the safety net for mis-matches.
        rev = {
            "schema_version": "0.2.0", "hash": "fn_" + "0" * 64,
            "name_hints": ["reverse"],
            "signature": {"type": {"kind": "fn", "params": [], "result": {"kind": "builtin", "name": "unit"}},
                          "refinements": [], "effects": [], "capabilities": [], "terminates": "unknown"},
            "examples": [{"args": [{"kind": "list", "elems": [{"kind": "int", "value": 1},
                                                              {"kind": "int", "value": 2}]}],
                          "result": {"kind": "list", "elems": [{"kind": "int", "value": 1}]}}],
            "properties": [{"name": "length_preserving",
                            "expr": {"kind": "app", "op": "eq", "args": [
                                {"kind": "app", "op": "length", "args": [{"kind": "var", "name": "result"}]},
                                {"kind": "app", "op": "length", "args": [{"kind": "var", "name": "arg0"}]}]}}],
            "intent_tags": [], "derived_from": None, "supersedes": None,
            "body_hash": "expr_" + "0" * 64,
        }
        res = self._check_properties(rev)
        self.assertEqual(res.returncode, 1)
        self.assertIn("CONTRADICTED", res.stdout)


if __name__ == "__main__":
    unittest.main()
