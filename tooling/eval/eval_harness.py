#!/usr/bin/env python3
"""Nova Lingua model-evaluation harness.

The standing question the corpus is built to answer is *does exposure to these artifacts let a model
read, write, and assemble Nova Lingua?* This harness measures exactly that, and it uses the reference
tooling as the grader — the same verified-by-default principle the corpus is built on, turned into a
metric. Every score is produced by `nl-validator`, not by a human or an LLM judge.

Three task shapes, all drawn from the verified corpus (`tooling/corpus/corpus.jsonl`):

- **write** — given an intent, a type signature, and worked examples, the model emits a function *body*
  in the surface syntax. Graded by `parse-body` (does it parse?) → `typecheck` (does it have the declared
  type?) → `run` (do the worked examples execute correctly against it?).
- **read** — given a body and an input, the model predicts the output value. Graded by comparing its
  value (canonicalized via `parse-value`/`unparse-value`) to the example's true result.
- **assemble** — given a goal and a set of available functions, the model picks an ordered pipeline.
  Graded by `compose` (does the chosen pipeline actually type-compose?).

`OracleModel` (see `model_client.py`) lets you self-test the grader with no API access: a model that
always answers correctly must score 100% on every task. Run that first.

    python3 eval_harness.py --oracle                 # verify the grader (no key needed)
    python3 eval_harness.py --model claude-opus-4-8   # run a real model (needs ANTHROPIC_API_KEY)
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import tempfile
from dataclasses import dataclass, field
from pathlib import Path

_HERE = Path(__file__).resolve()
_REPO = _HERE.parents[2]  # tooling/eval -> tooling -> repo root
VALIDATOR = _REPO / "tooling" / "validator" / "target" / "release" / "nl-validator"
CORPUS = _REPO / "tooling" / "corpus" / "corpus.jsonl"
SCHEMA = _REPO / "spec" / "function-record.v0.2.schema.json"


# --- nl-validator CLI (the grader) ---------------------------------------------------------------

def cli(args, stdin: str | None = None):
    return subprocess.run([str(VALIDATOR)] + args, capture_output=True, text=True, input=stdin)


def unparse(kind: str, ast) -> str | None:
    """Canonical surface string of a value/type/body AST."""
    p = cli([f"unparse-{kind}", "-"], stdin=json.dumps(ast))
    return p.stdout.strip() if p.returncode == 0 else None


def parse(kind: str, surface: str):
    """Surface string -> AST, or None if it doesn't parse. `parse-*` reads the surface from stdin when no
    positional argument is given (unlike `unparse-*`, where `-` means stdin)."""
    p = cli([f"parse-{kind}"], stdin=surface)
    if p.returncode != 0:
        return None
    try:
        return json.loads(p.stdout)
    except json.JSONDecodeError:
        return None


def canonical_value(ast) -> str | None:
    """A canonical surface form for comparing two values regardless of formatting."""
    return unparse("value", ast)


def strip_answer(text: str) -> str:
    """Defensive cleanup of a model's reply: drop ``` fences and surrounding prose, keep the payload."""
    t = text.strip()
    if t.startswith("```"):
        lines = t.splitlines()
        # drop opening fence (optionally ```lang) and the closing fence
        lines = lines[1:]
        if lines and lines[-1].strip().startswith("```"):
            lines = lines[:-1]
        t = "\n".join(lines).strip()
    # unwrap a single-backtick inline code span (e.g. `\a b -> a % b`)
    if len(t) >= 2 and t.startswith("`") and t.endswith("`"):
        t = t[1:-1].strip()
    return t


# --- tasks ---------------------------------------------------------------------------------------

@dataclass
class Task:
    id: str
    kind: str  # write | read | assemble
    system: str
    user: str
    gold: str
    grade_ctx: dict = field(default_factory=dict)  # inputs the grader needs


def _has_fn_ref(e):
    """True if any worked example takes a function-valued (fn_ref) argument. Such examples need the
    referenced helper record in the run directory to execute, which the standalone write/read graders
    don't supply — so they're excluded from the task pool (they remain valid corpus training data)."""
    for ex in e.get("views", {}).get("examples", []):
        for a in ex.get("args", []):
            if isinstance(a, dict) and a.get("kind") == "fn_ref":
                return True
    return False


