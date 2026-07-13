"""Django settings for the Novae Linguae commons reference node (MVP).

Local-first by design: SQLite, no external services, no public endpoint. The storage engine is an
implementation detail of this node (spec/commons.md is engine-agnostic) — a production instance can
swap in Postgres/pgvector + Redis without changing the protocol. See the commented Postgres block.
"""

import os
from pathlib import Path

BASE_DIR = Path(__file__).resolve().parent.parent          # tooling/commons-node
REPO_ROOT = BASE_DIR.parent.parent                         # repo root

SECRET_KEY = os.environ.get("COMMONS_SECRET_KEY", "dev-insecure-not-for-production")
DEBUG = os.environ.get("COMMONS_DEBUG", "1") == "1"
ALLOWED_HOSTS = os.environ.get("COMMONS_ALLOWED_HOSTS", "*").split(",")

INSTALLED_APPS = ["commons"]

# Minimal middleware — this is a machine-to-machine JSON API, so no sessions/auth/CSRF stack. The
# egress governor is listed first so its response phase runs last (metering the final, gzip-compressed
# bytes); GZip compresses JSON responses to cut egress (DEPLOYMENT.md).
MIDDLEWARE = [
    "commons.egress.EgressBudgetMiddleware",
    "django.middleware.gzip.GZipMiddleware",
    "django.middleware.common.CommonMiddleware",
]

ROOT_URLCONF = "commons_node.urls"
WSGI_APPLICATION = "commons_node.wsgi.application"

# Backend selection is env-driven so local dev mirrors production (Postgres + pgvector) while the
# zero-dependency SQLite path stays the default when nothing is configured. The protocol is
# engine-agnostic (spec/commons.md) — this is purely a node-local storage choice. Set COMMONS_PG_*
# (or run `docker compose up db`) to use Postgres; pgvector ANN search activates on that backend.
if os.environ.get("COMMONS_PG_NAME") or os.environ.get("COMMONS_PG_HOST"):
    DATABASES = {
        "default": {
            "ENGINE": "django.db.backends.postgresql",
            "NAME": os.environ.get("COMMONS_PG_NAME", "commons"),
            "USER": os.environ.get("COMMONS_PG_USER", "commons"),
            "PASSWORD": os.environ.get("COMMONS_PG_PASSWORD", "commons"),
            "HOST": os.environ.get("COMMONS_PG_HOST", "127.0.0.1"),
            "PORT": os.environ.get("COMMONS_PG_PORT", "5432"),
        }
    }
else:
    DATABASES = {
        "default": {
            "ENGINE": "django.db.backends.sqlite3",
            "NAME": os.environ.get("COMMONS_DB_PATH", str(BASE_DIR / "db.sqlite3")),
        }
    }

# Redis cache when configured (the production hot-cache element); locmem otherwise. A bigger operator
# points COMMONS_REDIS_URL at a managed/clustered Redis — no code change.
if os.environ.get("COMMONS_REDIS_URL"):
    CACHES = {"default": {"BACKEND": "django.core.cache.backends.redis.RedisCache",
                          "LOCATION": os.environ["COMMONS_REDIS_URL"]}}
else:
    CACHES = {"default": {"BACKEND": "django.core.cache.backends.locmem.LocMemCache"}}

DEFAULT_AUTO_FIELD = "django.db.models.BigAutoField"
USE_TZ = True

# --- Commons-specific configuration -----------------------------------------------------------

# Verification reuses the Rust reference validator so this node agrees byte-for-byte with the spec.
COMMONS_VALIDATOR = os.environ.get(
    "COMMONS_VALIDATOR",
    str(REPO_ROOT / "tooling" / "validator" / "target" / "release" / "nl-validator"),
)
COMMONS_SPEC_DIR = os.environ.get("COMMONS_SPEC_DIR", str(REPO_ROOT / "spec"))

# Binary blobs (spec/weights.md): opaque bytes served by content hash at /v0/blobs/{sha256}, OUTSIDE
# the record store and its gate — a weights record's files[].sha256 is what makes any host safe to
# fetch from (the client verifies after download). `manage.py addblob <file>` stores a file here under
# its sha256. In production a static server/CDN can front or replace the Django view.
COMMONS_BLOB_DIR = os.environ.get("COMMONS_BLOB_DIR", str(BASE_DIR / "blobs"))

