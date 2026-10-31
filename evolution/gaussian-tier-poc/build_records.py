#!/usr/bin/env python3
"""Author the Gaussian-integer tier records (evolution/gaussian-tier-poc) and run every record
through the verified-by-default gauntlet, writing the records, bodies and a machine-readable report.
Gaussian integers a + b·i as `(int, int)` pairs: add/sub/neg/conj/mul, the norm a² + b², units,
nearest-integer division with the Euclidean remainder (N(r) < N(w)), and Euclid's algorithm on the
norm. Pure, in-language bodies in surface syntax; no new builtin; nothing applied from another
module — this tier's question is NONLINEAR: every fact about the norm is a polynomial identity or
inequality of degree 2–4, and the remainder's bound is the module's headline theorem.

    python3 evolution/gaussian-tier-poc/build_records.py              # mint + gauntlet (needs z3)
    python3 evolution/gaussian-tier-poc/build_records.py --no-prove   # skip the `prove` rungs
    python3 evolution/gaussian-tier-poc/build_records.py --parse-only # mint only (syntax check)

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
INT, BOOL = B("int"), B("bool")
TUPLE = lambda *ts: {"kind": "tuple", "elems": list(ts)}  # noqa: E731
FN = lambda ps, r: {"kind": "fn", "params": ps, "result": r}  # noqa: E731
GI = TUPLE(INT, INT)


def ilit(v):
    return {"kind": "int", "value": v if abs(v) < 2**53 else str(v)}


blit = lambda b: {"kind": "bool", "value": b}  # noqa: E731
tup = lambda *vs: {"kind": "tuple", "elems": list(vs)}  # noqa: E731
gi = lambda a, b: tup(ilit(a), ilit(b))  # noqa: E731

ADDR = {}      # name -> fn_ address, for `{name}` placeholders in later bodies and predicates
REPORT = []
PARSE_ONLY = "--parse-only" in sys.argv


def build(name, surface, sig, examples, props=(), refinements=(), tags=(), hints=(), complexity=None,
          terminates="always"):
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
        "intent_tags": ["arithmetic/gaussian", "math/gaussian-integer", f"math/gaussian-integer/{name.replace('_', '-')}"] + list(tags),
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
    if PARSE_ONLY:
        print(f"{name:22} {record['hash'][:24]}…  {len(record['properties'])} laws, {len(refinements)} refinements")
    return record


def gauntlet(name, prove=True):
    if PARSE_ONLY:
        return
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
    print(f"{name:22} {row['run']:6} certified={row['certified']} {short}")
    for ln in row.get("properties_generative", []):
        print("    gen  ", ln[:150])
    for ln in row.get("prove", []):
        print("    prove", ln[:150])


# ---- reference values (plain Python integers) -----------------------------------------------------
def mul(a, b, c, d):
    return (a * c - b * d, a * d + b * c)


def norm(a, b):
    return a * a + b * b


def round_div(n, d):
    assert d >= 1
    return (2 * n + d) // (2 * d)


def div(a, b, c, d):
    n = norm(c, d)
    return (round_div(a * c + b * d, n), round_div(b * c - a * d, n))


def rem(a, b, c, d):
    q1, q2 = div(a, b, c, d)
    m1, m2 = mul(q1, q2, c, d)
    return (a - m1, b - m2)


def gcd(a, b, c, d):
    while (c, d) != (0, 0):
        (a, b), (c, d) = (c, d), rem(a, b, c, d)
    return (a, b)


def ex(args, result):
    return {"args": [ilit(a) for a in args], "result": result}


def main():
    prove = "--no-prove" not in sys.argv
    OUT.mkdir(exist_ok=True)
    for f in OUT.glob("*.json"):
        f.unlink()

    # 1. The additive group: componentwise, exact posts, the laws linear.
    build("gi_add", r"\a b c d -> (add a c, add b d)", FN([INT, INT, INT, INT], GI),
          [ex((1, 2, 3, 4), gi(4, 6)), ex((-1, 0, 1, 0), gi(0, 0)), ex((0, 5, 0, -7), gi(0, -2))],
          refinements=[("post", "fst(result) == a + c"), ("post", "snd(result) == b + d")],
          props=[("commutative", "forall a b c d. self(a, b, c, d) == self(c, d, a, b)"),
                 ("associative", "forall a b c d e f. self(fst(self(a, b, c, d)), snd(self(a, b, c, d)), e, f) == "
                                 "self(a, b, fst(self(c, d, e, f)), snd(self(c, d, e, f)))"),
                 ("zero_identity", "forall a b. self(a, b, 0, 0) == self(0, 0, a, b) && fst(self(a, b, 0, 0)) == a && snd(self(a, b, 0, 0)) == b")],
          hints=["gaussian_add", "complex_integer_add"], complexity="O(1)")
    gauntlet("gi_add", prove)

    build("gi_sub", r"\a b c d -> (sub a c, sub b d)", FN([INT, INT, INT, INT], GI),
          [ex((1, 2, 3, 4), gi(-2, -2)), ex((5, 5, 5, 5), gi(0, 0)), ex((0, 0, 1, -1), gi(-1, 1))],
          refinements=[("post", "fst(result) == a - c"), ("post", "snd(result) == b - d")],
          props=[("self_difference_is_zero", "forall a b. fst(self(a, b, a, b)) == 0 && snd(self(a, b, a, b)) == 0")],
          hints=["gaussian_sub"], complexity="O(1)")
    gauntlet("gi_sub", prove)

    build("gi_neg", r"\a b -> (neg a, neg b)", FN([INT, INT], GI),
          [ex((1, 2), gi(-1, -2)), ex((0, 0), gi(0, 0)), ex((-3, 4), gi(3, -4))],
          refinements=[("post", "fst(result) == 0 - a"), ("post", "snd(result) == 0 - b")],
          props=[("involution", "forall a b. fst(self(fst(self(a, b)), snd(self(a, b)))) == a && snd(self(fst(self(a, b)), snd(self(a, b)))) == b")],
          hints=["gaussian_negate"], complexity="O(1)")
    gauntlet("gi_neg", prove)

    # 2. Conjugation: the imaginary part flips; an involution; the norm is z·conj(z).
    build("gi_conj", r"\a b -> (a, neg b)", FN([INT, INT], GI),
          [ex((1, 2), gi(1, -2)), ex((0, 0), gi(0, 0)), ex((-3, -4), gi(-3, 4))],
          refinements=[("post", "fst(result) == a"), ("post", "snd(result) == 0 - b")],
          props=[("involution", "forall a b. fst(self(fst(self(a, b)), snd(self(a, b)))) == a && snd(self(fst(self(a, b)), snd(self(a, b)))) == b")],
          hints=["gaussian_conjugate", "complex_conjugate"], complexity="O(1)")
    gauntlet("gi_conj", prove)

    # 3. Multiplication: (a + bi)(c + di) = (ac − bd) + (ad + bc)i. Exact posts; the laws are
    # polynomial identities — commutativity (degree 2), associativity (degree 3, six variables).
    build("gi_mul", r"\a b c d -> (sub (mul a c) (mul b d), add (mul a d) (mul b c))", FN([INT, INT, INT, INT], GI),
          [ex((1, 2, 3, 4), gi(*mul(1, 2, 3, 4))), ex((0, 1, 0, 1), gi(-1, 0)), ex((2, 3, 2, -3), gi(13, 0)),
           ex((5, -7, 0, 0), gi(0, 0)), ex((5, -7, 1, 0), gi(5, -7))],
          refinements=[("post", "fst(result) == a * c - b * d"), ("post", "snd(result) == a * d + b * c")],
          props=[("commutative", "forall a b c d. self(a, b, c, d) == self(c, d, a, b)"),
                 ("one_identity", "forall a b. fst(self(a, b, 1, 0)) == a && snd(self(a, b, 1, 0)) == b"),
                 ("i_squared_is_minus_one", "fst(self(0, 1, 0, 1)) == 0 - 1 && snd(self(0, 1, 0, 1)) == 0"),
                 ("zero_annihilates", "forall c d. fst(self(0, 0, c, d)) == 0 && snd(self(0, 0, c, d)) == 0"),
                 ("associative", "forall a b c d e f. self(fst(self(a, b, c, d)), snd(self(a, b, c, d)), e, f) == "
                                 "self(a, b, fst(self(c, d, e, f)), snd(self(c, d, e, f)))")],
          hints=["gaussian_mul", "complex_integer_mul"], complexity="O(1)")
    gauntlet("gi_mul", prove)

    # 4. The norm N(a + bi) = a² + b²: non-negative (a sum of squares — nonlinear), zero only at
    # zero, invariant under conjugation (a law that APPLIES gi_conj by address).
    build("gi_norm", r"\a b -> add (mul a a) (mul b b)", FN([INT, INT], INT),
          [ex((3, 4), ilit(25)), ex((0, 0), ilit(0)), ex((-1, 1), ilit(2)), ex((1, 0), ilit(1)), ex((10**9, 10**9), ilit(2 * 10**18))],
          refinements=[("post", "result >= 0"), ("post", "result == a * a + b * b")],
          props=[("zero_only_at_zero", "forall a b. self(a, b) != 0 || (a == 0 && b == 0)"),
                 ("positive_off_zero", "forall a b. (a == 0 && b == 0) || self(a, b) >= 1"),
                 ("conj_invariant", "forall a b. self(fst({gi_conj}(a, b)), snd({gi_conj}(a, b))) == self(a, b)")],
          hints=["gaussian_norm", "complex_norm_squared"], complexity="O(1)")
    gauntlet("gi_norm", prove)

    # 4b. The norm is multiplicative — the Brahmagupta–Fibonacci identity, degree 4 — as a checker
    # that composes gi_mul and gi_norm by address; and conjugation distributes over products.
    build("gi_norm_mul_holds",
          r"\a b c d -> case {gi_mul} a b c d of { (p, q) => eq ({gi_norm} p q) (mul ({gi_norm} a b) ({gi_norm} c d)) }",
          FN([INT, INT, INT, INT], BOOL),
          [ex((1, 2, 3, 4), blit(True)), ex((0, 1, 0, 1), blit(True)), ex((-5, 7, 2, -9), blit(True))],
          props=[("norm_multiplicative", "forall a b c d. self(a, b, c, d) == true")],
          tags=["predicate/check"], hints=["norm_multiplicative_check", "brahmagupta_fibonacci"], complexity="O(1)")
    gauntlet("gi_norm_mul_holds", prove)

    build("gi_conj_mul_holds",
          r"\a b c d -> case {gi_mul} a b c d of { (p, q) => case {gi_conj} p q of { (p1, q1) => "
          r"case {gi_mul} a (neg b) c (neg d) of { (p2, q2) => and (eq p1 p2) (eq q1 q2) } } }",
          FN([INT, INT, INT, INT], BOOL),
          [ex((1, 2, 3, 4), blit(True)), ex((0, 1, 0, 1), blit(True)), ex((-5, 7, 2, -9), blit(True))],
          props=[("conj_multiplicative", "forall a b c d. self(a, b, c, d) == true")],
          tags=["predicate/check"], hints=["conjugation_multiplicative_check"], complexity="O(1)")
    gauntlet("gi_conj_mul_holds", prove)

    # 5. Units: norm 1, i.e. ±1 and ±i — the law says a unit lies on an axis (a² + b² = 1 with both
    # nonzero would be ≥ 2: integer nonlinear reasoning).
    build("gi_is_unit", r"\a b -> eq ({gi_norm} a b) 1", FN([INT, INT], BOOL),
          [ex((1, 0), blit(True)), ex((0, -1), blit(True)), ex((1, 1), blit(False)), ex((0, 0), blit(False))],
          props=[("units_lie_on_an_axis", "forall a b. self(a, b) == false || a == 0 || b == 0"),
                 ("the_four_units", "self(1, 0) == true && self(0 - 1, 0) == true && self(0, 1) == true && self(0, 0 - 1) == true")],
          tags=["predicate/check"], hints=["gaussian_unit"], complexity="O(1)")
    gauntlet("gi_is_unit", prove)

    # 6. Nearest-integer division of ints, ties upward: round(n / d) = ⌊(2n + d) / 2d⌋ for d ≥ 1.
    # Its contract is the rounding bound −d ≤ 2(n − q·d) < d — the Euclidean rung's territory with a
    # product q·d in it.
    build("gi_round_div", r"\n d -> div (add (mul 2 n) d) (mul 2 d)", FN([INT, INT], INT),
          [ex((7, 2), ilit(4)), ex((5, 2), ilit(3)), ex((-7, 2), ilit(-3)), ex((-5, 2), ilit(-2)), ex((6, 3), ilit(2)),
           ex((7, 3), ilit(2)), ex((8, 3), ilit(3)), ex((0, 9), ilit(0))],
          refinements=[("pre", "d >= 1"),
                       ("post", "0 - d <= 2 * (n - result * d)"),
                       ("post", "2 * (n - result * d) < d")],
          props=[("exact_on_multiples", "forall q d. d < 1 || self(q * d, d) == q"),
                 ("by_one", "forall n. self(n, 1) == n")],
          hints=["round_half_up_division", "nearest_integer_quotient"], complexity="O(1)")
    gauntlet("gi_round_div", prove)

    # 7. Division z / w, rounded componentwise: z·conj(w) / N(w). The posts carry gi_round_div's
    # bounds through the let-bound norm — each needs N(w) ≥ 1 from w ≠ 0 (nonlinear) to discharge
    # the callee's precondition.
    build("gi_div",
          r"\a b c d -> let n = {gi_norm} c d in ({gi_round_div} (add (mul a c) (mul b d)) n, {gi_round_div} (sub (mul b c) (mul a d)) n)",
          FN([INT, INT, INT, INT], GI),
          [ex((7, 3, 2, 1), gi(*div(7, 3, 2, 1))), ex((5, 5, 1, 1), gi(5, 0)), ex((1, 0, 1, 1), gi(*div(1, 0, 1, 1))),
           ex((-9, 4, 0, 2), gi(*div(-9, 4, 0, 2))), ex((6, -8, 1, 0), gi(6, -8))],
          refinements=[("pre", "c != 0 || d != 0"),
                       ("post", "0 - (c * c + d * d) <= 2 * ((a * c + b * d) - fst(result) * (c * c + d * d))"),
                       ("post", "2 * ((a * c + b * d) - fst(result) * (c * c + d * d)) < c * c + d * d"),
                       ("post", "0 - (c * c + d * d) <= 2 * ((b * c - a * d) - snd(result) * (c * c + d * d))"),
                       ("post", "2 * ((b * c - a * d) - snd(result) * (c * c + d * d)) < c * c + d * d")],
          props=[("by_one", "forall a b. fst(self(a, b, 1, 0)) == a && snd(self(a, b, 1, 0)) == b")],
          hints=["gaussian_div", "nearest_gaussian_quotient"], complexity="O(1)")
    gauntlet("gi_div", prove)

    # 8. The remainder z − q·w, and the module's headline theorem as its post: N(r) < N(w). By hand:
    # (z − q·w)·conj(w) = z·conj(w) − q·N(w) = (r1, r2) with |2·r1| ≤ N(w), |2·r2| ≤ N(w) (gi_div's
    # posts), so N(z − q·w)·N(w) = r1² + r2² ≤ N(w)²/2 (norm multiplicativity), so N(z − q·w) ≤
    # N(w)/2 < N(w). Degree 4, with a division — the measurement this module exists to make.
    build("gi_rem",
          r"\a b c d -> case {gi_div} a b c d of { (q1, q2) => case {gi_mul} q1 q2 c d of { (m1, m2) => (sub a m1, sub b m2) } }",
          FN([INT, INT, INT, INT], GI),
          [ex((7, 3, 2, 1), gi(*rem(7, 3, 2, 1))), ex((5, 5, 1, 1), gi(0, 0)), ex((1, 0, 1, 1), gi(*rem(1, 0, 1, 1))),
           ex((-9, 4, 0, 2), gi(*rem(-9, 4, 0, 2))), ex((6, -8, 1, 0), gi(0, 0))],
          refinements=[("pre", "c != 0 || d != 0"),
                       ("post", "{gi_norm}(fst(result), snd(result)) < {gi_norm}(c, d)")],
          props=[("by_one_is_zero", "forall a b. fst(self(a, b, 1, 0)) == 0 && snd(self(a, b, 1, 0)) == 0")],
          hints=["gaussian_rem", "euclidean_remainder"], complexity="O(1)")
    gauntlet("gi_rem", prove)

    # 8b. The Euclidean property as a checker law, composing gi_rem and gi_norm by address.
    build("gi_euclid_holds",
          r"\a b c d -> case {gi_rem} a b c d of { (r1, r2) => lt ({gi_norm} r1 r2) ({gi_norm} c d) }",
          FN([INT, INT, INT, INT], BOOL),
          [ex((7, 3, 2, 1), blit(True)), ex((5, 5, 1, 1), blit(True)), ex((-9, 4, 0, 2), blit(True)), ex((100, -37, 3, 5), blit(True))],
          props=[("remainder_smaller", "forall a b c d. (c == 0 && d == 0) || self(a, b, c, d) == true")],
          tags=["predicate/check"], hints=["euclidean_property_check"], complexity="O(1)")
    gauntlet("gi_euclid_holds", prove)

    # 9. Euclid's algorithm on Gaussian integers: descends by gi_rem, whose norm is smaller — a
    # measure the termination checker reads off a CALLEE's post (round 4: the measured descent —
    # N(c, d) over positions 2 and 3, smaller by gi_rem's certified `N(r) < N(w)` under its pre,
    # which the false arm of the guard discharges), and a NONLINEAR one (a² + b²). `right_zero`
    # is an unfolding; `nonzero` is a genuine strong induction on N(c, d): the result is nonzero
    # unless both inputs are — the hypothesis at (c, d, rem(z, w)) under N(rem) < N(w).
    build("gi_gcd",
          r"\a b c d -> case and (eq c 0) (eq d 0) of { true => (a, b); false => case {gi_rem} a b c d of { (r1, r2) => self c d r1 r2 } }",
          FN([INT, INT, INT, INT], GI),
          [ex((4, 2, 2, 0), gi(*gcd(4, 2, 2, 0))), ex((7, 3, 2, 1), gi(*gcd(7, 3, 2, 1))), ex((5, 0, 0, 0), gi(5, 0)),
           ex((6, 8, 4, -2), gi(*gcd(6, 8, 4, -2))), ex((0, 0, 3, 1), gi(3, 1))],
          props=[("right_zero", "forall a b. fst(self(a, b, 0, 0)) == a && snd(self(a, b, 0, 0)) == b"),
                 ("nonzero", "forall a b c d. (a == 0 && b == 0 && c == 0 && d == 0) || fst(self(a, b, c, d)) != 0 || snd(self(a, b, c, d)) != 0")],
          hints=["gaussian_gcd", "euclid_gaussian"], complexity="O(log n)")
    gauntlet("gi_gcd", prove)

    if PARSE_ONLY:
        print(f"\n{len(ADDR)} records minted (parse only)")
        return
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
