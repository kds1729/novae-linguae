"""Export records to a portable, self-verifying `.nlb` bundle (spec/resilience.md).

    python3 manage.py exportbundle out.nlb                     # all records
    python3 manage.py exportbundle out.nlb --filter '{"kind":"function-record"}'
    nl-ingest-py mylib/ | ... | python3 manage.py exportbundle - --source-repo … > mylib.nlb

The bundle is deterministic (same record set -> identical bytes) and self-verifying on ingest, so it
can be passed around any channel (mirror, IPFS, torrent, USB) and re-ingested with `loadbundle`.
"""

import json
import sys

from django.core.management.base import BaseCommand, CommandError

from commons.bundle import write_bundle
from commons.models import Record
from commons.query import candidate_records


class Command(BaseCommand):
    help = "Export records to a portable .nlb bundle (all, or matching a typed --filter)."

    def add_arguments(self, parser):
        parser.add_argument("output", help="output .nlb path, or - for stdout")
        parser.add_argument("--filter", help="typed query filter as JSON (same language as /v0/query)")
        parser.add_argument("--source-repo", help="provenance: source repository URL")
        parser.add_argument("--source-release", help="provenance: release tag/version")

    def handle(self, *args, **options):
        if options.get("filter"):
            try:
                flt = json.loads(options["filter"])
            except ValueError as exc:
                raise CommandError(f"--filter is not valid JSON: {exc}")
            records = [r.raw for r in candidate_records(flt, cap=10 ** 9)[0]]
        else:
            records = list(Record.objects.order_by("id").values_list("raw", flat=True))

        source = {k: v for k, v in (("repo", options.get("source_repo")),
                                    ("release", options.get("source_release"))) if v} or None

        dest = sys.stdout.buffer if options["output"] == "-" else options["output"]
        manifest = write_bundle(dest, records, source=source)
        # Summary to stderr so stdout stays pure bundle bytes when output is "-".
        self.stderr.write(f"exported {manifest['count']} records  "
                          f"schema_versions={manifest['schema_versions']}  "
                          f"digest={manifest['bundle_digest']}")
