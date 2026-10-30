#!/usr/bin/env python3
"""Author the rational-tier records (evolution/rational-tier-poc) and run every record through the
verified-by-default gauntlet, writing the records, bodies and a machine-readable report. Exact
rationals as pairs of ints (numerator, denominator ≥ 1), normalized through the INTEGER TIER's
certified `gcd` by content address — the first module that composes another module's certified
contracts across a module boundary. Pure, in-language bodies in surface syntax; no new builtin.

    python3 evolution/rational-tier-poc/build_records.py            # mint + gauntlet (needs z3)
    python3 evolution/rational-tier-poc/build_records.py --no-prove # skip the solver rungs

Output: records/<name>.v0.2.json + records/body-<name>.json, records/REPORT.json, records/REPORT.md.
The integer tier's `gcd` record and body are COPIED into records/ (same content, same address) so
`--records` resolves the cross-module reference; publishing them again is idempotent.
Needs `nl-validator` (NL_VALIDATOR, or the sibling release build)."""
import json
import os
import shutil
import subprocess
import sys
from fractions import Fraction
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent
V = Path(os.environ.get("NL_VALIDATOR") or REPO / "tooling/validator/target/release/nl-validator")
os.environ.setdefault("NL_DERIVED_CACHE", "1")  # the derived-contract cache (spec/evaluation.md)
OUT = HERE / "records"
INTEGER_TIER = REPO / "evolution/integer-tier-poc/records"


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
TUPLE = lambda *ts: {"kind": "tuple", "elems": list(ts)}  # noqa: E731
FN = lambda ps, r: {"kind": "fn", "params": ps, "result": r}  # noqa: E731
RAT = TUPLE(INT, INT)


def ilit(v):
    return {"kind": "int", "value": v if abs(v) < 2**53 else str(v)}


blit = lambda b: {"kind": "bool", "value": b}  # noqa: E731
tup = lambda *vs: {"kind": "tuple", "elems": list(vs)}  # noqa: E731
rat = lambda n, d: tup(ilit(n), ilit(d))  # noqa: E731

ADDR = {}      # name -> fn_ address, for `{name}` placeholders in later bodies
REPORT = []


def build(name, surface, sig, examples, props=(), refinements=(), tags=(), hints=(), complexity=None,
          terminates="always"):
    def subst(t):
        for k, v in ADDR.items():
            t = t.replace("{" + k + "}", v)
        return t
    body = parse_body(subst(surface))
    body_hash = hash_artifact(body, "body")
    # `{name}` placeholders resolve in predicates too: a post may apply a callee by address
    # (`{gcd}(abs(fst(result)), snd(result)) == 1`).
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
        "intent_tags": ["arithmetic/rational", "math/rational", f"math/rational/{name.replace('_', '-')}"] + list(tags),
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


# ---- reference values (Python's exact Fractions) ------------------------------------------------
def norm(n, d):
    f = Fraction(n, d)
    return (f.numerator, f.denominator)


def ex(args, result):
    return {"args": [ilit(a) for a in args], "result": result}


