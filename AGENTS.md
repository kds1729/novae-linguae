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

(`nl-validator-aarch64-macos` on Apple silicon; `shasum -a 256 -c` there.) The `.sha256` file
names the binary by its bare file name, so `-c` must run in the directory you downloaded into, on
the un-renamed file. If your shell cannot `cd` there (one command per call), let `env` do it:
`env -C <download-dir> sha256sum -c nl-validator-x86_64-linux.sha256` (GNU coreutils) prints
`nl-validator-x86_64-linux: OK`. The verified loop also needs **`z3`
on the PATH** (any 4.13+; `apt install z3`, `brew install z3`). Then go to *Finding a function
for a goal* below.

A verified run (`--verify`) proves the function's contract AND what it rests on in its callees,
with the solver: seconds to a few minutes (`isqrt` ~2 s, `mod_pow` ~5 s, `mod_inverse` ~20 s,
`crt2` ~40 s, `round_text_half_even` ~1 min, `ext_gcd` ~2 min), longer when a contract needs a
callee's laws too. On stderr
it prints ` certify  <name> …` when it starts, then one line per callee contract it derives
(`callee  gcd: 2 post(s) sound, laws 4 of 5 proved (28.0 s)`) and one per law attempt
(`law  proved     3.2 s  forall …` / `law  unproved  20.0 s  forall …` — an unproved law is
normal and harmless, see below), and after 30 s with no
other line `working  still working — 95 s so far (not hung)` — so a stderr line at least every
30 s means it is working. If your shell tool has a short time limit, run it in
the background and read its output file. A law the solver cannot close is reported as "not proved
here" — it does not fail the run unless you pass `--require-laws`. Each law ATTEMPT in a verified
run is capped at 20 s (`NL_LAW_BUDGET_SECS`; `0` = no cap, every law tried to the end — slower,
may prove more): the certification itself never depends on the cap (a contract the capped pass
cannot decide is re-proved without it), only which laws are reported proved. The FIRST run over
a set of callees is the slow one — ~3 min for a rational function over `rat_normalize` (measured
on v0.1.33: `rat_add` 162 s, most of it `rat_normalize`'s laws, attempted in parallel), ~1.5 min
for `lcm`, a few seconds for `binomial`; the `CONFIRMED` line ends with the run's wall time.
**Run verified runs ONE AT A TIME**: the law caps are wall-clock, so two runs sharing the CPU each
take longer and may report fewer laws proved, and neither reuses the other's proofs. For several
goals over the same callees, run them in sequence: the second reuses the first's (below). A verified run
keeps every callee contract it derived in `~/.cache/nl-validator/derived` (each entry a proof made on
your machine, keyed by content address and validator version), so the NEXT run over the same callees
takes seconds (measured on v0.1.25: `rat_sub` 126 s, then `rat_mul` 3.5 s; `lcm` 93 s; `fib(100)`
9 s). `NL_DERIVED_CACHE=0` turns that off; a path puts it elsewhere. Over a node the run also
reads each function's certifications for per-law HINTS (which proof rung worked for each law at
the certifier's, and which laws its full derivation could not prove) and prints a `hints  <name>: …`
line per function: the capped law attempts go straight to the rung that works and skip the ones
that never do. Hints only choose which attempts run — every law reported proved, your run proved;
`NL_LAW_HINTS=0` turns them off.

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
   plain words>", "k": 10, "filter": {"kind": "function-record"}}`. **Always pass
   `?include=summary`**: without it each hit is a bare `{hash, score}` and you will fetch every
   record to tell them apart (a stranger did: 20 fetches). **Pass the `kind` filter**: the commons
   also holds claim and assertion messages (`msg_…`), and without it they can fill every slot.
   The response is `{"results": [hit, …]}`, best first. A hit has `contract` only if the function
   declares a pre/post and `laws` only if it declares laws — a missing key means none, not an error.
   Scores are close together (a 0.01 margin is normal); treat the top 3–5 as a shortlist, not the
   top 1 as the answer. Older versions of a function share its slot: they are listed by hash in
   `versions`, and the hit shown is the version with the strongest contract.
2. Choose by the summary's `name`, `contract` (pre/post, surface syntax), `laws`, `params` (argument
   names, in order), `param_docs` (what each argument means, and its unit — the author's words)
   and `type_surface`. **Pass arguments in `params` order and read `param_docs` for which is
   which** — e.g. `digit_sum`'s `params` are `["b", "n"]`, the BASE first (`--arg 10 --arg <n>`;
   the other order answers a different question). A function the contract applies is written by name (`name@fn_1234abcd`
   where a bare name would be ambiguous); `refs` maps each full address to that name.
   The contract is what separates candidates with similar names — e.g. `allocate`'s
   `post: list_sum(result) == total` (the parts sum to the total) versus a plain `divide`.
   Read `complexity` before applying to a large argument: `binomial_pascal` is `O(2^n)` and does
   not finish on (60, 30); `binomial` is `O(n)`. A summary with NO `complexity` and `terminates`
   other than `always` (the early seed `fib`, naive recursion) promises nothing about running time —
   treat it as possibly exponential; the loop says so before applying it. A `pre` bounds where a
   function promises anything at all (`is_prime`: `n <= 9223372036854775807`); the loop refuses
   arguments that falsify it, naming the pre.
   `certifications` counts signed certificates about the function; whose they are is your
   policy's question (`nl-validator certified`), and `--require-certified` re-certifies locally anyway.
