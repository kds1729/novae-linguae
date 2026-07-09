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
        # clamp / sign / abs_diff / squares / total, plus the statement-subset extensions
        # sum_positives / count_evens (guarded folds), doubled / keep_positive /
        # squares_of_evens (list-building append loops -> map/filter), first_negative /
        # contains / double_first_even (early-return search loops -> filter/head), and
        # sum_minus_count / even_sum_and_count (independent multi-accumulator loops -> N folds),
        # and flatten / evens_of_rows (nested list-building loops -> a foldl of appends).
        self.assertEqual(len(records), 17)
        self.assertEqual(len(bodies), 17)               # every body is in the executable subset

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


class TestStringIdiomBodies(unittest.TestCase):
    """The phase-4 string-idiom translations (spec/expressiveness.md): `str`-annotated parameters
    drive `+` -> str_concat, `len` -> str_length, `s.split(sep)` -> str_split (separator-first —
    receiver and argument SWAP), `sep.join(xs)` -> str_join, `needle in s` -> str_contains, and
    `str(n)` -> to_string. Unannotated code keeps its numeric/list reading."""

    def _body(self, src):
        import ast as pyast
        import nl_body
        func = pyast.parse(src).body[0]
        b = nl_body.body_ast_from_py(func)
        self.assertIsNotNone(b, src)
        return json.dumps(b)

    def test_string_idioms_translate(self):
        cases = [
            ("def f(s: str):\n    return '<' + s + '>'\n", "str_concat"),
            ("def f(s: str):\n    return len(s)\n", "str_length"),
            ("def f(s: str):\n    return s.split(',')\n", "str_split"),
            ("def f(s: str):\n    return ';'.join(s.split(','))\n", "str_join"),
            ("def f(s: str):\n    return ',' in s\n", "str_contains"),
            ("def f(n):\n    return 'n=' + str(n)\n", "to_string"),
        ]
        for src, builtin in cases:
            self.assertIn(f'"{builtin}"', self._body(src), src)

    def test_split_swaps_receiver_and_separator(self):
        # s.split(",") must become str_split("," , s): separator FIRST.
        import ast as pyast
        import nl_body
        func = pyast.parse("def f(s: str):\n    return s.split(',')\n").body[0]
        body = nl_body.body_ast_from_py(func)["body"]
        self.assertEqual(body["fn"], {"kind": "var", "name": "str_split"})
        self.assertEqual(body["args"][0], {"kind": "lit", "value": {"kind": "string", "value": ","}})
        self.assertEqual(body["args"][1], {"kind": "var", "name": "s"})

    def test_fstrings_translate(self):
        import ast as pyast
        import nl_body
        # f"n={n}" -> str_concat("n=", to_string(n)); a str-annotated interpolation skips to_string.
        body = self._body('def f(n):\n    return f"n={n}"\n')
        self.assertIn('"str_concat"', body)
        self.assertIn('"to_string"', body)
        body2 = self._body('def f(s: str):\n    return f"[{s}]"\n')
        self.assertIn('"str_concat"', body2)
        self.assertNotIn('"to_string"', body2)
        # Conversions / format specs are out of subset (body falls back to None).
        func = pyast.parse('def f(n):\n    return f"{n!r}"\n').body[0]
        self.assertIsNone(nl_body.body_ast_from_py(func))
        func2 = pyast.parse('def f(n):\n    return f"{n:04d}"\n').body[0]
        self.assertIsNone(nl_body.body_ast_from_py(func2))

    def test_ts_string_idioms_translate(self):
        import nl_body
        # A `: string` TS annotation roots the same inference: split fires (separator-first swap),
        # the TS/JS array-join order maps too, includes -> str_contains, String(n) -> to_string.
        b = nl_body.body_ast_from_ts("f", '(s: string) => s.split(",")')
        self.assertIn('"str_split"', json.dumps(b))
        b2 = nl_body.body_ast_from_ts("f", '(xs) => xs.join(",")')
        self.assertIn('"str_join"', json.dumps(b2))
        self.assertEqual(b2["body"]["args"][0], {"kind": "lit", "value": {"kind": "string", "value": ","}},
                         "TS array-join must put the separator FIRST")
        b3 = nl_body.body_ast_from_ts("f", '(s: string) => s.includes("x")')
        self.assertIn('"str_contains"', json.dumps(b3))
        b4 = nl_body.body_ast_from_ts("f", '(n) => "n=" + String(n)')
        self.assertIn('"to_string"', json.dumps(b4))
        self.assertIn('"str_concat"', json.dumps(b4))
        # Unannotated TS split does NOT fire (receiver unproven-string).
        b5 = nl_body.body_ast_from_ts("f", '(s) => s.split(",")')
        self.assertNotIn('"str_split"', json.dumps(b5))

    def test_dict_idioms_translate(self):
        import ast as pyast
        import nl_body
        # The TOTAL dict subset: get-with-default, membership, len, sorted keys.
        b = self._body('def f(d: dict[str, int]):\n    return d.get("k", 0)\n')
        self.assertIn('"map_get"', b)
        self.assertIn('"Just"', b)  # the Maybe is consumed by a case
        b2 = self._body('def f(d: dict):\n    return "k" in d\n')
        self.assertIn('"map_get"', b2)
        b3 = self._body('def f(d: dict[str, int]):\n    return len(d)\n')
        self.assertIn('"map_size"', b3)
        b4 = self._body('def f(d: dict[str, int]):\n    return sorted(d.keys())\n')
        self.assertIn('"map_keys"', b4)
        # The bare 1-arg get (an Optional boundary) and subscript (raises) stay out of subset.
        func = pyast.parse('def f(d: dict):\n    return d.get("k")\n').body[0]
        self.assertIsNone(nl_body.body_ast_from_py(func))
        func2 = pyast.parse('def f(d: dict):\n    return d["k"]\n').body[0]
        self.assertIsNone(nl_body.body_ast_from_py(func2))
        # Unannotated receivers keep the untyped reading (no map_get).
        func3 = pyast.parse('def f(d):\n    return d.get("k", 0)\n').body[0]
        b5 = nl_body.body_ast_from_py(func3)
        self.assertNotIn('"map_get"', json.dumps(b5))

    def test_dict_values_encode_as_maps_or_records(self):
        import nl_values
        map_ty = {"kind": "apply", "ctor": {"kind": "builtin", "name": "Map"},
                  "args": [{"kind": "builtin", "name": "string"}, {"kind": "builtin", "name": "int"}]}
        # Map-typed expectation -> map kind, entries sorted by key.
        v = nl_values.to_value_ast({"b": 2, "a": 1}, map_ty)
        self.assertEqual(v["kind"], "map")
        self.assertEqual([e["key"] for e in v["entries"]], ["a", "b"])
        # No expectation + identifier keys -> the historical record encoding (hash-stable).
        v2 = nl_values.to_value_ast({"x": 1})
        self.assertEqual(v2["kind"], "record")
        # No expectation + non-identifier string keys -> map (previously an error).
        v3 = nl_values.to_value_ast({"two words": 1})
        self.assertEqual(v3["kind"], "map")

    def test_unannotated_keeps_numeric_reading(self):
        # Without a str annotation, + stays add and len stays length — no silent retyping.
        src = "def f(a, b):\n    return a + b\n"
        self.assertIn('"add"', self._body(src))
        self.assertNotIn('"str_concat"', self._body(src))
        src2 = "def f(xs):\n    return len(xs)\n"
        self.assertIn('"length"', self._body(src2))
        # And `in` over an unproven container stays out of subset (body falls back to None).
        import ast as pyast
        import nl_body
        func = pyast.parse("def f(x, xs):\n    return x in xs\n").body[0]
        self.assertIsNone(nl_body.body_ast_from_py(func))


