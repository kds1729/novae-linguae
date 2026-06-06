"""Conservative, best-effort effect & termination inference for Nova Lingua ingestion.

Populates the v0.2 record fields `signature.effects` (subset of the closed enum
fs.read / fs.write / net.read / net.write / alloc / time / random / io.console /
process.spawn / panic) and `signature.terminates` (always / conditional / never / unknown),
which the adapters previously hardcoded to `[]` / `"unknown"`.

IMPORTANT HONESTY CAVEAT — inference is a LOWER BOUND, never a purity certificate.
An empty `effects` list from this module does NOT assert provable purity; it asserts only that
no *syntactically recognisable* effectful operation was found. Effects reached through
higher-order calls, dynamic dispatch, reflection, or aliased/re-exported names are missed. The
`alloc` effect is deliberately not inferred at all (everything allocates; the signal carries no
information). `terminates` defaults to `"unknown"`; `"always"` is emitted only for provably
trivial straight-line bodies, `"conditional"` for directly self-recursive ones, and `"never"`
is never emitted (we cannot prove non-termination statically).

Two front-ends:
  * `effects_from_py` / `terminates_from_py` walk a real Python `ast` (used by nl-ingest-py).
  * `effects_from_tokens` / `terminates_from_tokens` do a word-boundary token scan (used by the
    string-scanning Haskell and TypeScript adapters, which have no real AST). Token scanning is
    necessarily coarser — it cannot prove the totality whitelist, so HS/TS never emit `"always"`.
The Rust adapter has its own parallel implementation over `syn` in nl_ingest.rs.
"""

import ast
import re

# ---------------------------------------------------------------------------
# Python front-end (real AST).
# ---------------------------------------------------------------------------

# os.* leaf classification.
_OS_READ = {"listdir", "scandir", "stat", "lstat", "walk", "getcwd", "readlink", "access", "fspath"}
_OS_WRITE = {"remove", "unlink", "rename", "replace", "mkdir", "makedirs", "rmdir", "removedirs",
             "chmod", "chown", "symlink", "link", "truncate", "mkfifo", "utime"}
_OS_PROC = {"system", "popen"}
_SOCK_READ = {"recv", "recvfrom", "recvmsg", "recv_into", "recvfrom_into"}
_SOCK_WRITE = {"send", "sendall", "sendto", "sendmsg", "connect", "connect_ex"}
_TIME = {"time", "monotonic", "perf_counter", "process_time", "sleep", "time_ns",
         "monotonic_ns", "perf_counter_ns", "process_time_ns"}

# Builtins whose calls are total (used by the terminates whitelist).
_TOTAL_BUILTINS = {"len", "abs", "min", "max", "sum", "round", "int", "float", "bool", "str",
                   "ord", "chr", "bin", "hex", "oct", "repr", "divmod", "pow"}


def _dotted_parts(node):
    """The dotted path of an attribute/name callee as a list, or None if not a simple path.
    `a.b.c` -> ['a','b','c']; `a` -> ['a']; `f()[0].g` -> None."""
    parts = []
    while isinstance(node, ast.Attribute):
        parts.append(node.attr)
        node = node.value
    if isinstance(node, ast.Name):
        parts.append(node.id)
        parts.reverse()
        return parts
    return None


def _resolve(callee, alias, fromimp):
    """Resolve a callee to (module, leaf). module is None for builtins / unresolved bare names.
    `alias` maps an `import x [as y]` local root to its module; `fromimp` maps a `from m import n`
    name to module m."""
    parts = _dotted_parts(callee)
    if not parts:
        return (None, None)
    if len(parts) == 1:
        name = parts[0]
        if name in fromimp:
            return (fromimp[name], name)
        return (None, name)
    head = alias.get(parts[0], parts[0])
    module = ".".join([head] + parts[1:-1])
    return (module, parts[-1])


def _open_effects(node):
    """fs effects of a builtin `open(path, mode)` call, read from a literal mode if present."""
    mode = None
    if len(node.args) >= 2 and isinstance(node.args[1], ast.Constant) and isinstance(node.args[1].value, str):
        mode = node.args[1].value
    for kw in node.keywords:
        if kw.arg == "mode" and isinstance(kw.value, ast.Constant) and isinstance(kw.value.value, str):
            mode = kw.value.value
    if mode is None:
        return {"fs.read"}  # default mode is 'r'
    eff = set()
    if any(c in mode for c in "wax") or "+" in mode:
        eff.add("fs.write")
    if "r" in mode or "+" in mode or not eff:
        eff.add("fs.read")
    return eff


