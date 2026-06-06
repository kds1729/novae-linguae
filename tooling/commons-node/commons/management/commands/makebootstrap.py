"""Publish a signed bootstrap descriptor (spec/resilience.md).

Host the output at a well-known URL (or any dead-drop) so a stranded node can discover live peers and
the latest seed bundle. Typically run after `exportbundle` (whose printed digest goes in --bundle-hash):

    python3 manage.py makebootstrap bootstrap.json \
        --peer https://node-a.example.org --peer https://node-b.example.org \
        --bundle-hash blake2b:... --bundle-url https://mirror.example.org/commons.nlb \
        --sign-seed "$PUBLISHER_SEED"
"""

import json
import sys

from django.core.management.base import BaseCommand

from commons.bootstrap import build_descriptor


class Command(BaseCommand):
    help = "Build a signed bootstrap descriptor (peers + latest-bundle pointer) to publish."

    def add_arguments(self, parser):
        parser.add_argument("output", nargs="?", default="-", help="output JSON path, or - for stdout")
        parser.add_argument("--peer", action="append", default=[], help="a peer endpoint URL (repeatable)")
        parser.add_argument("--bundle-hash", help="latest seed-bundle digest (blake2b:...)")
        parser.add_argument("--bundle-url", action="append", default=[],
                            help="URL to fetch the latest bundle (repeatable)")
        parser.add_argument("--sign-seed", help="sign with the did:nova derived from this seed")

    def handle(self, *args, **options):
        latest = None
        if options["bundle_url"]:
            latest = {"urls": options["bundle_url"]}
            if options.get("bundle_hash"):
                latest["hash"] = options["bundle_hash"]

        doc = build_descriptor(options["peer"], latest_bundle=latest, sign_seed=options.get("sign_seed"))
        data = (json.dumps(doc, indent=2) + "\n").encode("utf-8")
        if options["output"] == "-":
            sys.stdout.buffer.write(data)
        else:
            with open(options["output"], "wb") as f:
                f.write(data)

        prov = f" signed-by={doc['producer']}" if doc.get("signature") else " (unsigned)"
        self.stderr.write(f"bootstrap descriptor: {len(options['peer'])} peers, "
                          f"bundle={'yes' if latest else 'no'}{prov}")
