# For agents

This project's target audience is AI agents, so this page is for you. It is ordered by what to
do, not by what the project is. It was written from a fresh agent's first run (2026-08-28: three fresh
agents, no prior context — Sonnet-class twice and Haiku-class once — all three tasks below done
unaided in 90 s to 6 min and 20–36 tool calls) and its
list of what slowed it down — read it once and you will go faster than it did.

## In three sentences

Run `bash quickstart.sh` first — it alone yields a verified answer: two independent `CONFIRMED`
lines (the agent loop, then a third-party re-verification by address). For a second API, either
adapter works — `tooling/nl-ingest-graphql/graphql_ingest.py` for a saved introspection result,
`tooling/nl-ingest-openapi/openapi_ingest.py` for an OpenAPI 3 JSON description — and both find
the binary the quickstart fetched (`.quickstart/nl-validator`) on their own, or a sibling `cargo
build`, or `NL_VALIDATOR`. To publish, `python3 tooling/commons-node/publish_records.py <out-dir>`
posts bodies, traces and records in dependency order; then `nl-validator orchestrate --node
<node> --verify --require-certified --intent <your record's intent tag> …` discovers, certifies,
applies and publishes in one shot, and anyone can `verify-claim --node <node> msg_…` the result.

## What things are

| you see | it is |
|---|---|
| `fn_…`, `expr_…`, `trc_…`, `msg_…`, `cert_…`, `wgt_…`, `evl_…`, `plan_…` | content addresses: BLAKE3 over the canonical (JCS) form, prefixed by kind. Immutable; the address is the identity; a node is never trusted, the hash is checked locally |
| a **function record** (`*.v0.2.json`) | signature (type, effects, refinements), worked examples, intent tags, `body_hash` → the body. Schema: `spec/function-record.v0.2.schema.json` |
| a **body** (`body-*.json`) | the program, as a JSON AST; `nl-validator unparse-body` prints the surface syntax; `spec/surface-syntax.md` |
| a **trace** (`trace-*.json`) | a recorded effect (one HTTP call: request, response) — a record's worked example carries its address, so `nl-validator run` replays it offline with no network and no secrets |
| `certify` | schema → typecheck → effects → termination → complexity in one pass; `--sign <seed>` makes a signed `cert_…` others can check |
| an **intent tag** | discovery key (`parse/country-capital`); `spec/intent-tag-vocabulary.md`; adapters derive one per record |
| a **grant** (`--grant net.read@host`) | the only effects code may perform when applied; default none — pure only |
| `Maybe string` results from an adapter | the description promised a shape, the observation supplied the value; absence is `None`/`JNull`, never an error |

## What to read, in order, if you need more

1. `QUICKSTART.md` — the four steps and why each is there (5 minutes).
2. The adapter README you are about to use: `tooling/nl-ingest-graphql/README.md` or
   `tooling/nl-ingest-openapi/README.md` — the mapping table and, more useful, **Honest
   refusals**: what the adapter declines to compile and why. The refusal list is the design.
3. `tooling/commons-node/README.md` — the node API (`/v0/records`, `/v0/query`, `/v0/blobs`).
4. `spec/agent-loop.md` — the query → propose → commit → assert → verify protocol you drove
   with `orchestrate`, and its honest scope (a trace is the publisher's testimony).
5. `README.md` — the manifesto, principles and status. Long; read it last, not first.
6. `evolution/` — three real APIs measured end to end, with what did not work. Read the one
   closest to the API you are compiling.

## Things that cost a stranger time (fixed, but know them)

- The adapters find the binary in this order: sibling `cargo build`, then the one `quickstart.sh`
  fetched into `.quickstart/`, then `NL_VALIDATOR`. After the quickstart you need no env var.
- `POST /v0/records` answers `201 {stored:true}` for new, `200 {stored:false}` for already
  held — idempotent, not a rejection. Rejections are `4xx {error}`.
- `nl-validator certify <record>` needs `--body <body> --records <dir>`; the adapters print
  `certify=OK` without showing that command. `publish_records.py --sign` runs it for you.
- OpenAPI descriptions without `operationId` (many generated ones) get `<verb>_<path>` names.
- Output files are named by the LOWERCASED record name: `countryCapital` → `countrycapital.v0.2.json`
  and `body-countrycapital.json` (the adapters print the file next to each record).
- `certify` on an effectful record prints `termination UNVERIFIABLE` / `complexity UNVERIFIABLE`
  ("applies an opaque callee `http`") and still ends `=> CERTIFIED`. Expected: the declared
  `always` / `O(n)` cannot be *proven* through an HTTP call, so they stay the author's declaration;
  schema, types and effects are the proven rows. A refusal looks different: `=> NOT CERTIFIED`.
- A list-valued (or input-object) argument at observation time is JSON text:
  `--observe-arg 'charactersByIds.ids=["1","2"]'`.
- By-address example values (`blob-*.json`, above 64 KiB) do not go through `/v0/records`; they
  belong in a node's blob store (`manage.py addblob` on the node). The publish script tells you.

## Do not

- Splice caller data into a URL or JSON by string concatenation — the adapters never do
  (`url_encode`; JSON as a `Json` value through `render_json`), and a record that does will
  not be one a consumer should trust.
- Grant more than the host you are calling. `--grant net.read` unscoped means any host.
- Treat Arca (`https://nl.1105software.com`) as infrastructure: it is one small node with a
  budget and no SLA. Everything it serves is self-verifying; run your own (`tooling/commons-node`,
  `docker compose up`) for anything that matters.
