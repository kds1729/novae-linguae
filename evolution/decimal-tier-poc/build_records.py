#!/usr/bin/env python3
"""Author the decimal/money-tier records (evolution/decimal-tier-poc) and run every record through the
verified-by-default gauntlet, writing the records, bodies and a machine-readable report. Money as
INTEGER MINOR UNITS at a decimal scale (12.34 at scale 2 is 1234): the five rounding modes as
divisions, powers of ten, rescaling between scales, rate application (tax, discount, currency
conversion) with a half-unit error bound, and allocation of a total over weights whose parts SUM
EXACTLY to the total (the cumulative-rounding method — no lost or invented cents). Pure,
in-language bodies in surface syntax; no new builtin.

    python3 evolution/decimal-tier-poc/build_records.py            # mint + gauntlet (needs z3)
    python3 evolution/decimal-tier-poc/build_records.py --no-prove # skip the solver rungs

Output: records/<name>.v0.2.json + records/body-<name>.json, records/REPORT.json, records/REPORT.md.
Reference values come from Python's `decimal` module (ROUND_FLOOR/CEILING/DOWN/HALF_UP/HALF_EVEN)
and exact integer arithmetic. Needs `nl-validator` (NL_VALIDATOR, or the sibling release build)."""
import json
import os
import subprocess
import sys
from decimal import ROUND_CEILING, ROUND_DOWN, ROUND_FLOOR, ROUND_HALF_EVEN, ROUND_HALF_UP, Decimal, localcontext
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent
V = Path(os.environ.get("NL_VALIDATOR") or REPO / "tooling/validator/target/release/nl-validator")
os.environ.setdefault("NL_DERIVED_CACHE", "1")  # the derived-contract cache (spec/evaluation.md)
OUT = HERE / "records"


def sh(*args, check=True):
    r = subprocess.run([str(a) for a in args], capture_output=True, text=True)
    if check and r.returncode != 0:
        raise SystemExit(f"FAILED: {' '.join(map(str, args))}\n{r.stdout}{r.stderr}")
    return r


def parse_body(surface):
    return json.loads(sh(V, "parse-body", surface).stdout)


def parse_pred(surface):
    return json.loads(sh(V, "parse-predicate", surface).stdout)


def hash_artifact(obj, kind):
    p = OUT / ".hash_tmp.json"
    p.write_text(json.dumps(obj))
    out = sh(V, "hash", p, "--kind", kind).stdout.strip().split()[-1]
    p.unlink()
    return out


# ---- type / value helpers -----------------------------------------------------------------------
B = lambda n: {"kind": "builtin", "name": n}  # noqa: E731
INT, NAT, BOOL = B("int"), B("nat"), B("bool")
LIST = lambda t: {"kind": "apply", "ctor": B("List"), "args": [t]}  # noqa: E731
FN = lambda ps, r: {"kind": "fn", "params": ps, "result": r}  # noqa: E731
TUPLE = lambda *ts: {"kind": "tuple", "elems": list(ts)}  # noqa: E731
tup = lambda *vs: {"kind": "tuple", "elems": list(vs)}  # noqa: E731


def ilit(v):
    return {"kind": "int", "value": v if abs(v) < 2**53 else str(v)}


def nlit(v):
    return {"kind": "nat", "value": v if v < 2**53 else str(v)}


blit = lambda b: {"kind": "bool", "value": b}  # noqa: E731
slit = lambda s: {"kind": "string", "value": s}  # noqa: E731
ilist = lambda *vs: {"kind": "list", "elems": [ilit(v) for v in vs]}  # noqa: E731
just = lambda p: {"kind": "variant", "tag": "Just", "payload": p}  # noqa: E731
NONE = {"kind": "variant", "tag": "None"}
STR = B("string")
MAYBE = lambda t: {"kind": "sum", "variants": [{"tag": "Just", "type": t}, {"tag": "None"}]}  # noqa: E731


def fmt_ref(u, k):
    """The reference rendering: sign, integer part, and exactly k fractional digits."""
    s = "-" if u < 0 else ""
    a = abs(u)
    return s + (f"{a // 10**k}.{a % 10**k:0{k}d}" if k else str(a))

ADDR = {}      # name -> fn_ address, for `{name}` placeholders in later bodies
REPORT = []

