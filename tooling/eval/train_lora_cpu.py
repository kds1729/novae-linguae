#!/usr/bin/env python3
"""LoRA fine-tune an open-weights model on the verified corpus — LOCALLY, on CPU, for $0.

This is the non-Apple counterpart to the MLX runbook (`FINETUNING_OPENWEIGHTS.md`): same corpus, same
chat-format SFT data, same held-out grading by `eval_harness.py` — but trained with PyTorch + Hugging Face
PEFT on CPU, so it runs on a plain Linux laptop with no GPU and no MLX (see `FINETUNING_CPU.md`). The
trained adapter is evaluated by the harness's `hf:<repo>::<adapter>` backend (`model_client.HFModel`).

Data: a directory written by `export_finetune.py --mlx-data <dir>` (train.jsonl / valid.jsonl, one
`{"messages": [system, user, assistant]}` per line), or any JSONL of the same shape via `--train`. The
prompt (system+user) is masked out of the loss; the model learns to produce the assistant completion.

    # 1. export the training split (conventions-off, curated eval held out — no leakage)
    python3 export_finetune.py --corpus <combinatorial>.jsonl --conventions off --shots 0 \
        --holdout-corpus ../corpus/corpus.jsonl --mlx-data /var/tmp/claude/ftdata
    # 2. train a LoRA adapter on CPU
    ft-venv/bin/python train_lora_cpu.py --train /var/tmp/claude/ftdata/train.jsonl \
        --base Qwen/Qwen2.5-0.5B-Instruct --out /var/tmp/claude/adapter-0.5b
    # 3. grade it on the held-out curated set (conventions-off, shots-0), same as the MLX runbook
    ft-venv/bin/python eval_harness.py --model hf:Qwen/Qwen2.5-0.5B-Instruct::/var/tmp/claude/adapter-0.5b \
        --conventions off --shots 0

Deterministic: a fixed seed, greedy eval, no RNG in the data path.
"""
from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path


def load_messages(path: Path):
    rows = []
    for line in path.read_text().splitlines():
        line = line.strip()
        if not line:
            continue
        obj = json.loads(line)
        msgs = obj["messages"] if "messages" in obj else obj
        rows.append(msgs)
    return rows


def main():
    ap = argparse.ArgumentParser(description="LoRA fine-tune an open-weights model on CPU.")
    ap.add_argument("--train", required=True, help="train.jsonl ({'messages': [...]} per line)")
    ap.add_argument("--base", default="Qwen/Qwen2.5-0.5B-Instruct", help="base model repo")
    ap.add_argument("--out", required=True, help="output dir for the LoRA adapter")
    ap.add_argument("--epochs", type=float, default=3.0)
    ap.add_argument("--batch-size", type=int, default=1)
    ap.add_argument("--grad-accum", type=int, default=8)
    ap.add_argument("--lr", type=float, default=2e-4)
    ap.add_argument("--max-seq-len", type=int, default=1024)
    ap.add_argument("--lora-r", type=int, default=16)
    ap.add_argument("--lora-alpha", type=int, default=32)
    ap.add_argument("--lora-dropout", type=float, default=0.05)
    ap.add_argument("--seed", type=int, default=0)
    ap.add_argument("--threads", type=int, default=0, help="torch CPU threads (0 = leave default)")
    args = ap.parse_args()

    import torch
    from transformers import (AutoModelForCausalLM, AutoTokenizer, Trainer, TrainingArguments,
                              set_seed)
    from peft import LoraConfig, get_peft_model

    set_seed(args.seed)
    if args.threads:
        torch.set_num_threads(args.threads)

    tok = AutoTokenizer.from_pretrained(args.base)
    if tok.pad_token_id is None:
        tok.pad_token = tok.eos_token
    tok.padding_side = "right"

    # float32 on CPU: bf16/fp16 matmul is slow or unsupported for many CPU ops, and a 0.5-1.5B base in
    # fp32 fits comfortably in 15 GB. Newer transformers takes `dtype`; older took `torch_dtype`.
    try:
        model = AutoModelForCausalLM.from_pretrained(args.base, dtype=torch.float32)
    except TypeError:
        model = AutoModelForCausalLM.from_pretrained(args.base, torch_dtype=torch.float32)
    model.config.use_cache = False

    lora = LoraConfig(
        r=args.lora_r, lora_alpha=args.lora_alpha, lora_dropout=args.lora_dropout, bias="none",
        task_type="CAUSAL_LM",
        target_modules=["q_proj", "k_proj", "v_proj", "o_proj", "gate_proj", "up_proj", "down_proj"],
    )
    model = get_peft_model(model, lora)
    model.print_trainable_parameters()

    convos = load_messages(Path(args.train))

    def ids_of(x):
        # transformers 5.x apply_chat_template(tokenize=True) returns a BatchEncoding; older returned a
        # plain list of ids. Normalize to a list of token ids either way.
        return x["input_ids"] if hasattr(x, "keys") else x

    def encode(msgs):
        # Full conversation ids (with the assistant turn), and the prompt-only length so we can mask the
        # prompt out of the loss — the model is trained only on the assistant completion.
        full = ids_of(tok.apply_chat_template(msgs, add_generation_prompt=False))
        prompt_msgs = [m for m in msgs if m["role"] != "assistant"]
        prompt = ids_of(tok.apply_chat_template(prompt_msgs, add_generation_prompt=True))
        full = full[: args.max_seq_len]
        plen = min(len(prompt), len(full))
        labels = [-100] * plen + full[plen:]
        return {"input_ids": full, "labels": labels[: len(full)], "attention_mask": [1] * len(full)}

    dataset = [encode(m) for m in convos]
    dataset = [d for d in dataset if any(t != -100 for t in d["labels"])]  # drop truncated-away completions
    print(f"training on {len(dataset)} examples (of {len(convos)})", file=sys.stderr)

    pad_id = tok.pad_token_id

    def collate(batch):
        width = max(len(b["input_ids"]) for b in batch)
        input_ids, labels, attn = [], [], []
        for b in batch:
            n = width - len(b["input_ids"])
            input_ids.append(b["input_ids"] + [pad_id] * n)
            labels.append(b["labels"] + [-100] * n)
            attn.append(b["attention_mask"] + [0] * n)
        return {
            "input_ids": torch.tensor(input_ids, dtype=torch.long),
            "labels": torch.tensor(labels, dtype=torch.long),
            "attention_mask": torch.tensor(attn, dtype=torch.long),
        }

    targs = TrainingArguments(
        output_dir=args.out + "/checkpoints",
        num_train_epochs=args.epochs,
        per_device_train_batch_size=args.batch_size,
        gradient_accumulation_steps=args.grad_accum,
        learning_rate=args.lr,
        lr_scheduler_type="cosine",
        warmup_ratio=0.03,
        logging_steps=10,
        save_strategy="no",
        report_to=[],
        seed=args.seed,
        use_cpu=True,
        dataloader_num_workers=0,
    )

    trainer = Trainer(model=model, args=targs, train_dataset=dataset, data_collator=collate)
    trainer.train()

    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    model.save_pretrained(str(out))
    tok.save_pretrained(str(out))
    print(f"saved LoRA adapter -> {out}")


if __name__ == "__main__":
    main()
