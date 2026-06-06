# nl-bundle — package records into a portable `.nlb` commons bundle

`nl-bundle` turns Nova Lingua records (JSONL) into a `.nlb` bundle — the portable, self-verifying
archive defined in [`spec/resilience.md`](../../spec/resilience.md). It is for projects that **do not
run a commons node** but want to ship a commons-ready release artifact, the same way they ship a wheel,
a crate, or a jar.

```bash
# Lift a library with an ingest adapter, package the result as a release artifact:
python3 ../ingest-python/nl_ingest.py --module mylib mylib/*.py \
  | ./nl_bundle.py --source-repo https://github.com/org/mylib --source-release v1.2.3 \
                   -o mylib-1.2.3.nlb

# Attach mylib-1.2.3.nlb to your GitHub Release. Anyone then ingests it into a commons:
#   curl -sL https://github.com/org/mylib/releases/download/v1.2.3/mylib-1.2.3.nlb \
#     | python3 path/to/commons-node/manage.py loadbundle -
```

## Zero dependencies

A single self-contained file (`nl_bundle.py`) using only the Python standard library (3.8+) — no
`pip install`, no network. It is the standalone sibling of the node's
[`commons/bundle.py`](../commons-node/commons/bundle.py); the two produce **byte-identical** bundles for
the same record set (pinned by a conformance test in the node suite), so a bundle made here is
indistinguishable from one a node exports.

## What it does (and doesn't)

- Reads records as **JSONL** (one record per line) from files or stdin — exactly the `nl-ingest-*`
  adapter output.
- Writes a deterministic `.nlb` (`nlb/1`): a gzipped tar of `manifest.json` + `records.jsonl`. The same
  records always produce the same bytes.
- Records `--source-repo` / `--source-release` as **advisory provenance** in the manifest.
- It does **not** verify record hashes. Packaging is untrusted; the ingesting node re-verifies every
  record by hash (and signature) on `loadbundle`. A bundle can be withheld, but not poisoned.

## Usage

```
nl_bundle.py [files...] [-o OUT] [--source-repo URL] [--source-release TAG]
  files            JSONL record files (default: stdin)
  -o, --output     output .nlb path (default: - for stdout)
  --source-repo    provenance: source repository URL
  --source-release provenance: release tag/version
```

## Tests

```bash
python3 -m unittest discover -s tests
```

Covers the round-trip (write → re-read) and determinism. Byte-for-byte agreement with the node's
`commons/bundle.py` is enforced by `NlBundleConformanceTests` in the commons-node suite.

## License

Dual-licensed under Apache-2.0 OR MIT, same as the rest of *Novae Linguae*.
