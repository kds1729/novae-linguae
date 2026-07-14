#!/usr/bin/env python3
"""Cluster-then-assert sweep: publish proved `equivalent` claims over a live node's functions.

The commons-maintenance move for semantic equivalence (the cert_sweep/cost_sweep precedent): walk
every function record, bucket by canonical NORMAL FORM (`nl-validator normalize --hash` — equal
normal forms are a solver-free equivalence proof), and for every class with more than one member
publish signed `equivalent` claims (`assert-equivalent --publish`, which re-proves before signing)
in a star around the class's first member — enough for any consumer's union-find to reconstruct
the class, e.g. the node's `?collapse=equivalent` view or the agent loop's `collapse` step.

The default run is NF-tier only: the inductive prover can decide equivalences normalization
cannot, but a blind pairwise solver sweep pays a timeout for every NON-equivalent same-shape
pair. `--solver-tier` opts into that second tier, BOUNDED: one representative per normal-form
class, bucketed by exact summary type (pure records only — the prover cannot see effects; float
types skipped — the Int-theory guard), pairs attempted cross-class only under a union-find (a
proved merge retires its loser from every later pair), capped at `--solver-pairs` attempts with a
`--pair-timeout` backstop per pair. Refused pairs (DISTINCT / UNKNOWN / UNSUPPORTED) are recorded
in `--state` (a local JSON verdict cache) so a re-run resumes instead of re-paying them; proved
pairs need no cache — the node's claims make them skippable. Idempotent on the NF tier: pairs
already claimed on the node (per `GET /v0/records/{hash}/equivalences`) are skipped.

    python3 equiv_sweep.py --node https://nl.1105software.com [--seed …] [--dry-run]
    python3 equiv_sweep.py --node … --solver-tier [--solver-pairs 200] [--state sweep_state.json]
"""

import argparse
import collections
import json
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path

VALIDATOR = str(Path(__file__).resolve().parents[1] / "validator" / "target" / "release" / "nl-validator")


def get_json(node, path):
    """GET with edge-rate-limit awareness: the reference node's Caddy answers 429 per client IP,
    so a bulk walk backs off (honoring Retry-After when present) instead of shedding work."""
    for attempt in range(6):
        try:
            with urllib.request.urlopen(urllib.request.Request(node + path), timeout=120) as r:
                return json.load(r)
        except urllib.error.HTTPError as exc:
            if exc.code != 429 or attempt == 5:
                raise
            retry_after = exc.headers.get("Retry-After")
            time.sleep(float(retry_after) if retry_after else 2.0 * (attempt + 1))
    raise RuntimeError("unreachable")


