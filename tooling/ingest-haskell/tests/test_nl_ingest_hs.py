"""Tests for nl-ingest-hs.

    python3 -m unittest discover -s tooling/ingest-haskell/tests

Cross-validation against the Rust nl-validator is skipped if the release binary isn't built.
"""

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
TOOL_DIR = HERE.parent
REPO_ROOT = TOOL_DIR.parent.parent
SPEC_DIR = REPO_ROOT / "spec"
VALIDATOR = REPO_ROOT / "tooling" / "validator" / "target" / "release" / "nl-validator"
FR_SCHEMA = SPEC_DIR / "function-record.schema.json"
SAMPLE = HERE / "Sample.hs"

sys.path.insert(0, str(TOOL_DIR))
sys.path.insert(0, str(REPO_ROOT / "tooling" / "ingest-common"))
import nl_ingest_hs as h  # noqa: E402
import nl_core as c  # noqa: E402


class TestCore(unittest.TestCase):
    def test_blake3_reference(self):
        self.assertEqual(c.blake3_256_pure(b"").hex(),
                         "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262")


class TestComments(unittest.TestCase):
    def test_block_and_line(self):
        src = "foo :: Int  -- trailing\n{- block\n comment -}\nbar :: Int\n"
        out = h.strip_comments(src)
        self.assertNotIn("trailing", out)
        self.assertNotIn("block", out)
        self.assertIn("foo :: Int", out)
        self.assertIn("bar :: Int", out)

    def test_double_dash_in_operator_not_a_comment(self):
        # `-->` is an operator, not a comment start.
        self.assertIn("-->", h.strip_comments("a -->  b\n"))


class TestExports(unittest.TestCase):
    def test_explicit_list(self):
        mod, exports = h.parse_module("module M (foo, bar, Baz(..), (<+>)) where\n")
        self.assertEqual(mod, "M")
        self.assertEqual(exports, {"foo", "bar", "<+>"})

    def test_no_list_exports_everything(self):
        mod, exports = h.parse_module("module M where\n")
        self.assertEqual(mod, "M")
        self.assertIsNone(exports)


class TestSignatures(unittest.TestCase):
    def _sigs(self, src):
        return {n: ty for names, ty in h.parse_signatures(h.strip_comments(src)) for n in names}

    def test_simple(self):
        self.assertEqual(self._sigs("double :: Int -> Int\n")["double"], "Int -> Int")

    def test_multiline_with_colon_on_continuation(self):
        src = "mapMaybe\n  :: (a -> Maybe b)\n  -> [a]\n  -> [b]\n"
        self.assertEqual(self._sigs(src)["mapMaybe"], "(a -> Maybe b) -> [a] -> [b]")

    def test_shared_signature(self):
        sigs = self._sigs("f, g :: Int -> Int\n")
        self.assertEqual(sigs["f"], "Int -> Int")
        self.assertEqual(sigs["g"], "Int -> Int")

    def test_indented_not_top_level(self):
        # A where/let-bound signature is indented and must not be picked up.
        src = "foo :: Int\nfoo = bar\n  where bar :: Int\n        bar = 1\n"
        self.assertEqual(set(self._sigs(src)), {"foo"})


class TestArity(unittest.TestCase):
    def test_counts(self):
        self.assertEqual(h.arity_of("Int"), 0)
        self.assertEqual(h.arity_of("Int -> Int"), 1)
        self.assertEqual(h.arity_of("a -> b -> a"), 2)

    def test_nested_arrow_not_counted(self):
        self.assertEqual(h.arity_of("(b -> c) -> (a -> b) -> a -> c"), 3)

    def test_context_stripped(self):
        self.assertEqual(h.arity_of("Semigroup a => a -> a -> a"), 2)
        self.assertEqual(h.arity_of("(Eq a, Show a) => a -> Bool"), 1)

    def test_forall_stripped(self):
        self.assertEqual(h.arity_of("forall a. a -> a"), 1)


