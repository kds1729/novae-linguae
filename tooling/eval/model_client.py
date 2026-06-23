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
