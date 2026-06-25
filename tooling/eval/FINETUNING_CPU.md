# Fine-tuning on open weights, locally, on CPU ($0, no GPU)

This is the **no-GPU, no-MLX** counterpart to [`FINETUNING_OPENWEIGHTS.md`](FINETUNING_OPENWEIGHTS.md). Same
corpus, same chat-format SFT data, same held-out grading — but trained with **PyTorch + Hugging Face PEFT
(LoRA) on CPU**, so it runs on a plain Linux laptop with neither an NVIDIA GPU nor Apple MLX. The MLX
runbook's `mlx_lm.lora` step is replaced by [`train_lora_cpu.py`](train_lora_cpu.py); the eval backend is
`hf:<repo>::<adapter>` ([`model_client.HFModel`](model_client.py)) instead of `mlx:`.

Everything here is **local and free** — the only network use is the one-time base-model + dataset download.

## Why CPU is enough to start

The project's own finding is that held-out `write` generalization tracks **training-data shape diversity,
not model size**. So a small base (Qwen2.5-**0.5B**/1.5B) is the right starting point, and a small base is
exactly what trains fast on CPU. LoRA only updates a tiny adapter, and the corpus is a few thousand short
examples — an epoch is minutes-to-hours on a laptop CPU, not days.

## Environment (one-time)

A dedicated CPU venv (kept out of the repo, since torch is large):

```bash
uv venv --python 3.12 /var/tmp/claude/ft-venv
uv pip install --python /var/tmp/claude/ft-venv/bin/python --index-url https://download.pytorch.org/whl/cpu torch
uv pip install --python /var/tmp/claude/ft-venv/bin/python numpy transformers peft datasets accelerate
```

The PyTorch **CPU** wheel (from PyTorch's own index) avoids dragging in ~2 GB of unusable CUDA libraries.

## The loop: broaden → retrain → measure

```bash
HF=/var/tmp/claude/ft-venv/bin/python          # the CPU venv
COMBO=/var/tmp/claude/corpus_combo.jsonl        # training-scale corpus (gitignored, regenerable)

# 0. (broaden) edit tooling/corpus/gen_corpus.py combinatorial_specs(), then regenerate + verify:
python3 ../corpus/gen_corpus.py --combinatorial --out $COMBO

# 1. export the training split — conventions OFF, curated eval HELD OUT (leakage guard):
python3 export_finetune.py --corpus $COMBO --conventions off --shots 0 \
    --holdout-corpus ../corpus/corpus.jsonl --mlx-data /var/tmp/claude/ftdata

# 2. (retrain) LoRA fine-tune on CPU — the slow step, run it in the background / overnight:
$HF train_lora_cpu.py --train /var/tmp/claude/ftdata/train.jsonl \
    --base Qwen/Qwen2.5-0.5B-Instruct --out /var/tmp/claude/adapter-0.5b

# 3. (measure) grade base vs. tuned on the HELD-OUT curated set — conventions off, shots 0:
$HF eval_harness.py --model hf:Qwen/Qwen2.5-0.5B-Instruct --conventions off --shots 0          # BEFORE
$HF eval_harness.py --model hf:Qwen/Qwen2.5-0.5B-Instruct::/var/tmp/claude/adapter-0.5b \
    --conventions off --shots 0                                                                # AFTER
```

The grade is honest by construction: the curated corpus is held out of training (`--holdout-corpus` drops
any train task whose prompt+gold — or, for write, whose gold body — matches an eval task), and the grader is
`nl-validator`, not an LLM judge. Run `eval_harness.py --oracle` first (100%) to confirm the grader.

## Knobs (`train_lora_cpu.py`)

`--base` (default `Qwen/Qwen2.5-0.5B-Instruct`), `--epochs` (3), `--batch-size` (1), `--grad-accum` (8),
`--lr` (2e-4), `--max-seq-len` (1024), `--lora-r`/`--lora-alpha`/`--lora-dropout`, `--threads` (CPU
threads; 0 = torch default). float32 throughout (CPU bf16/fp16 is slow). For a 15 GB / no-swap box, 0.5B
and 1.5B are comfortable; 3B is the risky ceiling (tiny batch + shorter `--max-seq-len`).