def post_json(node, path, payload):
    req = urllib.request.Request(node + path, data=json.dumps(payload).encode(),
                                 headers={"content-type": "application/json"}, method="POST")
    with urllib.request.urlopen(req, timeout=120) as r:
        return json.load(r)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--node", required=True)
    ap.add_argument("--seed", default="novae-linguae-example-certifier",
                    help="signing identity for the published claims")
    ap.add_argument("--dry-run", action="store_true", help="report classes; publish nothing")
    ap.add_argument("--solver-tier", action="store_true",
                    help="after the NF tier, attempt bounded pairwise proofs across NF classes")
    ap.add_argument("--solver-pairs", type=int, default=200,
                    help="cap on solver-tier pair ATTEMPTS this run (proved or refused)")
    ap.add_argument("--pair-timeout", type=float, default=180.0,
                    help="hard per-pair backstop in seconds (the prover's own budgets sit below it)")
    ap.add_argument("--state", type=Path, default=None,
                    help="local JSON verdict cache; refused pairs are skipped on re-runs")
    args = ap.parse_args()
    node = args.node.rstrip("/")

    # 1. Every function record's (hash, body_hash) — the terminates filter enumerates functions.
    summaries, cursor = [], None
    while True:
        flt = {"terminates": ["always", "conditional", "unknown"], "limit": 1000}
        if cursor:
            flt["cursor"] = cursor
        got = post_json(node, "/v0/query?include=summary", flt)
        summaries += got["results"]
        cursor = got.get("cursor")
        if got.get("complete") or not got["results"]:
            break
    with_body = [(s["hash"], s["body_hash"]) for s in summaries if s.get("body_hash")]
    summary_of = {s["hash"]: s for s in summaries}
    print(f"functions: {len(summaries)} ({len(with_body)} with a resolvable body)")

    # 2. Normal-form address per distinct body (fetched once, normalized once).
    tmp = Path(tempfile.mkdtemp(prefix="nl-equiv-sweep-"))
    nf_of_body = {}
    for _, bh in with_body:
        if bh in nf_of_body:
            continue
        try:
            body = get_json(node, f"/v0/records/{bh}")
        except Exception as exc:
            nf_of_body[bh] = None
            print(f"  ! {bh[:24]}… unfetchable ({exc}); skipped", file=sys.stderr)
            continue
        p = tmp / f"{bh[:16]}.json"
        p.write_text(json.dumps(body))
        r = subprocess.run([VALIDATOR, "normalize", "--body", str(p), "--hash"],
                           capture_output=True, text=True)
        nf_of_body[bh] = r.stdout.strip() if r.returncode == 0 else None
        if r.returncode != 0:
            print(f"  ! {bh[:24]}… does not normalize: {(r.stderr or '').strip()}", file=sys.stderr)

    # 3. Classes: same normal form = same behavior (solver-free proof).
    classes = collections.defaultdict(list)
    for fn, bh in with_body:
        nf = nf_of_body.get(bh)
        if nf:
            classes[nf].append(fn)
    multi = {nf: sorted(set(fns)) for nf, fns in classes.items() if len(set(fns)) > 1}
    print(f"normal-form classes: {len(classes)}; with >1 member: {len(multi)}")

    # 4. Star-publish each class (skipping already-claimed pairs).
    published = skipped = failed = 0
    for nf, members in sorted(multi.items()):
        rep, rest = members[0], members[1:]
        print(f"class {nf[:24]}…  {len(members)} member(s): {', '.join(m[:20] + '…' for m in members)}")
        try:
            existing = get_json(node, f"/v0/records/{rep}/equivalences")["equivalences"]
            claimed = {tuple(sorted((e["body"]["claim"]["a"], e["body"]["claim"]["b"])))
                       for e in existing}
        except Exception:
            claimed = set()
        for other in rest:
            if tuple(sorted((rep, other))) in claimed:
                skipped += 1
                continue
            if args.dry_run:
                print(f"  dry-run: would assert {rep[:20]}… ≡ {other[:20]}…")
                continue
            time.sleep(0.5)  # pace the per-pair crawls under the edge rate limit
            r = subprocess.run(
                [VALIDATOR, "assert-equivalent", "--f", rep, "--g", other,
                 "--node", node, "--seed", args.seed, "--publish",
                 "--out", str(tmp / "assert.json")],
                capture_output=True, text=True)
            if r.returncode == 0:
                published += 1
                line = next((ln for ln in r.stdout.splitlines() if ln.startswith("published")), "")
                print(f"  {line or 'published'}")
            else:
                failed += 1
                print(f"  ! assert failed: {(r.stdout or r.stderr).strip().splitlines()[-1]}",
                      file=sys.stderr)
    print(f"\npublished {published} claim(s), skipped {skipped} already-claimed, {failed} failed")

    # 5. Solver tier (opt-in): the equivalences normalization cannot see. One representative per
    # NF class (singletons included — merging CLASSES is this tier's whole job), bucketed by exact
    # summary type; pure only (the prover cannot see effects) and no float in the signature (the
    # Int-theory guard). A union-find retires a proved pair's loser from every later pairing, and
    # the local `--state` cache keeps refused pairs from being re-paid on a resume.
    if not args.solver_tier:
        return
    print("\n== solver tier ==")
    sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "ingest-common"))
    from nl_synth import py_value, SynthError            # noqa: E402 — solver-tier-only deps
    from nl_values import to_value_ast, ValueEncodeError  # noqa: E402
    state = {}
    if args.state and args.state.exists():
        state = json.loads(args.state.read_text())

    reps = sorted(min(set(fns)) for fns in classes.values())

    def eligible(fn):
        s = summary_of.get(fn, {})
        t = s.get("type", "")
        return bool(t) and not s.get("effects") and "float" not in t

    def type_key(fn):
        """Canonical bucket key: the node's `type_str` is the PRODUCER'S serialization, and v0.2
        producers disagree on key order (measured live: `List int → int` split into two buckets) —
        re-dump sorted. A v0.1 plain-string type stays its own exact-match key."""
        t = summary_of[fn]["type"]
        try:
            return json.dumps(json.loads(t), sort_keys=True)
        except (ValueError, TypeError):
            return t

    buckets = collections.defaultdict(list)
    for fn in reps:
        if eligible(fn):
            buckets[type_key(fn)].append(fn)
    contested = {t: sorted(fns) for t, fns in buckets.items() if len(fns) > 1}
    raw_pairs = sum(len(f) * (len(f) - 1) // 2 for f in contested.values())
    print(f"representatives: {len(reps)}; same-type pure buckets with >1 class: {len(contested)} "
          f"({raw_pairs} raw pair(s) before probes / union-find / cache)")
    if args.dry_run:
        for t, fns in sorted(contested.items()):
            print(f"  [{len(fns)}] {t}")
        return

    # Probe filter: evaluate each representative's LOCAL body (already fetched for NF hashing) on a
    # few type-synthesized argument sets and group by output vector — reps that differ on any probe
    # are distinct by WITNESS, no solver spent (the int→int bucket alone is ~half the raw pairs,
    # nearly all genuinely distinct). A rep that cannot be probed (polymorphic type, fn_ref into the
    # commons, record param, eval error/timeout) stays a conservative candidate against the whole
    # bucket — those pairs queue AFTER the probe-equal ones, so the attempt cap is spent on likely
    # proofs first. Probes never JUSTIFY a claim; only the prover does.
    def probe_arg_sets(param_types):
        k = len(param_types)
        patterns = sorted({tuple([0] * k), tuple([1] * k),
                           tuple(i % 2 for i in range(k)), tuple((i + 1) % 2 for i in range(k))})
        try:
            return [[to_value_ast(py_value(t, v), expected=t) for t, v in zip(param_types, pat)]
                    for pat in patterns]
        except (SynthError, ValueEncodeError):
            return None

    def probe_vector(fn, arg_files):
        """Tuple of (rc, stdout) per probe, or None when the rep cannot be probed."""
        bh = dict(with_body).get(fn)
        body_file = tmp / f"{bh[:16]}.json" if bh else None
        if body_file is None or not body_file.exists():
            return None
        outs = []
        for files in arg_files:
            cmd = [VALIDATOR, "eval", str(body_file)]
            for f_ in files:
                cmd += ["--arg", str(f_)]
            try:
                r = subprocess.run(cmd, capture_output=True, text=True, timeout=10)
            except subprocess.TimeoutExpired:
                return None
            outs.append((r.returncode, r.stdout.strip()))
        if all(rc != 0 for rc, _ in outs):
            return None   # nothing observable (e.g. every probe trips an unresolvable fn_ref)
        return tuple(outs)

    candidate_pairs = []   # (priority, a, b, type_key) — priority 0 = probe-equal, 1 = unprobed side
    for bi, (t, fns) in enumerate(sorted(contested.items())):
        arg_sets = None
        try:
            tast = json.loads(t)
            if isinstance(tast, dict) and tast.get("kind") == "fn":
                arg_sets = probe_arg_sets(tast.get("params") or [])
        except (ValueError, TypeError):
            pass
        arg_files = []
        if arg_sets:
            for pi, vals in enumerate(arg_sets):
                files = []
                for ai, v in enumerate(vals):
                    p = tmp / f"probe_{bi}_{pi}_{ai}.json"
                    p.write_text(json.dumps(v))
                    files.append(p)
                arg_files.append(files)
        vec_of = {fn: (probe_vector(fn, arg_files) if arg_files else None) for fn in fns}
        by_vec = collections.defaultdict(list)
        unprobed = [fn for fn in fns if vec_of[fn] is None]
        for fn in fns:
            if vec_of[fn] is not None:
                by_vec[vec_of[fn]].append(fn)
        for group in by_vec.values():
            for i, a in enumerate(group):
                for b in group[i + 1:]:
                    candidate_pairs.append((0, a, b, t))
        for i, a in enumerate(unprobed):
            for b in unprobed[i + 1:]:
                candidate_pairs.append((1, a, b, t))
        for a in unprobed:
            for b in fns:
                if vec_of[b] is not None:
                    candidate_pairs.append((1, *sorted((a, b)), t))
    candidate_pairs.sort()
    probe_split = raw_pairs - len(candidate_pairs)
    print(f"probe filter: {probe_split} pair(s) distinct by witness; {len(candidate_pairs)} candidate(s) "
          f"({sum(1 for p in candidate_pairs if p[0] == 0)} probe-equal)")

    # Union-find, lazily populated. Seed: every NF class (member → its representative), then every
    # equivalence the node already holds about a participating representative — so a claim from an
    # earlier run (or the NF stars) removes its pair from the candidate set.
    parent = {}

    def find(x):
        parent.setdefault(x, x)
        while parent[x] != x:
            parent[x] = parent[parent[x]]
            x = parent[x]
        return x

    def union(a, b):
        ra, rb = find(a), find(b)
        if ra != rb:
            parent[max(ra, rb)] = min(ra, rb)

    for fns in classes.values():
        members = sorted(set(fns))
        for m in members[1:]:
            union(members[0], m)
    for fns in contested.values():
        for fn in fns:
            try:
                existing = get_json(node, f"/v0/records/{fn}/equivalences")["equivalences"]
            except Exception:
                existing = []
            for e in existing:
                c = e["body"]["claim"]
                union(c["a"], c["b"])

    def save_state():
        if args.state:
            args.state.write_text(json.dumps(state, indent=1, sort_keys=True) + "\n")

    attempts = 0
    tally = collections.Counter()
    capped = False
    for prio, a, b, t in candidate_pairs:
        if find(a) == find(b):
            continue              # already one class (node claim, or merged earlier this run)
        key = "|".join(sorted((a, b)))
        if key in state:
            tally[f"cached-{state[key]}"] += 1
            continue
        if attempts >= args.solver_pairs:
            capped = True
            break
        attempts += 1
        marker = "≈" if prio == 0 else "?"
        print(f"pair {attempts}/{args.solver_pairs} {marker}  {a[:20]}… vs {b[:20]}…")
        time.sleep(0.5)  # pace the per-pair crawls under the edge rate limit
        try:
            r = subprocess.run(
                [VALIDATOR, "assert-equivalent", "--f", a, "--g", b,
                 "--node", node, "--seed", args.seed, "--publish",
                 "--out", str(tmp / "assert.json")],
                capture_output=True, text=True, timeout=args.pair_timeout)
        except subprocess.TimeoutExpired:
            tally["timeout"] += 1
            state[key] = "timeout"
            save_state()
            print("  timeout — cached, resume skips it")
            continue
        if r.returncode == 0:
            union(a, b)
            tally["proved"] += 1
            line = next((ln for ln in r.stdout.splitlines()
                         if ln.startswith(("EQUIVALENT", "published"))), "EQUIVALENT")
            print(f"  {line}")
            continue
        # Classify on the WHOLE message — a DISTINCT counterexample (solver model dump) or an
        # UNSUPPORTED reason can span lines, so the last line alone loses the verdict keyword.
        full = (r.stderr or r.stdout).strip()
        verdict = next((v for v in ("DISTINCT", "UNKNOWN", "UNSUPPORTED", "NO-SOLVER") if v in full),
                       "error")
        tally[verdict.lower()] += 1
        if verdict != "error":
            state[key] = verdict.lower()
            save_state()
        line = next((ln for ln in full.splitlines() if verdict in ln), full.splitlines()[-1] if full else "")
        print(f"  {line}")
    if capped:
        print(f"\nattempt cap ({args.solver_pairs}) reached — re-run with --state to resume")
    print("solver tier: " + (", ".join(f"{k} {v}" for k, v in sorted(tally.items())) or "nothing to attempt"))


if __name__ == "__main__":
    main()