def _function_examples(corpus):
    return [e for e in corpus
            if e.get("modality") == "nova_lingua" and e.get("category") == "function"
            and e.get("polarity") == "positive" and e.get("views", {}).get("surface_body")
            and not _has_fn_ref(e)]


WRITE_SYSTEM = (
    "You write programs in Nova Lingua, a compact functional language for AI agents. A function body is a "
    "lambda in the surface syntax. Output ONLY the body — the surface string, nothing else: no prose, no "
    "code fences, no `name =` prefix.\n\n"
    "Surface conventions (follow exactly):\n"
    "- Application is juxtaposition: write `f x y`, NEVER `f(x, y)`. Parenthesize a nested argument: "
    "`self (tail xs)`, `length (filter p xs)`.\n"
    "- A lambda binds all parameters at once, space-separated: `\\a b -> ...`, NEVER curried `\\a -> \\b -> ...`.\n"
    "- Use infix operators: `+ - * / %` for arithmetic (`%` is modulo), `== != < <= > >=` for comparison, "
    "`&& || !` for logic. Prefer these over named functions.\n"
    "- Integer literals are written `int(N)`: `int(0)`, `int(1)`.\n"
    "- Conditionals use brace/arrow case: `case <bool> of { true => <e>; false => <e> }`.\n"
    "- Variant constructors: `None`, `Just(x)`, `Ok(x)`, `Err(x)`. `self` is the function itself (recursion).\n"
    "- The empty list is `nil` (NOT `[]`); build lists with `cons x xs`, e.g. `cons (head xs) (self (tail xs))`.\n"
    "- Other builtins, applied by juxtaposition: neg abs min max length append reverse cons head tail null "
    "map filter foldl foldr.\n\n"
    "Examples of well-formed bodies:\n"
    "  \\a b -> a + b\n"
    "  \\n -> n - int(1)\n"
    "  \\xs ys -> append xs ys\n"
    "  \\xs -> map (\\x -> x * x) xs\n"
    "  \\xs -> case null xs of { true => int(0); false => head xs + self (tail xs) }\n"
    "  \\xs -> case null xs of { true => nil; false => cons (head xs * head xs) (self (tail xs)) }\n"
    "  \\a b -> case b == int(0) of { true => None; false => Just(a / b) }\n\n"
    "More examples:\n"
)

READ_SYSTEM = (
    "You execute Nova Lingua programs by hand. Given a function body and an input, output ONLY the "
    "resulting value in surface syntax — nothing else, no prose, no fences.\n\n"
    "Value conventions (follow exactly):\n"
    "- Integers are written `int(N)`: `int(42)`, `int(-6)`. A bare `42` is WRONG (it parses as a different type).\n"
    "- Lists: `[int(2), int(4), int(6)]` (each element in its own canonical form).\n"
    "- Booleans: `true` / `false`. Variants: `None`, `Just(int(3))`, `Ok(int(2))`, `Err(int(0))`.\n\n"
    "Examples:\n"
)

ASSEMBLE_SYSTEM = (
    "You assemble Nova Lingua pipelines from existing functions instead of writing new code. Given a goal "
    "and a numbered list of available functions (each with its type), output ONLY the names of the "
    "functions to apply in order, comma-separated (e.g. `reverse, length`) — the output of each feeds the "
    "next. Output nothing else.\n\nExamples:\n"
)


def _shot_write(e) -> str:
    return (f"intent: {e['intent']}\ntype: {e['views']['surface_type']}\n"
            f"body: {e['views']['surface_body']}\n")


def build_write_tasks(corpus, n_shots=3):
    fns = _function_examples(corpus)
    shots = "".join(_shot_write(e) for e in fns[:n_shots])
    system = WRITE_SYSTEM + shots
    tasks = []
    for e in fns[n_shots:]:
        v = e["views"]
        ex_lines = "\n".join(
            f"  {', '.join(canonical_value(a) or '?' for a in ex['args'])} -> {canonical_value(ex['result'])}"
            for ex in v["examples"]
        )
        user = (f"intent: {e['intent']}\ntype: {v['surface_type']}\nexamples:\n{ex_lines}\n\n"
                f"Write the body.")
        tasks.append(Task(
            id=f"write/{e['id']}", kind="write", system=system, user=user,
            gold=v["surface_body"], grade_ctx={"record": v["record"]},
        ))
    return tasks


