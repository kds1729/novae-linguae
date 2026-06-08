"""Tests for nl-ingest-py.

Run from anywhere:

    python3 -m unittest discover -s tooling/ingest-python/tests

or directly:

    python3 tooling/ingest-python/tests/test_nl_ingest.py

The cross-implementation tests (against the Rust ``nl-validator``) are skipped automatically
if the release binary has not been built.
"""

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
TOOL_DIR = HERE.parent
REPO_ROOT = TOOL_DIR.parent.parent  # tooling/ingest-python -> tooling -> repo root
SPEC_DIR = REPO_ROOT / "spec"
VALIDATOR = REPO_ROOT / "tooling" / "validator" / "target" / "release" / "nl-validator"
FR_SCHEMA = SPEC_DIR / "function-record.schema.json"
SAMPLE = HERE / "sample.py"

sys.path.insert(0, str(TOOL_DIR))
import nl_ingest as n  # noqa: E402


class TestBlake3(unittest.TestCase):
    """Vendored pure-Python BLAKE3 against official reference vectors (input byte i = i % 251)."""

    VECTORS = {
        0: "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262",
        1: "2d3adedff11b61f14c886e35afa036736dcd87a74d27b5c1510225d0f592e213",
        64: "4eed7141ea4a5cd4b788606bd23f46e212af9cacebacdc7d1f4c6dc7f2511b98",
        1024: "42214739f095a406f3fc83deb889744ac00df831c10daa55189b5d121c855af7",
        1025: "d00278ae47eb27b34faecf67b4fe263f82d5412916c1ffd97c8cb7fb814b8444",
        2048: "e776b6028c7cd22a4d0ba182a8bf62205d2ef576467e838ed6f2529b85fba24a",
        3072: "b98cb0ff3623be03326b373de6b9095218513e64f1ee2edd2525c7ad1e5cffd2",
    }

    def test_reference_vectors(self):
        for length, expected in self.VECTORS.items():
            data = bytes(i % 251 for i in range(length))
            self.assertEqual(n._blake3_256_pure(data).hex(), expected, f"length {length}")


class TestJCS(unittest.TestCase):
    def test_spec_worked_example(self):
        # The exact record and canonical form from spec/canonical-serialization.md.
        record = {
            "schema_version": "0.1.0",
            "name_hints": ["map"],
            "signature": {
                "type": "forall a b. (a -> b) -> List a -> List b",
                "effects": [],
                "capabilities": [],
                "terminates": "conditional",
            },
            "examples": [{"args": ["double", [1, 2, 3]], "result": [2, 4, 6]}],
            "body_hash": "expr_8f2c7d6e5b4a392817160f0e0d0c0b0a09080706050403020100ffeeddccbbaa",
        }
        expected = (
            '{"body_hash":"expr_8f2c7d6e5b4a392817160f0e0d0c0b0a09080706050403020100ffeeddccbbaa",'
            '"examples":[{"args":["double",[1,2,3]],"result":[2,4,6]}],'
            '"name_hints":["map"],'
            '"schema_version":"0.1.0",'
            '"signature":{"capabilities":[],"effects":[],"terminates":"conditional",'
            '"type":"forall a b. (a -> b) -> List a -> List b"}}'
        )
        self.assertEqual(n.canonicalize(record).decode("utf-8"), expected)

    def test_key_ordering_independent_of_source_order(self):
        a = n.canonicalize({"b": 1, "a": 2})
        b = n.canonicalize({"a": 2, "b": 1})
        self.assertEqual(a, b)
        self.assertEqual(a.decode(), '{"a":2,"b":1}')

    def test_no_whitespace_and_tight_separators(self):
        out = n.canonicalize({"x": [1, 2], "y": {"z": True}}).decode()
        self.assertEqual(out, '{"x":[1,2],"y":{"z":true}}')


