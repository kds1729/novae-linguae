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
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import eval_harness as eh  # noqa: E402


def main():
    ap = argparse.ArgumentParser(description="Export the verified corpus as chat-format SFT pairs.")
    ap.add_argument("--corpus", default=str(eh.CORPUS), help="corpus JSONL to export (default: curated corpus.jsonl)")
    ap.add_argument("--out", default=str(Path(__file__).resolve().parent / "sft.jsonl"), help="output SFT JSONL")
    ap.add_argument("--conventions", default="on", choices=["on", "off"],
                    help="on: state the surface conventions in the system prompt (default). off: examples only.")
    ap.add_argument("--shots", type=int, default=0,
                    help="few-shot demos in the system prompt; 0 (default) trains every example as a target.")
    ap.add_argument("--kinds", default="write,read,assemble", help="comma-separated task kinds to export")
    args = ap.parse_args()

    if not eh.VALIDATOR.exists():
        sys.exit(f"nl-validator not built at {eh.VALIDATOR}")
    corpus_path = Path(args.corpus)
    if not corpus_path.exists():
        sys.exit(f"corpus not found at {corpus_path}")

    corpus = [json.loads(line) for line in corpus_path.read_text().splitlines() if line.strip()]
    conv = args.conventions == "on"
    builders = {
        "write": lambda: eh.build_write_tasks(corpus, n_shots=args.shots, conventions=conv),
        "read": lambda: eh.build_read_tasks(corpus, n_shots=args.shots, conventions=conv),
        "assemble": lambda: eh.build_assemble_tasks(corpus),
    }

    counts = {}
    with open(args.out, "w", encoding="utf-8") as fh:
        for kind in [k.strip() for k in args.kinds.split(",") if k.strip()]:
            tasks = builders[kind]()
            for t in tasks:
                rec = {"kind": kind, "messages": [
                    {"role": "system", "content": t.system},
                    {"role": "user", "content": t.user},
                    {"role": "assistant", "content": t.gold},
                ]}
                fh.write(json.dumps(rec, ensure_ascii=False) + "\n")
            counts[kind] = len(tasks)

    total = sum(counts.values())
    print(f"wrote {total} SFT pairs -> {args.out}")
    for k, n in counts.items():
        print(f"  {k:9s} {n}")


if __name__ == "__main__":
    main()