def build_read_tasks(corpus, n_shots=3):
    fns = _function_examples(corpus)
    # Build few-shot read demonstrations from the first examples' first worked example.
    def demo(e):
        v = e["views"]
        ex = v["examples"][0]
        args = ", ".join(canonical_value(a) or "?" for a in ex["args"])
        return f"body: {v['surface_body']}\ninput: {args}\noutput: {canonical_value(ex['result'])}\n"
    shots = "".join(demo(e) for e in fns[:n_shots])
    system = READ_SYSTEM + shots
    tasks = []
    for e in fns[n_shots:]:
        v = e["views"]
        ex = v["examples"][-1]  # use the last worked example as the held-out input
        args = ", ".join(canonical_value(a) or "?" for a in ex["args"])
        gold = canonical_value(ex["result"])
        if gold is None:
            continue
        user = f"body: {v['surface_body']}\ninput: {args}\n\nWhat is the output?"
        tasks.append(Task(
            id=f"read/{e['id']}", kind="read", system=system, user=user,
            gold=gold, grade_ctx={"expected": ex["result"]},
        ))
    return tasks


def build_assemble_tasks(corpus):
    """Use the corpus's positive composition pipelines as assembly tasks: the model must pick the right
    ordered stages from a candidate set (the correct stages plus distractors)."""
    comps = [e for e in corpus if e.get("category") == "composition" and e.get("polarity") == "positive"]
    fns = _function_examples(corpus)
    by_name = {e["views"]["record"]["name_hints"][0] if e["views"]["record"].get("name_hints")
               else e["id"]: e for e in fns}
    # name -> (intent, surface_type, record) for candidate listing and grading.
    name_meta = {}
    for e in fns:
        rec = e["views"]["record"]
        nm = rec["name_hints"][0] if rec.get("name_hints") else e["id"]
        name_meta[nm] = {"intent": e["intent"], "type": e["views"]["surface_type"], "record": rec}

    # A couple of one-shot examples (built from the first composition).
    def stages_of(e):
        return [s["name_hints"][0] if s.get("name_hints") else s["hash"][:8] for s in e["views"]["stages"]]

    if not comps:
        return []
    shot_e = comps[0]
    shot = (f"goal: {shot_e['intent']}\navailable: {', '.join(stages_of(shot_e))}\n"
            f"pipeline: {', '.join(stages_of(shot_e))}\n")
    system = ASSEMBLE_SYSTEM + shot

    distractor_pool = [n for n in name_meta if n not in stages_of(shot_e)]
    tasks = []
    for e in comps[1:]:
        stages = stages_of(e)
        if not all(s in name_meta for s in stages):
            continue  # can't grade if a stage isn't an addressable function
        # Candidate set: the correct stages plus up to 3 distractors, listed with their types.
        cands = list(dict.fromkeys(stages + distractor_pool))[: len(stages) + 3]
        listing = "\n".join(f"  {n} : {name_meta[n]['type']}" for n in cands)
        user = f"goal: {e['intent']}\navailable functions:\n{listing}\n\nWhich pipeline?"
        tasks.append(Task(
            id=f"assemble/{e['id']}", kind="assemble", system=system, user=user,
            gold=", ".join(stages),
            grade_ctx={"name_meta": {n: name_meta[n]["record"] for n in cands}},
        ))
    return tasks


# --- grading (every verdict comes from nl-validator) ---------------------------------------------

def grade_write(task, out, workdir):
    surface = strip_answer(out)
    body = parse("body", surface)
    res = {"parsed": body is not None, "well_typed": False, "runs": False}
    if body is None:
        return res
    d = Path(workdir) / task.id.replace("/", "_")
    d.mkdir(parents=True, exist_ok=True)
    rec_path, body_path = d / "record.json", d / "body.json"
    rec_path.write_text(json.dumps(task.grade_ctx["record"]))
    body_path.write_text(json.dumps(body))
    res["well_typed"] = cli(["typecheck", str(rec_path), "--body", str(body_path)]).returncode == 0
    res["runs"] = cli(["run", str(rec_path), "--body", str(body_path)]).returncode == 0
    res["pass"] = res["well_typed"] and res["runs"]
    return res


