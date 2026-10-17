#!/usr/bin/env python3
"""Author the integer-tier records (evolution/integer-tier-poc) and run every record through the
verified-by-default gauntlet, writing the records, bodies, a machine-readable report and the
README's measurement table. Pure, in-language, self-recursive bodies in surface syntax; no new
builtin; composition by content address.

    python3 evolution/integer-tier-poc/build_records.py            # mint + gauntlet (needs z3)
    python3 evolution/integer-tier-poc/build_records.py --no-prove # skip the solver rungs

Output: records/<name>.v0.2.json + records/body-<name>.json, records/REPORT.json, records/REPORT.md.
Needs `nl-validator` (NL_VALIDATOR, or the sibling release build)."""
import json
import os
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent
V = Path(os.environ.get("NL_VALIDATOR") or REPO / "tooling/validator/target/release/nl-validator")
OUT = HERE / "records"
P_NTT = 998244353          # 119 * 2^23 + 1, primitive root 3 — the NTT prime
I64_MAX = 9223372036854775807


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
TUPLE = lambda *ts: {"kind": "tuple", "elems": list(ts)}  # noqa: E731
MAYBE = lambda t: {"kind": "sum", "variants": [{"tag": "Just", "type": t}, {"tag": "None"}]}  # noqa: E731
FN = lambda ps, r: {"kind": "fn", "params": ps, "result": r}  # noqa: E731


def ilit(v):
    return {"kind": "int", "value": v if abs(v) < 2**53 else str(v)}


def nlit(v):
    return {"kind": "nat", "value": v if v < 2**53 else str(v)}


blit = lambda b: {"kind": "bool", "value": b}  # noqa: E731
ilist = lambda *vs: {"kind": "list", "elems": [ilit(v) for v in vs]}  # noqa: E731
nlist = lambda *vs: {"kind": "list", "elems": [nlit(v) for v in vs]}  # noqa: E731
tup = lambda *vs: {"kind": "tuple", "elems": list(vs)}  # noqa: E731
just = lambda p: {"kind": "variant", "tag": "Just", "payload": p}  # noqa: E731
NONE = {"kind": "variant", "tag": "None"}

ADDR = {}      # name -> fn_ address, for `{name}` placeholders in later bodies
REPORT = []


def build(name, surface, sig, examples, props=(), refinements=(), tags=(), hints=(), complexity=None,
          terminates="always"):
    text = surface
    for k, v in ADDR.items():
        text = text.replace("{" + k + "}", v)
    body = parse_body(text)
    body_hash = hash_artifact(body, "body")
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
        "intent_tags": ["arithmetic/integer", "math/number-theory", f"math/number-theory/{name.replace('_', '-')}"] + list(tags),
        "derived_from": None,
        "supersedes": None,
        "body_hash": body_hash,
    }
    if complexity:
        record["signature"]["complexity"] = complexity
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
        # The WIDE pass: sampled across magnitudes (to 2^60) — the evidence the bounded domain cannot give.
        r = sh(V, "check-properties", rp, "--body", bp, "--records", OUT, "--generate", "--cases", "200", "--wide", check=False)
        row["properties_wide"] = [ln.strip() for ln in r.stdout.splitlines()
                                  if any(k in ln for k in ("HELD", "REFUTED", "UNGENERATABLE"))][:12]
        if prove:
            r = sh(V, "prove", rp, "--body", bp, "--records", OUT, check=False)  # callees' certified contracts as axioms
            row["prove"] = [ln.strip() for ln in r.stdout.splitlines()
                            if any(k in ln for k in ("PROVED", "REFUTED", "UNKNOWN", "UNSUPPORTED"))][:12]
    REPORT.append(row)
    cks = row.get("checks", {})
    short = " ".join(f"{k}={v}" for k, v in cks.items())
    print(f"{name:20} {row['run']:6} certified={row['certified']} {short}")
    for ln in row.get("properties_generative", []):
        print("    gen  ", ln[:150])
    for ln in row.get("prove", []):
        print("    prove", ln[:150])


# ---- reference implementations (Python) for the worked examples ---------------------------------
def py_gcd(a, b):
    while b:
        a, b = b, a % b
    return a


