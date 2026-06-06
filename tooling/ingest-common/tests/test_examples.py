"""Tests for safe doctest example extraction (nl_examples)."""

import sys
import unittest
from pathlib import Path

_HERE = Path(__file__).resolve()
sys.path.insert(0, str(_HERE.parents[1]))                 # tooling/ingest-common
from nl_examples import examples_from_docstring           # noqa: E402

_DOC = '''Add two numbers.

>>> add(2, 3)
5
>>> add(-1, 1)
0
>>> add(x, 1)
2
>>> other(9)
9
'''


class DoctestExtractionTests(unittest.TestCase):
    def test_extracts_literal_positional_examples(self):
        # Only the two literal calls to `add` are extracted; the non-literal arg (x) and the call to
        # a different function are skipped. Nothing is executed — the result comes from the doctest.
        self.assertEqual(examples_from_docstring("add", _DOC), [
            {"args": [{"kind": "int", "value": 2}, {"kind": "int", "value": 3}],
             "result": {"kind": "int", "value": 5}},
            {"args": [{"kind": "int", "value": -1}, {"kind": "int", "value": 1}],
             "result": {"kind": "int", "value": 0}},
        ])

    def test_no_docstring(self):
        self.assertEqual(examples_from_docstring("f", None), [])

    def test_skips_unrepresentable_result(self):
        self.assertEqual(examples_from_docstring("f", ">>> f(1)\n{1, 2}\n"), [])   # set result

    def test_skips_keyword_calls(self):
        self.assertEqual(examples_from_docstring("f", ">>> f(x=1)\n1\n"), [])

    def test_type_hints_select_nat(self):
        nat = {"kind": "builtin", "name": "nat"}
        exs = examples_from_docstring("f", ">>> f(2)\n3\n", param_types=[nat], result_type=nat)
        self.assertEqual(exs[0]["args"][0], {"kind": "nat", "value": 2})
        self.assertEqual(exs[0]["result"], {"kind": "nat", "value": 3})

    def test_list_example(self):
        exs = examples_from_docstring("rev", ">>> rev([1, 2, 3])\n[3, 2, 1]\n")
        self.assertEqual(exs[0]["result"],
                         {"kind": "list", "elems": [{"kind": "int", "value": 3},
                                                    {"kind": "int", "value": 2},
                                                    {"kind": "int", "value": 1}]})


if __name__ == "__main__":
    unittest.main()