def grade_read(task, out, workdir):
    surface = strip_answer(out)
    got = parse("value", surface)
    expected_canon = canonical_value(task.grade_ctx["expected"])
    got_canon = canonical_value(got) if got is not None else None
    correct = got_canon is not None and got_canon == expected_canon
    return {"parsed": got is not None, "correct": correct, "pass": correct}


def grade_assemble(task, out, workdir):
    surface = strip_answer(out)
    names = [n.strip() for n in surface.replace("\n", ",").split(",") if n.strip()]
    meta = task.grade_ctx["name_meta"]
    if not names or any(n not in meta for n in names):
        return {"valid_names": False, "composes": False, "pass": False}
    d = Path(workdir) / task.id.replace("/", "_")
    d.mkdir(parents=True, exist_ok=True)
    paths = []
    for i, n in enumerate(names):
        p = d / f"{i}_{n}.json"
        p.write_text(json.dumps(meta[n]))
        paths.append(str(p))
    composes = cli(["compose"] + paths).returncode == 0
    return {"valid_names": True, "composes": composes, "pass": composes}


GRADERS = {"write": grade_write, "read": grade_read, "assemble": grade_assemble}


# --- runner + report -----------------------------------------------------------------------------

def run_eval(model, tasks, workdir):
    rows = []
    for t in tasks:
        out = model.answer(t)
        verdict = GRADERS[t.kind](t, out, workdir)
        rows.append({"id": t.id, "kind": t.kind, "output": out, "verdict": verdict})
    return rows


def summarize(rows):
    by_kind = {}
    for r in rows:
        k = r["kind"]
        s = by_kind.setdefault(k, {"n": 0, "pass": 0})
        s["n"] += 1
        s["pass"] += 1 if r["verdict"].get("pass") else 0
    return by_kind


def main():
    ap = argparse.ArgumentParser(description="Evaluate a model on reading/writing/assembling Nova Lingua.")
    ap.add_argument("--model", default="claude-opus-4-8", help="Anthropic model id (needs ANTHROPIC_API_KEY)")
    ap.add_argument("--oracle", action="store_true", help="use the perfect oracle model (self-tests the grader; no key)")
    ap.add_argument("--effort", default="high", help="effort level for the real model")
    ap.add_argument("--tasks", default="all", choices=["all", "write", "read", "assemble"])
    ap.add_argument("--limit", type=int, default=0, help="cap tasks per kind (0 = all)")
    ap.add_argument("--out", default=str(_HERE.parent / "results.jsonl"))
    args = ap.parse_args()

    if not VALIDATOR.exists():
        sys.exit(f"nl-validator not built at {VALIDATOR}")
    if not CORPUS.exists():
        sys.exit(f"corpus not found at {CORPUS} — generate it first (tooling/corpus/gen_corpus.py)")

    corpus = [json.loads(line) for line in CORPUS.read_text().splitlines() if line.strip()]

    builders = {"write": build_write_tasks, "read": build_read_tasks, "assemble": build_assemble_tasks}
    kinds = ["write", "read", "assemble"] if args.tasks == "all" else [args.tasks]
    tasks = []
    for k in kinds:
        kt = builders[k](corpus)
        if args.limit:
            kt = kt[: args.limit]
        tasks.extend(kt)

    if args.oracle:
        from model_client import OracleModel
        model = OracleModel()
    else:
        from model_client import AnthropicModel
        model = AnthropicModel(args.model, effort=args.effort)

    with tempfile.TemporaryDirectory(prefix="nleval-") as wd:
        rows = run_eval(model, tasks, wd)

    by_kind = summarize(rows)
    with open(args.out, "w") as fh:
        for r in rows:
            fh.write(json.dumps(r) + "\n")

    print(f"model: {model.name}   tasks: {len(rows)}")
    for k in kinds:
        if k in by_kind:
            s = by_kind[k]
            pct = 100.0 * s["pass"] / s["n"] if s["n"] else 0.0
            print(f"  {k:9s}  {s['pass']:3d}/{s['n']:<3d}  ({pct:5.1f}%)")
    total_n = sum(s["n"] for s in by_kind.values())
    total_p = sum(s["pass"] for s in by_kind.values())
    print(f"  {'TOTAL':9s}  {total_p:3d}/{total_n:<3d}  ({100.0 * total_p / total_n if total_n else 0:5.1f}%)")
    print(f"  -> {args.out}")


if __name__ == "__main__":
    main()