def py_ext_gcd(a, b):
    if b == 0:
        return (a, 1, 0)
    g, x, y = py_ext_gcd(b, a % b)
    return (g, y, x - (a // b) * y)


def py_isqrt(n):
    import math
    return math.isqrt(n)


def py_is_prime(n):
    if n < 2:
        return False
    for p in (2, 3, 5, 7, 11, 13, 17, 19, 23, 29, 31, 37):
        if n % p == 0:
            return n == p
    d, s = n - 1, 0
    while d % 2 == 0:
        d //= 2
        s += 1
    for a in (2, 3, 5, 7, 11, 13, 17, 19, 23, 29, 31, 37):
        x = pow(a, d, n)
        if x in (1, n - 1):
            continue
        for _ in range(s - 1):
            x = x * x % n
            if x == n - 1:
                break
        else:
            return False
    return True


def py_ntt(xs, invert=False):
    n = len(xs)
    if n == 1:
        return list(xs)
    w = pow(3, (P_NTT - 1) // n, P_NTT)
    if invert:
        w = pow(w, P_NTT - 2, P_NTT)
    e, o = py_ntt(xs[0::2], invert), py_ntt(xs[1::2], invert)
    out = [0] * n
    wk = 1
    for k in range(n // 2):
        t = wk * o[k] % P_NTT
        out[k] = (e[k] + t) % P_NTT
        out[k + n // 2] = (e[k] - t) % P_NTT
        wk = wk * w % P_NTT
    return out


def py_intt(xs):
    n = len(xs)
    inv_n = pow(n, P_NTT - 2, P_NTT)
    return [x * inv_n % P_NTT for x in py_ntt(xs, invert=True)]


def py_convolution(xs, ys):
    n = 1
    while n < len(xs) + len(ys) - 1:
        n *= 2
    fx = py_ntt(xs + [0] * (n - len(xs)))
    fy = py_ntt(ys + [0] * (n - len(ys)))
    return py_intt([a * b % P_NTT for a, b in zip(fx, fy)])[:len(xs) + len(ys) - 1]


# ---- the records --------------------------------------------------------------------------------
def main():
    prove = "--no-prove" not in sys.argv
    OUT.mkdir(exist_ok=True)
    for f in OUT.glob("*.json"):
        f.unlink()

    # 1. gcd — Euclid. Descends by `mod` (not a constant step): the termination checker's first gap.
    build("gcd", r"\a b -> case eq b 0 of { true => a; false => self b (mod a b) }",
          FN([NAT, NAT], NAT),
          [{"args": [nlit(48), nlit(18)], "result": nlit(6)}, {"args": [nlit(17), nlit(5)], "result": nlit(1)},
           {"args": [nlit(0), nlit(9)], "result": nlit(9)}, {"args": [nlit(2**61 - 1), nlit(3 * (2**61 - 1))], "result": nlit(2**61 - 1)},
           {"args": [nlit(1071), nlit(462)], "result": nlit(21)}],
          props=[("commutative", "forall a b. self(a, b) == self(b, a)"),
                 ("right_zero", "forall a. self(a, 0) == a"),
                 ("divides_both", "forall a b. a % self(a, b) == 0 && b % self(a, b) == 0"),
                 ("associative", "forall a b c. self(self(a, b), c) == self(a, self(b, c))")],
          # Positivity: a certified contract `lcm` relies on through composition (its `div` by gcd is sound
          # only because gcd ≥ 1 whenever an argument is).
          refinements=[("post", "(a < 1 && b < 1) || result >= 1")],
          hints=["greatest_common_divisor", "euclid"], complexity="O(log n)")
    gauntlet("gcd", prove)

    # 2. lcm — composes gcd by content address.
    build("lcm", r"\a b -> case eq a 0 of { true => 0; false => case eq b 0 of { true => 0; false => div (mul a b) ({gcd} a b) } }",
          FN([NAT, NAT], NAT),
          [{"args": [nlit(4), nlit(6)], "result": nlit(12)}, {"args": [nlit(21), nlit(6)], "result": nlit(42)},
           {"args": [nlit(0), nlit(5)], "result": nlit(0)}, {"args": [nlit(7), nlit(7)], "result": nlit(7)}],
          props=[("multiple_of_both", "forall a b. self(a, b) % a == 0 && self(a, b) % b == 0"),
                 ("commutative", "forall a b. self(a, b) == self(b, a)")],
          hints=["least_common_multiple"], complexity="O(log n)")
    gauntlet("lcm", prove)

    # 3. ext_gcd — Bézout coefficients as a nested pair (g, (x, y)). Inputs are nats (so the `mod`
    # descent has its floor); the coefficients are ints.
    build("ext_gcd", r"\a b -> case eq b 0 of { true => (a, (int(1), int(0))); false => "
                     r"case self b (mod a b) of { (g, (x, y)) => (g, (y, sub x (mul (div a b) y))) } }",
          FN([NAT, NAT], TUPLE(INT, TUPLE(INT, INT))),
          [{"args": [ilit(240), ilit(46)], "result": tup(ilit(2), tup(ilit(-9), ilit(47)))},
           {"args": [ilit(17), ilit(5)], "result": tup(ilit(1), tup(ilit(-2), ilit(7)))},
           {"args": [ilit(7), ilit(0)], "result": tup(ilit(7), tup(ilit(1), ilit(0)))}],
          # Bézout as a contract on the tuple result: g = a·x + b·y — the prover reads the tuple as an SMT
          # datatype, and the law composes into bezout_holds through the certified contract.
          refinements=[("post", "fst(result) == a * fst(snd(result)) + b * snd(snd(result))")],
          hints=["extended_euclid", "bezout"], complexity="O(log n)")
    gauntlet("ext_gcd", prove)

    # 3b. bezout_holds — the law a*x + b*y = g, as a checker record (predicates cannot project a tuple).
    build("bezout_holds", r"\a b -> case {ext_gcd} a b of { (g, (x, y)) => eq (add (mul a x) (mul b y)) g }",
          FN([INT, INT], BOOL),
          [{"args": [ilit(240), ilit(46)], "result": blit(True)}, {"args": [ilit(-12), ilit(18)], "result": blit(True)},
           {"args": [ilit(0), ilit(0)], "result": blit(True)}],
          # The checker ranges over ALL ints (its −12 example is deliberate: ext_gcd's body works
          # there too), but ext_gcd's Bézout post is CERTIFIED on its declared nat domain only — so
          # the law provable from the contract is the nat-qualified one; the int-domain law stays
          # generative evidence.
          props=[("bezout_identity", "forall a b. self(a, b) == true"),
                 ("bezout_identity_nat", "forall a b. a < 0 || b < 0 || self(a, b) == true")],
          tags=["predicate/check"], hints=["bezout_identity_check"], complexity="O(log n)")
    gauntlet("bezout_holds", prove)

    # 4. mod_inverse — through ext_gcd; None when not coprime.
    build("mod_inverse", r"\a m -> case {ext_gcd} (mod a m) m of { (g, (x, y)) => case eq g 1 of { true => Just(mod x m); false => None } }",
          FN([INT, INT], MAYBE(INT)),
          [{"args": [ilit(3), ilit(11)], "result": just(ilit(4))}, {"args": [ilit(10), ilit(17)], "result": just(ilit(12))},
           {"args": [ilit(6), ilit(9)], "result": NONE}, {"args": [ilit(-1), ilit(7)], "result": just(ilit(6))}],
          refinements=[("pre", "m > 1")],
          hints=["modular_inverse"], complexity="O(log n)")
    gauntlet("mod_inverse", prove)

    # The law is a*x ≡ 1 (mod m), i.e. (a*x) mod m == 1 mod m — NOT `== 1`: in Z/1Z every residue is 0,
    # and the generator found exactly that edge (a = 0, m = 1) when the law was first written with `1`.
    build("inverse_holds", r"\a m -> case {mod_inverse} a m of { Just(x) => eq (mod (mul a x) m) (mod 1 m); None => true }",
          FN([INT, INT], BOOL),
          [{"args": [ilit(3), ilit(11)], "result": blit(True)}, {"args": [ilit(6), ilit(9)], "result": blit(True)}],
          props=[("inverse_identity", "forall a m. self(a, m) == true")],
          tags=["predicate/check"], hints=["modular_inverse_check"], complexity="O(log n)")
    gauntlet("inverse_holds", prove)

    # 4b. The non-recursive modular primitives — the only records whose laws the first-order prover
    # can reach (no `self` recursion, no fn_ref): the measurement of what the SMT rung buys here.
    build("mod_add", r"\a b m -> mod (add (mod a m) (mod b m)) m",
          FN([INT, INT, INT], INT),
          [{"args": [ilit(7), ilit(9), ilit(5)], "result": ilit(1)}, {"args": [ilit(-3), ilit(1), ilit(7)], "result": ilit(5)}],
          props=[("congruent_to_plain_sum", "forall a b m. m == 0 || self(a, b, m) == (a + b) % m"),
                 ("commutative", "forall a b m. m == 0 || self(a, b, m) == self(b, a, m)")],
          refinements=[("pre", "m > 0"), ("post", "result >= 0 && result < m")],
          hints=["modular_addition"], complexity="O(1)")
    gauntlet("mod_add", prove)
    build("mod_mul", r"\a b m -> mod (mul (mod a m) (mod b m)) m",
          FN([INT, INT, INT], INT),
          [{"args": [ilit(7), ilit(9), ilit(5)], "result": ilit(3)}, {"args": [ilit(2**62), ilit(2**62), ilit(2**61 - 1)], "result": ilit(pow(2, 124, 2**61 - 1))}],
          props=[("congruent_to_plain_product", "forall a b m. m == 0 || self(a, b, m) == (a * b) % m"),
                 ("commutative", "forall a b m. m == 0 || self(a, b, m) == self(b, a, m)")],
          refinements=[("pre", "m > 0"), ("pre", f"m <= {I64_MAX}"), ("post", "result >= 0 && result < m")],
          hints=["modular_multiplication"], complexity="O(1)")
    gauntlet("mod_mul", prove)
    build("is_even", r"\n -> eq (mod n 2) 0",
          FN([INT], BOOL),
          [{"args": [ilit(4)], "result": blit(True)}, {"args": [ilit(-3)], "result": blit(False)}, {"args": [ilit(0)], "result": blit(True)}],
          props=[("period_two", "forall n. self(n) == self(n + 2)"), ("alternates", "forall n. self(n) != self(n + 1)")],
          tags=["predicate/number"], hints=["even", "parity"], complexity="O(1)")
    gauntlet("is_even", prove)

    # 5. pow_mod_naive — the reference: constant-step descent on the exponent (termination SOUND).
    build("pow_mod_naive", r"\b e m -> case gt e 0 of { true => mod (mul (self b (sub e 1) m) (mod b m)) m; false => mod 1 m }",
          FN([INT, NAT, INT], INT),
          [{"args": [ilit(2), nlit(10), ilit(1000)], "result": ilit(24)}, {"args": [ilit(3), nlit(0), ilit(7)], "result": ilit(1)},
           {"args": [ilit(-2), nlit(3), ilit(7)], "result": ilit(6)}],
          refinements=[("pre", "m > 0"), ("post", "result >= 0 && result < m")],
          hints=["modular_power_linear"], complexity="O(n)")
    gauntlet("pow_mod_naive", prove)

    # 6. mod_pow — square-and-multiply: halving descent (O(log n) to the complexity checker; the
    # termination checker's second gap). Modulus bounded so x*x stays inside i128 (measured: i128
    # multiplication wraps silently).
    build("mod_pow", r"\b e m -> case gt e 0 of { true => let h = self b (div e 2) m in let s = mod (mul h h) m in "
                     r"case eq (mod e 2) 0 of { true => s; false => mod (mul s (mod b m)) m }; false => mod 1 m }",
          FN([INT, NAT, INT], INT),
          [{"args": [ilit(2), nlit(10), ilit(1000)], "result": ilit(24)},
           {"args": [ilit(3), nlit(200), ilit(1000000007)], "result": ilit(pow(3, 200, 1000000007))},
           {"args": [ilit(2), nlit(2**62), ilit(2**61 - 1)], "result": ilit(pow(2, 2**62, 2**61 - 1))},
           {"args": [ilit(5), nlit(0), ilit(13)], "result": ilit(1)},
           {"args": [ilit(7), nlit(1), ilit(1)], "result": ilit(0)}],
          refinements=[("pre", "m > 0"), ("pre", f"m <= {I64_MAX}"), ("post", "result >= 0 && result < m")],
          hints=["modular_exponentiation", "square_and_multiply"], complexity="O(log n)")
    gauntlet("mod_pow", prove)

    build("pow_mod_agree", r"\b e m -> eq ({mod_pow} b e m) ({pow_mod_naive} b e m)",
          FN([INT, NAT, INT], BOOL),
          [{"args": [ilit(2), nlit(10), ilit(1000)], "result": blit(True)}, {"args": [ilit(-3), nlit(5), ilit(11)], "result": blit(True)}],
          props=[("fast_equals_naive", "forall b e m. self(b, e, m) == true")],
          tags=["predicate/check"], hints=["modular_power_agreement"], complexity="O(n)")
    gauntlet("pow_mod_agree", prove)

    # 7. isqrt — quartering descent, correction step; total on every int (negatives return themselves).
    build("isqrt", r"\n -> case lt n 2 of { true => n; false => let r = mul 2 (self (div n 4)) in "
                   r"case le (mul (add r 1) (add r 1)) n of { true => add r 1; false => r } }",
          FN([NAT], NAT),
          [{"args": [nlit(0)], "result": nlit(0)}, {"args": [nlit(15)], "result": nlit(3)}, {"args": [nlit(16)], "result": nlit(4)},
           {"args": [nlit(10**18)], "result": nlit(10**9)}, {"args": [nlit(2**100)], "result": nlit(2**50)},
           {"args": [nlit(2**100 - 1)], "result": nlit(2**50 - 1)}],
          props=[("square_root_of_square", "forall n. n < 0 || self(n * n) == n"),
                 ("floor_bounds", "forall n. n < 0 || (self(n) * self(n) <= n && n < (self(n) + 1) * (self(n) + 1))")],
          refinements=[("post", "result * result <= n"), ("post", "result <= n")],  # the second is the size bound the complexity fold reads
          hints=["integer_square_root"], complexity="O(log n)")
    gauntlet("isqrt", prove)

    # 8. Deterministic Miller–Rabin for n < 2^63 (twelve prime bases; mod_pow's bound), plus trial division.
    build("mr_squarings", r"\n x s -> case gt s 0 of { true => let y = mod (mul x x) n in case eq y (sub n 1) of { true => true; false => self n y (sub s 1) }; false => false }",
          FN([NAT, NAT, NAT], BOOL),
          [{"args": [nlit(561), nlit(263), nlit(3)], "result": blit(False)}, {"args": [nlit(13), nlit(5), nlit(1)], "result": blit(True)}],
          hints=["miller_rabin_squarings"], complexity="O(n)")
    gauntlet("mr_squarings", prove)
    build("mr_witness", r"\n d s a -> let x = {mod_pow} a d n in case eq x 1 of { true => true; false => case eq x (sub n 1) of { true => true; false => {mr_squarings} n x (sub s 1) } }",
          FN([NAT, NAT, NAT, NAT], BOOL),
          [{"args": [nlit(561), nlit(35), nlit(4), nlit(2)], "result": blit(False)}, {"args": [nlit(2047), nlit(1023), nlit(1), nlit(2)], "result": blit(True)},
           {"args": [nlit(2047), nlit(1023), nlit(1), nlit(3)], "result": blit(False)}],
          hints=["miller_rabin_witness"], complexity="O(n)")
    gauntlet("mr_witness", prove)
    # n − 1 = d · 2^s as TWO scalar records rather than one tuple: a tuple component carries no certified
    # bound, while each scalar carries a postcondition (`result <= d`) the numeric-induction rung proves —
    # and a proved bound is what lets the complexity fold read `mr_all`'s arguments as size-bounded.
    build("odd_part_d", r"\d -> case gt d 0 of { true => case eq (mod d 2) 0 of { true => self (div d 2); false => d }; false => d }",
          FN([NAT], NAT),
          [{"args": [nlit(560)], "result": nlit(35)}, {"args": [nlit(1)], "result": nlit(1)}, {"args": [nlit(1024)], "result": nlit(1)}, {"args": [nlit(0)], "result": nlit(0)}],
          props=[("odd_or_zero", "forall d. d < 1 || self(d) % 2 == 1")],
          refinements=[("post", "result <= d")],
          hints=["odd_part", "split_powers_of_two"], complexity="O(log n)")
    gauntlet("odd_part_d", prove)
    build("odd_part_s", r"\d -> case gt d 0 of { true => case eq (mod d 2) 0 of { true => add 1 (self (div d 2)); false => 0 }; false => 0 }",
          FN([NAT], NAT),
          [{"args": [nlit(560)], "result": nlit(4)}, {"args": [nlit(1)], "result": nlit(0)}, {"args": [nlit(1024)], "result": nlit(10)}, {"args": [nlit(0)], "result": nlit(0)}],
          refinements=[("post", "result <= d")],
          hints=["two_adic_valuation", "trailing_zero_count"], complexity="O(log n)")
    gauntlet("odd_part_s", prove)
    build("mr_all", r"\n d s bases -> case null bases of { true => true; false => let a = head bases in case ge a n of { true => true; false => "
                    r"case {mr_witness} n d s a of { true => self n d s (tail bases); false => false } } }",
          FN([NAT, NAT, NAT, LIST(NAT)], BOOL),
          [{"args": [nlit(561), nlit(35), nlit(4), nlist(2, 3)], "result": blit(False)}, {"args": [nlit(13), nlit(3), nlit(2), nlist(2, 3, 5)], "result": blit(True)}],
          hints=["miller_rabin_all_bases"], complexity="O(n^2)")  # list of bases × a witness test the checker reads as O(n)
    gauntlet("mr_all", prove)
    build("is_prime", r"\n -> case lt n 2 of { true => false; false => case lt n 4 of { true => true; false => case eq (mod n 2) 0 of { true => false; false => "
                      r"let d = {odd_part_d} (sub n 1) in let s = {odd_part_s} (sub n 1) in {mr_all} n d s (cons 2 (cons 3 (cons 5 (cons 7 (cons 11 (cons 13 (cons 17 (cons 19 (cons 23 (cons 29 (cons 31 (cons 37 nil)))))))))))) } } }",
          FN([NAT], BOOL),
          [{"args": [nlit(n)], "result": blit(py_is_prime(n))} for n in
           (0, 1, 2, 3, 4, 9, 37, 561, 1105, 2047, 1373653, 25326001, 3215031751, 1000000007, 3825123056546413051, 2**61 - 1, 9223372036854775783, 9223372036854775781)],
          refinements=[("pre", f"n <= {I64_MAX}")],
          # O(n) in the checker's measure: the squaring loop descends `s` by a constant step, and the
          # checker reads a constant-step recursion as linear in ITS input (it cannot know s ≤ log n).
          tags=["predicate/number"], hints=["primality", "miller_rabin"], complexity="O(n^2)")
    gauntlet("is_prime", prove)
    build("no_divisor_from", r"\n d -> case lt d 2 of { true => true; false => case eq (mod n d) 0 of { true => false; false => self n (sub d 1) } }",
          FN([NAT, NAT], BOOL),
          [{"args": [nlit(91), nlit(9)], "result": blit(False)}, {"args": [nlit(97), nlit(9)], "result": blit(True)}],
          hints=["trial_division_downward"], complexity="O(n)")
    gauntlet("no_divisor_from", prove)
    build("is_prime_trial", r"\n -> case lt n 2 of { true => false; false => {no_divisor_from} n ({isqrt} n) }",
          FN([NAT], BOOL),
          [{"args": [nlit(n)], "result": blit(py_is_prime(n))} for n in (0, 1, 2, 9, 97, 561, 7919, 1000003)],
          tags=["predicate/number"], hints=["primality_trial_division"], complexity="O(n)")
    gauntlet("is_prime_trial", prove)
    build("prime_agree", r"\n -> eq ({is_prime} n) ({is_prime_trial} n)",
          FN([NAT], BOOL),
          [{"args": [nlit(n)], "result": blit(True)} for n in (0, 2, 561, 2047, 1000003)],
          props=[("miller_rabin_agrees_with_trial", "forall n. self(n) == true")],
          tags=["predicate/check"], hints=["primality_agreement"], complexity="O(n^2)")
    gauntlet("prime_agree", prove)

    # 9. Chinese remainder for two coprime moduli.
    build("crt2", r"\a1 m1 a2 m2 -> case {mod_inverse} m1 m2 of { Just(inv) => let t = mod (mul (sub a2 a1) inv) m2 in Just(mod (add a1 (mul m1 t)) (mul m1 m2)); None => None }",
          FN([INT, INT, INT, INT], MAYBE(INT)),
          [{"args": [ilit(2), ilit(3), ilit(3), ilit(5)], "result": just(ilit(8))}, {"args": [ilit(1), ilit(4), ilit(3), ilit(6)], "result": NONE},
           {"args": [ilit(0), ilit(7), ilit(6), ilit(11)], "result": just(ilit(28))}],
          refinements=[("pre", "m1 > 1 && m2 > 1")],
          hints=["chinese_remainder"], complexity="O(log n)")
    gauntlet("crt2", prove)
    build("crt_holds", r"\a1 m1 a2 m2 -> case {crt2} a1 m1 a2 m2 of { Just(x) => and (eq (mod x m1) (mod a1 m1)) (eq (mod x m2) (mod a2 m2)); None => true }",
          FN([INT, INT, INT, INT], BOOL),
          [{"args": [ilit(2), ilit(3), ilit(3), ilit(5)], "result": blit(True)}],
          props=[("solves_both_congruences", "forall a1 m1 a2 m2. self(a1, m1, a2, m2) == true")],
          tags=["predicate/check"], hints=["chinese_remainder_check"], complexity="O(log n)")
    gauntlet("crt_holds", prove)

    # 10. Digits (little-endian) and back; base < 2 yields nothing rather than looping.
    build("digits", r"\b n -> case lt b 2 of { true => nil; false => case lt n 1 of { true => nil; false => cons (mod n b) (self b (div n b)) } }",
          FN([NAT, NAT], LIST(NAT)),
          [{"args": [nlit(10), nlit(1234)], "result": nlist(4, 3, 2, 1)}, {"args": [nlit(2), nlit(10)], "result": nlist(0, 1, 0, 1)},
           {"args": [nlit(10), nlit(0)], "result": nlist()}, {"args": [nlit(16), nlit(255)], "result": nlist(15, 15)}],
          hints=["to_digits", "digits_little_endian"], complexity="O(log n)")
    gauntlet("digits", prove)
    build("from_digits", r"\b ds -> case null ds of { true => 0; false => add (head ds) (mul b (self b (tail ds))) }",
          FN([NAT, LIST(NAT)], NAT),
          [{"args": [nlit(10), nlist(4, 3, 2, 1)], "result": nlit(1234)}, {"args": [nlit(2), nlist(0, 1, 0, 1)], "result": nlit(10)},
           {"args": [nlit(10), nlist()], "result": nlit(0)}],
          hints=["from_digits_little_endian"], complexity="O(n)")
    gauntlet("from_digits", prove)
    build("digits_roundtrip", r"\b n -> case lt b 2 of { true => true; false => case lt n 0 of { true => true; false => eq ({from_digits} b ({digits} b n)) n } }",
          FN([NAT, NAT], BOOL),
          [{"args": [nlit(10), nlit(1234)], "result": blit(True)}, {"args": [nlit(2), nlit(0)], "result": blit(True)}],
          props=[("digits_then_from_digits", "forall b n. self(b, n) == true")],
          tags=["predicate/check"], hints=["digits_roundtrip_check"], complexity="O(n)")  # from_digits is linear in the digit list
    gauntlet("digits_roundtrip", prove)
    build("digit_sum", r"\b n -> case lt b 2 of { true => 0; false => case lt n 1 of { true => 0; false => add (mod n b) (self b (div n b)) } }",
          FN([NAT, NAT], NAT),
          [{"args": [nlit(10), nlit(1234)], "result": nlit(10)}, {"args": [nlit(10), nlit(0)], "result": nlit(0)}, {"args": [nlit(2), nlit(255)], "result": nlit(8)}],
          props=[("sum_bounded_by_value", "forall b n. n < 0 || self(b, n) <= n")],
          hints=["sum_of_digits"], complexity="O(log n)")
    gauntlet("digit_sum", prove)

    # 11. Binomial coefficients by the multiplicative formula (exact at every step); C(n, k) = 0 past n.
    build("binomial", r"\n k -> case gt k 0 of { true => div (mul (self n (sub k 1)) (add (sub n k) 1)) k; false => 1 }",
          FN([NAT, NAT], NAT),
          [{"args": [nlit(5), nlit(2)], "result": nlit(10)}, {"args": [nlit(10), nlit(0)], "result": nlit(1)}, {"args": [nlit(4), nlit(5)], "result": nlit(0)},
           {"args": [nlit(60), nlit(30)], "result": nlit(118264581564861424)}, {"args": [nlit(100), nlit(50)], "result": nlit(100891344545564193334812497256)}],
          props=[("symmetry", "forall n k. n < 0 || k < 0 || k > n || self(n, k) == self(n, n - k)"),
                 ("pascal", "forall n k. n < 0 || k < 0 || self(n + 1, k + 1) == self(n, k) + self(n, k + 1)"),
                 ("choose_zero", "forall n. self(n, 0) == 1")],
          hints=["binomial_coefficient", "choose"], complexity="O(n)")
    gauntlet("binomial", prove)

    # 12. The number-theoretic transform over Z/pZ, p = 998244353 — the FFT's integer cousin, by
    # recursive even/odd splitting over lists (the butterfly is a zip that builds both halves).
    build("ntt_split", r"\xs -> case null xs of { true => (nil, nil); false => case null (tail xs) of { true => (cons (head xs) nil, nil); false => "
                       r"case self (tail (tail xs)) of { (ev, od) => (cons (head xs) ev, cons (head (tail xs)) od) } } }",
          FN([LIST(INT)], TUPLE(LIST(INT), LIST(INT))),
          [{"args": [ilist(1, 2, 3, 4)], "result": tup(ilist(1, 3), ilist(2, 4))}, {"args": [ilist(7)], "result": tup(ilist(7), ilist())},
           {"args": [ilist()], "result": tup(ilist(), ilist())}],
          tags=["transform/list"], hints=["deinterleave", "evens_odds"], complexity="O(n)")
    gauntlet("ntt_split", prove)
    build("ntt_butterfly", r"\e o wk w -> case null e of { true => (nil, nil); false => case null o of { true => (nil, nil); false => "
                           rf"let t = mod (mul wk (head o)) {P_NTT} in case self (tail e) (tail o) (mod (mul wk w) {P_NTT}) w of {{ (lo, hi) => "
                           rf"(cons (mod (add (head e) t) {P_NTT}) lo, cons (mod (sub (add (head e) {P_NTT}) t) {P_NTT}) hi) }} }} }}",
          FN([LIST(INT), LIST(INT), INT, INT], TUPLE(LIST(INT), LIST(INT))),
          [{"args": [ilist(3, 7), ilist(2, 5), ilit(1), ilit(998244352)], "result": tup(ilist(5, 2), ilist(1, 12))}],
          tags=["transform/list"], hints=["butterfly"], complexity="O(n)")
    gauntlet("ntt_butterfly", prove)
    # The recursion on the halves `ntt_split` returns is not a descent of ntt_rec's own parameter, and no
    # certified fact says a half is shorter — so the depth is carried EXPLICITLY: `levels` = log2 of the
    # length, a nat the recursion counts down (termination SOUND by the nat descent; the checker then reads
    # the two-way branching on a constant descent as exponential in `levels`, which IS n — recorded).
    build("log2_floor", r"\n -> case lt n 2 of { true => 0; false => add 1 (self (div n 2)) }",
          FN([NAT], NAT),
          [{"args": [nlit(1)], "result": nlit(0)}, {"args": [nlit(8)], "result": nlit(3)}, {"args": [nlit(1000)], "result": nlit(9)}, {"args": [nlit(2**62)], "result": nlit(62)}],
          props=[("below_power", "forall n. n < 1 || 2 * self(n) <= n + n")],
          hints=["integer_log2", "bit_length_minus_one"], complexity="O(log n)")
    gauntlet("log2_floor", prove)
    # Recursion on the halves `ntt_split` returns: the checkers read `ntt_split`'s body as returning
    # components strictly shorter than its input once it has ≥ 2 elements (the `null (tail xs)` guard),
    # so the recursion is a halving descent of `xs` — termination SOUND, complexity 2T(n/2) + O(n).
    build("ntt_rec", rf"\xs w -> case null xs of {{ true => nil; false => case null (tail xs) of {{ true => xs; false => case {{ntt_split}} xs of {{ (ev, od) => "
                     rf"let w2 = mod (mul w w) {P_NTT} in case {{ntt_butterfly}} (self ev w2) (self od w2) 1 w of {{ (lo, hi) => append lo hi }} }} }} }}",
          FN([LIST(INT), INT], LIST(INT)),
          [{"args": [ilist(1, 2, 3, 4), ilit(pow(3, (P_NTT - 1) // 4, P_NTT))], "result": ilist(*py_ntt([1, 2, 3, 4]))},
           {"args": [ilist(5), ilit(1)], "result": ilist(5)}],
          tags=["transform/list"], hints=["number_theoretic_transform_recursive"], complexity="O(n log n)")
    gauntlet("ntt_rec", prove)
    build("ntt", rf"\xs -> {{ntt_rec}} xs ({{mod_pow}} 3 (div {P_NTT - 1} (length xs)) {P_NTT})",
          FN([LIST(INT)], LIST(INT)),
          [{"args": [ilist(*v)], "result": ilist(*py_ntt(v))} for v in ([1, 2, 3, 4], [1, 0, 0, 0, 0, 0, 0, 0], [5, 6, 7, 8, 9, 10, 11, 12], [1])],
          refinements=[("pre", "length(xs) > 0")],
          tags=["transform/list"], hints=["number_theoretic_transform", "fft_mod_p"], complexity="O(n log n)")
    gauntlet("ntt", prove)
    # Scaling by a constant as a first-order structural recursion: `map` with a lambda is a higher-order
    # application the termination and complexity checkers cannot see through.
    build("scale_mod", rf"\xs c -> case null xs of {{ true => nil; false => cons (mod (mul (head xs) c) {P_NTT}) (self (tail xs) c) }}",
          FN([LIST(INT), INT], LIST(INT)),
          [{"args": [ilist(1, 2, 3), ilit(2)], "result": ilist(2, 4, 6)}, {"args": [ilist(), ilit(5)], "result": ilist()}],
          props=[("length_preserving", "forall xs c. length(self(xs, c)) == length(xs)")],
          tags=["transform/list"], hints=["scale_mod_p"], complexity="O(n)")
    gauntlet("scale_mod", prove)
    build("ntt_inverse", rf"\xs -> let n = length xs in let inv_n = {{mod_pow}} n {P_NTT - 2} {P_NTT} in "
                         rf"{{scale_mod}} ({{ntt_rec}} xs ({{mod_pow}} ({{mod_pow}} 3 (div {P_NTT - 1} n) {P_NTT}) {P_NTT - 2} {P_NTT})) inv_n",
          FN([LIST(INT)], LIST(INT)),
          [{"args": [ilist(*py_ntt(v))], "result": ilist(*v)} for v in ([1, 2, 3, 4], [5, 6, 7, 8, 9, 10, 11, 12])],
          refinements=[("pre", "length(xs) > 0")],
          tags=["transform/list"], hints=["inverse_number_theoretic_transform"], complexity="O(n log n)")
    gauntlet("ntt_inverse", prove)
    build("ntt_roundtrip", r"\xs -> eq ({ntt_inverse} ({ntt} xs)) xs",
          FN([LIST(INT)], BOOL),
          [{"args": [ilist(*v)], "result": blit(True)} for v in ([1, 2, 3, 4], [0, 0], [9], [1, 2, 3, 4, 5, 6, 7, 8])],
          tags=["predicate/check"], hints=["ntt_roundtrip_check"], complexity="O(n log n)")
    gauntlet("ntt_roundtrip", prove)
    build("pointwise_mod", rf"\xs ys -> case null xs of {{ true => nil; false => case null ys of {{ true => nil; false => cons (mod (mul (head xs) (head ys)) {P_NTT}) (self (tail xs) (tail ys)) }} }}",
          FN([LIST(INT), LIST(INT)], LIST(INT)),
          [{"args": [ilist(1, 2, 3), ilist(4, 5, 6)], "result": ilist(4, 10, 18)}],
          props=[("length_is_min", "forall xs ys. length(self(xs, ys)) <= length(xs) && length(self(xs, ys)) <= length(ys)")],
          tags=["transform/list"], hints=["pointwise_product_mod_p"], complexity="O(n)")
    gauntlet("pointwise_mod", prove)
    build("pad_to", r"\xs n -> case gt n 0 of { true => case null xs of { true => cons int(0) (self nil (sub n 1)); false => cons (head xs) (self (tail xs) (sub n 1)) }; false => nil }",
          FN([LIST(INT), NAT], LIST(INT)),
          [{"args": [ilist(1, 2), nlit(4)], "result": ilist(1, 2, 0, 0)}, {"args": [ilist(1, 2, 3), nlit(2)], "result": ilist(1, 2)}],
          props=[("length_is_n", "forall xs n. n < 0 || length(self(xs, n)) == n")],
          tags=["transform/list"], hints=["pad_or_truncate"], complexity="O(n)")
    gauntlet("pad_to", prove)
    build("next_pow2", r"\n -> case lt n 2 of { true => 1; false => mul 2 (self (div (add n 1) 2)) }",
          FN([NAT], NAT),
          [{"args": [nlit(5)], "result": nlit(8)}, {"args": [nlit(8)], "result": nlit(8)}, {"args": [nlit(1)], "result": nlit(1)}, {"args": [nlit(0)], "result": nlit(1)}],
          props=[("at_least_n", "forall n. self(n) >= n"), ("under_twice_n", "forall n. n < 1 || self(n) < 2 * n")],
          # The size bound the complexity fold reads for `pad_to xs (next_pow2 …)` in convolution. The
          # linear forms `result <= 2 * n + 1` / `result <= 2 * n` do NOT close by induction (the step
          # doubles a loose hypothesis); `max(1, 2n - 1)` is the inductive one (and exact at n = 0).
          refinements=[("post", "result <= max(1, 2 * n - 1)")],
          hints=["next_power_of_two"], complexity="O(log n)")
    gauntlet("next_pow2", prove)
    build("convolution", r"\xs ys -> let n = {next_pow2} (sub (add (length xs) (length ys)) 1) in "
                         r"{pad_to} ({ntt_inverse} ({pointwise_mod} ({ntt} ({pad_to} xs n)) ({ntt} ({pad_to} ys n)))) (sub (add (length xs) (length ys)) 1)",
          FN([LIST(INT), LIST(INT)], LIST(INT)),
          [{"args": [ilist(1, 2, 3), ilist(4, 5, 6)], "result": ilist(*py_convolution([1, 2, 3], [4, 5, 6]))},
           {"args": [ilist(1, 1), ilist(1, 1)], "result": ilist(1, 2, 1)},
           {"args": [ilist(2, 0, 1), ilist(3, 7)], "result": ilist(*py_convolution([2, 0, 1], [3, 7]))}],
          refinements=[("pre", "length(xs) > 0 && length(ys) > 0")],
          tags=["transform/list"], hints=["polynomial_multiplication_mod_p", "fft_convolution"], complexity="O(n log n)")
    gauntlet("convolution", prove)

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
