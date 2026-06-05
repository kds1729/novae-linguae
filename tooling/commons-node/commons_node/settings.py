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

# Minimal middleware — this is a machine-to-machine JSON API, so no sessions/auth/CSRF stack.
MIDDLEWARE = ["django.middleware.common.CommonMiddleware"]

ROOT_URLCONF = "commons_node.urls"
WSGI_APPLICATION = "commons_node.wsgi.application"

DATABASES = {
    "default": {
        "ENGINE": "django.db.backends.sqlite3",
        "NAME": os.environ.get("COMMONS_DB_PATH", str(BASE_DIR / "db.sqlite3")),
    }
}
# Production swap (later): set these env vars and `pip install psycopg pgvector`.
#   DATABASES["default"] = {
#       "ENGINE": "django.db.backends.postgresql",
#       "NAME": os.environ["COMMONS_PG_NAME"], "USER": ..., "PASSWORD": ..., "HOST": ..., "PORT": ...,
#   }

DEFAULT_AUTO_FIELD = "django.db.models.BigAutoField"
USE_TZ = True

# --- Commons-specific configuration -----------------------------------------------------------

# Verification reuses the Rust reference validator so this node agrees byte-for-byte with the spec.
COMMONS_VALIDATOR = os.environ.get(
    "COMMONS_VALIDATOR",
    str(REPO_ROOT / "tooling" / "validator" / "target" / "release" / "nl-validator"),
)
COMMONS_SPEC_DIR = os.environ.get("COMMONS_SPEC_DIR", str(REPO_ROOT / "spec"))

# Local mirroring policy (principle-7-permitted endpoint choices, never protocol gates).
COMMONS_MAX_RECORD_BYTES = int(os.environ.get("COMMONS_MAX_RECORD_BYTES", str(1 << 20)))  # 1 MiB
COMMONS_PEERS = [p for p in os.environ.get("COMMONS_PEERS", "").split(",") if p.strip()]