def main():
    prove = "--no-prove" not in sys.argv
    OUT.mkdir(exist_ok=True)
    for f in OUT.glob("*.json"):
        f.unlink()

    # 0. The integer tier's gcd, by content address — copied so `--records` resolves it. Its
    # certified contract here: `(a < 1 ∧ b < 1) ∨ result ≥ 1`, `result ≥ 0`, commutativity,
    # right-zero, divides-both and the greatest-divisor law (eleventh follow-up).
    gcd_rec = json.loads((INTEGER_TIER / "gcd.v0.2.json").read_text())
    shutil.copy(INTEGER_TIER / "gcd.v0.2.json", OUT / "gcd.v0.2.json")
    shutil.copy(INTEGER_TIER / "body-gcd.json", OUT / "body-gcd.json")
    ADDR["gcd"] = gcd_rec["hash"]
    print(f"gcd (integer tier)     {ADDR['gcd'][:24]}…  {len(gcd_rec['properties'])} laws")

    # 1. rat_normalize — the one place the sign and the gcd are handled: denominator positive,
    # both components divided by gcd(|n|, |d|); zero is (0, 1). Non-recursive (a recursion on the
    # sign flip would be a descent the termination checker has no measure for).
    G = "({gcd} (abs n) (abs d))"
    build("rat_normalize",
          r"\n d -> case eq n 0 of { true => (int(0), int(1)); false => case lt d 0 of { "
          r"true => (div (neg n) " + G + r", div (neg d) " + G + r"); "
          r"false => (div n " + G + r", div d " + G + r") } }",
          FN([INT, INT], RAT),
          [ex((6, -4), rat(*norm(6, -4))), ex((-6, -4), rat(*norm(-6, -4))), ex((0, -5), rat(0, 1)),
           ex((10, 4), rat(5, 2)), ex((7, 1), rat(7, 1)), ex((-9, 3), rat(-3, 1))],
          refinements=[("pre", "d != 0"),
                       # The denominator is positive: d ≠ 0, g = gcd(|n|, |d|) ≥ 1 and g | |d|.
                       ("post", "snd(result) >= 1"),
                       # Zero normalizes to (0, 1) — a first-order case the body states.
                       ("post", "n != 0 || (fst(result) == 0 && snd(result) == 1)"),
                       # LOWEST TERMS: gcd(|p|, q) = 1 — the module's headline fact, SOUND through the
                       # callee-side divisibility rung (gcd's greatest-divisor law at the product
                       # witness e·g, with the scaling lemma), round 3.
                       ("post", "{gcd}(abs(fst(result)), snd(result)) == 1")],
          # The two facts everything above rests on, as laws: the value is preserved (cross-
          # multiplication) and the result is in lowest terms. Coprimality is the module's headline
          # question — it follows from gcd's greatest-divisor law through the divisibility rung,
          # IF the prover can carry the quotients.
          props=[("value_preserved", "forall n d. d == 0 || fst(self(n, d)) * d == n * snd(self(n, d))"),
                 ("idempotent", "forall n d. d == 0 || self(fst(self(n, d)), snd(self(n, d))) == self(n, d)")],
          hints=["normalize_fraction", "lowest_terms"], complexity="O(log n)")
    gauntlet("rat_normalize", prove)

    # 1b. rat_coprime_holds — lowest terms, as a checker record (the predicate cannot apply gcd).
    build("rat_coprime_holds",
          r"\n d -> case {rat_normalize} n d of { (p, q) => eq ({gcd} (abs p) q) 1 }",
          FN([INT, INT], BOOL),
          [ex((6, -4), blit(True)), ex((10, 4), blit(True)), ex((0, 7), blit(True)), ex((12, 18), blit(True))],
          props=[("lowest_terms", "forall n d. d == 0 || self(n, d) == true")],
          tags=["predicate/check"], hints=["lowest_terms_check"], complexity="O(log n)")
    gauntlet("rat_coprime_holds", prove)

    # 2. rat_neg — on a normalized pair the sign lives in the numerator; nothing to renormalize.
    build("rat_neg", r"\n d -> (neg n, d)", FN([INT, INT], RAT),
          [ex((3, 2), rat(-3, 2)), ex((-5, 7), rat(5, 7)), ex((0, 1), rat(0, 1))],
          refinements=[("post", "snd(result) == d")],
          props=[("involution_num", "forall n d. fst(self(fst(self(n, d)), snd(self(n, d)))) == n"),
                 ("involution_den", "forall n d. snd(self(fst(self(n, d)), snd(self(n, d)))) == d")],
          hints=["negate_fraction"], complexity="O(1)")
    gauntlet("rat_neg", prove)

    # 3. rat_inv — the reciprocal, renormalized (the sign moves to the numerator).
    build("rat_inv", r"\n d -> {rat_normalize} d n", FN([INT, INT], RAT),
          [ex((3, 2), rat(2, 3)), ex((-5, 7), rat(-7, 5)), ex((4, -6), rat(-3, 2))],
          refinements=[("pre", "n != 0"),
                       # From normalize's value_preserved law at the body's application: the value is 1/x.
                       ("post", "fst(result) * n == d * snd(result)")],
          # Composition by contract: normalize's positive denominator, at the swapped arguments.
          props=[("denominator_positive", "forall n d. n == 0 || snd(self(n, d)) >= 1")],
          hints=["reciprocal"], complexity="O(log n)")
    gauntlet("rat_inv", prove)

    # 4. rat_add / rat_sub / rat_mul — the cross-multiplied forms, normalized.
    build("rat_add", r"\a b c d -> {rat_normalize} (add (mul a d) (mul c b)) (mul b d)", FN([INT, INT, INT, INT], RAT),
          [ex((1, 2, 1, 3), rat(5, 6)), ex((1, 2, 1, 2), rat(1, 1)), ex((-1, 3, 1, 6), rat(-1, 6)), ex((2, 3, 0, 1), rat(2, 3))],
          refinements=[("pre", "b >= 1 && d >= 1"),
                       # From normalize's contract: b·d ≥ 1 when both are (nonlinear, but a product of positives).
                       ("post", "snd(result) >= 1"),
                       # The value: result = (a·d + c·b) / (b·d), as cross-multiplication (normalize's law
                       # at the body's application — composition by contract across the module boundary).
                       ("post", "fst(result) * (b * d) == (a * d + c * b) * snd(result)")],
          props=[("commutative", "forall a b c d. self(a, b, c, d) == self(c, d, a, b)")],
          hints=["add_fractions"], complexity="O(log n)")
    gauntlet("rat_add", prove)

    build("rat_sub", r"\a b c d -> {rat_normalize} (sub (mul a d) (mul c b)) (mul b d)", FN([INT, INT, INT, INT], RAT),
          [ex((1, 2, 1, 3), rat(1, 6)), ex((1, 2, 1, 2), rat(0, 1)), ex((-1, 3, 1, 6), rat(-1, 2))],
          refinements=[("pre", "b >= 1 && d >= 1"), ("post", "snd(result) >= 1"),
                       ("post", "fst(result) * (b * d) == (a * d - c * b) * snd(result)")],
          # x − x = 0: the numerator a·b − a·b vanishes, and normalize's zero post does the rest.
          props=[("self_difference_is_zero", "forall a b. b < 1 || fst(self(a, b, a, b)) == 0 && snd(self(a, b, a, b)) == 1")],
          hints=["subtract_fractions"], complexity="O(log n)")
    gauntlet("rat_sub", prove)

    build("rat_mul", r"\a b c d -> {rat_normalize} (mul a c) (mul b d)", FN([INT, INT, INT, INT], RAT),
          [ex((1, 2, 2, 3), rat(1, 3)), ex((-3, 4, 4, 3), rat(-1, 1)), ex((0, 5, 7, 2), rat(0, 1))],
          refinements=[("pre", "b >= 1 && d >= 1"), ("post", "snd(result) >= 1"),
                       ("post", "fst(result) * (b * d) == (a * c) * snd(result)")],
          props=[("commutative", "forall a b c d. self(a, b, c, d) == self(c, d, a, b)"),
                 ("zero_annihilates", "forall b c d. b < 1 || d < 1 || fst(self(0, b, c, d)) == 0")],
          hints=["multiply_fractions"], complexity="O(log n)")
    gauntlet("rat_mul", prove)

    # 5. rat_eq / rat_lt — decided by cross-multiplication, no normalization needed.
    build("rat_eq", r"\a b c d -> eq (mul a d) (mul c b)", FN([INT, INT, INT, INT], BOOL),
          [ex((1, 2, 2, 4), blit(True)), ex((1, 2, 1, 3), blit(False)), ex((-3, 6, 1, -2), blit(True))],
          props=[("reflexive", "forall a b. self(a, b, a, b) == true"),
                 ("symmetric", "forall a b c d. self(a, b, c, d) == self(c, d, a, b)")],
          tags=["predicate/check"], hints=["fractions_equal"], complexity="O(1)")
    gauntlet("rat_eq", prove)

    build("rat_lt", r"\a b c d -> lt (mul a d) (mul c b)", FN([INT, INT, INT, INT], BOOL),
          [ex((1, 2, 2, 3), blit(True)), ex((2, 3, 1, 2), blit(False)), ex((1, 2, 1, 2), blit(False)), ex((-1, 2, 0, 1), blit(True))],
          refinements=[("pre", "b >= 1 && d >= 1")],
          props=[("irreflexive", "forall a b. self(a, b, a, b) == false"),
                 ("asymmetric", "forall a b c d. b < 1 || d < 1 || self(a, b, c, d) == false || self(c, d, a, b) == false")],
          tags=["predicate/check"], hints=["fraction_less_than"], complexity="O(1)")
    gauntlet("rat_lt", prove)

    # 6. rat_floor — the integer part, Euclidean (towards −∞ for a positive denominator).
    build("rat_floor", r"\n d -> div n d", FN([INT, INT], INT),
          [ex((7, 2), ilit(3)), ex((-7, 2), ilit(-4)), ex((6, 3), ilit(2)), ex((0, 5), ilit(0))],
          refinements=[("pre", "d >= 1")],
          # The defining inequalities: floor·d ≤ n < (floor + 1)·d — the Euclidean rung's territory.
          props=[("lower_bound", "forall n d. d < 1 || self(n, d) * d <= n"),
                 ("upper_bound", "forall n d. d < 1 || n < (self(n, d) + 1) * d")],
          hints=["floor_of_fraction"], complexity="O(1)")
    gauntlet("rat_floor", prove)

    # 7. Algebra as checker records: identities that compose two records. A contract does not pin
    # a function, so these say what the records' CONTRACTS let the prover conclude — a measurement.
    build("rat_add_zero_holds",
          r"\a b -> case {rat_add} a b (int(0)) (int(1)) of { (p, q) => case {rat_normalize} a b of { (p2, q2) => and (eq p p2) (eq q q2) } }",
          FN([INT, INT], BOOL),
          [ex((2, 3), blit(True)), ex((-4, 6), blit(True)), ex((0, 9), blit(True))],
          props=[("add_zero", "forall a b. b < 1 || self(a, b) == true")],
          tags=["predicate/check"], hints=["additive_identity_check"], complexity="O(log n)")
    gauntlet("rat_add_zero_holds", prove)

    build("rat_mul_one_holds",
          r"\a b -> case {rat_mul} a b (int(1)) (int(1)) of { (p, q) => case {rat_normalize} a b of { (p2, q2) => and (eq p p2) (eq q q2) } }",
          FN([INT, INT], BOOL),
          [ex((2, 3), blit(True)), ex((-4, 6), blit(True)), ex((0, 9), blit(True))],
          props=[("mul_one", "forall a b. b < 1 || self(a, b) == true")],
          tags=["predicate/check"], hints=["multiplicative_identity_check"], complexity="O(log n)")
    gauntlet("rat_mul_one_holds", prove)

    build("rat_eq_normalize_holds",
          r"\a b c d -> case {rat_normalize} a b of { (p, q) => case {rat_normalize} c d of { (r, s) => "
          r"eq ({rat_eq} a b c d) (and (eq p r) (eq q s)) } }",
          FN([INT, INT, INT, INT], BOOL),
          [ex((1, 2, 2, 4), blit(True)), ex((1, 2, 1, 3), blit(True)), ex((-3, 6, 1, -2), blit(True))],
          # Equal as values ⇔ equal normal forms: the uniqueness of lowest terms.
          props=[("equality_is_normal_form_equality", "forall a b c d. b < 1 || d < 1 || self(a, b, c, d) == true")],
          tags=["predicate/check"], hints=["normal_form_uniqueness_check"], complexity="O(log n)")
    gauntlet("rat_eq_normalize_holds", prove)

    build("rat_add_assoc_holds",
          r"\a b c d e f -> case {rat_add} a b c d of { (p, q) => case {rat_add} p q e f of { (l1, l2) => "
          r"case {rat_add} c d e f of { (r, s) => case {rat_add} a b r s of { (m1, m2) => and (eq l1 m1) (eq l2 m2) } } } }",
          FN([INT, INT, INT, INT, INT, INT], BOOL),
          [ex((1, 2, 1, 3, 1, 6), blit(True)), ex((-1, 2, 1, 3, 2, 5), blit(True)), ex((0, 1, 0, 1, 0, 1), blit(True))],
          props=[("associative", "forall a b c d e f. b < 1 || d < 1 || f < 1 || self(a, b, c, d, e, f) == true")],
          tags=["predicate/check"], hints=["additive_associativity_check"], complexity="O(log n)")
    gauntlet("rat_add_assoc_holds", prove)

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
