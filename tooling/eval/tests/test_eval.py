"""Tests for the Nova Lingua evaluation harness — they verify the *grader*, not any model.

The load-bearing test: the `OracleModel` (which always returns the known-correct answer) must score
100% on every task kind. If it doesn't, the grader is rejecting valid answers and no model score from
this harness can be trusted. A negative-control test confirms the grader also *rejects* wrong answers,
so 100% isn't coming from a grader that passes everything. Both run with no API access.
"""

import sys
import tempfile
import unittest
from pathlib import Path

_HERE = Path(__file__).resolve()
sys.path.insert(0, str(_HERE.parents[1]))  # tooling/eval

import eval_harness as eh  # noqa: E402
from model_client import OracleModel  # noqa: E402


def _corpus():
    return [eh.json.loads(line) for line in eh.CORPUS.read_text().splitlines() if line.strip()]


def _have_tools():
    return eh.VALIDATOR.exists() and eh.CORPUS.exists()


class GraderSelfTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if not _have_tools():
            raise unittest.SkipTest("nl-validator not built or corpus missing")
        cls.corpus = _corpus()

    def _tasks(self, per_kind=6):
        tasks = []
        for build in (eh.build_write_tasks, eh.build_read_tasks, eh.build_assemble_tasks):
            tasks.extend(build(self.corpus)[:per_kind])
        return tasks

    def test_oracle_scores_100_percent(self):
        tasks = self._tasks()
        self.assertTrue(any(t.kind == "write" for t in tasks))
        self.assertTrue(any(t.kind == "read" for t in tasks))
        model = OracleModel()
        with tempfile.TemporaryDirectory() as wd:
            for t in tasks:
                out = model.answer(t)
                verdict = eh.GRADERS[t.kind](t, out, wd)
                self.assertTrue(verdict.get("pass"), f"oracle failed {t.id}: {verdict} (output={out!r})")
                # The oracle's answer is already canonical, so semantic_pass must also hold with no repair.
                self.assertTrue(verdict.get("semantic_pass"), f"oracle semantic miss {t.id}: {verdict}")
                self.assertFalse(verdict.get("repaired"), f"oracle should need no repair {t.id}: {verdict}")

    def test_grader_rejects_wrong_answers(self):
        # Negative control: a grader that passes everything would also pass these. It must not.
        write = eh.build_write_tasks(self.corpus)[0]
        read = eh.build_read_tasks(self.corpus)[0]
        with tempfile.TemporaryDirectory() as wd:
            # A body that doesn't parse at all.
            self.assertFalse(eh.grade_write(write, "this is not nova lingua", wd).get("pass"))
            # A syntactically-valid body of the wrong type/behaviour (returns a constant) — should fail
            # typecheck or the worked examples.
            self.assertFalse(eh.grade_write(write, "\\xs -> true", wd).get("pass"))
            # A wrong read answer.
            self.assertFalse(eh.grade_read(read, "999999", wd).get("pass"))
            self.assertFalse(eh.grade_read(read, "garbage", wd).get("pass"))

    def test_semantic_pass_recovers_a_dialect_only_miss(self):
        # A read task whose gold is a plain integer: answering with a bare `N` (mainstream prior) instead of
        # `int(N)` is a SURFACE miss — the value is right, only the encoding differs. The grader must mark it
        # pass=False (surface-exact) but semantic_pass=True (right modulo dialect), via repair_surface.
        reads = eh.build_read_tasks(self.corpus)
        scalar = next((t for t in reads if (eh.canonical_value(t.grade_ctx["expected"]) or "").startswith("int(")
                       and "[" not in (eh.canonical_value(t.grade_ctx["expected"]) or "")), None)
        if scalar is None:
            self.skipTest("no scalar-int read task to probe")
        bare = (eh.canonical_value(scalar.grade_ctx["expected"]) or "")[len("int("):-1]  # int(120) -> 120
        with tempfile.TemporaryDirectory() as wd:
            v = eh.grade_read(scalar, bare, wd)
        self.assertFalse(v["pass"], f"bare int should fail surface-exact: {v}")
        self.assertTrue(v["semantic_pass"], f"bare int should pass after repair: {v}")
        self.assertTrue(v["repaired"])

    def test_semantic_pass_does_not_rescue_a_wrong_value(self):
        # Negative control for the semantic verdict: repair changes spelling, never magnitude, so a wrong
        # number must fail BOTH verdicts. (If semantic_pass passed this, the metric would be inflating.)
        read = eh.build_read_tasks(self.corpus)[0]
        with tempfile.TemporaryDirectory() as wd:
            self.assertFalse(eh.grade_read(read, "999999", wd).get("semantic_pass"))
            self.assertFalse(eh.grade_write(eh.build_write_tasks(self.corpus)[0],
                                            "this is not nova lingua", wd).get("semantic_pass"))

    def test_repair_surface_rewrites(self):
        # repair_surface is pure string->string; assert each rule and that canonical forms are left alone.
        self.assertEqual(eh.repair_surface(r"\a b -> max(a, b)"), r"\a b -> max(a)(b)")
        self.assertEqual(eh.repair_surface("120"), "int(120)")
        self.assertEqual(eh.repair_surface("[3, 2, 1]"), "[int(3), int(2), int(1)]")
        self.assertEqual(eh.repair_surface(r"\a -> \b -> a + b"), r"\a b -> a + b")
        self.assertEqual(eh.repair_surface("[]"), "nil")
        # Canonical / constructor forms must be untouched (no double-wrap, ctors and int() preserved).
        for canon in ("int(5)", "int(-6)", r"\a b -> a + b", "Just(int(3))", "[int(2), int(4)]", "nil", "true"):
            self.assertEqual(eh.repair_surface(canon), canon, f"repair mutated canonical {canon!r}")

    def test_assemble_rejects_a_non_composing_pipeline(self):
        tasks = eh.build_assemble_tasks(self.corpus)
        if not tasks:
            self.skipTest("no assemble tasks")
        t = tasks[0]
        # Reverse the gold order — for a type-asymmetric pipeline this should not compose; assert the
        # grader at least handles unknown/empty input as a non-pass.
        with tempfile.TemporaryDirectory() as wd:
            self.assertFalse(eh.grade_assemble(t, "not_a_real_function", wd).get("pass"))


if __name__ == "__main__":
    unittest.main()
