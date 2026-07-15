"""Model clients for the Nova Lingua evaluation harness.

The harness asks a model to read, write, and assemble Nova Lingua artifacts and grades the output by
running it through `nl-validator` (see `eval_harness.py`). Two clients implement the same `answer(task)`
seam:

- `OracleModel` returns each task's known-correct answer. It exists to **self-test the grader**: a model
  that always answers correctly must score 100%. This verifies the validate/typecheck/run grading pipeline
  end to end with no API access — run it before ever spending a token.
- `AnthropicModel` calls a real Claude model (default Opus 4.8) via the Anthropic SDK. It reads
  `ANTHROPIC_API_KEY` from the environment; nothing is hard-coded.
- `OpenAIModel` calls an OpenAI chat model — including a fine-tuned `ft:...` id — via the OpenAI SDK,
  reading `OPENAI_API_KEY`. This is how we evaluate a model fine-tuned on the corpus (the headline test;
  see `FINETUNING.md` + `export_finetune.py`).
- `MLXModel` runs an open-weights model LOCALLY via Apple MLX (no API, no key, no cost) — optionally with
  a LoRA adapter fine-tuned on the corpus. This is the open-weights, self-hostable arm of the fine-tune
  experiment (see `FINETUNING_OPENWEIGHTS.md`): the same `answer(task)` seam, so the grader is identical.
"""

from __future__ import annotations


class OracleModel:
    """A perfect 'model' that returns each task's gold answer — used to self-test the grader."""

    name = "oracle"

    def answer(self, task) -> str:
        return task.gold


class AnthropicModel:
    """A real Claude model. `answer(task)` sends the task's system+user prompt and returns the text.

    Defaults to Claude Opus 4.8 with adaptive thinking and high effort (the project's verified-by-default
    ethos rewards careful output). A safety refusal returns the empty string, which the grader scores as a
    failed task rather than crashing.
    """

    def __init__(self, model: str = "claude-opus-4-8", effort: str = "high", max_tokens: int = 4096):
        import anthropic  # imported lazily so the harness + oracle self-test run without the SDK

        self.client = anthropic.Anthropic()  # resolves ANTHROPIC_API_KEY from the environment
        self.model = model
        self.effort = effort
        self.max_tokens = max_tokens
        self.name = model

    def answer(self, task) -> str:
        resp = self.client.messages.create(
            model=self.model,
            max_tokens=self.max_tokens,
            thinking={"type": "adaptive"},
            output_config={"effort": self.effort},
            system=task.system,
            messages=[{"role": "user", "content": task.user}],
        )
        if resp.stop_reason == "refusal":
            return ""
        return "".join(b.text for b in resp.content if b.type == "text").strip()


class OpenAIModel:
    """An OpenAI chat model (including a fine-tuned `ft:...` id), via the OpenAI SDK. Reads OPENAI_API_KEY.

    Used to evaluate a model fine-tuned on the corpus: the same `answer(task)` seam, so the grader is
    identical. Greedy decoding (temperature 0) for a stable, comparable read of the dialect.
    """

    def __init__(self, model: str, max_tokens: int = 1024):
        import openai  # imported lazily so the harness + oracle self-test run without the SDK

        self.client = openai.OpenAI()  # resolves OPENAI_API_KEY from the environment
        self.model = model
        self.max_tokens = max_tokens
        self.name = model

    def answer(self, task) -> str:
        resp = self.client.chat.completions.create(
            model=self.model,
            max_tokens=self.max_tokens,
            temperature=0,
            messages=[
                {"role": "system", "content": task.system},
                {"role": "user", "content": task.user},
            ],
        )
        return (resp.choices[0].message.content or "").strip()


class MLXModel:
    """An open-weights model run LOCALLY via Apple MLX — no API, no key, no cost. Optionally loads a LoRA
    adapter fine-tuned on the corpus, so the same client evaluates both the base model and the tuned one.

    The spec is `<repo>` for the base model or `<repo>::<adapter_dir>` for base + LoRA adapter (the harness
    passes whatever followed the `mlx:` prefix). Greedy decoding (temp 0) for a stable, comparable read of
    the dialect — matching `OpenAIModel`. `mlx_lm` is imported lazily so the oracle self-test and the
    API-model paths never need it installed.
    """

    def __init__(self, spec: str, max_tokens: int = 512):
        from mlx_lm import load, generate  # lazy: only the local-eval path needs MLX
        from mlx_lm.sample_utils import make_sampler

        repo, _, adapter = spec.partition("::")
        model, tokenizer = load(repo, adapter_path=(adapter or None))
        self._model = model
        self._tokenizer = tokenizer
        self._generate = generate
        self._sampler = make_sampler(temp=0.0)  # greedy
        self.max_tokens = max_tokens
        self.name = f"mlx:{repo}" + (f"::{adapter}" if adapter else "")

    def answer(self, task) -> str:
        prompt = self._tokenizer.apply_chat_template(
            [{"role": "system", "content": task.system},
             {"role": "user", "content": task.user}],
            add_generation_prompt=True,
        )
        text = self._generate(
            self._model, self._tokenizer, prompt=prompt,
            max_tokens=self.max_tokens, sampler=self._sampler, verbose=False,
        )
        return (text or "").strip()