class TestRecords(unittest.TestCase):
    def test_exported_only(self):
        recs = h.records_from_source(SAMPLE.read_text(), None, include_private=False)
        names = {(r["name_hints"][0] if r["name_hints"] else "<op>") for r in recs}
        self.assertEqual(names, {"double", "mapmaybe", "compose", "konst", "<op>"})

    def test_include_private(self):
        recs = h.records_from_source(SAMPLE.read_text(), None, include_private=True)
        self.assertIn("secrethelper", {r["name_hints"][0] for r in recs if r["name_hints"]})

    def test_module_hint_from_header(self):
        recs = h.records_from_source(SAMPLE.read_text(), None, include_private=False)
        double = next(r for r in recs if r["name_hints"] and r["name_hints"][0] == "double")
        self.assertIn("data_sample_double", double["name_hints"])

    def test_operator_has_empty_hints(self):
        recs = h.records_from_source(SAMPLE.read_text(), None, include_private=False)
        op = next(r for r in recs if not r["name_hints"])
        self.assertEqual(op["signature"]["type"], "Semigroup a => a -> a -> a")
        self.assertEqual(len(op["examples"][0]["args"]), 2)

    def test_hash_self_consistent(self):
        rec = h.records_from_source("module M (f) where\nf :: Int -> Int\nf x = x\n", None, False)[0]
        self.assertEqual(c.content_hash(rec, "fn", strip=("hash",)), rec["hash"])


@unittest.skipUnless(VALIDATOR.exists(), "nl-validator release binary not built")
class TestCrossValidation(unittest.TestCase):
    def _run(self, *args):
        return subprocess.run([str(VALIDATOR), *args], capture_output=True, text=True)

    def test_sample_records_validate_and_verify(self):
        recs = h.records_from_source(SAMPLE.read_text(), None, include_private=True)
        self.assertGreaterEqual(len(recs), 6)
        for rec in recs:
            with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
                json.dump(rec, f)
                path = f.name
            v = self._run("validate", str(FR_SCHEMA), path)
            self.assertEqual(v.returncode, 0, v.stderr)
            r = self._run("verify", path)
            self.assertEqual(r.returncode, 0, r.stderr)

    def test_validator_hash_matches(self):
        rec = h.records_from_source(SAMPLE.read_text(), None, include_private=True)[0]
        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
            json.dump(rec, f)
            path = f.name
        out = self._run("hash", path)
        self.assertEqual(out.stdout.strip(), rec["hash"])


class TestV2TypeMapping(unittest.TestCase):
    def test_containers_and_arrows(self):
        self.assertEqual(
            h.hs_type_ast("[Int] -> Maybe Bool"),
            {"kind": "fn",
             "params": [{"kind": "apply", "ctor": {"kind": "builtin", "name": "List"},
                         "args": [{"kind": "builtin", "name": "int"}]}],
             "result": {"kind": "apply", "ctor": {"kind": "builtin", "name": "Maybe"},
                        "args": [{"kind": "builtin", "name": "bool"}]}})

    def test_type_vars_quantified(self):
        t = h.hs_type_ast("a -> [a] -> [a]")
        self.assertEqual(t["kind"], "forall")
        self.assertEqual(t["vars"], ["a"])


@unittest.skipUnless(VALIDATOR.exists(), "nl-validator release binary not built")
class TestV2Records(unittest.TestCase):
    FR_V2 = SPEC_DIR / "function-record.v0.2.schema.json"
    SRC = (
        "module Demo (double, noex) where\n\n"
        "-- | Doubles.\n--\n-- >>> double 5\n-- 10\n-- >>> double 0\n-- 0\n"
        "double :: Int -> Int\ndouble n = n * 2\n\n"
        "-- | No doctest.\nnoex :: Int -> Int\nnoex x = x\n"
    )

    def _run(self, *a):
        return subprocess.run([str(VALIDATOR), *a], capture_output=True, text=True)

    def test_v2_from_haddock_doctest_and_fallback(self):
        recs = {r["name_hints"][0]: r for r in h.records_from_source(self.SRC, "demo", False, v2=True)}
        d = recs["double"]
        self.assertEqual(d["schema_version"], "0.2.0")
        self.assertEqual(d["signature"]["type"],
                         {"kind": "fn", "params": [{"kind": "builtin", "name": "int"}],
                          "result": {"kind": "builtin", "name": "int"}})
        self.assertEqual(d["examples"][0],
                         {"args": [{"kind": "int", "value": 5}], "result": {"kind": "int", "value": 10}})
        self.assertEqual(recs["noex"]["schema_version"], "0.1.0")   # no doctest -> v0.1 fallback

        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
            json.dump(d, f)
            path = f.name
        self.assertEqual(self._run("validate", str(self.FR_V2), path).returncode, 0)
        self.assertEqual(self._run("verify", path).returncode, 0)


if __name__ == "__main__":
    unittest.main(verbosity=2)
