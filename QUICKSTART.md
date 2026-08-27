# Novae Linguae in ten minutes

You need `python3` (3.10+), `curl`, and a network. Nothing else — no Rust, no venv, no account.

```bash
git clone https://github.com/kds1729/novae-linguae
cd novae-linguae
bash quickstart.sh
```

That script does four things, each printing what it did. Here is what you are looking at.

## 1. `nl-validator`

The reference implementation is one static binary (Linux x86_64 or Apple-silicon macOS, from
[Releases](https://github.com/kds1729/novae-linguae/releases); anything else: `cd tooling/validator
&& cargo build --release`). It canonicalizes, hashes, signs, type-checks, proves, certifies, runs,
and replays every artifact the project defines. Everything below is that one binary plus a
stdlib-only Python script.

## 2. A real API becomes verified functions

```
python3 tooling/nl-ingest-graphql/graphql_ingest.py countries.introspection.json --out records \
    --verify-against https://countries.trevorblades.com/graphql --observe-arg country.code=DE …
```

The script fetched the public Countries GraphQL API's own description (its introspection schema)
and **compiled** it: one function record per root field, plus one per typed leaf — 21 records.
Nobody wrote them. Each is a *Nova Lingua* program (`records/body-*.json`; the script prints one in
the surface syntax) that builds the request as a value — caller data ride as a JSON *value*
serialized by `render_json`, never spliced into a string — and narrows the response by pattern.

Then the gate ran: each record was **certified** (`nl-validator certify` — schema, types, effects,
termination, complexity) and its worked example was **observed** once against the live API. The
observation is a content-addressed trace (`trc_…`), attached to the example. Which is why the last
line of step 2 works: `nl-validator run` re-checks the example **offline** — no network, no
credentials — by replaying the trace. A record's evidence travels with it.

Two things you may notice. Ten `country*` records share one trace: their requests are
byte-identical, so the API was called once and the siblings replayed it. And `countryCapital`
returns `Maybe string`, not `string`: the description promises a shape, the observation supplies
the value, and absence (`country(code:"ZZ")` → `null`) is a value, not an error.

## 3. The agent loop, against a live commons

```
nl-validator orchestrate --node https://nl.1105software.com --verify --require-certified \
    --intent parse/country-capital --arg … --grant net.read@countries.trevorblades.com --publish
```

Arca is a public commons node holding records like the ones you just made (these very ones, in
fact — published earlier). The orchestrator asked it, **by intent tag**, for a function that
projects a country's capital; got back one content address; fetched the record and its body and
**hash-verified them locally** (the node is not trusted); **certified** it; applied it to
`("https://countries.trevorblades.com/graphql", "DE")` under a grant scoped to that one host —
the only effect the code may perform — got `Just "Berlin"`, and re-verified. **CONFIRMED.** The
recorded observation and a signed `assert` claiming the result were published back to the node.

## 4. A stranger checks the claim

```
nl-validator verify-claim --node https://nl.1105software.com msg_…
```

This is you, as a third party: an address and a node URL, **no grants, no secrets, no network
call to the API**. The claim's function is fetched by hash, the recorded trace replayed, the
assert re-run. **CONFIRMED** — the claim follows from the publisher's recorded evidence (which is
testimony; the trace is theirs — that is the honest scope, and it is stated).

## What to try next

- **Your own API.** Save its introspection result (`evolution/graphql-poc/repro/introspection_query.json`
  is the query) and run the adapter against it; or an OpenAPI 3 description through
  `tooling/nl-ingest-openapi/openapi_ingest.py`. The adapter tells you, loudly, what it refuses
  and why — that list is the interesting part.
- **Publish and be verified.** `POST` your records, bodies and traces to a node's `/v0/records`
  (Arca's gate accepts any well-formed artifact; `evolution/graphql-poc` shows a publish script),
  then `verify-claim` your own assert from another machine. Or run your own node:
  `tooling/commons-node/` is a `docker compose up`.
- **Read the design.** [README.md](README.md) is the manifesto and status; `spec/` is normative;
  `evolution/` is evidence — three real APIs, with the numbers and the parts that did not work.

Arca is one small node on a personal budget: rate-limited, no SLA, may throttle or disappear.
Everything it serves is content-addressed and self-verifying, so nothing you did above depended
on trusting it.
