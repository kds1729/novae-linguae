"""Optional semantic-equivalence service (`/v0/equiv`) — node-local, best-effort, not a protocol gate.

Decides whether two functions compute the same thing — `∀x. f(x) = g(x)` over the unbounded domain — by
shelling out to the validator's `equiv` (which reuses the SMT + structural-induction + lemma-discovery
prover). This is the operable form of the "semantic equivalence vs hash equivalence" question: two
records can be hash-different yet behaviorally identical.

Takes two inline body-expression ASTs (`f`, `g`) — bodies are not stored in the commons, so there is no
by-hash form. Returns EQUIVALENT / DISTINCT (with a counterexample) / UNKNOWN / UNSUPPORTED. Needs a
solver on PATH (else NO-SOLVER). Bounded by the same timeout as /v0/prove.
"""

import json
import os
import shutil
import subprocess
import tempfile

from django.conf import settings

_VERDICT = {"EQUIVALENT": "equivalent", "DISTINCT": "distinct", "UNKNOWN": "unknown",
            "UNSUPPORTED": "unsupported", "NO-SOLVER": "no_solver"}


class EquivError(Exception):
    def __init__(self, code, detail="", status=400):
        super().__init__(code)
        self.code = code
        self.detail = detail
        self.status = status


def run_equiv(body_f, body_g):
    if not isinstance(body_f, dict) or not isinstance(body_g, dict):
        raise EquivError("bad_request", "`f` and `g` must be body-expression objects")
    tmp = tempfile.mkdtemp(prefix="nlequiv-")
    try:
        fp, gp = os.path.join(tmp, "f.json"), os.path.join(tmp, "g.json")
        with open(fp, "w") as f:
            json.dump(body_f, f)
        with open(gp, "w") as f:
            json.dump(body_g, f)
        try:
            proc = subprocess.run(
                [settings.COMMONS_VALIDATOR, "equiv", "--body-f", fp, "--body-g", gp, "--solver", settings.COMMONS_SOLVER],
                capture_output=True, text=True, timeout=settings.COMMONS_PROVE_TIMEOUT,
            )
        except FileNotFoundError as exc:
            raise EquivError("verifier_unavailable", str(exc), status=503)
        except subprocess.TimeoutExpired:
            raise EquivError("equiv_timeout", f"equivalence check exceeded {settings.COMMONS_PROVE_TIMEOUT:g}s", status=504)
        # The verdict line goes to stdout (EQUIVALENT/UNKNOWN/UNSUPPORTED) or stderr (DISTINCT/NO-SOLVER,
        # which the CLI signals as an error). Scan both for the first verdict token.
        for line in (proc.stdout + "\n" + proc.stderr).splitlines():
            toks = line.strip().split(None, 1)
            if toks and toks[0] in _VERDICT:
                return {"verdict": _VERDICT[toks[0]], "detail": toks[1].strip() if len(toks) > 1 else "",
                        "solver": settings.COMMONS_SOLVER}
        raise EquivError("equiv_failed", (proc.stderr or proc.stdout or "").strip()[:500] or "no output", status=500)
    finally:
        shutil.rmtree(tmp, ignore_errors=True)