class HFModel:
    """An open-weights model run LOCALLY on CPU via Hugging Face `transformers` — no API, no key, no cost.
    Optionally loads a PEFT/LoRA adapter fine-tuned on the corpus, so the same client evaluates both the
    base model and the tuned one. This is the non-Apple counterpart to `MLXModel` (a Linux/CPU box has no
    MLX and no CUDA): the same `answer(task)` seam, so the grader is identical (see `train_lora_cpu.py` +
    `FINETUNING_CPU.md`).

    The spec is `<repo>` for the base model or `<repo>::<adapter_dir>` for base + LoRA adapter (the harness
    passes whatever followed the `hf:` prefix). Greedy decoding (temp 0) for a stable, comparable read of
    the dialect — matching `OpenAIModel`/`MLXModel`. `torch`/`transformers` are imported lazily so the
    oracle self-test and the API-model paths never need them installed.
    """

    def __init__(self, spec: str, max_tokens: int = 512):
        import os
        import torch  # lazy: only the local CPU-eval path needs the PyTorch stack
        from transformers import AutoModelForCausalLM, AutoTokenizer

        repo, _, adapter = spec.partition("::")
        self._torch = torch
        self._tokenizer = load_hf_tokenizer(repo)
        # fp32 by default (best CPU op support); set NL_HF_DTYPE=bfloat16 to fit a 3B+ base in 15 GB.
        dt = {"float32": torch.float32, "bfloat16": torch.bfloat16,
              "float16": torch.float16}.get(os.environ.get("NL_HF_DTYPE", "float32"), torch.float32)
        model = load_hf_causal_model(repo, dt)
        if adapter:
            from peft import PeftModel
            model = PeftModel.from_pretrained(model, adapter)
        self._device = "cuda" if torch.cuda.is_available() else "cpu"
        model.to(self._device)
        model.eval()
        self._model = model
        self.max_tokens = max_tokens
        self.name = f"hf:{repo}" + (f"::{adapter}" if adapter else "")

    def answer(self, task) -> str:
        # `enable_thinking=False` reaches the chat template's jinja context: hybrid-thinking bases
        # (the Qwen3/3.5 line default to thinking ON) render the non-thinking prompt; templates
        # without the variable ignore it (Qwen2.5 — measured no-op). Greedy shots-0 grading needs
        # the answer, not a reasoning transcript.
        enc = self._tokenizer.apply_chat_template(
            [{"role": "system", "content": task.system},
             {"role": "user", "content": task.user}],
            add_generation_prompt=True, return_tensors="pt", return_dict=True,
            enable_thinking=False,
        )
        enc = enc.to(self._device)
        input_len = enc["input_ids"].shape[1]
        pad_id = self._tokenizer.pad_token_id
        if pad_id is None:
            pad_id = self._tokenizer.eos_token_id
        with self._torch.no_grad():
            out = self._model.generate(
                **enc, max_new_tokens=self.max_tokens, do_sample=False, pad_token_id=pad_id,
            )
        text = self._tokenizer.decode(out[0, input_len:], skip_special_tokens=True)
        return strip_think_block(text or "").strip()


def strip_think_block(text: str) -> str:
    """Drop a leading `<think>…</think>` reasoning block a hybrid-thinking base may emit despite
    `enable_thinking=False` (belt and braces — the dialect never contains the closing tag, so
    taking what follows the LAST closer is lossless for well-formed answers)."""
    if "</think>" in text:
        return text.rsplit("</think>", 1)[1]
    return text


def load_hf_tokenizer(repo: str):
    """AutoTokenizer, falling back to AutoProcessor for multimodal repos (the Qwen3.5 line ships a
    processor whose tokenizer half carries the chat template). Returns an object exposing
    `apply_chat_template` / `decode` / `pad_token_id` — both classes do."""
    from transformers import AutoTokenizer

    try:
        return AutoTokenizer.from_pretrained(repo)
    except Exception:
        from transformers import AutoProcessor

        proc = AutoProcessor.from_pretrained(repo)
        return getattr(proc, "tokenizer", proc)


def load_hf_causal_model(repo: str, dt):
    """AutoModelForCausalLM (newer transformers: `dtype`, older: `torch_dtype`), falling back to
    the multimodal auto-class for repos whose architecture registers there (Qwen3.5); text-only
    usage is unchanged — the language modeling head is what `generate` drives either way."""
    from transformers import AutoModelForCausalLM

    def _load(cls):
        try:
            return cls.from_pretrained(repo, dtype=dt)
        except TypeError:
            return cls.from_pretrained(repo, torch_dtype=dt)

    try:
        return _load(AutoModelForCausalLM)
    except Exception:
        import transformers

        cls = getattr(transformers, "AutoModelForMultimodalLM", None) or getattr(
            transformers, "AutoModelForImageTextToText"
        )
        return _load(cls)
