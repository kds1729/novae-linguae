"""Ingest an `.nlb` bundle — verify-then-store every record (spec/resilience.md).

    python3 manage.py loadbundle mylib.nlb
    curl -s https://example.org/mylib-1.2.3.nlb | python3 manage.py loadbundle -

Same admission gate as `POST /v0/records` / `loadrecords`: each record is re-verified by hash (and
signature for messages), so a bundle's producer is untrusted — a bundle can be withheld but not
poisoned. The bundle's `bundle_digest` is checked first as a cheap whole-payload integrity guard.
"""

import sys
from pathlib import Path

from django.conf import settings
from django.core.management.base import BaseCommand, CommandError

from commons.bundle import BundleError, read_bundle_full, verify_manifest
from commons.ingest import ingest_records


class Command(BaseCommand):
    help = "Load records from a .nlb bundle (verify-then-store each), like loadrecords for a bundle."

    def add_arguments(self, parser):
        parser.add_argument("file", help=".nlb path, or - for stdin")
        parser.add_argument("--quiet", action="store_true", help="do not print per-record rejects")
        parser.add_argument("--require-signed", action="store_true",
                            help="reject the bundle unless its manifest carries a VALID signature")

    def handle(self, *args, **options):
        src = sys.stdin.buffer if options["file"] == "-" else options["file"]
        try:
            manifest, records, blobs = read_bundle_full(src)
        except BundleError as exc:
            raise CommandError(str(exc))

        # Provenance is advisory (every record is re-verified by hash below); report it, and enforce
        # only under --require-signed.
        status, producer = verify_manifest(manifest)
        if options["require_signed"] and status != "valid":
            raise CommandError(f"bundle signature is '{status}' (producer={producer}); --require-signed")
        prov = {"valid": f"signed by {producer} (verified)",
                "invalid": f"WARNING: INVALID signature (producer={producer})",
                "unsigned": "unsigned"}[status]

        quiet = options["quiet"]
        on_reject = None if quiet else (lambda c, d: self.stderr.write(f"reject {c}: {d[:100]}"))
        stored, skipped, failed = ingest_records(records, on_reject=on_reject)

        # Carried blobs land in the blob store — already sha256-verified by read_bundle_full, and
        # content-addressed files cannot conflict, so import is idempotent.
        blobs_stored = 0
        if blobs:
            blob_dir = Path(settings.COMMONS_BLOB_DIR)
            blob_dir.mkdir(parents=True, exist_ok=True)
            for sha, data in blobs.items():
                dest = blob_dir / sha
                if not dest.exists():
                    dest.write_bytes(data)
                    blobs_stored += 1

        src_note = ""
        if isinstance(manifest.get("source"), dict):
            s = manifest["source"]
            src_note = f"  source={s.get('repo', '')}@{s.get('release', '')}"
        blob_note = f"  blobs_stored={blobs_stored}/{len(blobs)}" if blobs else ""
        self.stdout.write(f"bundle {manifest.get('format_version')} count={manifest.get('count')}  "
                          f"provenance={prov}{src_note}  "
                          f"stored={stored} skipped={skipped} failed={failed}{blob_note}")
