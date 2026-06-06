"""Embeddings for semantic search (spec/commons.md, `POST /v0/search`).

Search is **best-effort and node-local**: the node advertises which model produced its vectors (in
the search response `model` field and in `/v0/info`), and two nodes MAY rank differently. The
reference node ships a stdlib-only, deterministic **lexical** embedder so it stays 100% local and
dependency-light, and reproducible byte-for-byte (principle 5: deterministic by default). A neural
model can replace it behind `get_embedder()` with no protocol change — that factory is the seam.

An embedding maps a record (or a free-text query) to a fixed-length, L2-normalized float vector,
positioned so that lexically/structurally similar content sits nearby. Ranking is by cosine
similarity, which on normalized vectors is just the dot product.

The lexical embedder builds the vector mechanically from the record's own tokens — `name_hints`, the
type string, `intent_tags`, refinement/property expressions (and, for messages, the speech-act kind
and body) — via the hashing trick. It captures lexical/structural overlap, not deep meaning; that is
the explicit trade for zero dependencies and determinism.
"""

import hashlib
import json
import math
import re
import urllib.request

from django.conf import settings

# Default dimensionality of the lexical vector. It is part of the model's identity: a record embedded
# at one dimension is not comparable to one embedded at another, so a non-default dim bumps model_id.
_DEFAULT_DIM = 256

_CAMEL = re.compile(r"(?<=[a-z0-9])(?=[A-Z])")   # camelCase boundary: forEach -> for Each
_NON_ALNUM = re.compile(r"[^0-9a-z]+")            # snake_case / punctuation split (after lowercasing)
_DIGIT_LETTER = re.compile(r"(?<=[a-z])(?=[0-9])|(?<=[0-9])(?=[a-z])")  # b64encode -> b 64 encode

# Content-free English/structural words that only add noise to lexical similarity. Kept tiny and
# generic so it never strips a meaningful identifier token (code identifiers rarely are stopwords).
_STOP = frozenset(
    "a an the to of and or in on for with from by as is are be was were "
    "that this it its at into not no".split()
)


def _tokens(text):
    """Lowercased token bag: split on whitespace, camelCase, snake/punctuation, and letter/digit
    boundaries; drop a small stopword set. So `b64encode` -> b, 64, encode and `rgb_to_hsv` -> rgb,
    hsv (the connective `to` is dropped)."""
    out = []
    for chunk in str(text).split():
        for piece in _CAMEL.sub(" ", chunk).split():      # split identifiers on camelCase first
            for tok in _NON_ALNUM.split(piece.lower()):    # then on underscores / punctuation
                for sub in _DIGIT_LETTER.split(tok):       # then on letter<->digit boundaries
                    if sub and sub not in _STOP:
                        out.append(sub)
    return out


def _record_text(rec):
    """Collect the searchable text out of a record (function record or message)."""
    parts = []
    parts += rec.get("name_hints", []) or []
    parts += rec.get("intent_tags", []) or []

    sig = rec.get("signature")
    if isinstance(sig, dict):
        t = sig.get("type")
        if isinstance(t, str):
            parts.append(t)
        elif t is not None:
            parts.append(json.dumps(t))                    # structured (v0.2) type AST -> text
        for key in ("complexity", "terminates"):
            if sig.get(key):
                parts.append(str(sig[key]))
        for r in sig.get("refinements", []) or []:
            if isinstance(r, dict):
                if r.get("kind"):
                    parts.append(str(r["kind"]))
                if isinstance(r.get("expr"), str):
                    parts.append(r["expr"])

    for p in rec.get("properties", []) or []:
        if isinstance(p, dict):
            if p.get("name"):
                parts.append(str(p["name"]))
            if isinstance(p.get("expr"), str):
                parts.append(p["expr"])

    # Messages: the speech-act kind ("request"/"assert"/...) and the body carry the meaning.
    if isinstance(rec.get("kind"), str):
        parts.append(rec["kind"])
    if isinstance(rec.get("body"), dict):
        parts.append(json.dumps(rec["body"]))

    return " ".join(parts)


def _bucket_sign(token, dim):
    """Deterministic (bucket, sign) for a token via blake2b — NOT builtin hash() (salted per process)."""
    n = int.from_bytes(hashlib.blake2b(token.encode("utf-8"), digest_size=8).digest(), "big")
    return n % dim, (1.0 if (n >> 63) & 1 else -1.0)


def cosine(a, b):
    """Cosine similarity. Stored vectors are L2-normalized, so this is just the dot product."""
    if not a or not b or len(a) != len(b):
        return 0.0
    return sum(x * y for x, y in zip(a, b))