# Round 6: one plain-words line per parameter (`param_docs`, function-record v0.2) — the meaning and
# unit a stranger agent had to decode from the contracts (`from_places` = decimal places of the
# amount). Testimony beside the names; certify checks one line per parameter.
_DIV = ["the dividend (an integer amount, e.g. minor units)", "the divisor, at least 1"]
PARAM_DOCS = {
    "div_floor": _DIV, "div_ceil": _DIV, "div_trunc": _DIV, "div_half_up": _DIV, "div_half_even": _DIV,
    "pow10": ["the exponent k, a count of decimal places (result is 10^k)"],
    "rescale": ["the amount, as an integer in units of 10^-from_places (12.345 at 3 places is 12345)",
                "decimal places the amount is stored with (2 = cents)",
                "decimal places wanted in the result; fewer places rounds half to even"],
    "apply_rate": ["the amount, as an integer in minor units",
                   "the rate's numerator (rate = rate_num / rate_den; 8.25% is 825 / 10000)",
                   "the rate's denominator, at least 1"],
    "percent_bps": ["the amount, as an integer in minor units",
                    "the rate in basis points (1 bp = 0.01%; 8.25% is 825)"],
    "fx_convert": ["the source amount, as an integer in units of 10^-from_places",
                   "decimal places of the source amount (2 = cents)",
                   "the exchange rate's numerator (target per source; 1.0825 is 10825 / 10000)",
                   "the exchange rate's denominator, at least 1",
                   "decimal places wanted in the target amount (2 = cents)"],
    "list_sum": ["the list of integers to add"],
    "alloc_from": ["the total being split", "the weights still to allocate",
                   "the weight already allocated before these", "the sum of all weights, at least 1"],
    "allocate": ["the total to split, as an integer in minor units",
                 "one non-negative weight per part (equal weights split evenly); their sum at least 1"],
    "format_amount": ["the amount, as an integer in units of 10^-k", "decimal places to render (2 = cents)"],
    "parse_amount": ["decimal text such as \"-12.05\" (no exponent, no thousands separators)",
                     "decimal places of the integer result; text with more places is refused (None)"],
    "amount_text_roundtrip": ["the amount, as an integer in units of 10^-k", "decimal places"],
    "parse_decimal": ["decimal text such as \"1.0825\" or \"-12.05\" (no exponent, no separators)"],
    "round_text_half_even": ["decimal text such as \"7.125\"",
                             "decimal places wanted in the integer result (2 = cents)"],
    "fx_convert_text": ["the source amount as decimal text, e.g. \"466.00\"",
                        "the exchange rate as decimal text, target per source, e.g. \"1.0825\"",
                        "decimal places wanted in the integer result (2 = cents)"],
}


def build(name, surface, sig, examples, props=(), refinements=(), tags=(), hints=(), complexity=None,
          terminates="always", param_docs=None):
    def subst(t):
        for k, v in ADDR.items():
            t = t.replace("{" + k + "}", v)
        return t
    body = parse_body(subst(surface))
    body_hash = hash_artifact(body, "body")
    refinements = [(k, subst(e)) for k, e in refinements]
    props = [(n, subst(e)) for n, e in props]
    record = {
        "schema_version": "0.2.0",
        "hash": "fn_" + "0" * 64,
        "name_hints": [name] + list(hints),
        "signature": {
            "type": sig,
            "refinements": [{"kind": k, "expr": parse_pred(e)} for k, e in refinements],
            "effects": [],
            "capabilities": [],
            "terminates": terminates,
        },
        "examples": examples,
        "properties": [{"name": n, "expr": parse_pred(e)} for n, e in props],
        "intent_tags": ["arithmetic/decimal", "math/decimal", f"math/decimal/{name.replace('_', '-')}"] + list(tags),
        "derived_from": None,
        "supersedes": None,
        "body_hash": body_hash,
    }
    if complexity:
        record["signature"]["complexity"] = complexity
    param_docs = param_docs or PARAM_DOCS.get(name)
    if param_docs:
        record["param_docs"] = list(param_docs)
    del record["hash"]
    record["hash"] = hash_artifact(record, "function-record")
    (OUT / f"body-{name}.json").write_text(json.dumps(body, indent=1, sort_keys=True) + "\n")
    (OUT / f"{name}.v0.2.json").write_text(json.dumps(record, indent=1, sort_keys=True) + "\n")
    ADDR[name] = record["hash"]
    return record