3. Apply it with the verified loop using a tag from its `intent_tags`:
   `nl-validator orchestrate --node <node> --verify --require-certified --intent
   math/decimal/allocate --arg 10000 --arg '[1, 1, 1]'` → `CONFIRMED …` then
   `result: [int(3333), int(3333), int(3334)]`, then what is proved about that result:

       proved about this result (read them: a bound is not the value):
         post  length(result) == length(weights)
         post  list_sum(result) == total

   **How much to trust the answer is in that block, not in the word CONFIRMED.** CONFIRMED always
   means: fetched and hash-checked, certified (types, effects, termination and cost checked; a
   declared post it could not prove shows as `contract 1 of 2 post(s) proved` and is absent from
   the block), applied, and a re-run agreed. Whether the VALUE is pinned down depends
   on what the proved posts and laws say — `div_half_even`'s posts determine its answer;
   `mod_pow`'s only says `0 <= result < m` (a bound: the exponentiation itself rests on the body);
   `is_prime` declares none, and the block says `proved: nothing about this result beyond
   certification and an agreeing re-run`. When it matters, read the body
   (`nl-validator unparse-body`) or prefer a candidate whose contract states the answer.

Decimal numbers from a user ("7.125", "1.0825") are not floats here — money must be exact. Pass
them as TEXT to a function that takes text (`round_text_half_even`, `fx_convert_text`,
`parse_decimal` in the decimal tier — `--arg '"7.125"'`); its contract states the rounding rule
(within half a unit, a tie to the even) and which numeric function it applies; or scale them yourself to integers (7.125 at 3 places is 7125).
The integer rounding functions (`div_half_even`, `div_half_up`, `div_floor`, `div_ceil`,
`div_trunc`) round the FRACTION n/d — the dividend first, then the divisor (≥ 1): to round −3.5
ties-to-even, apply `div_half_even` to `--arg -35 --arg 10` (→ `-4`); `round_text_half_even` on
`"-3.5"` with 0 places is the same rule on text (→ `Just(-4)`; `None` for text that is not a decimal).
The decimal tier's MONEY rule is round half to EVEN (banker's rounding): `rescale`, `apply_rate`,
`percent_bps` and the `*_text` functions all use it; `div_half_up` and friends exist for other rules,
named by their suffix. Results are MINOR UNITS at the places asked for: `round_text_half_even "0.125" 2`
→ `Just(12)` means 0.12.

End to end, "7.25% of $1,234.56": the amount in cents is 123456 and the rate in basis points is
725 (1 bp = 0.01%), so
`nl-validator orchestrate --node <node> --verify --require-certified --intent
math/decimal/percent-bps --arg 123456 --arg 725` → `CONFIRMED …`, `result: 8951`, so $89.51 (the
exact value is 8950.56 cents). The proved block says how it rounded:

    post  2 * abs(amount * basis_points - result * 10000) <= 10000
    post  2 * abs(amount * basis_points - result * 10000) != 10000 || result % 2 == 0

That is, the result is within half a cent of amount × bps / 10000, and an exact tie goes to the even
cent. `apply_rate 123456 725 10000` gives the same answer for a rate with any denominator. Scale the
result back yourself: minor units ÷ 100 for cents. A percentage that is not whole basis points
(8.875% sales tax on $59.99) goes in as TEXT: `--intent math/decimal/percent-of-text --arg '"59.99"'
--arg '"8.875"' --arg 2` → `Just(532)`, $5.32, the same rounding rule stated over the parsed digits.

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
  numeral is a `nat` (accepted where an `int` is expected), `-5` or `int(-5)` an int (`--arg -22`
works as written). After
  `CONFIRMED`, a `result:` line prints the answer in that same syntax (a non-negative int shows
  as `int(1462)`, a negative one as `-322`). `CONFIRMED` says what was proved: `contract proved
  (3 post(s))` means the certifier proved every postcondition, and ``laws `a`, `b` proved`` names
  every law the loop proved (one it could not is named as ``not proved here``). The run is
  decided by certification, the contract and the re-run result; laws are reported, and
  `--require-laws` makes an unproved one fail the run (`NOT-PROVEN`). The transcript prints one line per step; `--verbose` adds the candidate scores, the merged
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
