#!/usr/bin/env python3
"""Export the verified corpus as chat-format SFT pairs for fine-tuning.

The corpus is verified (every function validates / type-checks / runs), and the eval harness already knows
how to frame it as read/write/assemble tasks. This reuses those task builders so the TRAINING format is
*identical* to the EVAL format — the model is trained on exactly the prompts it will be graded on. Each
corpus function yields a **write** pair (intent + type + worked examples -> surface body) and a **read**
pair (body + input -> output value); compositions yield **assemble** pairs. Output is one JSON object per
line:

    {"kind": "write", "messages": [{"role": "system", ...}, {"role": "user", ...}, {"role": "assistant", ...}]}

which is the de-facto SFT format accepted by most fine-tuning stacks (OpenAI / Together / Fireworks /
Axolotl / TRL). Point `--corpus` at the large combinatorial corpus (`gen_corpus.py --combinatorial`) for a
training-scale dataset; `--shots 0` (default) states the surface conventions in the system prompt and makes
every example a target (nothing held out). This is LOCAL/FREE — it only runs nl-validator, never the API.

    python3 export_finetune.py --corpus /tmp/corpus-train.jsonl --out /tmp/sft.jsonl
"""
from __future__ import annotations

import argparse
import json
import random
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import eval_harness as eh  # noqa: E402


def main():
    ap = argparse.ArgumentParser(description="Export the verified corpus as chat-format SFT pairs.")
    ap.add_argument("--corpus", default=str(eh.CORPUS), help="corpus JSONL to export (default: curated corpus.jsonl)")
    ap.add_argument("--out", default=str(Path(__file__).resolve().parent / "sft.jsonl"), help="output SFT JSONL")
    ap.add_argument("--mlx-data", default=None,
                    help="instead of one SFT file, write an MLX --data dir (train.jsonl + valid.jsonl, "
                         "messages-only chat format) for mlx_lm.lora. Deterministic shuffle + split.")
    ap.add_argument("--valid-frac", type=float, default=0.05, help="held-out fraction for MLX valid.jsonl (--mlx-data)")
    ap.add_argument("--seed", type=int, default=0, help="shuffle seed for the MLX train/valid split (reproducible)")
    ap.add_argument("--conventions", default="on", choices=["on", "off"],
                    help="on: state the surface conventions in the system prompt (default). off: examples only.")
    ap.add_argument("--shots", type=int, default=0,
                    help="few-shot demos in the system prompt; 0 (default) trains every example as a target.")
    ap.add_argument("--kinds", default="write,read,assemble", help="comma-separated task kinds to export")
    ap.add_argument("--holdout-corpus", default=None,
                    help="exclude any training task whose (prompt,gold) appears in this corpus's tasks — "
                         "the eval set, to prevent train/eval leakage. The combinatorial corpus is a "
                         "SUPERSET of the curated corpus, so without this the curated eval is 100%% leaked.")
    args = ap.parse_args()

    if not eh.VALIDATOR.exists():
        sys.exit(f"nl-validator not built at {eh.VALIDATOR}")
    corpus_path = Path(args.corpus)
    if not corpus_path.exists():
        sys.exit(f"corpus not found at {corpus_path}")

    corpus = [json.loads(line) for line in corpus_path.read_text().splitlines() if line.strip()]
    conv = args.conventions == "on"

    def build(c, kind):
        if kind == "write":
            return eh.build_write_tasks(c, n_shots=args.shots, conventions=conv)
        if kind == "read":
            return eh.build_read_tasks(c, n_shots=args.shots, conventions=conv)
        return eh.build_assemble_tasks(c)

    # Leakage guard: build the holdout (eval) tasks the same way and drop any leaking training task. Two
    # leakage channels, both closed:
    #   1. (prompt, gold) pair — the exact task seen verbatim. Applies to every kind.
    #   2. gold BODY alone, for write/assemble — a different prompt with the SAME answer body still lets the
    #      model memorize the exact surface string (e.g. a combinatorial `contains_0` vs the eval's
    #      `contains_zero`). NOT applied to `read`, whose gold is a value (e.g. `6`) that collides constantly
    #      across unrelated tasks — there the prompt (which carries the body+input) is the unique key.
    holdout_pairs, holdout_golds = set(), set()
    if args.holdout_corpus:
        hpath = Path(args.holdout_corpus)
        if not hpath.exists():
            sys.exit(f"holdout corpus not found at {hpath}")
        hcorpus = [json.loads(line) for line in hpath.read_text().splitlines() if line.strip()]
        for kind in [k.strip() for k in args.kinds.split(",") if k.strip()]:
            for t in build(hcorpus, kind):
                holdout_pairs.add((t.user, t.gold))
                if kind in ("write", "assemble"):
                    holdout_golds.add(t.gold)

    counts = {}
    excluded = 0
    records = []
    for kind in [k.strip() for k in args.kinds.split(",") if k.strip()]:
        kept = 0
        for t in build(corpus, kind):
            leaked = (t.user, t.gold) in holdout_pairs or (kind in ("write", "assemble") and t.gold in holdout_golds)
            if leaked:
                excluded += 1
                continue
            records.append({"kind": kind, "messages": [
                {"role": "system", "content": t.system},
                {"role": "user", "content": t.user},
                {"role": "assistant", "content": t.gold},
            ]})
            kept += 1
        counts[kind] = kept
    total = sum(counts.values())
    if args.holdout_corpus:
        print(f"leakage guard: excluded {excluded} training tasks that matched the holdout corpus")

    if args.mlx_data:
        # MLX-LM expects a --data directory of {"messages": [...]} lines (it applies the chat template and
        # masks the prompt, training on the assistant completion). Drop the bookkeeping `kind` key. A fixed
        # seed makes the train/valid split byte-reproducible.
        data_dir = Path(args.mlx_data)
        data_dir.mkdir(parents=True, exist_ok=True)
        shuffled = list(records)
        random.Random(args.seed).shuffle(shuffled)
        n_valid = max(1, int(len(shuffled) * args.valid_frac)) if len(shuffled) > 1 else 0
        valid, train = shuffled[:n_valid], shuffled[n_valid:]
        for name, rows in (("train", train), ("valid", valid)):
            with open(data_dir / f"{name}.jsonl", "w", encoding="utf-8") as fh:
                for r in rows:
                    fh.write(json.dumps({"messages": r["messages"]}, ensure_ascii=False) + "\n")
        print(f"wrote MLX data ({total} pairs) -> {data_dir}/  (train {len(train)}, valid {len(valid)})")
        for k, n in counts.items():
            print(f"  {k:9s} {n}")
        return

    with open(args.out, "w", encoding="utf-8") as fh:
        for rec in records:
            fh.write(json.dumps(rec, ensure_ascii=False) + "\n")
    print(f"wrote {total} SFT pairs -> {args.out}")
    for k, n in counts.items():
        print(f"  {k:9s} {n}")


if __name__ == "__main__":
    main()
