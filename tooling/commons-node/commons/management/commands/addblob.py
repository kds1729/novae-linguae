"""Store a binary blob in COMMONS_BLOB_DIR under its sha256 (spec/weights.md).

The blob store is content-addressed and gate-free: the file is hashed, copied to
`COMMONS_BLOB_DIR/<sha256>`, and served at `/v0/blobs/<sha256>`. A weights record's
`files[].sha256` is what makes the blob fetchable-safely from anywhere — the client verifies
after download. Idempotent: re-adding an existing blob is a no-op (content-addressed files
cannot conflict).
"""

import hashlib
import shutil
from pathlib import Path

from django.conf import settings
from django.core.management.base import BaseCommand, CommandError


class Command(BaseCommand):
    help = "Store a file in the blob store under its sha256; print '<sha256>  <bytes>  <status>'."

    def add_arguments(self, parser):
        parser.add_argument("files", nargs="+", help="File(s) to add to the blob store.")

    def handle(self, *args, **options):
        blob_dir = Path(settings.COMMONS_BLOB_DIR)
        blob_dir.mkdir(parents=True, exist_ok=True)
        for name in options["files"]:
            src = Path(name)
            if not src.is_file():
                raise CommandError(f"not a file: {src}")
            h = hashlib.sha256()
            with open(src, "rb") as f:
                for chunk in iter(lambda: f.read(1 << 20), b""):
                    h.update(chunk)
            digest = h.hexdigest()
            dest = blob_dir / digest
            if dest.exists():
                status = "already present"
            else:
                shutil.copyfile(src, dest)
                status = "stored"
            self.stdout.write(f"{digest}  {src.stat().st_size}  {status}  ({src.name})")
