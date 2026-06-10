# Security Policy

## Status of the cryptography

*Novae Linguae* includes cryptographic constructions — Ed25519 signing, X25519
key-agreement, XChaCha20-Poly1305 sealing, HKDF-SHA-256, and a post-quantum
hybrid (X25519 + ML-KEM-768 / FIPS 203). These are implemented to a portable
conformance contract ([`spec/crypto-conformance.md`](spec/crypto-conformance.md))
and cross-checked between a stdlib-only Python reference
([`tooling/crypto-python/`](tooling/crypto-python/)) and a constant-time Rust
implementation ([`tooling/validator/src/seal.rs`](tooling/validator/src/seal.rs)),
reproducing the same envelopes byte-for-byte and anchored to published RFC / NIST
known-answer vectors.

**They have not been independently audited.** The pure-Python ML-KEM-768 and the
reference encryption impl are *reference implementations* — correct against their
vectors and useful for interop and review, but not yet vetted by a third-party
cryptography audit and not hardened against every side-channel. Do not rely on
this code to protect high-value secrets in production until it has been audited
by qualified reviewers. Production deployments should prefer vetted, constant-time
libraries that reproduce the same conformance vectors.

This caveat is by design and called out in the project README and `CONTRIBUTING.md`;
a security audit is explicitly on the roadmap.

## Reporting a vulnerability

If you find a security issue — a cryptographic flaw, a signature-verification
bypass, a delegation/trust-policy escape, a way to forge a content-address or a
proof certificate, or any other exploitable defect — **please report it privately
first. Do not open a public issue for it.**

Preferred channels:

1. **Email** — <info@1105software.com>. Encrypt or ask for a key if the report
   itself is sensitive.
2. **GitHub private vulnerability reporting** — use the *Report a vulnerability*
   button under this repository's **Security** tab (GitHub → Security advisories).
   This keeps the report private until a fix is ready.

Either way, please **do not** open a public issue with the details.

Please include enough to reproduce: the affected component, version/commit, and a
proof-of-concept or clear description of the impact.

## Disclosure

We aim to acknowledge a report within a few days, agree on a remediation timeline,
and coordinate disclosure once a fix (or mitigation) is available. Because the
commons is content-addressed and self-verifying, fixes ship as new artifacts with
new hashes — old, vulnerable artifacts can be distrusted at the endpoint
(principle 7) rather than silently overwritten.

## Scope

In scope: the reference validator and prover (`nl-validator`), the crypto
reference impls, the commons reference node, the ingestion adapters, and the
specifications under [`spec/`](spec/).

Out of scope: third-party dependencies (report upstream), the availability of any
particular hosted node, and configuration mistakes in a deployer's own
environment.
