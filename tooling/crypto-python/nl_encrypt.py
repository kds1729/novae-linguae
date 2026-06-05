#!/usr/bin/env python3
"""nl-encrypt: seal and open Nova Locutio encrypted envelopes (v0.2).

A thin CLI over ``nl_crypto``: hybrid multi-recipient sealing (random content-encryption key,
X25519 key-wrap from did:nova keys, XChaCha20-Poly1305 AEAD) per ``spec/encryption.md``.

    # Seal stdin to two recipients (pretty JSON envelope on stdout)
    echo -n '{"secret":1}' | ./nl_encrypt.py seal --to did:nova:<hex> --to did:nova:<hex> --pretty

    # Open an envelope with your DID + user seed (raw plaintext to stdout)
    ./nl_encrypt.py open --did did:nova:<hex> --seed my-seed envelope.json

    # Show the X25519 public key a DID (or seed) resolves to
    ./nl_encrypt.py pubkey --did did:nova:<hex>
    ./nl_encrypt.py pubkey --seed my-seed

Security note: ``nl_crypto`` is a clear, verifiable *reference* implementation, not hardened against
side-channels. Use a vetted library for real secrets; this CLI is for spec conformance and examples.
"""

from __future__ import annotations

import argparse
import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import nl_crypto as nc


def _read_input(path) -> bytes:
    if path is None or path == "-":
        return sys.stdin.buffer.read()
    with open(path, "rb") as f:
        return f.read()


def _cmd_seal(args) -> int:
    plaintext = _read_input(args.infile)
    aad = args.aad.encode("utf-8") if args.aad is not None else b""
    rng = nc.seeded_rng(bytes.fromhex(args.deterministic)) if args.deterministic else nc.random_bytes
    try:
        envelope = nc.seal(plaintext, args.to, aad=aad, rng=rng)
    except ValueError as e:
        print(f"nl-encrypt: {e}", file=sys.stderr)
        return 1
    indent = 2 if args.pretty else None
    sep = None if args.pretty else (",", ":")
    sys.stdout.write(json.dumps(envelope, indent=indent, separators=sep))
    sys.stdout.write("\n")
    return 0


def _cmd_open(args) -> int:
    envelope = json.loads(_read_input(args.infile).decode("utf-8"))
    try:
        if args.seed is not None:
            plaintext = nc.open_with_seed(envelope, args.did, args.seed)
        else:
            plaintext = nc.open_envelope(envelope, args.did, bytes.fromhex(args.x25519_secret))
    except ValueError as e:
        print(f"nl-encrypt: cannot open envelope: {e}", file=sys.stderr)
        return 1
    sys.stdout.buffer.write(plaintext)
    return 0


def _cmd_pubkey(args) -> int:
    if args.did is not None:
        print(nc.x25519_pub_from_did(args.did).hex())
    else:
        _, pub = nc.x25519_keypair_from_user_seed(args.seed)
        print(pub.hex())
    return 0


def _build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(prog="nl-encrypt", description=__doc__.splitlines()[0])
    sub = p.add_subparsers(dest="cmd", required=True)

    s = sub.add_parser("seal", help="seal plaintext to one or more did:nova recipients")
    s.add_argument("--to", action="append", required=True, metavar="DID",
                   help="recipient did:nova (repeatable)")
    s.add_argument("--aad", default=None, help="additional authenticated data (utf-8 text)")
    s.add_argument("--deterministic", default=None, metavar="HEX",
                   help="seed (hex) for a reproducible envelope; omit for secure randomness")
    s.add_argument("--pretty", action="store_true", help="pretty-print the envelope JSON")
    s.add_argument("infile", nargs="?", default="-", help="plaintext file, or - for stdin (default)")
    s.set_defaults(func=_cmd_seal)

    o = sub.add_parser("open", help="open an envelope and write the recovered plaintext to stdout")
    o.add_argument("--did", required=True, help="your recipient did:nova in the envelope")
    g = o.add_mutually_exclusive_group(required=True)
    g.add_argument("--seed", help="your user seed (X25519 secret is derived as nl-validator does)")
    g.add_argument("--x25519-secret", dest="x25519_secret", metavar="HEX",
                   help="your raw 32-byte X25519 secret (hex), as an alternative to --seed")
    o.add_argument("infile", nargs="?", default="-", help="envelope JSON file, or - for stdin")
    o.set_defaults(func=_cmd_open)

    k = sub.add_parser("pubkey", help="print the X25519 public key for a DID or seed")
    kg = k.add_mutually_exclusive_group(required=True)
    kg.add_argument("--did", help="did:nova whose Ed25519 key is mapped to X25519")
    kg.add_argument("--seed", help="user seed whose X25519 public key is derived")
    k.set_defaults(func=_cmd_pubkey)
    return p


def main(argv=None) -> int:
    args = _build_parser().parse_args(argv)
    return args.func(args)


if __name__ == "__main__":
    raise SystemExit(main())
