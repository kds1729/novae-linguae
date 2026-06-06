"""Bulk-load Nova Lingua records (JSONL) into the commons, verifying each — the same admission
path as `POST /v0/records`, in-process.

This is the connective tissue: the ingestion adapters (`nl-ingest`, `nl-ingest-py`, `nl-ingest-hs`,
`nl-ingest-ts`) emit one record per line, so they pipe straight in:

    python3 nl_ingest_py.py --module mypkg mypkg.py | python3 manage.py loadrecords
    python3 manage.py loadrecords records.jsonl
"""

import json
import sys

from django.core.management.base import BaseCommand

from commons import verify as V
from commons.ingest import create_record
from commons.models import Record


class Command(BaseCommand):
    help = "Load records from JSONL (one record per line), verifying each before storing."

    def add_arguments(self, parser):
        parser.add_argument("file", nargs="?", default="-",
                            help="JSONL file, or - for stdin (default)")
        parser.add_argument("--quiet", action="store_true", help="do not print per-record rejects")

    def handle(self, *args, **options):
        stream = sys.stdin if options["file"] == "-" else open(options["file"], encoding="utf-8")
        stored = skipped = failed = 0
        try:
            for line in stream:
                line = line.strip()
                if not line:
                    continue
                try:
                    raw = json.loads(line)
                except ValueError:
                    failed += 1
                    if not options["quiet"]:
                        self.stderr.write("reject malformed_json")
                    continue
                try:
                    kind, version = V.verify_record(raw)
                except V.VerifyError as exc:
                    failed += 1
                    if not options["quiet"]:
                        self.stderr.write(f"reject {exc.code}: {exc.detail[:100]}")
                    continue
                if Record.objects.filter(hash=raw["hash"]).exists():
                    skipped += 1
                    continue
                create_record(raw, kind, version)
                stored += 1
        finally:
            if stream is not sys.stdin:
                stream.close()
        self.stdout.write(f"stored={stored} skipped={skipped} failed={failed}")
