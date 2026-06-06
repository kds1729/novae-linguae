"""Optional toolchain seam for the string-scanner adapters (Haskell, TypeScript).

The string scanners are the deterministic, zero-dependency DEFAULT (principle 5: the same source
produces the same record regardless of what tooling happens to be installed). When the user opts in
with ``--toolchain`` AND the relevant compiler is on PATH, a source ENRICHER runs first: it uses the
real compiler to recover type information the scanner cannot (e.g. fully-resolved / inferred
signatures), and the existing scanner then parses the enriched source. The enricher is a pure
source -> source transform, so the entire record-building / hashing path is reused unchanged, and any
failure (missing tool, compile error, timeout) falls back to the original source — the toolchain path
is never *worse* than the scanner. This same hook is where a fuller AST-descriptor backend can plug in.

Determinism caveat: enabling ``--toolchain`` makes a record depend on the local compiler, so it is
opt-in, never the default; two agents that want identical hashes either both use the scanner or pin
the same compiler version.
"""

import shutil
import subprocess


def tool_on_path(name):
    """True if executable `name` is resolvable on PATH."""
    return shutil.which(name) is not None


def run_enricher(cmd, source, timeout=30):
    """Run `cmd` (an argv list) feeding `source` on stdin; return its stdout, or `source` UNCHANGED
    on any failure (missing tool, non-zero exit, timeout, empty output). Never raises."""
    try:
        proc = subprocess.run(cmd, input=source, capture_output=True, text=True, timeout=timeout)
    except (OSError, subprocess.SubprocessError):
        return source
    if proc.returncode != 0 or not proc.stdout.strip():
        return source
    return proc.stdout