def gauntlet(name, prove=True):
    rp, bp = OUT / f"{name}.v0.2.json", OUT / f"body-{name}.json"
    row = {"name": name}
    r = sh(V, "validate", REPO / "spec/function-record.v0.2.schema.json", rp, check=False)
    row["schema"] = "VALID" if r.returncode == 0 else "INVALID"
    r = sh(V, "run", rp, "--records", OUT, check=False)
    row["run"] = "PASS" if r.returncode == 0 else "FAIL: " + (r.stdout + r.stderr).strip().splitlines()[-1][:160]
    r = sh(V, "certify", rp, "--body", bp, "--records", OUT, "--json", check=False)
    try:
        cert = json.loads(r.stdout)
        row["certified"] = cert.get("certified")
        row["checks"] = {c["check"]: c.get("verdict") for c in cert.get("checks", [])}
        row["check_details"] = {c["check"]: c.get("detail", "")[:240] for c in cert.get("checks", [])}
    except ValueError:
        row["certified"] = False
        row["checks"] = {"certify": "ERROR: " + (r.stdout + r.stderr).strip()[-200:]}
    rec = json.loads(rp.read_text())
    if rec["properties"]:
        r = sh(V, "check-properties", rp, "--body", bp, "--records", OUT, "--generate", "--cases", "400", check=False)
        row["properties_generative"] = [ln.strip() for ln in r.stdout.splitlines()
                                        if any(k in ln for k in ("HELD", "REFUTED", "UNGENERATABLE", "EXHAUSTIVE", "CONSISTENT", "INCONSISTENT"))][:12]
        r = sh(V, "check-properties", rp, "--body", bp, "--records", OUT, "--generate", "--cases", "200", "--wide", check=False)
        row["properties_wide"] = [ln.strip() for ln in r.stdout.splitlines()
                                  if any(k in ln for k in ("HELD", "REFUTED", "UNGENERATABLE"))][:12]
        if prove:
            r = sh(V, "prove", rp, "--body", bp, "--records", OUT, check=False)
            row["prove"] = [ln.strip() for ln in r.stdout.splitlines()
                            if any(k in ln for k in ("PROVED", "REFUTED", "UNKNOWN", "UNSUPPORTED"))][:12]
    REPORT.append(row)
    cks = row.get("checks", {})
    short = " ".join(f"{k}={v}" for k, v in cks.items())
    print(f"{name:22} {row['run']:6} certified={row['certified']} {short}", flush=True)
    for k, d in row.get("check_details", {}).items():
        if cks.get(k) not in ("SOUND", "WELL-TYPED", "N/A", "VALID", None) and d:
            print(f"    detail {k}: {d[:200]}")
    for ln in row.get("properties_generative", []):
        print("    gen  ", ln[:150])
    for ln in row.get("properties_wide", []):
        if "HELD" not in ln:
            print("    wide ", ln[:150])
    for ln in row.get("prove", []):
        print("    prove", ln[:150])


# ---- reference values (Python's decimal module, exact) ------------------------------------------
def rdiv(n, d, mode):
    """n / d rounded to an integer under a `decimal` rounding mode — exactly (precision ample)."""
    with localcontext() as ctx:
        ctx.prec = 200
        return int((Decimal(n) / Decimal(d)).quantize(Decimal(1), rounding=mode))


def ex(args, result):
    return {"args": args, "result": result}


def ii(*vs):
    return [ilit(v) for v in vs]