class TestSearchLoopBodies(unittest.TestCase):
    """The early-return search-loop translation: `for x in xs: if c: return e` + a default return
    becomes `let hits = filter(\\x -> c, xs) in case null(hits) of true => default; false =>
    let x = head(hits) in e` — exact in a pure total language (the skipped short-circuit is
    unobservable), reusing existing builtins."""

    def _body(self, src):
        import ast as pyast
        import nl_body
        func = pyast.parse(src).body[0]
        return nl_body.body_ast_from_py(func)

    def test_guarded_search_translates(self):
        src = ("def f(xs):\n"
               "    for x in xs:\n"
               "        if x < 0:\n"
               "            return x\n"
               "    return 0\n")
        s = json.dumps(self._body(src))
        for builtin in ("filter", "null", "head"):
            self.assertIn(f'"{builtin}"', s, src)

    def test_transformed_hit_rebinds_loop_var(self):
        src = ("def f(xs):\n"
               "    for x in xs:\n"
               "        if x % 2 == 0:\n"
               "            return x * 2\n"
               "    return -1\n")
        b = self._body(src)
        self.assertIsNotNone(b)

        # The found branch binds the loop name to head(hits) so the return expression reads it.
        def lets(node):
            if isinstance(node, dict):
                if node.get("kind") == "let":
                    yield node
                for v in node.values():
                    yield from lets(v)
            elif isinstance(node, list):
                for v in node:
                    yield from lets(v)
        x_lets = [l for l in lets(b) if l["name"] == "x"]
        self.assertTrue(any(l["value"].get("fn", {}).get("name") == "head" for l in x_lets), x_lets)

    def test_fresh_hits_name_avoids_collision(self):
        src = ("def f(xs, hits):\n"
               "    for x in xs:\n"
               "        if x > hits:\n"
               "            return x\n"
               "    return hits\n")
        s = json.dumps(self._body(src))
        self.assertIn('"hits_"', s)  # the binder stepped past the parameter's name

    def test_loop_var_after_loop_is_out_of_subset(self):
        # Python leaves x bound to the LAST element after the loop; the translation would not,
        # so reading it afterwards must be refused rather than silently mistranslated.
        src = ("def f(xs):\n"
               "    for x in xs:\n"
               "        if x < 0:\n"
               "            return x\n"
               "    return x\n")
        self.assertIsNone(self._body(src))


if __name__ == "__main__":
    unittest.main(verbosity=2)