def _call_effects(module, leaf, node):
    """Effects of a single resolved call. Over-claiming (a false positive) is the safe direction
    for an effect signature; under-claiming is the documented risk."""
    out = set()
    if module is None:  # builtin or unresolved bare name
        if leaf == "open":
            out |= _open_effects(node)
        elif leaf in ("print", "input"):
            out.add("io.console")
        elif leaf in ("read_text", "read_bytes"):  # pathlib.Path methods (distinctive names)
            out.add("fs.read")
        elif leaf in ("write_text", "write_bytes"):
            out.add("fs.write")
        return out
    root = module.split(".")[0]
    if root == "os":
        if module == "os":
            if leaf in _OS_READ:
                out.add("fs.read")
            elif leaf in _OS_WRITE:
                out.add("fs.write")
            elif leaf in _OS_PROC or leaf.startswith("exec") or leaf.startswith("spawn") or leaf.startswith("posix_spawn"):
                out.add("process.spawn")
            elif leaf in ("urandom", "getrandom"):
                out.add("random")
        elif module == "os.path":
            out.add("fs.read")
    elif root == "io" and leaf == "open":
        out |= {"fs.read", "fs.write"}
    elif root == "shutil":
        out |= {"fs.read", "fs.write"}
    elif root == "pathlib":
        if leaf in ("read_text", "read_bytes"):
            out.add("fs.read")
        elif leaf in ("write_text", "write_bytes"):
            out.add("fs.write")
    elif root == "socket":
        if leaf in _SOCK_READ:
            out.add("net.read")
        elif leaf in _SOCK_WRITE:
            out.add("net.write")
        elif leaf == "create_connection":
            out |= {"net.read", "net.write"}
    elif root in ("requests", "httpx", "aiohttp", "urllib3"):
        if leaf in ("get", "head", "options"):
            out.add("net.read")
        elif leaf in ("post", "put", "patch", "delete"):
            out.add("net.write")
        else:
            out |= {"net.read", "net.write"}
    elif root == "urllib":
        if leaf in ("urlopen", "urlretrieve"):
            out |= {"net.read", "net.write"}
    elif root in ("http", "httplib"):
        out |= {"net.read", "net.write"}
    elif root in ("random", "secrets"):
        out.add("random")
    elif module.endswith(".random"):  # e.g. numpy.random
        out.add("random")
    elif root == "time":
        if leaf in _TIME:
            out.add("time")
    elif root == "datetime":
        if leaf in ("now", "today", "utcnow"):
            out.add("time")
    elif root == "subprocess":
        out.add("process.spawn")
    elif root == "multiprocessing":
        if leaf in ("Process", "Pool"):
            out.add("process.spawn")
    elif module in ("sys.stdout", "sys.stderr") and leaf == "write":
        out.add("io.console")
    elif module == "sys" and leaf == "exit":
        out.add("panic")
    return out


_NESTED_SCOPE = (ast.FunctionDef, ast.AsyncFunctionDef, ast.Lambda, ast.ClassDef)


def _iter_own(node):
    """Descendants of `node`, NOT descending into nested function/lambda/class scopes (their effects
    belong to those inner definitions unless called)."""
    for child in ast.iter_child_nodes(node):
        if isinstance(child, _NESTED_SCOPE):
            continue
        yield child
        yield from _iter_own(child)


def _iter_own_body(func):
    for stmt in func.body:
        if isinstance(stmt, _NESTED_SCOPE):
            continue
        yield stmt
        yield from _iter_own(stmt)


def _leading_assert_ids(func):
    """Node ids of the leading run of `assert` statements (which nl_predicates turns into
    preconditions) — these are contract guards, not runtime panics, so they don't add `panic`."""
    body = func.body
    start = 1 if (body and isinstance(body[0], ast.Expr)
                  and isinstance(body[0].value, ast.Constant)
                  and isinstance(body[0].value.value, str)) else 0
    ids = set()
    for stmt in body[start:]:
        if isinstance(stmt, ast.Assert):
            ids.add(id(stmt))
        else:
            break
    return ids


def effects_from_py(func, alias=None, fromimp=None):
    """Inferred effects of a Python function (ast.FunctionDef/AsyncFunctionDef), sorted. `alias`
    and `fromimp` are the module import maps (see `_resolve`); pass them for accurate net/fs/time
    classification of qualified calls."""
    alias = alias or {}
    fromimp = fromimp or {}
    leading = _leading_assert_ids(func)
    found = set()
    for node in _iter_own_body(func):
        if isinstance(node, ast.Call):
            module, leaf = _resolve(node.func, alias, fromimp)
            if leaf is not None:
                found |= _call_effects(module, leaf, node)
        elif isinstance(node, ast.Raise):
            found.add("panic")
        elif isinstance(node, ast.Assert):
            if id(node) not in leading:
                found.add("panic")
    return sorted(found)


