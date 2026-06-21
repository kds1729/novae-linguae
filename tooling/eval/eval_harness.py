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
always answers correctly must score 100% on every task. It is the DEFAULT, so a forgotten flag never bills.

    python3 eval_harness.py                           # free oracle self-test (default — no key, no cost)
    python3 eval_harness.py --oracle                  # same, stated explicitly
    python3 eval_harness.py --model claude-opus-4-8    # REAL billed run (needs ANTHROPIC_API_KEY)

COST: only `--model` triggers a real API call, which bills ANTHROPIC_API_KEY *outside* any Pro/Max
subscription. A full-pool run (272 tasks) measures ~$1. Cost is driven by prompt length (stating the
conventions roughly triples input tokens, so `--conventions on` costs ~2x `off`) and by how many runs you
do — NOT by `--effort`: these short single-answer tasks need almost no thinking, so high and medium cost
about the same (~$1) and score within a point of each other. The ~$10-30 figures seen historically were
*many* runs (an on/off/shots sweep plus iteration), not one expensive run. Control cost by running only
when you mean to and not sweeping repeatedly — the eval is a benchmark scored over the whole pool, so don't
sample to save money. With no `--model` (or with `--oracle`) the run is the free, local grader self-test.
"""

from __future__ import annotations

import argparse
import json
import os
import re
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


# --- surface repair: separating semantic competence from the surface-dialect tax -----------------
#
# A model can compute the right answer yet lose the point to a surface convention that differs from its
# mainstream priors — call-parens application (`max(a, b)`), bare integer literals (`42`, which parses as
# `nat`), curried lambdas (`\a -> \b ->`). To tell *reasoning* failures apart from *dialect* failures,
# every grader reports two verdicts: `pass` (surface-exact — the answer graded as written) and
# `semantic_pass` (the answer graded after a set of mechanical, value-preserving rewrites that normalize
# known dialect deviations to Nova Lingua's surface forms).
#
# The safety property that makes `semantic_pass` trustworthy: every repair is a pure NOTATIONAL rewrite —
# it changes spelling, never the computed value or a number's magnitude. A botched rewrite produces a
# string that fails to parse / typecheck / run, which *lowers* `semantic_pass`; it can never turn a wrong
# answer into a passing one (e.g. wrapping a bare `5` as `int(5)` changes only the type tag, so it can fix
# an encoding mismatch but never make a wrong number right). `semantic_pass` is therefore a conservative
# LOWER BOUND on "right modulo dialect", and `semantic_pass - pass` is a measured floor on the dialect tax.

_INT_LIT = re.compile(r"int\(\s*-?\d+\s*\)|nat\(\s*-?\d+\s*\)|(?<![\w.])(-?\d+)(?![\w.])")


def _wrap_ints(s: str) -> str:
    """Bare integer literals -> `int(N)`. A bare `42` parses as `nat`; the corpus gold uses `int`. Only the
    type tag changes. Existing `int(...)`/`nat(...)` spans match first and pass through untouched, so this
    never double-wraps and never alters a magnitude."""
    return _INT_LIT.sub(lambda m: m.group(0) if m.group(1) is None else f"int({m.group(1)})", s)


def _calls_to_juxt(s: str) -> str:
    """`f(a, b)` -> `f(a)(b)`. The parser already reads `f(x)` as application (juxtaposition with a
    parenthesized argument); only the *comma* breaks a multi-arg call. So we rewrite each top-level comma
    inside a round-paren group that follows a lowercase call head into `)(`, turning call-parens into
    curried application. Commas inside list brackets `[...]` are left alone, as are capitalized constructor
    heads (`Just(...)`, `Ok(...)`) and the `int(...)`/`nat(...)` literal forms."""
    out, stack = [], []  # stack entry True => a comma-rewriting call paren
    i, n = 0, len(s)
    while i < n:
        c = s[i]
        if c == "(":
            prev = s[i - 1] if i > 0 else ""
            is_call = prev.isalnum() or prev in "_)]"
            if is_call:
                j = i - 1
                while j >= 0 and (s[j].isalnum() or s[j] == "_"):
                    j -= 1
                head = s[j + 1:i]
                if head[:1].isupper() or head in ("int", "nat"):
                    is_call = False
            stack.append(is_call)
            out.append(c)
        elif c == "[":
            stack.append(False)
            out.append(c)
        elif c in ")]":
            if stack:
                stack.pop()
            out.append(c)
        elif c == "," and stack and stack[-1]:
            out.append(")(")
            while i + 1 < n and s[i + 1] == " ":  # swallow whitespace after the rewritten separator
                i += 1
        else:
            out.append(c)
        i += 1
    return "".join(out)


_CURRY = re.compile(r"\\\s*([A-Za-z_]\w*(?:\s+[A-Za-z_]\w*)*)\s*->\s*\\")


def _collapse_curry(s: str) -> str:
    """`\\a -> \\b -> e` -> `\\a b -> e`. Curried and multi-binder lambdas both parse, but the corpus gold
    and the declared multi-arg type use the multi-binder form; collapsing matches it. Same computation,
    different arity grouping. Iterated so deeper chains (`\\a -> \\b -> \\c ->`) fully collapse."""
    prev = None
    while prev != s:
        prev = s
        s = _CURRY.sub(r"\\\1 ", s)
    return s


def repair_surface(s: str) -> str:
    """Normalize known, value-preserving dialect deviations to Nova Lingua's surface forms (see the module
    note above). Used to compute `semantic_pass`; never used to compute the surface-exact `pass`."""
    s = _calls_to_juxt(s)
    s = _collapse_curry(s)
    s = _wrap_ints(s)
    s = re.sub(r"\[\s*\]", "nil", s)  # empty list literal -> nil
    return s


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
    """True if any worked example takes a function-valued (fn_ref) argument. Such examples reference a
    helper by content-address; to execute them the grader needs that helper's record + body in the run
    directory. Examples that carry `views.helpers` supply exactly that (see gen_corpus build_and_verify)."""
    for ex in e.get("views", {}).get("examples", []):
        for a in ex.get("args", []):
            if isinstance(a, dict) and a.get("kind") == "fn_ref":
                return True
    return False


def _function_examples(corpus, include_fn_ref=False):
    """Positive Nova Lingua function records with a surface body. First-order records are always included.
    Higher-order (fn_ref) records are included only when `include_fn_ref` is set AND they carry the
    helper records needed to run them — the WRITE grader can then materialize the helpers and link the
    referenced function by address. (READ excludes them: the helper is opaque by address, so a model
    can't predict the output.)"""
    base = [e for e in corpus
            if e.get("modality") == "nova_lingua" and e.get("category") == "function"
            and e.get("polarity") == "positive" and e.get("views", {}).get("surface_body")]
    out = []
    for e in base:
        if _has_fn_ref(e):
            if include_fn_ref and e["views"].get("helpers"):
                out.append(e)
        else:
            out.append(e)
    return out


# Each task's system prompt is assembled from an INTRO (always present — the task framing and output
# format) and an optional CONVENTIONS block (the surface/value rules plus hand-curated example bodies).
# The `--conventions off` mode drops the CONVENTIONS block entirely, leaving only the INTRO and the
# few-shot examples drawn from the corpus. That isolates the standing question the corpus is built to
# answer: do the corpus artifacts ALONE teach a model the dialect, or does it need the rules spelled out?
# The 2026-06-20 baseline (conventions on) took Opus 4.8 from 37% to 97%; conventions-off measures how
# much of that the corpus recovers on its own as a function of shot count.

WRITE_INTRO = (
    "You write programs in Nova Lingua, a compact functional language for AI agents. A function body is a "
    "lambda in the surface syntax. Output ONLY the body — the surface string, nothing else: no prose, no "
    "code fences, no `name =` prefix.\n\n"
)

WRITE_CONVENTIONS = (
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
)

READ_INTRO = (
    "You execute Nova Lingua programs by hand. Given a function body and an input, output ONLY the "
    "resulting value in surface syntax — nothing else, no prose, no fences.\n\n"
)

READ_CONVENTIONS = (
    "Value conventions (follow exactly):\n"
    "- Integers are written `int(N)`: `int(42)`, `int(-6)`. A bare `42` is WRONG (it parses as a different type).\n"
    "- Lists: `[int(2), int(4), int(6)]` (each element in its own canonical form).\n"
    "- Booleans: `true` / `false`. Variants: `None`, `Just(int(3))`, `Ok(int(2))`, `Err(int(0))`.\n\n"
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


def build_write_tasks(corpus, n_shots=3, conventions=True):
    # Higher-order (fn_ref) records ARE in the write pool: the model writes the body from intent + type +
    # examples, and the grader runs it by materializing the carried helpers and resolving the fn_ref by
    # address. Few-shot examples are drawn from the first-order records (the fn_ref ones sort last), so the
    # shots are unaffected.
    fns = _function_examples(corpus, include_fn_ref=True)
    shot_pool = [e for e in fns if not _has_fn_ref(e)]
    shots = "".join(_shot_write(e) for e in shot_pool[:n_shots])
    # Conventions on: INTRO + rules + curated bodies, then "More examples:" + corpus shots.
    # Conventions off: INTRO + only the corpus shots under a plain "Examples:" header.
    header = "More examples:\n" if conventions else "Examples:\n"
    system = WRITE_INTRO + (WRITE_CONVENTIONS if conventions else "") + header + shots
    shot_ids = {e["id"] for e in shot_pool[:n_shots]}
    tasks = []
    for e in fns:
        if e["id"] in shot_ids:
            continue  # don't grade an example we showed as a shot
        v = e["views"]
        # Render a function-valued (fn_ref) argument by the helper's name rather than its raw hash, so a
        # higher-order example reads `double_dep, [..] -> [..]` instead of `fn_<64 hex>, [..] -> [..]`.
        helper_name = {h["record"]["hash"]: h["name"] for h in v.get("helpers", [])}

        def render_arg(a):
            if isinstance(a, dict) and a.get("kind") == "fn_ref":
                return helper_name.get(a.get("target"), "<fn>")
            return canonical_value(a) or "?"
        ex_lines = "\n".join(
            f"  {', '.join(render_arg(a) for a in ex['args'])} -> {canonical_value(ex['result'])}"
            for ex in v["examples"]
        )
        user = (f"intent: {e['intent']}\ntype: {v['surface_type']}\nexamples:\n{ex_lines}\n\n"
                f"Write the body.")
        tasks.append(Task(
            id=f"write/{e['id']}", kind="write", system=system, user=user,
            gold=v["surface_body"], grade_ctx={"record": v["record"], "helpers": v.get("helpers", [])},
        ))
    return tasks


def build_read_tasks(corpus, n_shots=3, conventions=True):
    fns = _function_examples(corpus)
    # Build few-shot read demonstrations from the first examples' first worked example.
    def demo(e):
        v = e["views"]
        ex = v["examples"][0]
        args = ", ".join(canonical_value(a) or "?" for a in ex["args"])
        return f"body: {v['surface_body']}\ninput: {args}\noutput: {canonical_value(ex['result'])}\n"
    shots = "".join(demo(e) for e in fns[:n_shots])
    system = READ_INTRO + (READ_CONVENTIONS if conventions else "") + "Examples:\n" + shots
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

def _run_write(task, surface, workdir, sub):
    """Grade a single write surface string: parse-body -> typecheck -> run the worked examples. `sub`
    keeps the raw and repaired attempts in separate scratch dirs."""
    body = parse("body", surface)
    res = {"parsed": body is not None, "well_typed": False, "runs": False, "pass": False}
    if body is None:
        return res
    d = Path(workdir) / (task.id.replace("/", "_") + sub)
    d.mkdir(parents=True, exist_ok=True)
    rec_path, body_path = d / "record.json", d / "body.json"
    rec_path.write_text(json.dumps(task.grade_ctx["record"]))
    body_path.write_text(json.dumps(body))
    res["well_typed"] = cli(["typecheck", str(rec_path), "--body", str(body_path)]).returncode == 0
    # Higher-order records carry helpers: drop the helper records + bodies into the dir and pass --records
    # so the example's fn_ref argument resolves to the helper by address while the MODEL's body is run.
    helpers = task.grade_ctx.get("helpers") or []
    if helpers:
        for i, h in enumerate(helpers):
            (d / f"helper_{i}_rec.json").write_text(json.dumps(h["record"]))
            (d / f"helper_{i}_body.json").write_text(json.dumps(h["body"]))
        run_args = ["run", str(rec_path), "--body", str(body_path), "--records", str(d)]
    else:
        run_args = ["run", str(rec_path), "--body", str(body_path)]
    res["runs"] = cli(run_args).returncode == 0
    res["pass"] = res["well_typed"] and res["runs"]
    return res


def grade_write(task, out, workdir):
    surface = strip_answer(out)
    res = _run_write(task, surface, workdir, "")
    # semantic_pass: would a mechanical dialect repair of the same answer have passed? (See repair_surface.)
    res["semantic_pass"], res["repaired"] = res["pass"], False
    if not res["pass"]:
        rep = repair_surface(surface)
        if rep != surface and _run_write(task, rep, workdir, "_rep")["pass"]:
            res["semantic_pass"], res["repaired"] = True, True
    return res


def _read_correct(task, surface):
    """(parsed?, matches-expected?) for a single read surface string, compared canonically."""
    got = parse("value", surface)
    if got is None:
        return False, False
    got_canon = canonical_value(got)
    return True, (got_canon is not None and got_canon == canonical_value(task.grade_ctx["expected"]))


def grade_read(task, out, workdir):
    surface = strip_answer(out)
    parsed, correct = _read_correct(task, surface)
    res = {"parsed": parsed, "correct": correct, "pass": correct,
           "semantic_pass": correct, "repaired": False}
    if not correct:
        rep = repair_surface(surface)
        if rep != surface and _read_correct(task, rep)[1]:
            res["semantic_pass"], res["repaired"] = True, True
    return res


def grade_assemble(task, out, workdir):
    surface = strip_answer(out)
    names = [n.strip() for n in surface.replace("\n", ",").split(",") if n.strip()]
    meta = task.grade_ctx["name_meta"]
    # assemble has no surface-dialect dimension — the answer is a list of exact function names — so
    # semantic_pass tracks pass exactly. The fields are still emitted so every verdict has a uniform shape.
    if not names or any(n not in meta for n in names):
        return {"valid_names": False, "composes": False, "pass": False,
                "semantic_pass": False, "repaired": False}
    d = Path(workdir) / task.id.replace("/", "_")
    d.mkdir(parents=True, exist_ok=True)
    paths = []
    for i, n in enumerate(names):
        p = d / f"{i}_{n}.json"
        p.write_text(json.dumps(meta[n]))
        paths.append(str(p))
    composes = cli(["compose"] + paths).returncode == 0
    return {"valid_names": True, "composes": composes, "pass": composes,
            "semantic_pass": composes, "repaired": False}


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
        s = by_kind.setdefault(k, {"n": 0, "pass": 0, "semantic": 0})
        s["n"] += 1
        s["pass"] += 1 if r["verdict"].get("pass") else 0
        s["semantic"] += 1 if r["verdict"].get("semantic_pass") else 0
    return by_kind


def main():
    ap = argparse.ArgumentParser(description="Evaluate a model on reading/writing/assembling Nova Lingua.")
    ap.add_argument("--model", default=None,
                    help="Anthropic model id for a REAL (billed) run, e.g. claude-opus-4-8. Requires "
                         "ANTHROPIC_API_KEY and bills it OUTSIDE any Pro/Max subscription. Omit to run the "
                         "free oracle self-test (the default).")
    ap.add_argument("--oracle", action="store_true",
                    help="force the free oracle model (no API, no cost). Also the default when --model is absent.")
    ap.add_argument("--effort", default="medium",
                    help="effort level for the real model (low|medium|high|...). Default 'medium'. Measured: "
                         "effort barely affects this eval's cost or score — these short tasks need almost no "
                         "thinking, so high ~ medium (~$1 either way).")
    ap.add_argument("--tasks", default="all", choices=["all", "write", "read", "assemble"])
    ap.add_argument("--limit", type=int, default=0, help="cap tasks per kind (0 = all)")
    ap.add_argument("--conventions", default="on", choices=["on", "off"],
                    help="on: spell out the surface/value conventions in the prompt (default). "
                         "off: give only the corpus few-shot examples — tests whether the corpus alone teaches the dialect.")
    ap.add_argument("--shots", type=int, default=3, help="number of corpus few-shot examples in the prompt (write/read)")
    ap.add_argument("--out", default=str(_HERE.parent / "results.jsonl"))
    args = ap.parse_args()

    if not VALIDATOR.exists():
        sys.exit(f"nl-validator not built at {VALIDATOR}")
    if not CORPUS.exists():
        sys.exit(f"corpus not found at {CORPUS} — generate it first (tooling/corpus/gen_corpus.py)")

    corpus = [json.loads(line) for line in CORPUS.read_text().splitlines() if line.strip()]

    conventions = args.conventions == "on"
    builders = {
        "write": lambda c: build_write_tasks(c, n_shots=args.shots, conventions=conventions),
        "read": lambda c: build_read_tasks(c, n_shots=args.shots, conventions=conventions),
        "assemble": build_assemble_tasks,  # no convention bullets; one-shot framing only
    }
    kinds = ["write", "read", "assemble"] if args.tasks == "all" else [args.tasks]
    tasks = []
    for k in kinds:
        kt = builders[k](corpus)
        if args.limit:
            kt = kt[: args.limit]
        tasks.extend(kt)

    # Cost guard: a real (billed) run happens ONLY when --model is given explicitly. With no --model (or
    # with --oracle) we run the free oracle self-test, so a forgotten flag can never bill the API. The eval
    # is a benchmark — always scored over the FULL pool — so don't sample to save money. Cost scales with
    # prompt length and run count (NOT effort — high ~ medium here), so the control is running sparingly.
    if args.model and not args.oracle:
        print(f"!! REAL MODEL RUN: '{args.model}' at effort '{args.effort}' over {len(tasks)} tasks — this\n"
              f"!! calls the Anthropic API and BILLS ANTHROPIC_API_KEY outside any Pro/Max subscription\n"
              f"!! (a full-pool run measures ~$1; cost scales with prompt length and run count, not effort).\n"
              f"!! Ctrl-C now to abort.",
              file=sys.stderr)
        from model_client import AnthropicModel
        model = AnthropicModel(args.model, effort=args.effort)
    else:
        from model_client import OracleModel
        model = OracleModel()

    with tempfile.TemporaryDirectory(prefix="nleval-") as wd:
        rows = run_eval(model, tasks, wd)

    by_kind = summarize(rows)
    with open(args.out, "w") as fh:
        for r in rows:
            fh.write(json.dumps(r) + "\n")

    def col(passed, n):
        return f"{passed:3d}/{n:<3d} ({100.0 * passed / n if n else 0.0:5.1f}%)"

    print(f"model: {model.name}   conventions: {args.conventions}   shots: {args.shots}   tasks: {len(rows)}")
    # surface = graded as written; semantic = graded after mechanical dialect repair (a conservative lower
    # bound on "right modulo dialect"). The gap between them is the surface-dialect tax.
    print(f"  {'kind':9s}  {'surface':>16s}  {'semantic':>16s}")
    for k in kinds:
        if k in by_kind:
            s = by_kind[k]
            print(f"  {k:9s}  {col(s['pass'], s['n']):>16s}  {col(s['semantic'], s['n']):>16s}")
    total_n = sum(s["n"] for s in by_kind.values())
    total_p = sum(s["pass"] for s in by_kind.values())
    total_m = sum(s["semantic"] for s in by_kind.values())
    print(f"  {'TOTAL':9s}  {col(total_p, total_n):>16s}  {col(total_m, total_n):>16s}")
    print(f"  -> {args.out}")


if __name__ == "__main__":
    main()
