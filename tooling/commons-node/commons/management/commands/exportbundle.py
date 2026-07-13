"""Export records to a portable, self-verifying `.nlb` bundle (spec/resilience.md).

    python3 manage.py exportbundle out.nlb                     # all records
    python3 manage.py exportbundle out.nlb --filter '{"kind":"function-record"}'
    nl-ingest-py mylib/ | ... | python3 manage.py exportbundle - --source-repo … > mylib.nlb

The bundle is deterministic (same record set -> identical bytes) and self-verifying on ingest, so it
can be passed around any channel (mirror, IPFS, torrent, USB) and re-ingested with `loadbundle`.
"""

import json
import sys
from pathlib import Path

from django.conf import settings
from django.core.management.base import BaseCommand, CommandError

from commons.bundle import write_bundle
from commons.ingest import materialized_raw
from commons.models import Record
from commons.query import candidate_records
from commons.tasks import _referenced_blobs


class Command(BaseCommand):
    help = "Export records to a portable .nlb bundle (all, or matching a typed --filter)."

    def add_arguments(self, parser):
        parser.add_argument("output", help="output .nlb path, or - for stdout")
        parser.add_argument("--filter", help="typed query filter as JSON (same language as /v0/query)")
        parser.add_argument("--since", type=int,
                            help="only records newer than this id (the /v0/sync cursor) — for "
                                 "incremental delta bundles; the next cursor is printed on stderr")
        parser.add_argument("--source-repo", help="provenance: source repository URL")
        parser.add_argument("--source-release", help="provenance: release tag/version")
        parser.add_argument("--sign-seed", help="sign the manifest with the did:nova derived from "
                                                "this seed (advisory provenance)")
        parser.add_argument("--no-blobs", action="store_true",
                            help="records only — omit the referenced blobs (by-address example "
                                 "values, weights files) the bundle carries by default so restored "
                                 "records stay checkable")

    def handle(self, *args, **options):
        since = options.get("since")
        if options.get("filter"):
            try:
                flt = json.loads(options["filter"])
            except ValueError as exc:
                raise CommandError(f"--filter is not valid JSON: {exc}")
            rows = candidate_records(flt, cap=10 ** 9)[0]
            if since is not None:
                rows = [r for r in rows if r.id > since]
            # materialized: a tiered body's row is a pointer — the bundle carries the real record.
            records = [materialized_raw(r) for r in rows]
            next_cursor = max((r.id for r in rows), default=since or 0)
        else:
            qs = Record.objects.order_by("id")
            if since is not None:
                qs = qs.filter(id__gt=since)
            rows = list(qs)
            records = [materialized_raw(r) for r in rows]
            next_cursor = rows[-1].id if rows else (since or 0)

        source = {k: v for k, v in (("repo", options.get("source_repo")),
                                    ("release", options.get("source_release"))) if v} or None

        # Carry the blobs the exported records reference (present in the local store), so a
        # restored by-address example or weights record is CHECKABLE, not merely resolvable.
        blobs, absent = {}, []
        if not options.get("no_blobs"):
            blob_dir = Path(settings.COMMONS_BLOB_DIR)
            for sha in sorted({s for r in records for s in _referenced_blobs(r)}):
                path = blob_dir / sha
                if path.is_file():
                    blobs[sha] = path
                else:
                    absent.append(sha)

        dest = sys.stdout.buffer if options["output"] == "-" else options["output"]
        manifest = write_bundle(dest, records, source=source, sign_seed=options.get("sign_seed"),
                                blobs=blobs)
        # Summary to stderr so stdout stays pure bundle bytes when output is "-".
        signed = f"  signed-by={manifest['producer']}" if manifest.get("signature") else ""
        blob_note = ""
        if blobs:
            b = manifest["blobs"]
            blob_note = f"  blobs={b['count']} ({b['bytes']} bytes)"
        for sha in absent:
            self.stderr.write(f"note: referenced blob {sha} not in the local store — NOT carried "
                              "(the restored record resolves but its by-address content must come "
                              "from a peer)")
        self.stderr.write(f"exported {manifest['count']} records{blob_note}  "
                          f"schema_versions={manifest['schema_versions']}  "
                          f"digest={manifest['bundle_digest']}  next-cursor={next_cursor}{signed}")