def terminates_from_py(func):
    """Conservative termination class. `"conditional"` if directly self-recursive; `"always"` only
    for a straight-line body (no loops/comprehensions) whose every call is a total builtin;
    `"unknown"` otherwise. Never `"never"`."""
    name = func.name
    has_loop = False
    self_rec = False
    bad_call = False
    for node in _iter_own_body(func):
        if isinstance(node, (ast.While, ast.For, ast.AsyncFor, ast.ListComp, ast.SetComp,
                             ast.DictComp, ast.GeneratorExp)):
            has_loop = True
        elif isinstance(node, ast.Call):
            callee = node.func
            if isinstance(callee, ast.Name):
                if callee.id == name:
                    self_rec = True
                elif callee.id not in _TOTAL_BUILTINS:
                    bad_call = True
            else:
                bad_call = True  # method / qualified call: unknown totality
    if self_rec:
        return "conditional"
    if has_loop or bad_call:
        return "unknown"
    return "always"


# ---------------------------------------------------------------------------
# Token front-end (Haskell / TypeScript string scanners).
# ---------------------------------------------------------------------------

# Each entry: effect -> list of tokens. Tokens may contain '.' (e.g. Math.random); matching is
# anchored on identifier boundaries at both ends, so `fetch` does not match `fetchData`.
_HS_TOKENS = {
    "fs.read": ["readFile", "getContents", "hGetContents", "hGetLine", "hGetChar"],
    "fs.write": ["writeFile", "appendFile", "removeFile", "renameFile", "createDirectory",
                 "removeDirectory", "createDirectoryIfMissing"],
    "io.console": ["putStr", "putStrLn", "print", "getLine", "getChar", "readLn", "interact"],
    "net.read": ["recv", "Network.Socket", "Network.HTTP"],
    "net.write": ["send", "connect"],
    "random": ["randomIO", "randomRIO", "newStdGen", "getStdGen", "getStdRandom", "mkStdGen", "randoms"],
    "time": ["getCurrentTime", "getCPUTime", "getPOSIXTime", "threadDelay"],
    "process.spawn": ["createProcess", "callCommand", "callProcess", "readProcess", "spawnProcess",
                      "System.Process"],
    "panic": ["error", "undefined", "throw", "throwIO", "fromJust"],
}
_TS_TOKENS = {
    "fs.read": ["readFile", "readFileSync", "createReadStream"],
    "fs.write": ["writeFile", "writeFileSync", "appendFile", "appendFileSync", "createWriteStream",
                 "unlink", "unlinkSync", "mkdir", "mkdirSync"],
    "net.read": ["fetch", "XMLHttpRequest", "axios", "WebSocket"],
    "net.write": ["fetch", "axios", "WebSocket"],
    "random": ["Math.random", "crypto.getRandomValues", "crypto.randomBytes"],
    "time": ["Date.now", "performance.now", "setTimeout", "setInterval"],
    "io.console": ["console", "process.stdout", "process.stdin"],
    "process.spawn": ["child_process", "execSync", "spawnSync", "execFile", "execFileSync"],
    "panic": ["throw"],
}
_TOKEN_TABLES = {"hs": _HS_TOKENS, "ts": _TS_TOKENS}


def _has_token(text, token):
    """True if `token` appears in `text` bounded by non-identifier characters on both ends."""
    pat = r"(?<![A-Za-z0-9_])" + re.escape(token) + r"(?![A-Za-z0-9_])"
    return re.search(pat, text) is not None


def effects_from_tokens(text, lang):
    """Inferred effects from a word-boundary token scan of a function's source slice. `lang` is
    'hs' or 'ts'. Coarser than the AST front-end and likewise a lower bound."""
    table = _TOKEN_TABLES.get(lang)
    if not table or not text:
        return []
    found = set()
    for effect, tokens in table.items():
        if any(_has_token(text, t) for t in tokens):
            found.add(effect)
    return sorted(found)


def _token_count(text, name):
    return len(re.findall(r"(?<![A-Za-z0-9_'])" + re.escape(name) + r"(?![A-Za-z0-9_'])", text))


def terminates_from_tokens(name, body, lang):
    """Termination class from a token scan: `"conditional"` when the function name recurs in its
    own body (appears more than once, i.e. beyond its defining occurrence), else `"unknown"`.
    Token scanning cannot prove totality, so `"always"` is never emitted here."""
    if not name or not body:
        return "unknown"
    return "conditional" if _token_count(body, name) >= 2 else "unknown"
