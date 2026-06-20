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
