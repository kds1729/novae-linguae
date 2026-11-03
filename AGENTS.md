# For agents

This project's target audience is AI agents, so this page is for you. It is ordered by what to
do, not by what the project is. It was written from a fresh agent's first run (2026-08-28: three fresh
agents, no prior context — Sonnet-class twice and Haiku-class once — all three tasks below done
unaided in 90 s to 6 min and 20–36 tool calls) and its
list of what slowed it down — read it once and you will go faster than it did.

## Just the binary (no side effects)

If you have a task of your own, fetch the validator and skip the demo:

    curl -fsSLO https://github.com/kds1729/novae-linguae/releases/latest/download/nl-validator-x86_64-linux
    curl -fsSLO https://github.com/kds1729/novae-linguae/releases/latest/download/nl-validator-x86_64-linux.sha256
    sha256sum -c nl-validator-x86_64-linux.sha256
    chmod +x nl-validator-x86_64-linux

(`nl-validator-aarch64-macos` on Apple silicon; `shasum -a 256 -c` there.) The verified loop also
needs `z3` on the PATH. Then go to *Finding a function for a goal* below.

A verified run (`--verify`) proves the function's contract AND what it rests on in its callees,
with the solver: usually well under a minute (`mod_inverse` over `ext_gcd`: ~20 s from v0.1.15;
3 min before), longer when a contract needs a callee's laws too. It says so on stderr when it
starts certifying. If your shell tool has a short time limit, run it in the background and wait.

## In three sentences

Run `bash quickstart.sh` first — it alone yields a verified answer: two independent `CONFIRMED`
lines (the agent loop, then a third-party re-verification by address). It PUBLISHES that run's
signed claim to the public node (`--publish`) — a demo of the full loop; skip it if you only want
the tool. For a second API, any
description-layer adapter works — `tooling/nl-ingest-graphql/graphql_ingest.py` for a saved
introspection result, `tooling/nl-ingest-openapi/openapi_ingest.py` for an OpenAPI 3 JSON
description, `tooling/nl-ingest-smithy/smithy_ingest.py` for an AWS Smithy model (JSON-RPC
services; needs a signing proxy as its endpoint) — and all find
the binary the quickstart fetched (`.quickstart/nl-validator`) on their own, or a sibling `cargo
build`, or `NL_VALIDATOR`; the OpenAPI adapter moves gate-failed records to `<out>/failed/` and
ends with a `summary:` tally, so `<out-dir>` holds only what passed. To publish, `python3 tooling/commons-node/publish_records.py <out-dir>`
posts bodies, traces and records in dependency order; then `nl-validator orchestrate --node
<node> --verify --require-certified --intent <your record's intent tag> …` discovers, certifies,
applies and publishes in one shot, and anyone can `verify-claim --node <node> msg_…` the result.

## Finding a function for a goal (you have a task, not a tag)

1. `POST https://nl.1105software.com/v0/search?include=summary` with `{"query": "<the goal in
   plain words>", "k": 10}`. **Always pass `?include=summary`**: without it each hit is a bare
   `{hash, score}` and you will fetch every record to tell them apart (a stranger did: 20 fetches).
   Scores are close together (a 0.01 margin is normal); treat the top 3–5 as a shortlist, not the
   top 1 as the answer. Older versions of a function share its slot: they are listed by hash in
   `versions`, and the hit shown is the version with the strongest contract.
2. Choose by the summary's `contract` (pre/post, surface syntax), `laws`, `params` (argument
   names, in order), `param_docs` (what each argument means, and its unit — the author's words)
   and `type_surface`. A function the contract applies is written by name (`name@fn_1234abcd`
   where a bare name would be ambiguous); `refs` maps each full address to that name.
   The contract is what separates candidates with similar names — e.g. `allocate`'s
   `post: list_sum(result) == total` (the parts sum to the total) versus a plain `divide`.
   `certifications` counts signed certificates about the function; whose they are is your
   policy's question (`nl-validator certified`), and `--require-certified` re-certifies locally anyway.
3. Apply it with the verified loop using a tag from its `intent_tags`:
   `nl-validator orchestrate --node <node> --verify --require-certified --intent
   math/decimal/allocate --arg 10000 --arg '[1, 1, 1]'` → `CONFIRMED …` then
   `result: [int(3333), int(3333), int(3334)]`.

Decimal numbers from a user ("7.125", "1.0825") are not floats here — money must be exact. Pass
them as TEXT to a function that takes text (`round_text_half_even`, `fx_convert_text`,
`parse_decimal` in the decimal tier — `--arg '"7.125"'`); its contract states the rounding rule
(within half a unit, a tie to the even) and which numeric function it applies; or scale them yourself to integers (7.125 at 3 places is 7125).

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
2. The adapter README you are about to use: `tooling/nl-ingest-graphql/README.md`,
   `tooling/nl-ingest-openapi/README.md` or `tooling/nl-ingest-smithy/README.md` — the mapping
   table and, more useful, **Honest
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
  `.quickstart/` (the binary, the records, the loop's files) lives inside the clone and is ignored.
- `orchestrate --publish` needs `--seed <string>` (the signer of the published assert); a plain
  apply does not (a fixed default identity signs the transcript). `--arg`/`--expect` take a file,
  inline JSON, or a surface literal — `--arg 10000 --arg '[1, 1, 1]' --arg '"text"'`; a bare
  numeral is a `nat` (accepted where an `int` is expected), `int(-5)` an explicit int. After
  `CONFIRMED`, a `result:` line prints the answer in that same syntax (a non-negative int shows
  as `int(1462)`, a negative one as `-322`). `CONFIRMED` says what was proved: `contract proved
  (3 post(s))` means the certifier proved every postcondition, and ``law `name` proved`` names a
  law. The transcript prints one line per step; `--verbose` adds the candidate scores, the merged
  pairs and every certify check.
- A record's discovery tag is in its `intent_tags` (adapters derive `parse/<lowercase-hyphenated-
  field>`), its argument order in its `signature`. The adapters print the record's file, not these.
- Publishing to a public node is permanent: append-only, world-visible, no delete. `--sign` is what
  lets a consumer rely on YOUR certification instead of re-certifying (which `--require-certified`
  always can, locally).
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
