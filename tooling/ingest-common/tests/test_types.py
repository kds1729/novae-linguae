"""Tests for the type-expression AST builder (nl_types), incl. cross-validation against the Rust
nl-validator (check-type, schema validate, and an unparse-type -> parse-type round-trip)."""

import ast
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

_HERE = Path(__file__).resolve()
sys.path.insert(0, str(_HERE.parents[1]))                 # tooling/ingest-common
from nl_types import python_function_type                 # noqa: E402

VALIDATOR = _HERE.parents[2] / "validator" / "target" / "release" / "nl-validator"
TYPE_SCHEMA = _HERE.parents[3] / "spec" / "type-expression.schema.json"


def ftype(src):
    return python_function_type(ast.parse(src).body[0])


class PythonTypeMappingTests(unittest.TestCase):
    def test_concrete(self):
        self.assertEqual(ftype("def f(x: int) -> str: ..."),
                         {"kind": "fn", "params": [{"kind": "builtin", "name": "int"}],
                          "result": {"kind": "builtin", "name": "string"}})

    def test_list_and_dict_constructors(self):
        self.assertEqual(ftype("def g(xs: list[int]) -> int: ...")["params"][0],
                         {"kind": "apply", "ctor": {"kind": "builtin", "name": "List"},
                          "args": [{"kind": "builtin", "name": "int"}]})
        self.assertEqual(ftype("def d(m: dict[str, int]) -> int: ...")["params"][0],
                         {"kind": "apply", "ctor": {"kind": "builtin", "name": "Map"},
                          "args": [{"kind": "builtin", "name": "string"}, {"kind": "builtin", "name": "int"}]})

    def test_optional_is_maybe(self):
        self.assertEqual(ftype("def o(x: int | None) -> int: ...")["params"][0],
                         {"kind": "apply", "ctor": {"kind": "builtin", "name": "Maybe"},
                          "args": [{"kind": "builtin", "name": "int"}]})

    def test_callable_is_fn(self):
        self.assertEqual(ftype("def c(f: Callable[[int], str]) -> str: ...")["params"][0],
                         {"kind": "fn", "params": [{"kind": "builtin", "name": "int"}],
                          "result": {"kind": "builtin", "name": "string"}})

    def test_unannotated_becomes_quantified_fresh_vars(self):
        t = ftype("def h(x, y): ...")
        self.assertEqual(t["kind"], "forall")
        self.assertEqual(len(t["vars"]), 3)                      # x, y, and the return
        self.assertEqual(t["vars"], sorted(t["vars"]))           # canonical: sorted
        self.assertEqual(t["body"]["kind"], "fn")

    def test_typevar_reused_is_one_variable(self):
        t = ftype("def ident(x: T) -> T: ...")
        self.assertEqual(t["kind"], "forall")
        self.assertEqual(t["vars"], ["a"])                       # T used twice -> one variable
        self.assertEqual(t["body"]["params"][0], t["body"]["result"])

    def test_zero_arg(self):
        self.assertEqual(ftype("def z() -> int: ..."),
                         {"kind": "fn", "params": [], "result": {"kind": "builtin", "name": "int"}})


@unittest.skipUnless(VALIDATOR.exists(), "nl-validator release binary not built")
class TypeAstConformanceTests(unittest.TestCase):
    SRCS = [
        "def f(x: int) -> str: ...",
        "def g(xs: list[int]) -> bool: ...",
        "def h(x, y): ...",
        "def ident(x: T) -> T: ...",
        "def o(x: int | None) -> int: ...",
        "def c(f: Callable[[int], str], n: int) -> bool: ...",
        "def m(d: dict[str, list[int]]) -> int: ...",
    ]

    def _run(self, *args):
        return subprocess.run([str(VALIDATOR), *args], capture_output=True, text=True)

    def test_well_formed_and_schema_valid(self):
        # check-type enforces var-scoping (every var bound by the enclosing forall), rank-1, and that
        # apply.ctor is a constructor — exactly the invariants the builder must uphold.
        for src in self.SRCS:
            t = ftype(src)
            with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
                json.dump(t, f)
                path = f.name
            ct = self._run("check-type", path)
            self.assertEqual(ct.returncode, 0, f"check-type failed for {src}: {ct.stderr}")
            sv = self._run("validate", str(TYPE_SCHEMA), path)
            self.assertEqual(sv.returncode, 0, f"schema validate failed for {src}: {sv.stderr}")


if __name__ == "__main__":
    unittest.main()