class TestEndToEndHash(unittest.TestCase):
    """JCS + BLAKE3 must reproduce the hashes the project already pins on its examples."""

    def _check(self, filename, prefix, strip):
        path = SPEC_DIR / "examples" / filename
        if not path.exists():
            self.skipTest(f"{path} not present")
        rec = json.loads(path.read_text())
        self.assertEqual(n.content_hash(rec, prefix, strip=strip), rec["hash"])

    def test_map_record(self):
        self._check("map.json", "fn", ("hash",))

    def test_double_v02_record(self):
        self._check("double.v0.2.json", "fn", ("hash",))


class TestTypeMapping(unittest.TestCase):
    def _types(self, src):
        recs = n.records_from_source(src, None, include_private=False)
        return {r["name_hints"][0]: r["signature"]["type"] for r in recs}

    def test_atomic_and_containers(self):
        src = (
            "def f(n: int, s: str, b: bool, x: float, y: bytes) -> None: ...\n"
            "def g(xs: list[int], d: dict[str, int], st: set[str]) -> int: ...\n"
        )
        t = self._types(src)
        self.assertEqual(t["f"], "(int, string, bool, float, bytes) -> unit")
        self.assertEqual(t["g"], "(List int, Map string int, Set string) -> int")

    def test_optional_and_union(self):
        src = (
            "from typing import Optional, Union\n"
            "def a(x: Optional[int]) -> int: ...\n"
            "def b(x: int | None) -> str: ...\n"
            "def c(x: Union[int, str]) -> bool: ...\n"
        )
        t = self._types(src)
        self.assertEqual(t["a"], "(Maybe int) -> int")
        self.assertEqual(t["b"], "(Maybe int) -> string")
        self.assertEqual(t["c"], "(int | string) -> bool")

    def test_typevars_become_forall(self):
        src = (
            "from typing import TypeVar\n"
            "T = TypeVar('T')\n"
            "def ident(x: T) -> T: ...\n"
        )
        t = self._types(src)
        self.assertEqual(t["ident"], "forall t. (t) -> t")

    def test_pep695_typevars(self):
        src = "def head[A](xs: list[A]) -> A: ...\n"
        t = self._types(src)
        self.assertEqual(t["head"], "forall a. (List a) -> a")

    def test_unannotated_is_unknown(self):
        src = "def h(a, b): ...\n"
        t = self._types(src)
        self.assertEqual(t["h"], "(unknown, unknown) -> unknown")


class TestVisibility(unittest.TestCase):
    def test_dunder_all_is_authoritative(self):
        src = (
            "__all__ = ['keep']\n"
            "def keep(): ...\n"
            "def drop(): ...\n"
        )
        names = [r["name_hints"][0] for r in n.records_from_source(src, None, False)]
        self.assertEqual(names, ["keep"])

    def test_underscore_excluded_without_all(self):
        src = "def public(): ...\ndef _private(): ...\n"
        names = [r["name_hints"][0] for r in n.records_from_source(src, None, False)]
        self.assertEqual(names, ["public"])

    def test_include_private_overrides(self):
        src = "__all__ = ['a']\ndef a(): ...\ndef _b(): ...\n"
        names = [r["name_hints"][0] for r in n.records_from_source(src, None, True)]
        # _b is ingested but its name_hint is sanitized to 'b' (the pattern forbids a leading '_').
        self.assertEqual(sorted(names), ["a", "b"])


class TestRecordShape(unittest.TestCase):
    def test_hash_is_self_consistent(self):
        recs = n.records_from_source("def f(a: int, b: int) -> int:\n    return a + b\n", "m", False)
        rec = recs[0]
        self.assertEqual(n.content_hash(rec, "fn", strip=("hash",)), rec["hash"])
        self.assertTrue(rec["hash"].startswith("fn_"))
        self.assertTrue(rec["body_hash"].startswith("expr_"))
        self.assertEqual(rec["name_hints"], ["f", "m_f"])
        self.assertEqual(rec["examples"][0]["args"], [None, None])  # arity 2
        self.assertEqual(rec["schema_version"], "0.1.0")

    def test_body_hash_changes_with_body(self):
        r1 = n.records_from_source("def f() -> int:\n    return 1\n", None, False)[0]
        r2 = n.records_from_source("def f() -> int:\n    return 2\n", None, False)[0]
        self.assertNotEqual(r1["body_hash"], r2["body_hash"])


