"""Standalone tests for nl-bundle (stdlib only; no Django, no node)."""

import gzip
import io
import json
import sys
import tarfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
import nl_bundle as nb  # noqa: E402

R1 = {"hash": "fn_" + "a" * 64, "schema_version": "0.1.0", "name_hints": ["a"]}
R2 = {"hash": "fn_" + "b" * 64, "schema_version": "0.2.0", "name_hints": ["b"]}


class NlBundleTests(unittest.TestCase):
    def test_round_trip(self):
        buf = io.BytesIO()
        manifest = nb.write_bundle(buf, [R1, R2], source={"repo": "x"})
        self.assertEqual(manifest["count"], 2)
        with tarfile.open(fileobj=io.BytesIO(gzip.decompress(buf.getvalue()))) as tar:
            man = json.loads(tar.extractfile(nb.MANIFEST_NAME).read())
            recs = [json.loads(line) for line
                    in tar.extractfile(nb.RECORDS_NAME).read().decode().splitlines() if line.strip()]
        self.assertEqual(man["source"], {"repo": "x"})
        self.assertEqual(sorted(r["hash"] for r in recs), sorted([R1["hash"], R2["hash"]]))

    def test_deterministic_and_order_independent(self):
        a, b = io.BytesIO(), io.BytesIO()
        nb.write_bundle(a, [R2, R1])
        nb.write_bundle(b, [R1, R2])
        self.assertEqual(a.getvalue(), b.getvalue())

    def test_blob_carriage_matches_node_module_byte_for_byte(self):
        # Blob members (by-address example values) ride as blobs/<sha256>, verified on write; the
        # standalone packager and the node's commons/bundle.py must stay byte-identical for the
        # same content — now including carried blobs.
        import hashlib
        node_dir = Path(__file__).resolve().parents[2] / "commons-node" / "commons"
        sys.path.insert(0, str(node_dir.parent))
        from commons import bundle as node_bundle
        data = b'{"kind":"int","value":10}'
        sha = hashlib.sha256(data).hexdigest()
        a, b = io.BytesIO(), io.BytesIO()
        ours = nb.write_bundle(a, [R1], blobs={sha: data})
        theirs = node_bundle.write_bundle(b, [R1], blobs={sha: data})
        self.assertEqual(ours, theirs)
        self.assertEqual(a.getvalue(), b.getvalue(), "standalone and node bundles must stay byte-identical")
        self.assertEqual(ours["blobs"], {"count": 1, "bytes": len(data)})
        with self.assertRaises(SystemExit):
            nb.write_bundle(io.BytesIO(), [R1], blobs={sha: b"lying bytes"})


if __name__ == "__main__":
    unittest.main()
