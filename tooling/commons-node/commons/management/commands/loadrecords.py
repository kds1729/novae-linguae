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

from commons.ingest import ingest_records


class Command(BaseCommand):
    help = "Load records from JSONL (one record per line), verifying each before storing."

    def add_arguments(self, parser):
        parser.add_argument("file", nargs="?", default="-",
                            help="JSONL file, or - for stdin (default)")
        parser.add_argument("--quiet", action="store_true", help="do not print per-record rejects")

    def handle(self, *args, **options):
        quiet = options["quiet"]
        stream = sys.stdin if options["file"] == "-" else open(options["file"], encoding="utf-8")
        parsed, malformed = [], 0
        try:
            for line in stream:
                line = line.strip()
                if not line:
                    continue
                try:
                    parsed.append(json.loads(line))
                except ValueError:
                    malformed += 1
                    if not quiet:
                        self.stderr.write("reject malformed_json")
        finally:
            if stream is not sys.stdin:
                stream.close()

        on_reject = None if quiet else (lambda c, d: self.stderr.write(f"reject {c}: {d[:100]}"))
        stored, skipped, failed = ingest_records(parsed, on_reject=on_reject)
        failed += malformed
        self.stdout.write(f"stored={stored} skipped={skipped} failed={failed}")
