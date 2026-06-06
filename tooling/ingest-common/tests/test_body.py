"""Tests for the pragmatic body-expression AST builder (nl_body), incl. cross-validation against
nl-validator check-body (well-formedness) and hash (the body content-address)."""

import ast
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

_HERE = Path(__file__).resolve()
sys.path.insert(0, str(_HERE.parents[1]))  # tooling/ingest-common
from nl_body import body_ast_from_py, body_ast_from_hs, body_ast_from_ts  # noqa: E402
from nl_core import blake3_256, canonicalize, format_hash  # noqa: E402

VALIDATOR = _HERE.parents[2] / "validator" / "target" / "release" / "nl-validator"


def py_body(src):
    return body_ast_from_py(ast.parse(src).body[0])


class PythonBodyTests(unittest.TestCase):
    def test_arithmetic_expression(self):
        self.assertEqual(
            py_body("def f(n):\n    return n * 2"),
            {"kind": "app", "fn": {"kind": "var", "name": "mul"},
             "args": [{"kind": "var", "name": "n"}, {"kind": "lit", "value": {"kind": "int", "value": 2}}]},
        )

    def test_call_and_var(self):
        self.assertEqual(py_body("def f(x):\n    return x"), {"kind": "var", "name": "x"})
        self.assertEqual(
            py_body("def f(x):\n    return g(x)"),
            {"kind": "app", "fn": {"kind": "var", "name": "g"}, "args": [{"kind": "var", "name": "x"}]},
        )

    def test_docstring_is_skipped(self):
        self.assertEqual(py_body('def f(x):\n    "doc"\n    return x'), {"kind": "var", "name": "x"})

    def test_out_of_subset_returns_none(self):
        self.assertIsNone(py_body("def f(x):\n    y = x\n    return y"))   # local binding
        self.assertIsNone(py_body("def f(x):\n    if x:\n        return 1\n    return 0"))  # control flow
        self.assertIsNone(py_body("def f(x):\n    return [i for i in x]"))  # comprehension
        self.assertIsNone(py_body("def f(x):\n    return"))  # bare return


class TokenBodyTests(unittest.TestCase):
    def test_haskell_bare_and_application(self):
        self.assertEqual(body_ast_from_hs("f", "f x = x"), {"kind": "var", "name": "x"})
        self.assertEqual(
            body_ast_from_hs("f", "f x = g x"),
            {"kind": "app", "fn": {"kind": "var", "name": "g"}, "args": [{"kind": "var", "name": "x"}]},
        )
        self.assertIsNone(body_ast_from_hs("f", "f x = x + 1"))         # operator: out of subset
        self.assertIsNone(body_ast_from_hs("f", "f x\n  | x > 0 = 1"))  # guard

    def test_typescript_arrow(self):
        self.assertEqual(body_ast_from_ts("f", "export const f = (x) => x"), {"kind": "var", "name": "x"})
        self.assertEqual(
            body_ast_from_ts("f", "export const f = (x) => g(x)"),
            {"kind": "app", "fn": {"kind": "var", "name": "g"}, "args": [{"kind": "var", "name": "x"}]},
        )
        self.assertIsNone(body_ast_from_ts("f", "export function f(x) { return x; }"))  # block body


@unittest.skipUnless(VALIDATOR.exists(), "nl-validator release binary not built")
class CrossValidationTests(unittest.TestCase):
    def _run(self, args, payload):
        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as fh:
            json.dump(payload, fh)
            path = fh.name
        return subprocess.run([str(VALIDATOR), *args, path], capture_output=True, text=True)

    def test_body_is_well_formed_and_hash_matches_validator(self):
        body = py_body("def f(n):\n    return g(n, 2)")
        cb = self._run(["check-body"], body)
        self.assertEqual(cb.returncode, 0, cb.stderr)
        # The address we compute equals the validator's body content-address.
        ours = format_hash("expr", blake3_256(canonicalize(body)))
        got = self._run(["hash"], body)
        self.assertEqual(got.returncode, 0, got.stderr)
        self.assertEqual(got.stdout.strip(), ours)


if __name__ == "__main__":
    unittest.main()
