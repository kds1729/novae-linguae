"""Tests for the value-expression AST builder (nl_values), incl. cross-validation against
nl-validator check-value."""

import base64
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

_HERE = Path(__file__).resolve()
sys.path.insert(0, str(_HERE.parents[1]))                 # tooling/ingest-common
from nl_values import ValueEncodeError, to_value_ast      # noqa: E402

VALIDATOR = _HERE.parents[2] / "validator" / "target" / "release" / "nl-validator"


class ToValueAstTests(unittest.TestCase):
    def test_scalars(self):
        self.assertEqual(to_value_ast(True), {"kind": "bool", "value": True})
        self.assertEqual(to_value_ast(5), {"kind": "int", "value": 5})
        self.assertEqual(to_value_ast(3.5), {"kind": "float", "value": 3.5})
        self.assertEqual(to_value_ast("hi"), {"kind": "string", "value": "hi"})
        self.assertEqual(to_value_ast(None), {"kind": "unit"})
        self.assertEqual(to_value_ast(b"ab"),
                         {"kind": "bytes", "value": base64.b64encode(b"ab").decode()})

    def test_nat_only_with_hint_and_non_negative(self):
        nat = {"kind": "builtin", "name": "nat"}
        self.assertEqual(to_value_ast(7, nat), {"kind": "nat", "value": 7})
        self.assertEqual(to_value_ast(-7, nat), {"kind": "int", "value": -7})   # negative stays int

    def test_bigint_becomes_string(self):
        self.assertEqual(to_value_ast(2 ** 60), {"kind": "int", "value": str(2 ** 60)})

    def test_containers(self):
        self.assertEqual(to_value_ast([1, 2]),
                         {"kind": "list", "elems": [{"kind": "int", "value": 1},
                                                    {"kind": "int", "value": 2}]})
        self.assertEqual(to_value_ast(()), {"kind": "unit"})            # empty tuple -> unit
        self.assertEqual(to_value_ast((1,)), {"kind": "int", "value": 1})   # 1-tuple -> the element
        self.assertEqual(to_value_ast((1, 2))["kind"], "tuple")
        self.assertEqual(to_value_ast({"x": 1}),
                         {"kind": "record", "fields": [{"name": "x", "value": {"kind": "int", "value": 1}}]})

    def test_non_identifier_key_dict_is_a_map(self):
        # A dict whose keys aren't identifier-shaped encodes as a `map` value (the 2026-07-04
        # dict-as-map rule) — a hyphenated key is a valid map key, not unrepresentable.
        self.assertEqual(to_value_ast({"Bad-Key": 1}),
                         {"kind": "map", "entries": [{"key": "Bad-Key", "value": {"kind": "int", "value": 1}}]})

    def test_unrepresentable_raise(self):
        # A set, an int-keyed dict (not string keys), an arbitrary object, and a non-finite float
        # are all genuinely unrepresentable.
        for bad in ({1, 2}, {3: 4}, object(), float("inf")):
            with self.assertRaises(ValueEncodeError):
                to_value_ast(bad)

    @unittest.skipUnless(VALIDATOR.exists(), "nl-validator release binary not built")
    def test_cross_validate_with_check_value(self):
        for sample in [True, 5, 3.5, "hi", None, b"xy", [1, 2, 3], (1, 2), {"a": 1, "b": [2, 3]}]:
            ast_obj = to_value_ast(sample)
            with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
                json.dump(ast_obj, f)
                path = f.name
            r = subprocess.run([str(VALIDATOR), "check-value", path], capture_output=True, text=True)
            self.assertEqual(r.returncode, 0, f"{sample!r} -> {ast_obj}: {r.stderr}")


if __name__ == "__main__":
    unittest.main()