@unittest.skipUnless(VALIDATOR.exists(), "nl-validator release binary not built")
class TestCrossValidation(unittest.TestCase):
    """The decisive contract: every record the Python tool emits must pass the Rust validator."""

    @classmethod
    def setUpClass(cls):
        src = SAMPLE.read_text()
        cls.records = n.records_from_source(src, "sample", include_private=True)
        cls.assertTrue(cls, len(cls.records) >= 7)

    def _run(self, *args):
        return subprocess.run([str(VALIDATOR), *args], capture_output=True, text=True)

    def test_all_records_validate_and_verify(self):
        for rec in self.records:
            with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
                json.dump(rec, f)
                path = f.name
            v = self._run("validate", str(FR_SCHEMA), path)
            self.assertEqual(v.returncode, 0, f"validate failed for {rec['name_hints'][0]}: {v.stderr}")
            r = self._run("verify", path)
            self.assertEqual(r.returncode, 0, f"verify failed for {rec['name_hints'][0]}: {r.stderr}")

    def test_validator_hash_matches_python_hash(self):
        rec = self.records[0]
        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
            json.dump(rec, f)
            path = f.name
        out = self._run("hash", path)
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertEqual(out.stdout.strip(), rec["hash"])

    def test_large_record_crosses_chunk_boundary(self):
        # Force a >1024-byte canonical form so the record hash exercises multi-chunk BLAKE3,
        # then confirm the Rust validator agrees on the hash.
        big = "def f(" + ", ".join(f"a{i}: int" for i in range(120)) + ") -> int:\n    return 0\n"
        rec = n.records_from_source(big, None, False)[0]
        self.assertGreater(len(n.canonicalize({k: v for k, v in rec.items() if k != 'hash'})), 1024)
        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
            json.dump(rec, f)
            path = f.name
        out = self._run("hash", path)
        self.assertEqual(out.stdout.strip(), rec["hash"], "multi-chunk hash disagreement with nl-validator")


