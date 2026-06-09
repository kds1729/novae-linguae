"""Optional proof service (`/v0/prove`) — node-local, best-effort, **not** a protocol gate.

Runs the Rust reference validator's `prove` over a record's `forall` `properties[]` with an SMT solver
(z3 by default), reporting each property as PROVED / REFUTED / UNKNOWN / NOT-PROVED / UNSUPPORTED — over
the *unbounded* domain, with the solver checking the negation of the law (and falling back to structural
induction with lemma discovery for list/recursion laws). Like `search`, this is a convenience the node
offers, not part of the admission decision (principle 7): proving says nothing about whether a record is
stored. It needs a solver on PATH, or every property comes back NO-SOLVER.

The endpoint accepts either a stored record by `hash` (resolved from this node) or an inline `record`
(plus an optional `body` AST — required only for properties that reference `self`, since bodies are not
themselves stored in the commons). Work is bounded by a wall-clock timeout and a per-call property cap.
"""

import json
import os
import shutil
import subprocess
import tempfile

from django.conf import settings


class ProveError(Exception):
    def __init__(self, code, detail="", status=400):
        super().__init__(code)
        self.code = code
        self.detail = detail
        self.status = status


def _parse(stdout):
    """Parse the validator's `name: STATUS  detail` lines into structured per-property results."""
    results = []
    for line in stdout.splitlines():
        line = line.strip()
        if not line or ": " not in line:
            continue  # e.g. "no properties to prove"
        name, rest = line.split(":", 1)
        parts = rest.strip().split(None, 1)
        if not parts:
            continue
        results.append({
            "name": name.strip(),
            "status": parts[0],
            "detail": parts[1].strip() if len(parts) > 1 else "",
        })
    return results


def run_prove(record, body=None):
    """Prove `record`'s properties (optionally with its `body` AST). Returns a structured verdict dict;
    raises ProveError on bad input / missing solver / timeout."""
    if not isinstance(record, dict):
        raise ProveError("bad_request", "record must be a JSON object")
    props = record.get("properties")
    if not isinstance(props, list) or not props:
        raise ProveError("no_properties", "record has no `properties[]` to prove", status=422)
    cap = settings.COMMONS_PROVE_MAX_PROPERTIES
    if len(props) > cap:
        raise ProveError("too_many_properties", f"{len(props)} properties exceeds the node cap of {cap}", status=422)

    tmp = tempfile.mkdtemp(prefix="nlprove-")
    try:
        rec_path = os.path.join(tmp, "record.json")
        with open(rec_path, "w") as f:
            json.dump(record, f)
        args = ["prove", rec_path, "--solver", settings.COMMONS_SOLVER]
        if body is not None:
            body_path = os.path.join(tmp, "body.json")
            with open(body_path, "w") as f:
                json.dump(body, f)
            args += ["--body", body_path]
        try:
            proc = subprocess.run(
                [settings.COMMONS_VALIDATOR, *args],
                capture_output=True, text=True, timeout=settings.COMMONS_PROVE_TIMEOUT,
            )
        except FileNotFoundError as exc:
            raise ProveError("verifier_unavailable", str(exc), status=503)
        except subprocess.TimeoutExpired:
            raise ProveError("prove_timeout", f"proving exceeded {settings.COMMONS_PROVE_TIMEOUT:g}s", status=504)
        results = _parse(proc.stdout)
        if not results:
            # Nothing to report: either no properties (shouldn't reach here) or the validator errored.
            raise ProveError("prove_failed", (proc.stderr or proc.stdout or "").strip()[:500] or "no output", status=500)
    finally:
        shutil.rmtree(tmp, ignore_errors=True)

    summary = {}
    for r in results:
        key = r["status"].lower().replace("-", "_") or "unknown"
        summary[key] = summary.get(key, 0) + 1
    return {"solver": settings.COMMONS_SOLVER, "results": results, "summary": summary}
