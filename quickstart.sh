#!/usr/bin/env bash
# Novae Linguae in ten minutes — see QUICKSTART.md for what each step means.
#
# What this does, end to end, on a machine with only python3 and curl:
#   1. fetches a prebuilt `nl-validator` (or uses one you built);
#   2. compiles a REAL public GraphQL API (countries.trevorblades.com) into verified function
#      records with the description-layer adapter — every record certified and its worked
#      example a recorded, offline-replayable observation;
#   3. asks the live commons node (Arca) for a function by intent and applies it under a
#      host-scoped grant — the verified agent loop, ending in a CONFIRMED claim;
#   4. re-verifies that claim as a third party: by address only, no grants, no secrets.
#
#   bash quickstart.sh            # everything
#   NL_VALIDATOR=/path/to/nl-validator bash quickstart.sh   # use your own build
set -euo pipefail

REPO="$(cd "$(dirname "$0")" && pwd)"
WORK="${NL_QUICKSTART_DIR:-$REPO/.quickstart}"
NODE="${NL_NODE:-https://nl.1105software.com}"
API="https://countries.trevorblades.com/graphql"
RELEASE="${NL_RELEASE:-https://github.com/kds1729/novae-linguae/releases/latest/download}"
mkdir -p "$WORK"
cd "$WORK"

say() { printf '\n\033[1m== %s\033[0m\n' "$*"; }

# ---- 1. the validator ------------------------------------------------------------------------
say "1/4  nl-validator"
if [ -n "${NL_VALIDATOR:-}" ]; then
  V="$NL_VALIDATOR"
elif [ -x "$REPO/tooling/validator/target/release/nl-validator" ]; then
  V="$REPO/tooling/validator/target/release/nl-validator"
else
  case "$(uname -s)-$(uname -m)" in
    Linux-x86_64)   ASSET=nl-validator-x86_64-linux ;;
    Darwin-arm64)   ASSET=nl-validator-aarch64-macos ;;
    *) echo "no prebuilt binary for $(uname -s)-$(uname -m); build one: cd tooling/validator && cargo build --release"; exit 1 ;;
  esac
  if [ ! -x "$WORK/nl-validator" ]; then
    echo "downloading $RELEASE/$ASSET"
    curl -fsSL -o nl-validator "$RELEASE/$ASSET"
    curl -fsSL -o nl-validator.sha256 "$RELEASE/$ASSET.sha256"
    (cd "$WORK" && sed "s/$ASSET/nl-validator/" nl-validator.sha256 | (sha256sum -c - 2>/dev/null || shasum -a 256 -c -))
    chmod +x nl-validator
  fi
  V="$WORK/nl-validator"
fi
"$V" --version
export NL_VALIDATOR="$V"

# ---- 2. compile a real API into verified records ---------------------------------------------
say "2/4  compile countries.trevorblades.com (GraphQL) into verified records"
if [ ! -s countries.introspection.json ]; then
  curl -fsS -X POST -H 'Content-Type: application/json' \
    -d @"$REPO/evolution/graphql-poc/repro/introspection_query.json" "$API" > countries.introspection.json
fi
rm -rf records
python3 "$REPO/tooling/nl-ingest-graphql/graphql_ingest.py" countries.introspection.json --out records \
  --verify-against "$API" --observe-arg country.code=DE --observe-arg continent.code=EU \
  --observe-arg language.code=de | grep -E 'observation-gate|summary'
echo
echo "the countryCapital record, as the language spells it:"
"$V" unparse-body records/body-countrycapital.json | cut -c1-160; echo "…"
echo "its worked example is an OBSERVATION: trace $(python3 -c "import json;print(json.load(open('records/countrycapital.v0.2.json'))['examples'][0]['trace'][:24])")…"
echo "replaying it offline, with no network and no secrets:"
"$V" run records/countrycapital.v0.2.json --records records | tail -1

# ---- 3. the verified agent loop against the live commons ---------------------------------------
say "3/4  ask the live commons for 'the capital of a country', verified, under a host-scoped grant"
printf '{"kind":"string","value":"%s"}' "$API" > arg-base.json
printf '{"kind":"string","value":"DE"}' > arg-de.json
printf '{"kind":"variant","tag":"Just","payload":{"kind":"string","value":"Berlin"}}' > expect-berlin.json
"$V" orchestrate --node "$NODE" --verify --require-certified --intent parse/country-capital \
  --arg arg-base.json --arg arg-de.json --expect expect-berlin.json \
  --grant net.read@countries.trevorblades.com --seed "quickstart-$(hostname)-$$" --publish \
  | grep -E 'query|ack|certify|assert|CONFIRMED|published' | tee loop.out
MSG=$(grep -o 'msg_[0-9a-f]\{64\}' loop.out | tail -1)

# ---- 4. third-party verification: an address and a node URL, nothing else ---------------------
say "4/4  verify that claim as a stranger: by address, no grants, no secrets"
"$V" verify-claim --node "$NODE" "$MSG" | tail -1
echo
echo "done. records in $WORK/records; the claim you just verified is $MSG"
