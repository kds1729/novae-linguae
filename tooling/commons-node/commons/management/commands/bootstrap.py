"""Bootstrap from a signed descriptor (spec/resilience.md): discover live peers + the latest seed
bundle when the usual entry points are blocked, trust-but-verify.

    python3 manage.py bootstrap --from https://a.example.org/.well-known/nlb-bootstrap.json \
                                --from file:///mnt/usb/bootstrap.json \
                                --trust did:nova:... --pull

`--from` URLs are tried in order (fallback). With `--trust`, the descriptor must be validly signed by
a trusted did:nova. `--pull` fetches the latest bundle (checking its digest matches the signed
descriptor) and ingests it through the same verify-then-store gate as publish.
"""

from django.core.management.base import BaseCommand, CommandError

from commons.bootstrap import BootstrapError, pull_bundle, resolve
from commons.ingest import ingest_records


class Command(BaseCommand):
    help = "Resolve a bootstrap descriptor (peers + latest bundle); --pull to fetch and ingest it."

    def add_arguments(self, parser):
        parser.add_argument("--from", dest="urls", action="append", required=True,
                            help="descriptor URL (repeatable, tried in order); http(s):// or file://")
        parser.add_argument("--trust", action="append", default=None,
                            help="require a valid signature by this did:nova (repeatable)")
        parser.add_argument("--pull", action="store_true", help="fetch the latest bundle and ingest it")
        parser.add_argument("--quiet", action="store_true", help="do not print per-record rejects")

    def handle(self, *args, **options):
        try:
            doc, status, producer, src = resolve(options["urls"], trusted_dids=options.get("trust"))
        except BootstrapError as exc:
            raise CommandError(str(exc))

        prov = status + (f" ({producer})" if producer else "")
        self.stdout.write(f"resolved from {src}  provenance={prov}")
        for peer in doc.get("peers", []):
            self.stdout.write(f"  peer: {peer}")
        lb = doc.get("latest_bundle")
        if lb:
            self.stdout.write(f"  latest_bundle: {lb.get('hash')} via {lb.get('urls')}")

        if options["pull"]:
            try:
                manifest, records = pull_bundle(doc)
            except BootstrapError as exc:
                raise CommandError(str(exc))
            on_reject = None if options["quiet"] else (
                lambda c, d: self.stderr.write(f"reject {c}: {d[:100]}"))
            stored, skipped, failed = ingest_records(records, on_reject=on_reject)
            self.stdout.write(f"pulled bundle count={manifest.get('count')} "
                              f"stored={stored} skipped={skipped} failed={failed}")
