# Production image for the Novae Linguae commons node (Arca). Multi-stage:
#   1. build the Rust `nl-validator` binary (the byte-for-byte verification gate, commons/verify.py);
#   2. a slim Python image running the Django app under gunicorn, with the binary + schemas baked in.
# The web and worker containers share this image (different commands); see docker-compose.prod.yml.
#
# BUILD CONTEXT IS THE REPO ROOT (novae-linguae/), because the image needs both tooling/ and spec/:
#   docker build -f tooling/commons-node/Dockerfile -t arca-node .

# ---- stage 1: build nl-validator -------------------------------------------------------------------
FROM rust:1-bookworm AS validator
WORKDIR /build
COPY tooling/validator ./validator
# Only the nl-validator binary is needed (verify/validate); building just it skips the other bins.
RUN cargo build --release --manifest-path validator/Cargo.toml --bin nl-validator

# ---- stage 2: the Python web/worker image ----------------------------------------------------------
FROM python:3.12-slim AS app
ENV PYTHONUNBUFFERED=1 PYTHONDONTWRITEBYTECODE=1

# z3 powers the optional /v0/prove service (commons/prove.py); without it every property is NO-SOLVER.
RUN apt-get update && apt-get install -y --no-install-recommends z3 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY tooling/commons-node/requirements.txt ./requirements.txt
RUN pip install --no-cache-dir -r requirements.txt

# App, schemas, and the verification binary. Paths are pinned via env so the node never relies on the
# source-tree REPO_ROOT layout inside the container.
COPY tooling/commons-node ./commons-node
COPY spec ./spec
COPY --from=validator /build/validator/target/release/nl-validator /usr/local/bin/nl-validator

ENV COMMONS_VALIDATOR=/usr/local/bin/nl-validator \
    COMMONS_SPEC_DIR=/app/spec \
    DJANGO_SETTINGS_MODULE=commons_node.settings

WORKDIR /app/commons-node

# Drop privileges.
RUN useradd --system --no-create-home arca && chown -R arca /app
USER arca

EXPOSE 8000
# Default command is the web server; the worker container overrides this (see compose).
CMD ["gunicorn", "commons_node.wsgi:application", "--bind", "0.0.0.0:8000", \
     "--workers", "3", "--threads", "2", "--timeout", "60", "--access-logfile", "-"]
