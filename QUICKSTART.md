# Novae Linguae in ten minutes

You need `python3` (3.10+), `curl`, and a network. Nothing else — no Rust, no venv, no account.

```bash
git clone https://github.com/kds1729/novae-linguae
cd novae-linguae
bash quickstart.sh
```

That script does four things, each printing what it did. Here is what you are looking at.

## 1. `nl-validator`

The reference implementation is one binary (Linux x86_64, fully static, and Apple-silicon macOS,
from [Releases](https://github.com/kds1729/novae-linguae/releases) — CI-built, full test suite run
against each, checksum-verified by the script; anything else: `cd tooling/validator; cargo build
--release` with stable Rust — the script finds that build). It canonicalizes, hashes, signs, type-checks, proves, certifies, runs,
and replays every artifact the project defines. Everything below is that one binary plus a
stdlib-only Python script, and everything the script writes — the binary, the records, the loop's
files — goes into `.quickstart/` inside the clone (ignored by git); the binary itself is
`.quickstart/nl-validator`.

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
observation is a content-addressed trace (`trc_…`), attached to the example. To do that by hand on any record: `nl-validator certify records/<name>.v0.2.json
--body records/body-<name>.json --records records`. Which is why the last
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
  is the query; follow redirects — `curl -L`) and run the adapter against it; or an OpenAPI 3
  description through `tooling/nl-ingest-openapi/openapi_ingest.py`. The adapter tells you, loudly,
  what it refuses and why — that list is the interesting part. A field with several optional
  arguments (`person(id, personID)`) is observed with ONE of them (`--observe-arg person.id=…`);
  the server's own error comes back verbatim, with the arguments the adapter sent. Each record's
  discovery tag is `parse/<lowercase-hyphenated-field>` (`countryCapital` → `parse/country-capital`)
  and its argument order is the record's `signature` (base URL first, then the observed arguments)
  — both are in the `.v0.2.json` the adapter prints next to each record, and each adapter's
  per-record line ends with `intent=<tag>`. For OpenAPI: `--verify-against` is the examples' base
  — every documented example runs against it, mutating verbs included, so aim it at a sandbox
  (the public Petstore answers 500 to its own documented writes); the description's own server
  url is not called, and a relative one (`/api/v3`) is joined onto the base you give; gate-failed
  records move to `<out>/failed/` and the closing `summary:` line counts what is left to publish.
  To try the adapter with nothing at stake, run the in-repo fake service as the sandbox
  (`python3 tooling/fake-service/fake_service.py --port 18878`, its README has the endpoints) and
  compile `tooling/nl-ingest-openapi/examples/item-store.openapi.json` against it with
  `--secret-name api_token --verify-against http://127.0.0.1:18878 --token test-token`.
  Orchestrating a REST record the adapter made looks like `nl-validator orchestrate --node <node>
  --verify --require-certified --intent query/lookup/get-item-status --arg base.json --arg
  name.json --grant net.read@127.0.0.1 --secret api_token=test-token --seed <seed> --publish`:
  the arguments follow the record's `signature` (the base URL first), a record behind bearer or
  api-key auth needs `--secret <scheme>=<value>` (the scheme name is the adapter's
  `--secret-name`, default the description's scheme key), and the grant's host is the base's.
- **Publish and be verified.** `python3 tooling/commons-node/publish_records.py records/` posts
  your bodies, traces and records to a node's `/v0/records` in the right order (Arca's gate
  accepts any well-formed artifact; `201 stored` is new, `200 stored:false` means it already had
  it — publishing is idempotent). It is also **permanent**: a commons node is append-only and
  public, so what you publish stays, under its address, for anyone to fetch — `quickstart.sh`'s
  own last step publishes its claim the same way. Publish what you would stand behind. Add `--sign <seed>` to certify and publish certifications too:
  without one, every consumer re-certifies your record itself (`--require-certified` does, locally);
  with one, a consumer whose policy trusts your certifier can rely on your signed `cert_…` instead.
  **A public node is append-only and world-visible: what you publish stays, under its address,
  with no delete.** Try a throwaway set against your own node first if that matters. Then
  `orchestrate --node … --verify --require-certified --intent <your record's tag> --arg <base.json>
  --arg <id.json> --grant net.read@<host> --seed <any string> --publish` — each `--arg` is a file
  holding a value expression (`{"kind":"string","value":"https://…"}`; `quickstart.sh` shows the
  exact lines) and `--seed` is the identity that signs the published assert — and `verify-claim`
  the resulting assert from another machine — the stranger's check. Or run your own node:
  `tooling/commons-node/` is a `docker compose up`.
- **Read the design.** [README.md](README.md) is the manifesto and status; `spec/` is normative;
  `evolution/` is evidence — three real APIs, with the numbers and the parts that did not work.

Arca is one small node on a personal budget: rate-limited, no SLA, may throttle or disappear.
Everything it serves is content-addressed and self-verifying, so nothing you did above depended
on trusting it.