# Optional proof service (/v0/prove): runs `nl-validator prove` over a record's forall properties[]
# with an SMT solver. Node-local and best-effort (like search), never an admission gate; it needs a
# solver on PATH or every property reports NO-SOLVER. The timeout caps the (occasionally solver-heavy)
# request, and the property cap bounds work per call.
COMMONS_SOLVER = os.environ.get("COMMONS_SOLVER", "z3")
COMMONS_PROVE_TIMEOUT = float(os.environ.get("COMMONS_PROVE_TIMEOUT", "60"))  # wall-clock seconds
COMMONS_PROVE_MAX_PROPERTIES = int(os.environ.get("COMMONS_PROVE_MAX_PROPERTIES", "32"))

# Local mirroring policy (principle-7-permitted endpoint choices, never protocol gates).
COMMONS_MAX_RECORD_BYTES = int(os.environ.get("COMMONS_MAX_RECORD_BYTES", str(1 << 20)))  # 1 MiB
# Body storage tiering (spec/commons.md, open question 4): a BARE BODY larger than the record cap
# is still admitted — verified as always, then stored in the blob store with only a thin pointer
# row in the metadata index — up to this ceiling. Bodies are pure self-addressing content (no
# signature, no queryable metadata), which is what makes the bigger ceiling safe.
COMMONS_MAX_BODY_BYTES = int(os.environ.get("COMMONS_MAX_BODY_BYTES", str(8 << 20)))  # 8 MiB
COMMONS_PEERS = [p for p in os.environ.get("COMMONS_PEERS", "").split(",") if p.strip()]

# Semantic search (spec/commons.md `POST /v0/search`). The reference node ships a stdlib-only,
# deterministic lexical embedder; this is the seam where a neural backend would be selected. The
# active model id is advertised in /v0/info and in every search response. See commons/embedding.py.
COMMONS_EMBEDDER = os.environ.get("COMMONS_EMBEDDER", "lexical-hashing-v0")
COMMONS_EMBEDDING_DIM = int(os.environ.get("COMMONS_EMBEDDING_DIM", "256"))
# Model server for a neural embedder (e.g. HF Text-Embeddings-Inference, or any /embed-compatible
# endpoint — a GPU fleet looks identical here). Used only when COMMONS_EMBEDDER is not the lexical id.
COMMONS_EMBEDDINGS_URL = os.environ.get("COMMONS_EMBEDDINGS_URL", "")
COMMONS_EMBEDDINGS_TIMEOUT = float(os.environ.get("COMMONS_EMBEDDINGS_TIMEOUT", "30"))
COMMONS_EMBEDDINGS_BATCH = int(os.environ.get("COMMONS_EMBEDDINGS_BATCH", "32"))  # inputs per request

# Egress-budget governor (commons/egress.py, DEPLOYMENT.md): a monthly ceiling on outbound bytes — the
# only variable cost — past which the node returns 503. 0 disables throttling (usage still metered and
# shown in /v0/info). A hard cap on the bill, not a protocol gate.
COMMONS_EGRESS_BUDGET_BYTES = int(os.environ.get("COMMONS_EGRESS_BUDGET_BYTES", "0"))

# Replication worker (commons/tasks.py): the Celery app mirrors verified records from COMMONS_PEERS.
# Broker defaults to the configured Redis. Off (no worker running) leaves the node a pure origin.
COMMONS_CELERY_BROKER = os.environ.get("COMMONS_CELERY_BROKER", os.environ.get("COMMONS_REDIS_URL", ""))
COMMONS_REPLICATE_INTERVAL = float(os.environ.get("COMMONS_REPLICATE_INTERVAL", "300"))  # seconds
COMMONS_EMBED_INTERVAL = float(os.environ.get("COMMONS_EMBED_INTERVAL", "120"))  # backfill null embeddings
COMMONS_REPLICATE_BATCH = int(os.environ.get("COMMONS_REPLICATE_BATCH", "200"))  # records per peer per run
COMMONS_SYNC_PER_PEER_LIMIT = int(os.environ.get("COMMONS_SYNC_PER_PEER_LIMIT", "500"))  # /v0/sync page
# Blobs the mirrored records reference (examples[].result_blob, weights files[]) are pulled from the
# peer's gate-free /v0/blobs store, sha256-verified, up to this many per peer per run. A blob can be
# hundreds of MB (adapter weights), so the per-run bound is small; the scan is self-healing (no
# cursor — anything still missing is re-counted next run).
COMMONS_REPLICATE_BLOB_BATCH = int(os.environ.get("COMMONS_REPLICATE_BLOB_BATCH", "16"))