@unittest.skipUnless(VALIDATOR.exists(), "nl-validator release binary not built")
class TestV2Records(unittest.TestCase):
    FR_V2 = SPEC_DIR / "function-record.v0.2.schema.json"
    SRC = (
        'def add(a: int, b: int) -> int:\n'
        '    """Sum two integers.\n\n    >>> add(2, 3)\n    5\n    """\n'
        '    return a + b\n\n'
        'def noex(x: int) -> int:\n'
        '    "No doctest here."\n'
        '    return x\n'
    )

    def _run(self, *a):
        return subprocess.run([str(VALIDATOR), *a], capture_output=True, text=True)

    def test_v2_record_from_doctest_and_fallback(self):
        recs = {r["name_hints"][0]: r for r in n.records_from_source(self.SRC, "m", False, v2=True)}

        add = recs["add"]
        self.assertEqual(add["schema_version"], "0.2.0")
        self.assertEqual(add["signature"]["type"], {
            "kind": "fn",
            "params": [{"kind": "builtin", "name": "int"}, {"kind": "builtin", "name": "int"}],
            "result": {"kind": "builtin", "name": "int"}})
        self.assertEqual(add["examples"], [{
            "args": [{"kind": "int", "value": 2}, {"kind": "int", "value": 3}],
            "result": {"kind": "int", "value": 5}}])

        # A function with no usable doctest falls back to a v0.1 record (never dropped).
        self.assertEqual(recs["noex"]["schema_version"], "0.1.0")

        # The v0.2 record validates against the v0.2 schema and its hash verifies.
        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
            json.dump(add, f)
            path = f.name
        self.assertEqual(self._run("validate", str(self.FR_V2), path).returncode, 0,
                         self._run("validate", str(self.FR_V2), path).stderr)
        self.assertEqual(self._run("verify", path).returncode, 0)

    def test_v2_float_example_verifies(self):
        # A float-valued example exercises canonical float serialization; the hash must still verify
        # against the Rust validator (proving the Python JCS float output matches serde_jcs).
        src = ('def half(x: int) -> float:\n'
               '    """Halve.\n\n    >>> half(5)\n    2.5\n    """\n'
               '    return x / 2\n')
        rec = n.records_from_source(src, None, False, v2=True)[0]
        self.assertEqual(rec["schema_version"], "0.2.0")
        self.assertEqual(rec["examples"][0]["result"], {"kind": "float", "value": 2.5})
        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
            json.dump(rec, f)
            path = f.name
        self.assertEqual(self._run("verify", path).returncode, 0)
        self.assertEqual(self._run("validate", str(self.FR_V2), path).returncode, 0)

    def test_v2_precondition_refinement(self):
        # A leading `assert` becomes a refinement precondition (predicate AST).
        src = ('def safe_div(a: int, b: int) -> float:\n'
               '    """Divide.\n\n    >>> safe_div(6, 2)\n    3.0\n    """\n'
               '    assert b != 0\n'
               '    return a / b\n')
        rec = n.records_from_source(src, None, False, v2=True)[0]
        self.assertEqual(rec["signature"]["refinements"], [{
            "kind": "pre",
            "expr": {"kind": "app", "op": "neq",
                     "args": [{"kind": "var", "name": "b"}, {"kind": "lit", "value": 0}]}}])
        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
            json.dump(rec, f)
            path = f.name
        self.assertEqual(self._run("validate", str(self.FR_V2), path).returncode, 0)
        self.assertEqual(self._run("verify", path).returncode, 0)
        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
            json.dump(rec["signature"]["refinements"][0]["expr"], f)
            pp = f.name
        self.assertEqual(self._run("check-predicate", pp).returncode, 0)


@unittest.skipUnless(VALIDATOR.exists(), "nl-validator release binary not built")
class TestExecutableCorpus(unittest.TestCase):
    """End-to-end: ingest real-shaped library functions (conditionals → case, local bindings → let,
    a mapped `abs` builtin) to v0.2 records with doctest examples AND executable body ASTs, then
    RUN them with `nl-validator run --records` — the ingested corpus actually executes."""

    def test_ingested_functions_run_against_their_doctests(self):
        src = (Path(__file__).resolve().parent / "sample_executable.py").read_text(encoding="utf-8")
        records = n.records_from_source(src, "sample", include_private=False, v2=True)
        bodies = n.bodies_from_source(src, include_private=False)
        self.assertEqual(len(records), 5)               # clamp / sign / abs_diff / squares / total
        self.assertEqual(len(bodies), 5)                # every body is in the executable subset

        with tempfile.TemporaryDirectory() as tmp:
            d = Path(tmp)
            for h, body in bodies.items():
                (d / f"{h}.json").write_text(json.dumps(body))
            for rec in records:
                (d / f"{rec['hash']}.json").write_text(json.dumps(rec))
            for rec in records:
                name = rec["name_hints"][0]
                # The record's body_hash is a real emitted body, not a synthetic source-hash fallback.
                self.assertIn(rec["body_hash"], bodies, name)
                out = subprocess.run(
                    [str(VALIDATOR), "run", str(d / f"{rec['hash']}.json"), "--records", str(d)],
                    capture_output=True, text=True)
                self.assertEqual(out.returncode, 0, f"{name}: {out.stdout}{out.stderr}")
                self.assertIn("examples passed", out.stdout, name)


if __name__ == "__main__":
    unittest.main(verbosity=2)
