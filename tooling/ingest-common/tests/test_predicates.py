"""Tests for the predicate-expression AST builder (nl_predicates), incl. cross-validation against
nl-validator check-predicate (op-arity well-formedness)."""

import ast
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

_HERE = Path(__file__).resolve()
sys.path.insert(0, str(_HERE.parents[1]))                 # tooling/ingest-common
from nl_predicates import PredicateError, predicate_from_py  # noqa: E402

VALIDATOR = _HERE.parents[2] / "validator" / "target" / "release" / "nl-validator"


def pred(src):
    return predicate_from_py(ast.parse(src, mode="eval").body)


class PredicateMappingTests(unittest.TestCase):
    def test_comparison(self):
        self.assertEqual(pred("n > 0"),
                         {"kind": "app", "op": "gt",
                          "args": [{"kind": "var", "name": "n"}, {"kind": "lit", "value": 0}]})

    def test_length_equality(self):
        self.assertEqual(pred("len(output) == len(input)"), {
            "kind": "app", "op": "eq", "args": [
                {"kind": "app", "op": "length", "args": [{"kind": "var", "name": "output"}]},
                {"kind": "app", "op": "length", "args": [{"kind": "var", "name": "input"}]}]})

    def test_bool_and_arith(self):
        self.assertEqual(pred("a and b"),
                         {"kind": "app", "op": "and",
                          "args": [{"kind": "var", "name": "a"}, {"kind": "var", "name": "b"}]})
        self.assertEqual(pred("not done")["op"], "not")
        self.assertEqual(pred("x + 1")["op"], "add")

    def test_chained_and_variadic_are_binary(self):
        c = pred("0 < x < 10")
        self.assertEqual((c["op"], [a["op"] for a in c["args"]]), ("and", ["lt", "lt"]))
        v = pred("a and b and c")
        self.assertEqual((v["op"], v["args"][1]["op"]), ("and", "and"))   # and(a, and(b, c))

    def test_unsupported_raise(self):
        for s in ("f(x)", "x in xs", "x is None", "obj.method()"):
            with self.assertRaises(PredicateError):
                pred(s)


@unittest.skipUnless(VALIDATOR.exists(), "nl-validator release binary not built")
class CheckPredicateConformanceTests(unittest.TestCase):
    def test_well_formed(self):
        for s in ("n > 0", "len(output) == len(input)", "a and b or not c", "0 <= i < n",
                  "x % 2 == 0", "-x < y"):
            p = pred(s)
            with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
                json.dump(p, f)
                path = f.name
            r = subprocess.run([str(VALIDATOR), "check-predicate", path], capture_output=True, text=True)
            self.assertEqual(r.returncode, 0, f"check-predicate failed for {s!r}: {r.stderr}")


if __name__ == "__main__":
    unittest.main()