class Embedder:
    """Pluggable embedder. A neural backend implements this and is selected by get_embedder()."""

    model_id = "abstract"

    def embed(self, record_or_text):
        raise NotImplementedError

    def embed_batch(self, items):
        """Embed many records/strings. Override when the backend supports a real batch call (a remote
        model server, GPU batching); the default loop keeps small/pure embedders simple."""
        return [self.embed(x) for x in items]


class LexicalHashingEmbedder(Embedder):
    """Stdlib-only deterministic lexical embedding (hashing trick + sublinear TF + L2 norm)."""

    def __init__(self, dim=_DEFAULT_DIM):
        self.dim = int(dim)
        # Dim is part of model identity; the canonical 256-dim model keeps the bare id.
        self.model_id = "lexical-hashing-v0" if self.dim == _DEFAULT_DIM \
            else f"lexical-hashing-v0-d{self.dim}"

    def _vectorize(self, tokens):
        tf = {}
        for t in tokens:
            tf[t] = tf.get(t, 0) + 1
        vec = [0.0] * self.dim
        for tok, count in tf.items():
            bucket, sign = _bucket_sign(tok, self.dim)
            vec[bucket] += sign * (1.0 + math.log(count))   # sublinear term weighting
        norm = math.sqrt(sum(x * x for x in vec))
        return [x / norm for x in vec] if norm > 0 else vec

    def embed(self, record_or_text):
        text = record_or_text if isinstance(record_or_text, str) else _record_text(record_or_text)
        return self._vectorize(_tokens(text))


class NeuralEmbedder(Embedder):
    """Calls an external embedding model server over HTTP (HF Text-Embeddings-Inference, Ollama, or
    any `/embed`-compatible endpoint). The server is where an industrial operator puts a GPU fleet —
    the node code is identical, only `COMMONS_EMBEDDINGS_URL` changes. Vectors are used as returned
    (TEI L2-normalizes), and ranked only against vectors from the same `model_id`."""

    def __init__(self, model_id, url, dim, timeout=30.0, max_batch=32):
        if not url:
            raise ValueError("a neural COMMONS_EMBEDDER needs COMMONS_EMBEDDINGS_URL set")
        self.model_id = model_id
        self.url = url.rstrip("/")
        self.dim = int(dim)
        self.timeout = timeout
        self.max_batch = max(1, int(max_batch))   # model servers cap inputs/request (TEI default 32)

    def _post(self, texts):
        body = json.dumps({"inputs": texts}).encode("utf-8")
        req = urllib.request.Request(self.url + "/embed", data=body,
                                     headers={"content-type": "application/json"}, method="POST")
        with urllib.request.urlopen(req, timeout=self.timeout) as resp:
            data = json.loads(resp.read().decode("utf-8"))
        if isinstance(data, dict):   # tolerate OpenAI-style / wrapped response shapes
            data = data.get("embeddings") or data.get("data") or []
            data = [d["embedding"] if isinstance(d, dict) else d for d in data]
        return data

    def embed(self, record_or_text):
        return self.embed_batch([record_or_text])[0]

    def embed_batch(self, items):
        texts = [x if isinstance(x, str) else _record_text(x) for x in items]
        out = []
        for i in range(0, len(texts), self.max_batch):   # chunk to the server's per-request cap
            out.extend(self._post(texts[i:i + self.max_batch]))
        return out


_CACHE = {}


def get_embedder():
    """Return the node's embedder (cached). The seam where a neural backend plugs in: the lexical id
    selects the stdlib embedder (zero-infra default); any other id selects a NeuralEmbedder pointed at
    COMMONS_EMBEDDINGS_URL."""
    name = getattr(settings, "COMMONS_EMBEDDER", "lexical-hashing-v0")
    dim = int(getattr(settings, "COMMONS_EMBEDDING_DIM", _DEFAULT_DIM))
    key = (name, dim)
    if key not in _CACHE:
        if name.startswith("lexical-hashing"):
            _CACHE[key] = LexicalHashingEmbedder(dim=dim)
        else:
            _CACHE[key] = NeuralEmbedder(
                model_id=name, url=getattr(settings, "COMMONS_EMBEDDINGS_URL", ""),
                dim=dim, timeout=float(getattr(settings, "COMMONS_EMBEDDINGS_TIMEOUT", 30)),
                max_batch=int(getattr(settings, "COMMONS_EMBEDDINGS_BATCH", 32)))
    return _CACHE[key]