def alloc_ref(total, ws):
    w, c, out = sum(ws), 0, []
    for x in ws:
        out.append((total * (c + x)) // w - (total * c) // w)
        c += x
    return out


DIV_CASES = [(7, 2), (-7, 2), (5, 2), (-5, 2), (3, 2), (-3, 2), (6, 3), (0, 5), (1234, 100), (-1250, 100), (1350, 100)]


def div_examples(mode):
    return [ex(ii(n, d), ilit(rdiv(n, d, mode))) for n, d in DIV_CASES]


def main():
    prove = "--no-prove" not in sys.argv
    OUT.mkdir(exist_ok=True)
    for f in OUT.glob("*.json"):
        f.unlink()

    # 1. The five rounding modes as integer divisions by a positive divisor. Each contract is the
    # mode's DEFINITION as inequalities on the remainder — what a caller's proof reads.
    DIV = FN([INT, INT], INT)
    build("div_floor", r"\n d -> div n d", DIV, div_examples(ROUND_FLOOR),
          refinements=[("pre", "d >= 1"), ("post", "result * d <= n && n < (result + 1) * d")],
          props=[("exact", "forall k d. d < 1 || self(k * d, d) == k"),
                 ("monotone", "forall n m d. d < 1 || n > m || self(n, d) <= self(m, d)")],
          hints=["round_floor", "round_toward_negative_infinity"], complexity="O(1)")
    gauntlet("div_floor", prove)

    build("div_ceil", r"\n d -> neg (div (neg n) d)", DIV, div_examples(ROUND_CEILING),
          refinements=[("pre", "d >= 1"), ("post", "(result - 1) * d < n && n <= result * d")],
          props=[("exact", "forall k d. d < 1 || self(k * d, d) == k"),
                 ("dual_of_floor", "forall n d. d < 1 || self(n, d) == -((-n) / d)")],
          hints=["round_ceiling", "round_toward_positive_infinity"], complexity="O(1)")
    gauntlet("div_ceil", prove)

    build("div_trunc", r"\n d -> case lt n 0 of { true => neg (div (neg n) d); false => div n d }", DIV,
          div_examples(ROUND_DOWN),
          refinements=[("pre", "d >= 1"),
                       ("post", "abs(result * d) <= abs(n) && abs(n) < abs(result * d) + d"),
                       ("post", "n < 0 || result >= 0"), ("post", "n > 0 || result <= 0")],
          props=[("odd_symmetric", "forall n d. d < 1 || self(-n, d) == -self(n, d)")],
          hints=["round_down", "round_toward_zero", "truncate"], complexity="O(1)")
    gauntlet("div_trunc", prove)

    # Half away from zero ("commercial rounding"): |n|·2 + d over 2d, sign restored.
    build("div_half_up",
          r"\n d -> case lt n 0 of { true => neg (div (add (mul 2 (neg n)) d) (mul 2 d)); false => div (add (mul 2 n) d) (mul 2 d) }",
          DIV, div_examples(ROUND_HALF_UP),
          refinements=[("pre", "d >= 1"),
                       # Within half a unit of the exact quotient…
                       ("post", "2 * abs(n - result * d) <= d"),
                       # …and a tie goes AWAY from zero.
                       ("post", "2 * abs(n - result * d) != d || abs(result * d) > abs(n)")],
          props=[("exact", "forall k d. d < 1 || self(k * d, d) == k"),
                 ("odd_symmetric", "forall n d. d < 1 || self(-n, d) == -self(n, d)")],
          hints=["round_half_up", "round_half_away_from_zero", "commercial_rounding"], complexity="O(1)")
    gauntlet("div_half_up", prove)

    # Half to even ("banker's rounding", IEEE 754's default): unbiased over many roundings.
    build("div_half_even",
          r"\n d -> let q = div n d in let r = sub n (mul q d) in case lt (mul 2 r) d of { true => q; false => "
          r"case gt (mul 2 r) d of { true => add q 1; false => case eq (mod q 2) 0 of { true => q; false => add q 1 } } }",
          DIV, div_examples(ROUND_HALF_EVEN),
          refinements=[("pre", "d >= 1"),
                       ("post", "2 * abs(n - result * d) <= d"),
                       # A tie goes to the EVEN neighbour.
                       ("post", "2 * abs(n - result * d) != d || result % 2 == 0")],
          props=[("exact", "forall k d. d < 1 || self(k * d, d) == k"),
                 ("odd_symmetric", "forall n d. d < 1 || self(-n, d) == -self(n, d)"),
                 ("monotone", "forall n m d. d < 1 || n > m || self(n, d) <= self(m, d)")],
          hints=["round_half_even", "bankers_rounding", "round_half_to_even"], complexity="O(1)")
    gauntlet("div_half_even", prove)

    # 2. Powers of ten — the scale factor between minor units and decimal places.
    build("pow10", r"\k -> case gt k 0 of { true => mul 10 (self (sub k 1)); false => 1 }",
          FN([NAT], INT),
          [ex([nlit(0)], ilit(1)), ex([nlit(2)], ilit(100)), ex([nlit(18)], ilit(10**18)), ex([nlit(30)], ilit(10**30))],
          refinements=[("post", "result >= 1")],
          # Stated under a `forall`: the induction backends refuse a closed law on a recursive body
          # ("not a `forall`", measured round 2) — the guarded form is the same fact.
          props=[("zero", "forall k. k != 0 || self(k) == 1"),
                 ("product", "forall a b. a < 0 || b < 0 || self(a + b) == self(a) * self(b)"),
                 ("increasing", "forall k. k < 0 || self(k + 1) > self(k)")],
          hints=["power_of_ten", "scale_factor"], complexity="O(n)")
    gauntlet("pow10", prove)

    # 3. rescale — minor units at scale `f` to scale `t`: exact upward (×10^(t−f)), banker's-rounded
    # downward (÷10^(f−t)). 12.345 (12345 at 3) → 1234 at 2 (the tie goes to the even 4).
    # Round 5: parameters named for what they are (the summary's `params` shows the body's names; a
    # stranger could not tell `f`/`t` were decimal-place counts), and the TIE RULE as a post — the
    # error bound alone admits half-up too (stranger 10 read the body to find out).
    build("rescale",
          r"\amount from_places to_places -> case le from_places to_places of { "
          r"true => mul amount ({pow10} (sub to_places from_places)); "
          r"false => {div_half_even} amount ({pow10} (sub from_places to_places)) }",
          FN([INT, NAT, NAT], INT),
          [ex([ilit(1234), nlit(2), nlit(4)], ilit(123400)), ex([ilit(12345), nlit(3), nlit(2)], ilit(1234)),
           ex([ilit(12355), nlit(3), nlit(2)], ilit(1236)), ex([ilit(-12345), nlit(3), nlit(2)], ilit(-1234)),
           ex([ilit(199), nlit(2), nlit(0)], ilit(2)), ex([ilit(7), nlit(1), nlit(1)], ilit(7))],
          refinements=[("post", "from_places > to_places || result == amount * {pow10}(to_places - from_places)"),
                       ("post", "from_places <= to_places || 2 * abs(amount - result * {pow10}(from_places - to_places)) <= {pow10}(from_places - to_places)"),
                       ("post", "from_places <= to_places || 2 * abs(amount - result * {pow10}(from_places - to_places)) != {pow10}(from_places - to_places) || result % 2 == 0")],
          # Up then down is the identity: no information is lost going to MORE decimal places.
          props=[("roundtrip", "forall u f t. f < 0 || t < f || self(self(u, f, t), t, f) == u")],
          hints=["change_scale", "quantize", "round_to_places"], complexity="O(n)")
    gauntlet("rescale", prove)

    # 4. Rates: amount × num / den, banker's-rounded — tax, discount, interest, currency conversion
    # with an exact rational rate. The contract is the error bound: within half a minor unit.
    build("apply_rate", r"\amount rate_num rate_den -> {div_half_even} (mul amount rate_num) rate_den",
          FN([INT, INT, INT], INT),
          [ex(ii(1999, 8, 100), ilit(rdiv(1999 * 8, 100, ROUND_HALF_EVEN))),
           ex(ii(1000, 1, 3), ilit(333)), ex(ii(250, 1, 100), ilit(2)), ex(ii(-1050, 1, 10), ilit(-105))],
          refinements=[("pre", "rate_den >= 1"),
                       ("post", "2 * abs(amount * rate_num - result * rate_den) <= rate_den"),
                       # Banker's rounding: a tie goes to the even neighbour.
                       ("post", "2 * abs(amount * rate_num - result * rate_den) != rate_den || result % 2 == 0")],
          props=[("unit_rate", "forall a d. d < 1 || self(a, d, d) == a"),
                 ("zero_amount", "forall n d. d < 1 || self(0, n, d) == 0")],
          hints=["multiply_by_rate", "tax", "discount", "convert_currency"], complexity="O(1)")
    gauntlet("apply_rate", prove)

    build("percent_bps", r"\amount basis_points -> {apply_rate} amount basis_points 10000",
          FN([INT, INT], INT),
          [ex(ii(10000, 825), ilit(825)), ex(ii(1999, 825), ilit(rdiv(1999 * 825, 10000, ROUND_HALF_EVEN))),
           ex(ii(12345, 5000), ilit(rdiv(12345 * 5000, 10000, ROUND_HALF_EVEN)))],
          refinements=[("post", "2 * abs(amount * basis_points - result * 10000) <= 10000"),
                       ("post", "2 * abs(amount * basis_points - result * 10000) != 10000 || result % 2 == 0")],
          props=[("whole", "forall a. self(a, 10000) == a")],
          hints=["percentage", "basis_points"], complexity="O(1)")
    gauntlet("percent_bps", prove)

    # Currency conversion between scales: `a` minor units at scale fs, rate num/den (target per
    # source), result in minor units at scale ts — one rounding, at the end.
    build("fx_convert",
          r"\amount from_places rate_num rate_den to_places -> {div_half_even} "
          r"(mul (mul amount rate_num) ({pow10} to_places)) (mul rate_den ({pow10} from_places))",
          FN([INT, NAT, INT, INT, NAT], INT),
          [ex([ilit(10000), nlit(2), ilit(108), ilit(100), nlit(2)], ilit(10800)),
           ex([ilit(100), nlit(2), ilit(15725), ilit(100), nlit(0)], ilit(157)),
           ex([ilit(1), nlit(0), ilit(1), ilit(3), nlit(2)], ilit(33)),
           # €250.00 at 1.0825 = 27062.5 cents: the tie goes to the even 27062.
           ex([ilit(25000), nlit(2), ilit(10825), ilit(10000), nlit(2)], ilit(27062))],
          refinements=[("pre", "rate_den >= 1"),
                       ("post", "2 * abs(amount * rate_num * {pow10}(to_places) - result * (rate_den * {pow10}(from_places))) <= rate_den * {pow10}(from_places)"),
                       ("post", "2 * abs(amount * rate_num * {pow10}(to_places) - result * (rate_den * {pow10}(from_places))) != rate_den * {pow10}(from_places) || result % 2 == 0")],
          hints=["currency_conversion", "exchange"], complexity="O(n)")
    gauntlet("fx_convert", prove)

    # 5. Allocation: split a total over weights so the parts SUM EXACTLY to the total. Cumulative
    # rounding: part_i = ⌊T·C_i/W⌋ − ⌊T·C_{i−1}/W⌋ (C the running weight) — the sum telescopes.
    build("list_sum", r"\xs -> case null xs of { true => 0; false => add (head xs) (self (tail xs)) }",
          FN([LIST(INT)], INT),
          [ex([ilist(1, 2, 3)], ilit(6)), ex([ilist()], ilit(0)), ex([ilist(-4, 4, 5)], ilit(5))],
          # The unfolding, as laws: a caller sees `list_sum` only through its contract, and the
          # telescoping proof below needs exactly one step of it.
          props=[("empty", "forall xs. length(xs) != 0 || self(xs) == 0"),
                 ("cons", "forall x xs. self(cons(x, xs)) == x + self(xs)"),
                 ("append", "forall xs ys. self(append(xs, ys)) == self(xs) + self(ys)")],
          hints=["sum"], complexity="O(n)")
    gauntlet("list_sum", prove)

    build("alloc_from",
          r"\t ws c w -> case null ws of { true => nil; false => let c2 = add c (head ws) in "
          r"cons (sub (div (mul t c2) w) (div (mul t c) w)) (self t (tail ws) c2 w) }",
          FN([INT, LIST(INT), INT, INT], LIST(INT)),
          [ex([ilit(100), ilist(1, 1, 1), ilit(0), ilit(3)], ilist(*alloc_ref(100, [1, 1, 1]))),
           ex([ilit(1000), ilist(1, 2), ilit(1), ilit(4)], ilist(250, 500))],
          refinements=[("pre", "w >= 1")],
          props=[("length", "forall t ws c w. length(self(t, ws, c, w)) == length(ws)"),
                 # The telescoping sum — the invariant the whole allocation rests on.
                 ("telescopes", "forall t ws c w. w < 1 || {list_sum}(self(t, ws, c, w)) == (t * (c + {list_sum}(ws))) / w - (t * c) / w")],
          hints=["allocate_cumulative"], complexity="O(n)")
    gauntlet("alloc_from", prove)

    build("allocate", r"\total weights -> {alloc_from} total weights 0 ({list_sum} weights)",
          FN([INT, LIST(INT)], LIST(INT)),
          [ex([ilit(100), ilist(1, 1, 1)], ilist(*alloc_ref(100, [1, 1, 1]))),
           ex([ilit(1000), ilist(70, 20, 10)], ilist(*alloc_ref(1000, [70, 20, 10]))),
           ex([ilit(-1001), ilist(1, 1)], ilist(*alloc_ref(-1001, [1, 1]))),
           ex([ilit(5), ilist(3, 0, 7)], ilist(*alloc_ref(5, [3, 0, 7])))],
          refinements=[("pre", "{list_sum}(weights) >= 1"),
                       ("post", "length(result) == length(weights)"),
                       # NO LOST CENTS: the parts sum to the total, exactly.
                       ("post", "{list_sum}(result) == total")],
          hints=["split_proportionally", "allocate_money", "largest_remainder"], complexity="O(n)")
    gauntlet("allocate", prove)

    # 6. Text: render minor units as a decimal string and parse one back — the boundary where an
    # agent meets amounts in JSON, CSV and API payloads. Parsing never rounds: more fractional
    # digits than the scale, a stray sign or a non-canonical integer part is None.
    # The k fractional digits, zero-padded, are the digits of 10^k + r after the leading "1" — no
    # padding loop (one built by repeated `str_concat` is O(k²) — measured: the fold said so).
    build("format_amount",
          r'\u k -> let p = {pow10} k in let a = abs u in let ip = to_string (div a p) in '
          r'let sign = case lt u 0 of { true => "-"; false => "" } in '
          r'case eq k 0 of { true => str_concat sign ip; false => '
          r'let fs = str_join "" (tail (str_split "" (to_string (add p (mod a p))))) in '
          r'str_concat (str_concat (str_concat sign ip) ".") fs }',
          FN([INT, NAT], STR),
          [ex([ilit(u), nlit(k)], slit(fmt_ref(u, k))) for u, k in
           [(-1205, 2), (5, 2), (-5, 2), (100, 0), (-7, 0), (123456789, 4), (0, 3)]],
          hints=["render_amount", "to_decimal_string"], complexity="O(n)")
    gauntlet("format_amount", prove)

    build("parse_amount",
          # A sign is accepted only as the FIRST character: split on "-" must give ["", digits]
          # (round 3 accepted "1-2" as −2.00 — `str_contains` alone did not pin the position).
          r'\s k -> let pieces = str_split "-" s in let isneg = gt (length pieces) 1 in '
          r'case and isneg (or (gt (length pieces) 2) (not (eq (head pieces) ""))) of { true => None; false => '
          r'let body = last pieces in '
          r'let parts = str_split "." body in '
          r'case parse_int (head parts) of { None => None; Just(i) => case lt i 0 of { true => None; false => '
          r'let whole = mul i ({pow10} k) in '
          r'case eq (length parts) 1 of { true => Just(case isneg of { true => neg whole; false => whole }); false => '
          r'case eq (length parts) 2 of { false => None; true => let f = head (tail parts) in let n = str_length f in '
          r'case or (lt n 1) (gt n k) of { true => None; false => '
          r'case parse_int (str_concat "1" f) of { None => None; Just(g) => '
          r'let v = add whole (mul (sub g ({pow10} n)) ({pow10} (sub k n))) in '
          r'Just(case isneg of { true => neg v; false => v }) } } } } } } }',
          FN([STR, NAT], MAYBE(INT)),
          [ex([slit(s), nlit(k)], r) for s, k, r in
           [("-12.05", 2, just(ilit(-1205))), ("0.05", 2, just(ilit(5))), ("12.5", 2, just(ilit(1250))),
            ("7", 2, just(ilit(700))), ("1.234", 2, NONE), ("12.", 2, NONE), ("abc", 2, NONE),
            ("1-2", 2, NONE), ("--5", 2, NONE), ("-", 2, NONE), ("007", 0, NONE)]],
          hints=["parse_decimal_amount", "from_decimal_string"], complexity="O(n)")
    gauntlet("parse_amount", prove)

    build("amount_text_roundtrip",
          r"\u k -> case {parse_amount} ({format_amount} u k) k of { Just(v) => eq v u; None => false }",
          FN([INT, NAT], BOOL),
          [ex([ilit(-1205), nlit(2)], blit(True)), ex([ilit(5), nlit(3)], blit(True)), ex([ilit(-7), nlit(0)], blit(True))],
          props=[("format_then_parse", "forall u k. k < 0 || self(u, k) == true")],
          tags=["predicate/check"], hints=["amount_roundtrip_check"], complexity="O(n)")
    gauntlet("amount_text_roundtrip", prove)

    # 7. Round 6 — decimal TEXT in, so an agent passes the user's numbers verbatim ("7.125",
    # "1.0825") instead of scaling them by hand (stranger 11's friction). Deliberately not a CLI float
    # literal: 7.125 as an IEEE float is inexact, and the scale is the function's business, stated in
    # its contract. `parse_decimal` reads the scale off the text; the two appliers are the numeric
    # records over it, and their contracts SAY so — the tie rule is one `refs` hop away.
    build("parse_decimal",
          # places = characters after the point: len(text) − len(integer part) − 1, floored at 0 (no
          # point → 0). Arithmetic, not a `case`, so the complexity fold can bound it; a second point
          # makes `parse_amount` refuse the text anyway.
          r'\text -> let parts = str_split "." text in '
          r'let places = max 0 (sub (sub (str_length text) (str_length (head parts))) 1) in '
          r'case {parse_amount} text places of { Just(v) => Just((v, places)); None => None }',
          FN([STR], MAYBE(TUPLE(INT, INT))),
          [ex([slit(s)], r) for s, r in
           [("1.0825", just(tup(ilit(10825), ilit(4)))), ("-12.05", just(tup(ilit(-1205), ilit(2)))),
            ("7", just(tup(ilit(7), ilit(0)))), ("0.5", just(tup(ilit(5), ilit(1)))),
            ("1.2.3", NONE), ("abc", NONE), ("1-2", NONE)]],
          hints=["decimal_from_text", "scaled_integer_of_decimal"], complexity="O(n)")
    gauntlet("parse_decimal", prove)

    build("round_text_half_even",
          r"\text places -> case {parse_decimal} text of { Just((v, p)) => Just({rescale} v p places); None => None }",
          FN([STR, NAT], MAYBE(INT)),
          [ex([slit(s), nlit(k)], r) for s, k, r in
           [("7.125", 2, just(ilit(712))), ("7.135", 2, just(ilit(714))), ("12.5", 0, just(ilit(12))),
            ("-2.5", 0, just(ilit(-2))), ("3", 2, just(ilit(300))), ("x", 2, NONE)]],
          # The predicate language builds no `Just(…)` value — compare by cases.
          refinements=[("post", "case {parse_decimal}(text) of { "
                                "Just(t) => (case result of { Just(r) => r == {rescale}(fst(t), snd(t), places); None => false }); "
                                "None => (case result of { Just(r) => false; None => true }) }"),
                       # Round 7: the rounding rule stated HERE, over the parsed digits — stranger 12
                       # fetched `rescale` to learn it. Within half a unit; a tie goes to the even.
                       ("post", "case {parse_decimal}(text) of { None => true; Just(t) => (case result of { None => true; "
                                "Just(r) => snd(t) <= places || 2 * abs(fst(t) - r * {pow10}(snd(t) - places)) <= {pow10}(snd(t) - places) }) }"),
                       ("post", "case {parse_decimal}(text) of { None => true; Just(t) => (case result of { None => true; "
                                "Just(r) => snd(t) <= places || 2 * abs(fst(t) - r * {pow10}(snd(t) - places)) != {pow10}(snd(t) - places) || r % 2 == 0 }) }")],
          hints=["round_decimal_text", "bankers_rounding_text", "round_half_to_even_text"], complexity="O(n)")
    gauntlet("round_text_half_even", prove)

    build("fx_convert_text",
          r"\amount_text rate_text to_places -> case {parse_decimal} amount_text of { None => None; Just((a, ap)) => "
          r"case {parse_decimal} rate_text of { None => None; Just((r, rp)) => Just({fx_convert} a ap r ({pow10} rp) to_places) } }",
          FN([STR, STR, NAT], MAYBE(INT)),
          [ex([slit(a), slit(r), nlit(k)], res) for a, r, k, res in
           [("466.00", "1.0825", 2, just(ilit(50444))), ("250", "1.0825", 2, just(ilit(27062))),
            ("100.00", "1", 2, just(ilit(10000))), ("x", "1", 2, NONE), ("1", "y", 2, NONE)]],
          refinements=[("post", "case {parse_decimal}(amount_text) of { "
                                "None => (case result of { Just(r) => false; None => true }); "
                                "Just(x) => (case {parse_decimal}(rate_text) of { "
                                "None => (case result of { Just(r) => false; None => true }); "
                                "Just(y) => (case result of { Just(r) => r == {fx_convert}(fst(x), snd(x), fst(y), {pow10}(snd(y)), to_places); None => false }) }) }"),
                       # Round 7: the rounding rule stated here, over the parsed digits (amount a at ap
                       # places, rate r at rp places: exact value a·r·10^to / 10^(ap+rp)). Guarded by
                       # ap, rp >= 0 (always so — `parse_decimal` floors at 0 — but its body takes `head`
                       # of a list, outside the refinement fragment, so no post of its can say it): the
                       # guard is what discharges `fx_convert`'s pre `rate_den >= 1` via `pow10`'s post.
                       ("post", "case {parse_decimal}(amount_text) of { None => true; Just(x) => (case {parse_decimal}(rate_text) of { None => true; "
                                "Just(y) => (case result of { None => true; Just(r) => snd(x) < 0 || snd(y) < 0 || "
                                "2 * abs(fst(x) * fst(y) * {pow10}(to_places) - r * ({pow10}(snd(y)) * {pow10}(snd(x)))) <= {pow10}(snd(y)) * {pow10}(snd(x)) }) }) }"),
                       ("post", "case {parse_decimal}(amount_text) of { None => true; Just(x) => (case {parse_decimal}(rate_text) of { None => true; "
                                "Just(y) => (case result of { None => true; Just(r) => snd(x) < 0 || snd(y) < 0 || "
                                "2 * abs(fst(x) * fst(y) * {pow10}(to_places) - r * ({pow10}(snd(y)) * {pow10}(snd(x)))) != {pow10}(snd(y)) * {pow10}(snd(x)) || r % 2 == 0 }) }) }")],
          hints=["currency_conversion_text", "exchange_decimal_text"], complexity="O(n)")
    gauntlet("fx_convert_text", prove)

    (OUT / "REPORT.json").write_text(json.dumps(REPORT, indent=1) + "\n")
    lines = ["| record | run | certified | typecheck | effects | refinement | termination | complexity | properties (generative) | prove |",
             "|---|---|---|---|---|---|---|---|---|---|"]
    for r in REPORT:
        c = r.get("checks", {})
        refn = " / ".join(f"{k.split(':', 1)[1]}={v}" for k, v in c.items() if k.startswith("refinement")) or ""

        def verdict(line, marker):
            name = line.split(":", 1)[0]
            tail = line.split(marker, 1)[1].strip() if marker in line else line.split(":", 1)[1].strip()
            return f"{name}={tail.split()[0]}" if tail else name
        gen = "; ".join(verdict(x, "generative:") for x in r.get("properties_generative", [])) or "—"
        prv = "; ".join(verdict(x, ":") for x in r.get("prove", [])) or "—"
        lines.append(f"| `{r['name']}` | {r['run'].split(':')[0]} | {r.get('certified')} | {c.get('typecheck', '')} | {c.get('effects', '')} | "
                     f"{refn} | {c.get('termination', '')} | {c.get('complexity', '')} | {gen} | {prv} |")
    (OUT / "REPORT.md").write_text("\n".join(lines) + "\n")
    print(f"\n{len(REPORT)} records; {sum(1 for r in REPORT if r['run'] == 'PASS')} run PASS; "
          f"{sum(1 for r in REPORT if r.get('certified'))} certified")


if __name__ == "__main__":
    main()
