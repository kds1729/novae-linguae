#!/usr/bin/env bash
# Hermetic reproduction of the GraphQL observation gate: no network, no credentials, no vendor.
# Starts the in-repo fake service on a private port, ingests its own introspection result on
# both transports, and leaves every record certified + offline-replayed in $OUT.
#
#   bash evolution/graphql-poc/repro/run_hermetic.sh [port]
#
# Expected (GET, with --auth-bearer): 10 records materialized of 10 licensed from 4 live calls.
# Expected (POST, no auth):            8 records materialized of 10 licensed from 4 live calls;
#                                      `secret` fails 401 and `secretValue` inherits the verdict.
set -u
ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
PORT="${1:-18895}"
OUT="${OUT:-$(mktemp -d /tmp/nl-graphql-repro-XXXXXX)}"
python3 "$ROOT/tooling/fake-service/fake_service.py" --port "$PORT" &
SVC=$!
trap 'kill $SVC 2>/dev/null' EXIT
for _ in $(seq 50); do curl -sf "http://127.0.0.1:$PORT/health" >/dev/null 2>&1 && break; sleep 0.1; done
SCHEMA="$ROOT/tooling/nl-ingest-graphql/examples/item-store.graphql.json"
echo "== GET transport, bearer bound"
python3 "$ROOT/tooling/nl-ingest-graphql/graphql_ingest.py" "$SCHEMA" --out "$OUT/get" \
    --verify-against "http://127.0.0.1:$PORT/graphql" --observe-arg item.name=gw18-widget \
    --auth-bearer api_token --token test-token | grep -E "observation-gate|summary"
echo "== POST transport, no auth (the auth-only field must fail honestly)"
python3 "$ROOT/tooling/nl-ingest-graphql/graphql_ingest.py" "$SCHEMA" --out "$OUT/post" \
    --transport post --verify-against "http://127.0.0.1:$PORT/graphql" --observe-arg item.name=nope \
    | grep -E "observation-gate|summary"
echo "records in $OUT"
