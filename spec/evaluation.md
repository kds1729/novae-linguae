# Evaluation & type checking (v0.1)

*Status: v0.1, implemented in [`tooling/validator`](../tooling/validator/src/) — `interp.rs`
(evaluator) and `typecheck.rs` (type checker), exposed by `nl-validator eval` / `run` / `typecheck`
/ `check-properties --body` / `prove` (SMT, `prove.rs`).*

This is the language's **semantic core**: the rules by which a Nova Lingua body
([`body-expression.schema.json`](body-expression.schema.json)) *computes* a value, and by which a
body is checked to *have* its declared type ([`type-expression.schema.json`](type-expression.schema.json)).
It makes principle 3 ("verified by default") cover execution and type soundness, and principle 9
("the runtime is AI-targeted too") concrete. The reference implementation is normative by example:
the conformance is the committed examples running correctly.

Concretely, the executable conformance suite for this semantic core is the **verified corpus**
([`tooling/corpus/`](../tooling/corpus/)): every example is schema-validated, type-checked, executed
against its worked examples, and its properties proved, all by `nl-validator` — and the negative
examples are confirmed to be *rejected for their stated reason*. An independent implementation is
conformant when it reproduces the corpus verdicts, including the rejections. That re-runnable oracle,
not pseudocode, is what this spec offers a second implementer.

## Evaluation

A call-by-value lambda calculus over the value-expression AST
([`value-expression.schema.json`](value-expression.schema.json)):

- **`var`** resolves in the lexical environment, then the builtin library, then the `nil` constant.
- **`lit`** is its value-expression decoded.
- **`lambda`** is a closure capturing the environment; application supports **currying** (too few args
  → a partial application) and over-application (extra args applied to the result).
- **`self`** — a record's body runs as a *recursive* closure: each application re-binds `self` to the
  whole function before evaluating the body, so a self-recursive body (`length_rec`, `factorial`) calls
  back into itself. Binding the full function (never a partially-applied remainder) keeps recursion
  correct even when the function is itself partially applied.
- **`app`** evaluates the function and arguments and applies.
- **`let`** binds monomorphically.
- **`case`** evaluates the scrutinee and tries arms in order over the four pattern kinds
  (`wildcard` / `bind` / `lit` / `variant`); a non-matching scrutinee is a runtime error
  (exhaustiveness is the checker's job, not the evaluator's). `if` does not exist — it is `case` on a
  `bool` (principle 8).
- **`field`** projects a record field.
- **`variant`** constructs a sum value — a fixed `tag` with an optional `payload` *expression* evaluated
  in the current environment (`Just(a / b)`, `None`). This is the body-expression form; the `lit` path
  builds only constant variants. Sum values are destructured by `case` over `variant` patterns.

**Builtins** (total, pure): arithmetic (`add` `sub` `mul` `div` `mod` `neg` `abs` `min` `max`),
comparison (`eq` `neq` `lt` `le` `gt` `ge`), booleans (`and` `or` `xor` `not`), lists (`nil` `cons`
`head` `tail` `last` `init` `length` `null` `append` `reverse`), tuples (`fst` `snd`), `id`, and the higher-order
`map` `filter` `foldl` `foldr` `compose` `apply`. `eq`/structural equality is the semantics of the
`lit` pattern.

**JSON builtins** ([`expressiveness.md`](expressiveness.md) phase 3 — the language's own canonical
form, manipulable from inside). `Json` is an ordinary sum type over the existing variant system —
`JNull | JBool bool | JNum int/float | JStr string | JList (List Json) | JObj (Map string Json)` —
destructured by `case` like any variant, so a field projection is `case parse_json s of { Just(JObj(m))
=> case map_get "k" m of { Just(JNum(p)) => p; _ => … }; _ => … }`. Two conversions:
`parse_json : string → Maybe Json` (total via `Maybe` — malformed text is `None`; duplicate object keys
resolve last-wins) and `render_json : Json → string`, which emits the **JCS-canonical** text — so
`render_json` of a `parse_json` *is* canonicalization, and equal JSON documents render identically
(principles 2, 5).

**Map builtins** ([`expressiveness.md`](expressiveness.md) phase 2 — total, pure, deterministic; key
argument first, like the string ops): the nullary constant `map_empty : Map string a`;
`map_put : (key, a, Map string a) → Map string a` (insert-or-overwrite);
`map_get : (key, Map string a) → Maybe a` (an absent key is `None`, never an error);
`map_del : (key, Map string a) → Map string a` (absent key is a no-op); `map_size : Map string a → nat`;
`map_keys : Map string a → List string` (sorted — iteration order is deterministic, principle 5).
The runtime representation is an ordered map, so a map **value** (value kind `map`, entries sorted by
key — the well-formedness checker rejects unsorted or duplicate keys) serializes canonically: equal maps
hash equal. Surface value form: `map {"k" => v, …}` / `map {}`.

**String builtins** ([`expressiveness.md`](expressiveness.md) phase 1 — total, pure, deterministic;
pattern/separator arguments first, so partial applications are reusable predicates/splitters):
`str_concat : (string, string) → string`; `str_length : string → nat` (**Unicode scalar values**, not
bytes or graphemes); `str_contains : (needle, s) → bool` (empty needle → `true`); `str_split : (sep, s)
→ List string` (keeps empties — `str_split(",", "a,,b") = ["a","","b"]`; absent separator → `[s]`;
empty separator → one singleton per scalar value); `str_join : (sep, List string) → string`
(`str_join(sep, str_split(sep, s)) = s` for non-empty `sep`); `to_string : int → string` (canonical
decimal); `parse_int : string → Maybe int` — accepts exactly canonical decimal (optional leading `-`,
no leading zeros / `-0` / whitespace / `+`; overflow → `None`): **totality via `Maybe`**, the pattern
that replaces `error`, with the round-trip `parse_int(to_string(n)) = Just(n)`.
Tier-2, pulled by GW4 (the sorted-report workflow): `str_lt : (string, string) → bool` — strict
lexicographic order over Unicode scalar values, **the same order canonical map keys use**
(`map_keys`/check-value), deliberately not a locale collation; and `str_lower : string → string` —
the Unicode **default (untailored)** lowercase mapping, deterministic and locale-independent (no
Turkish-i tailoring). `str_lt` maps into the prover's string fragment (SMT-LIB `str.<` has exactly
these semantics); `str_lower` stays out (no theory counterpart).
Tier-2, pulled by GW10 (description depth — query strings): `url_encode : string → string` — RFC
3986 **strict** percent-encoding (unreserved characters pass; every other UTF-8 byte becomes `%XX`
uppercase hex), total and deterministic; raw `str_concat` of caller data into a URL is unsound (a
space or `&` changes the request). Like `str_lower` it stays out of the SMT fragment (no theory
counterpart) — its argument still infers the string *sort*, and laws over it read UNSUPPORTED.

**Float report builtins** (tier-2, pulled by GW5 — the numeric-report workflow): `to_float : int →
float` — total; IEEE-754 nearest-even for magnitudes beyond 2⁵³ (deterministic rounding, stated
rather than hidden). `div`/`mod` are **numeric-polymorphic** like `add`/`mul` (int keeps euclidean
semantics; float is IEEE) and **partial at a zero divisor in both domains** — the float path errors
rather than yield `Infinity`/`NaN`, which are unrepresentable in canonical JCS. `to_string` is
numeric-polymorphic too: canonical decimal for `int`, and for `float` the **JCS / ECMAScript
Number-to-String canonical rendering** — the same rendering the hashing layer's canonicalizer
emits, so `to_string(3.0) = "3"` and `to_string(3.25) = "3.25"`; a non-finite float (reachable only
via arithmetic overflow) is refused, not rendered. **The float-domain prover guard** ships with the
pull: the proof fragment's arithmetic is the solver's *Int* theory, so `prove`,
`check-refinement`, and `cluster` refuse a record whose declared signature mentions `float`
(UNSUPPORTED / UNVERIFIABLE / a singleton class) — an IEEE law like non-associativity of `add`
would otherwise be mis-proved over ℤ. `parse_float`, rounding, and format control stay unpulled.

**Composition.** Applying a `fn_ref` value resolves its target (a function content-address, or a
body's own `expr_` address) against a link map and runs the referenced body — so records assemble and
run end-to-end (principle 4). `nl-validator run --records <dir>` builds the link map from a directory
and resolves both a record's `body_hash` and its `fn_ref` arguments.

**Scope (v0.1, honest).** Integers are 128-bit and `nat` is a non-negative `int`; an arithmetic result
that does not fit **refuses** (`integer overflow`, 2026-10-17 — before that it wrapped silently), and
two ints always compare exactly (a float round-trip past 2^53 is not a comparison). A recursion deeper
than 100,000 nested applications refuses too (the evaluator is a tree walker without tail-call
elimination; the bound is a stated error, not a stack overflow). Most builtins are
pure; the effectful ones (`print` → `io.console`, `rand` → `random`, `now` → `time` — `now : a →
int` reads the wall clock as Unix epoch MILLISECONDS live, and a replay returns the recorded
instant, so a time-budgeted poll (wait until a service's `RetryAfter`) is expressible and still
deterministic on replay; `sleep` → `time` — `sleep : int → unit` pauses a LIVE evaluation for
that many milliseconds, at most 60 000 per call, so a bounded poll over an asynchronous service
can pace itself; a replay does not wait — `panic` → `panic`) are gated by a capability sandbox
(see **Effect enforcement** below). The evaluator does not enforce exhaustiveness or types;
those are the checker's job.

## Type checking

Hindley-Milner inference, unifying the body's inferred type with the declared `signature.type`:

- Fresh unification variables, union-find substitution with the occurs check.
- Builtins are **polymorphic schemes** instantiated fresh per use (`map : (a→b) → List a → List b`,
  `eq : a → a → bool`, …).
- **Arithmetic is numeric-polymorphic.** `add`/`sub`/`mul`/`min`/`max`/`neg`/`abs` and the comparisons
  `lt`/`le`/`gt`/`ge` use a *numeric* type variable — one that unifies only with `int` or `float` (or with
  another variable, which then itself becomes numeric). So they check over either numeric type but reject a
  non-number (`\b -> add(b, b)` against `bool → bool` is ILL-TYPED). `div`/`mod` stay `int`-only.
- `let` is monomorphic (a deliberate simplification).
- **`self` is bound to the declared signature**, so a self-recursive body type-checks: the recursive call
  shares the function's own (skolemized) type. Monomorphic recursion only — `self` is a single monotype,
  not re-generalized — which is what the recursive records need (`length_rec : List a → int` checks with
  `self(tail xs)` typed by the same signature).
- A declared `forall` type's variables are **skolemized to rigid constants**, so the body must be
  genuinely polymorphic: `\x -> x` checks against `forall a. a → a`, but `\x -> add(x, x)` does not.
- Verdict: **WELL-TYPED** (with the type) or **ILL-TYPED** (with the mismatch, exit 1).

**Scope (v0.1, honest).** `nat` is normalized to `int` (no refinement-aware checking here);
sum/`variant` and `ref` (named-type-by-address) types are opaque (a single `Sum`), so a `variant`
construction types as `Sum` (its payload expression is still inferred, so an error inside it is caught)
and unifies with any declared sum result, and `case` arms over a sum are checked structurally with fresh
payload types rather than resolved; refinements and effects are separate concerns, not checked here.

## Refinement checking

The type checker erases `nat` to `int`, so the one refinement the type language bakes in — a `nat` is a
non-negative `int` — goes unchecked: a body declared `… -> nat` that can produce a negative `int`
type-checks clean. `nl-validator check-refinement <record> --body <body>` closes that hole. For a
`nat`-result function it proves

```text
∀ params. (⋀ nat-typed params ≥ 0) ⟹ body(params) ≥ 0
```

via the [SMT proof](#smt-proof-the-unbounded-rung) backend, with the [inductive](#inductive-proof-unbounded-recursive-structures)
fallback for a recursive body (a recursive `length : List a -> nat` is proved `≥ 0` by induction: base
`0 ≥ 0`, step `1 + self(tail) ≥ 0` under the IH). The parameters' `nat`-ness is the **precondition** —
`double : nat -> nat` is sound because `n ≥ 0 ⟹ n + n ≥ 0`. Verdicts: **SOUND** (proved), **VIOLATED** (a
solver counterexample — a real input on which the body goes negative, e.g. `\a b -> sub(a, b)` declared
`nat` at `a = 0, b = 1`; exit 1), **UNVERIFIABLE** (out of the decidable fragment or solver-undecided —
never a false SOUND), or **NOT-APPLICABLE** (the result type is not `nat`). Conservative by construction:
only a closed proof yields SOUND, only a counterexample yields VIOLATED.

It also checks **declared `signature.refinements[]`** — `pre`/`post` predicates. A `post` predicate is a
contract on the output, which it names through the **reserved variable `result`** (and may also mention the
parameters); a `pre` predicate is a precondition on the parameters, *assumed* when discharging the posts.
Each postcondition discharges `∀ params. (⋀ pre ∧ ⋀ nat-typed params ≥ 0) ⟹ post[result := body(params)]`
— the type-implied `nat` refinement is just the implicit post `result ≥ 0`. So a record declaring
`pre: a ≥ b` and `post: result ≥ 0` for `\a b -> sub(a, b)` is **SOUND** (the precondition gates it — without
it the post is **VIOLATED** at `a < b`), and a `post: result = a + b` is sound for an `add` body but
**VIOLATED** for a `mul` body. (`inv` refinements are reserved — not checked in v0.1.)

**Contracts compose (2026-10-17).** A body that applies another record by `fn_ref` is outside the
first-order fragment — the callee's body is not inlined. Over a linked view (`check-refinement
--records`, `prove --records`, and `certify --records`), the callee becomes an **uninterpreted
function under its certified contract**: for every postcondition the callee's own
`check-refinement` PROVES on the same view (re-derived, never read from its declaration; the
callee's own callees first, transitively), the axiom `∀ params. (pre ∧ nat-typed params ≥ 0) ⇒
post[result := callee(params)]` is emitted beside the obligation, and so is every LAW of the callee
that its own `prove` proves (`∀ vars. law[self := callee]`, guarded to the callee's `nat` domain). So
`lcm`'s `result ≥ 0` follows from `gcd`'s proved `result ≥ 1`, and `lcm`'s commutativity from `gcd`'s
proved commutativity, without re-proving `gcd` — modular verification through content addresses. Honesty rule: a certificate that used a callee contract reports a satisfiable
negation as **UNKNOWN**, never REFUTED — the uninterpreted callee is under-constrained away from
its contract, so a model may be spurious. Callees whose sorts leave the Int/Bool/String fragment
(tuples, lists, `Maybe`) contribute no contract and keep the obligation UNVERIFIABLE, stated.

## Termination checking

Every record declares `signature.terminates` (`always` / `conditional` / `never` / `unknown`), but it was
only *propagated* (a composite is `always` only if every stage is), never *checked* — a record could claim
`always` for a body that loops. `nl-validator check-termination <record> --body <body>` verifies a declared
`always` **structurally**, with no solver. Over the first-order fragment (the arithmetic/boolean/comparison
builtins plus `head`/`tail`/`last`/`init`/`cons`/`null`/`length`/`append`/`reverse` and the string ops) a body provably halts when it is
**non-recursive**, or **structurally recursive**: every `self`-call's recursion argument is `tail^k(p)`
(`k ≥ 1`) of one fixed parameter `p` — a list is a finite inductive structure and `tail` strictly shrinks
it, so the recursion is well-founded and halts (normally, or with an error at `nil`). The analysis is
**sound and conservative**: a recursion whose argument is not a strict structural descent, `self`-calls
descending on different parameters, or any **higher-order / opaque** application (`map`/`filter`/`fold`,
applying a parameter or an `fn_ref`, whose callee's termination is unseen) is `Unknown` — never a false
`always`, the same honesty stance `check-effects` takes for opaque callees. **Numeric descents** (the
integer-tier module, 2026-10-17) widen the structural rule: a `case` guard establishes a FLOOR for a
parameter (`p > lit`, `p >= lit`, the mirrored `lit < p`, and the FALSE arms of `p < lit` / `p <= lit` —
the natural base-case spelling; a `nat` under a non-zero guard has floor 1), and above a floor a
self-call's argument descends when it is a guarded constant decrement `sub(p, c)`, a **halving**
`div(p, c)` / `div(add(p, k), c)` with `(floor + k) div c < floor` (the gap grows with `p`, so checking
at the floor suffices; a VARIABLE divisor counts when its own floor is ≥ 2), or **Euclid's step**
`mod(_, p)` with floor ≥ 1 (the remainder is in `[0, p)`). **Composition by content address** is folded
over a linked view (`--records`): a `fn_ref` callee whose OWN body provably terminates — re-derived,
transitively, never read from its declaration — is one more terminating step rather than an opaque
call (a cycle through addresses stays unresolved). A recursion on the COMPONENTS of a callee's tuple
result (`ntt_rec` on the halves `ntt_split` returns) is well-founded when the callee's own body shows
each component strictly shorter than its list input — a `tail^k` recursion, `k ≥ 2`, consing at most
`k − 1` elements per component per step, so a component is at most `n − ⌊n/k⌋` long — and the guards
in force establish `length ≥ k` (the FALSE arm of `null(tail^(k−1)(p))`). Verdicts: **SOUND** (declared
`always`, verified), **VERIFIED** (provably always but declared weaker, so the declaration could be
strengthened), or **UNVERIFIABLE** (declared `always`, not provable by this structural analysis). There is
no refutation path — structural analysis cannot disprove termination, only fail to prove it.

## Complexity checking

Every record may declare `signature.complexity` (an `O(…)` running-time bound), but — like `terminates` — it
was only *propagated* through pipelines (the `compose` path derives a composite bound), never *checked*
against the body: a record could claim `O(n)` for an `O(n²)` implementation. `nl-validator check-complexity
<record> --body <body>` verifies it **structurally**, with no solver. It infers a **sound upper bound** on the
body's running time as a class in the input size and compares it to the declaration. Over the same first-order
fragment, the classification is by op cost (the scalar ops and `head`/`tail`/`cons`/`null` are `O(1)`;
`length`/`append`/`reverse`/`last`/`init` and the string ops are `O(n)`): a **non-recursive** body is `O(1)` or `O(n)` (a finite AST over data
that stays `O(n)`); a **structural recursion** is solved as a recurrence `T(n) = a·T(n−k) + w`, where `a` is
the branching factor (the number of `self`-calls on the worst-case execution path — `case` arms are mutually
exclusive, so `filter` stays `O(n)` rather than reading as exponential), `k` the descent, and `w` the per-step
non-recursive work: one self-call with `O(1)` work → `O(n)`, one with `O(n)` work (an `append` of the
recursive result — naive `reverse`) → `O(n²)`, two-or-more constant-descent calls → **exponential** (a sound
upper bound: naive `fib`), and a **halving** descent (`div(p, c)`, `div(add(p, k), c)`, a variable divisor
guarded ≥ 2, or Euclid's `mod(_, p)` — Lamé's bound) → `O(log n)` / `O(n log n)`. Over a linked view
(`--records`) a `fn_ref` callee's own inferred class is the cost of applying it, provided every argument's
VALUE is size-bounded by the input (parameters, literals, and `add`/`sub`/`div`/`mod`/`length`/… over them —
not a product of two inputs, not a recursive result: a callee's `O(m)` is re-expressible in our `n` only
when `m = O(n)`). A callee's RESULT counts as size-bounded when a **certified fact** bounds it: a declared
postcondition `result < p` / `result <= p` that `check-refinement` proves on the linked view (so
`mod_pow`'s `0 ≤ result < m`, proved by numeric induction, lets its result feed the next callee), or a
list result the output-size analysis bounds linearly in the input (a halving recursion consing O(1)
per level — `digits` — is Θ(log n) ≤ Θ(n)). A callee's TUPLE result is size-bounded when its
components provably SHRINK (the shrinking-callee descent above: a self-call on such a component is a
HALVING descent of the parameter the callee was applied to, so `ntt_rec` reads as
`T(n) = 2T(n/2) + O(n)`) — and the names a `case` binds from a size-bounded scrutinee are size-bounded
too. A function's OWN recursive result is size-bounded when its declared list result is at most linear
in its input, which the output-size analysis establishes for a two-way halving recursion whose per-level
material is O(1): the components a **preserving** callee returns (a `tail^k` recursion on its list
parameters consing at most `k` elements per component per step — `ntt_butterfly`'s, each no longer than
the list it was applied to) count as the recurrence's carried terms when each is used at most once and
bound by a distinct recursive result, so `append lo hi` in `ntt_rec` is `S(n) = 2S(n/2) + O(1) = Θ(n)`.
Only then does the butterfly applied to `self ev`/`self od` fold, and the whole NTT chain certifies
`O(n log n)`. A `mod` by a literal is bounded whatever its dividend (`w·w mod P`). Because a
declared `O(f)` is an *upper-bound* claim, the check compares the inferred sound bound `O(g)` to it: `g ≤ f`
→ **SOUND** (or **VERIFIED** when `g` is provably tighter, so the declaration could be strengthened); `g > f`
or a **higher-order / opaque** body (`map`/`filter`/`fold`, applying a parameter or `fn_ref`) → **UNVERIFIABLE**;
no declaration → **N/A** with the inferred bound reported. Like termination there is **no refutation path**
(an inferred `g > f` only means our bound is looser than the claim, not that the claim is false — proving a
*lower* bound is a different analysis), so a bound can be verified but never disproved. This closes the last
declared-but-unverified metadata field, alongside `typecheck` (type), `check-effects` (effects),
`check-refinement` (the `nat`/pre/post contracts), and `check-termination` (termination).

`check-complexity` also verifies the **structured `signature.cost`** (v0.3) — the richer form the `compose`
precise-complexity path threads through a pipeline: its **`time`** class is checked exactly like the flat
`complexity`, and its **`output_size`** (how the result's size grows with the input — `constant` /
`preserving` / `bounded` / `quadratic` / `cubic`) is verified against a **structurally inferred sound upper
bound** on the result size. This closes a real gap: `compose` re-expresses each downstream stage's cost in the
pipeline's input size *using* `output_size`, and until now trusted it blindly — a stage declared `preserving`
that actually expanded would make the composite's time bound unsound. Time and output size are **independent**:
naive `reverse` is `O(n²)` **time** but size-**preserving** (`Θ(n)`) output, and `check-complexity` confirms
both. The inference is sound and conservative — a scalar result is `constant`; a `List` build is analyzed
structurally (`cons`/`append`/`reverse` recurrences); a higher-order/opaque or polymorphic result is
`Unknown` (never claimed smaller than it is).

## Certification — every check in one pass

`nl-validator certify <record> --body <body>` runs **all** of the "verified by default" checks against a
record in a single pass — `typecheck`, `check-effects`, `check-refinement`, `check-termination`, and
`check-complexity` (+ structured `cost`) — and emits one verdict. It prints a per-check table, or a
machine-readable certificate with `--json` (the record + body content-addresses, every check's verdict, and
the overall `certified` flag). A record is **CERTIFIED** unless a check *actively fails its declaration* — an
ILL-TYPED body, an UNDER-DECLARED effect, or a VIOLATED refinement (exit 1); the conservative UNVERIFIABLE
verdicts (a bound or termination the structural analysis can't confirm — e.g. a body using an I/O builtin
that may block) are noted but do not revoke certification, since none of those checks can *disprove* a claim,
only fail to establish it. So `certify` turns "verified by default" from six separate invocations into one
signed-off, re-checkable certificate.

`certify --sign <seed>` goes one step further and emits a **signed certification record** — a first-class
commons artifact (top-level `kind: "certification"`, `certification.schema.json`) hashed and Ed25519-signed
by the same rules as a message, addressed `cert_<hex>`, with `subject`/`body_hash` naming what was certified.
It is a certifier's tamper-evident attestation that a record is verified, which other agents can rely on
without re-running the checks (and re-verify with `nl-validator verify`). This closes the loop with the trust
model and the agent loop: `orchestrate --verify --require-certified` **certifies a discovered function before
applying it** and aborts if it isn't certified — "assemble, don't write" (principle 4) refined to *assemble
only from verified parts* (principle 3).

## Run-backed property verification

`check-properties` evaluates a record's `properties[]` against its `examples[]`. Statically it is
honest about its limits (UNVERIFIABLE for anything needing to re-apply a function or quantify). With
`--body`, those become decidable by **running**: the function-under-test `self` is the executable
body, `map`/`filter`/`fold`/`compose`/`apply` are the builtins, and a `forall` ranges over the worked
examples' arguments. A **CONSISTENT** verdict then means "ran true on every example and false on none"
— still example-bound, not a proof.

## Generative property testing

`check-properties --generate [--cases N]` is the rung above example-bound CONSISTENT: instead of
ranging a `forall` over the worked examples, it **searches** for a counterexample. For each quantified
variable it infers a value generator from how the variable is used in the predicate (a list argument
of `length`/`map`/`reverse`/… → a list; an arithmetic/comparison operand → an integer; a boolean
connective operand → a bool; a string-op operand → a **string** over a small deterministic alphabet
that includes separators and a non-ASCII scalar, with `str_join`'s second argument a *list of*
strings — so laws over `str_split`/`str_join`, which the SMT string fragment cannot reach, are at
least generatively checked: the split/join round-trip HOLDS over the generated domain), samples `N`
inputs (default 100), runs the body, and reports per property:

- **EXHAUSTIVE (n cases)** — when the inferred domain is finite and small (booleans, a bounded int
  range, short lists), *every* case is enumerated rather than sampled: a proof over that domain
  (total for an all-boolean property; exhaustive over the bounded range for ints/lists);
- **HELD (n cases)** — the domain was too large to enumerate, so `n` inputs were sampled with no
  counterexample;
- **REFUTED** — with a **shrunk**, minimal counterexample (e.g. `n = 0`); fails the check (exit 1),
  a strictly stronger signal than example-CONTRADICTED;
- **UNGENERATABLE** — the property quantifies over a *function* (the higher-order argument of
  `map`/`filter`/`fold`/`compose`/`apply`, which we do not synthesize), so it is honestly skipped
  rather than silently passed.

The sampler is a fixed-seeded xorshift PRNG — no clock, no OS randomness — so a run is deterministic
(principle 5): the same record and `N` give the same verdict and the same counterexample, and a
REFUTED is replayable. Generation ranges over the inferred *type* — including the signature's **`nat`**:
a variable fed to a `nat`-typed parameter of `self` ranges over nats (exhaustive 0..8, sampled 0..64),
because sampling negatives into a nat parameter manufactures counterexamples outside the function's
domain (measured 2026-10-17: Euclid is not symmetric on negative ints) — ignoring refinements and
preconditions; an input the body rejects at runtime is a **skipped** case, never a counterexample, so
a partial function's domain gaps don't manufacture false refutations. **`--wide`** replaces the small
bounded domain with sampling ACROSS MAGNITUDES (log-uniform to 2^60, nats non-negative): number
theory's cases live far above −4..4, and a law that HELD over 200 wide samples is evidence the small
domain cannot give — still sampling, never a proof. A CONSISTENT/UNVERIFIABLE law
that quantifies only over first-order data (e.g. `map`'s `forall xs. eq(map(id, xs), xs)`, which the
example path cannot reach because the worked examples bind `xs` to the wrong shape) becomes a real
HELD over hundreds of generated inputs (or EXHAUSTIVE when its domain is small). For a large domain
it remains a sampled search rather than a proof — but it finds counterexamples the examples never
would, and over a small domain it *is* a proof.

## SMT proof (the unbounded rung)

`nl-validator prove <record> --body <body>` discharges a `forall` law over the **unbounded** domain,
the rung above sampling and bounded enumeration. Each property and the function body are translated to
**SMT-LIB 2** (the `Int`/`Bool`/`String` fragment: arithmetic, comparisons, boolean connectives, the
string operations `str_concat`/`str_length`/`str_contains` mapped onto the solver's native string
theory (`str.++`/`str.len`/`str.contains` — with the needle/haystack argument swap, ours is
needle-first), string literals and string equality, `let`, boolean `case` → `ite`, and `self` inlined
as a `define-fun` whose **parameter sorts are inferred from body usage** — a string-consuming function
quantifies over `String`, not `Int`); the tool asserts the **negation** of the law and asks a solver
(z3 by default, anything that speaks SMT-LIB on `--solver`) whether it is satisfiable:

- **PROVED** — the solver returns `unsat`: no counterexample exists *anywhere*, a real proof over all
  inputs (not just a sampled range). E.g. `double`'s `forall n. eq(self(n), add(n,n))` is proved for
  every integer; a four-variable commutativity law that the bounded enumerator can't cover is proved
  outright; `str_length(str_concat(a, b)) = str_length(a) + str_length(b)` and `wrap_parens` adding
  exactly two characters are proved over **every string**.
- **REFUTED** — `sat`: the solver's model is a concrete counterexample (e.g. `n = 0`, or the empty
  string against `forall s. str_length(s) > 0`); exit 1.
- **UNKNOWN** — the solver gave up.
- **UNSUPPORTED** — the property or body is outside the `Int`/`Bool`/`String` fragment (lists,
  `str_split`/`str_join` — no counterpart in the solver's string theory — and `to_string`/`parse_int`,
  which deliberately do NOT map onto `str.from_int`/`str.to_int` because the solver's semantics differ
  from ours on negatives (mapping them would be unsound); higher-order arguments, recursion, opaque
  callees). Reported honestly, never silently "proved" — the same boundary the generative checker
  draws as UNGENERATABLE.

**Tuples and sums are SMT datatypes** (2026-10-17). A function whose result is a tuple (`ext_gcd`'s
`(g, (x, y))`) or a sum (`mod_inverse`'s `Maybe int`) lowers against datatypes registered from the
DECLARED types — the function's own signature (installed for the proof) and its callees' contracts —
so tuple construction and destructuring patterns, variant construction and matching, and the
`fst`/`snd` projections become constructors, accessors and testers; every certificate declares the
datatypes it uses. A bare variant (`Just(e)`) takes the unique registered sum carrying its tag; an
ambiguity refuses. This is what lets a contract be stated about a tuple result (`ext_gcd`'s Bézout
identity, `fst(result) == a·fst(snd(result)) + b·snd(snd(result))`) and a checker record match a
callee's `Maybe` result. Lists stay with the inductive backend.

**`case` in a predicate** (2026-10-29). A contract on a sum-typed result names the payload the way
the body that produced it does: `case result of { Just(x) => (a * x) % m == 1 % m; None => true }`
is `mod_inverse`'s postcondition. The predicate `case` is the body-expression `case` (same node,
same pattern kinds — `spec/surface-syntax.md` §2), lowered as a tester-guarded `ite` over the
datatype and evaluated by the generative checker through the interpreter's pattern matcher; the
declared result type's datatypes are registered before a refinement obligation is lowered, since
the obligation inlines the body. A `Maybe`-valued function is composable by contract with this:
`crt2`'s contract is proved through `mod_inverse`'s, not its definition.

`prove` ranges a law over the function's **declared domain** (2026-10-17): a law variable the law
feeds to a `nat`-typed parameter of `self` is asserted `≥ 0`, as the generative checker already
samples it — without that, a law over `lcm : nat nat → nat` is asked to hold where `gcd`'s
contract axioms say nothing, and the solver times out on a question the record never posed.

The emitted SMT-LIB script **is the proof certificate** (`--smt-out <dir>` writes one per property):
any SMT solver re-checks it independently, so a receiver verifies by re-checking the certificate
rather than trusting this tool (principles 3, 5). Solving is deterministic for a given script + solver,
so the certificate is the replay log. Scope of this first-order pass: decidable quantifier-free
linear/nonlinear integer + boolean reasoning. Laws over recursive structures (lists) are handled by the
inductive backend below.

## Inductive proof (unbounded recursive structures)

When the first-order pass reports UNSUPPORTED because a law ranges over lists, `prove` falls back to a
**structural-induction** backend (`prove.rs` → `induct.rs`). A plain SMT query over recursively-defined
functions plus a universal quantifier is undecidable — the solver will not invent the induction — so
the tool supplies the induction principle and lets the solver discharge each case. For
`forall xs. P(xs)` over `Lst = nil | cons(Int, Lst)`:

- **base** — prove `P(nil)`;
- **step** — for fresh `h`, `t`, *assume* `P(t)` (the induction hypothesis) and prove `P(cons(h, t))`.

Each case is an SMT-LIB obligation: the list operations the law uses (`length`, `append`, `reverse`,
`map`, `filter`, `cons`, …) are emitted as z3 `define-fun-rec` definitions over the `Lst` datatype, the
case substitution is applied, the IH is asserted (step only), and the negated goal is checked. **Both
`unsat` ⇒ PROVED by induction.** `map`/`filter` take at most one function/predicate, modelled as `id`,
a single uninterpreted symbol — so `forall f xs. length(map(f, xs)) = length(xs)` is proved with `f`
*uninterpreted* (i.e. for every f) — or a single **closed lambda literal**, which is emitted as a
`define-fun` over its actual body (so `length(filter(\x -> lt(x, 0), xs)) = <the recursive count>`
proves: the induction reasons about the real element predicate; alpha-equal lambdas are one form;
a capturing lambda, or two *different* lambdas across one goal, refuse — one shared symbol for
different functions could prove a false equivalence). The base and step scripts together are the
re-checkable certificate.

### Lemma discovery

Where one unfold of the definitions plus the IH does not close the step — a law that needs an auxiliary
lemma, classically `reverse(reverse(xs)) = xs` — the prover does not give up. It selects relevant lemmas
from a **curated catalog** of standard list-algebra laws (`lemmas.rs`: `append_nil`, `append_assoc`,
`reverse_append`, `length_append`, `map_append`, `filter_append`, `length_nonneg`), **proves each one by induction first**
— recursively, since lemmas depend on one another: `reverse_append` rests on `append_assoc` + `append_nil`
— and then re-runs the stalled obligation with the proved lemmas asserted as universally-quantified axioms.
`reverse(reverse(xs)) = xs` is now **PROVED**, discovering `reverse_append` (and transitively
`append_nil`, `append_assoc`).

**Order-independent instantiation + minimal subsets.** Two e-matching hazards make naïve axiom assertion
fragile, and the higher-order list laws expose both. First, z3's choice of *which* quantified lemma to
instantiate depends on **assertion order** — the same lemma set closes a goal in one order and returns
UNKNOWN in another. Each lemma axiom therefore carries an explicit **trigger** (`:pattern`) on its
left-hand side (the rewrite-from term), pinning instantiation regardless of order. Second, asserting
*every* admissible lemma at once overwhelms instantiation (associativity + reverse/append distribution are
classic trigger loops): so when the full catalog set stalls, the prover retries with **minimal subsets**,
smallest first, and closes with the least set that works — `filter(p, reverse xs) = reverse(filter p xs)`
needs exactly `filter_append` + `append_nil`, and the extra `reverse_append`/`append_assoc` axioms break
it. The exploratory subset attempts run under a short solver budget (a real list-law proof closes in well
under a second), so the search doesn't dominate wall-clock. With both, `map(f, reverse xs) = reverse(map f
xs)` and `filter(p, reverse xs) = reverse(filter p xs)` are **PROVED**.

This is sound by construction: a lemma is assumed only after it is itself discharged, so assuming only
*true* facts can never close a *false* goal — `reverse(xs) = xs` stays NOT-PROVED, and a true law whose
lemma the catalog lacks (one needing a *non-catalog* lemma, e.g. `reverse(append(reverse xs, ys)) =
append(reverse ys, xs)` under the catalog alone) stays UNKNOWN, never a false PROVED. Lemma relevance is
gated by the goal's *prelude closure* (the recursive functions it already defines — and a `reverse` goal
pulls in `append`), so an unrelated lemma's recursive definition can't derail the solver into a timeout. The certificate is the whole proof tree — the goal's base + step
(assuming the lemmas) plus **each lemma's own base + step** — every obligation `unsat` on its own, so a
receiver re-checks the entire tree rather than trusting it (principles 3, 5). The solver still runs
under a wall-clock timeout, so an undecidable query reports UNKNOWN rather than hanging.

Proved live by induction: `map(id, xs) = xs`, `length(map(f, xs)) = length(xs)`, `length(append(xs, ys))
= length(xs) + length(ys)`; **`reverse(reverse(xs)) = xs`**, **`map(f, reverse xs) = reverse(map f xs)`**,
and **`filter(p, reverse xs) = reverse(filter p xs)`** via lemma discovery.

### Theory exploration

When the *curated* catalog can't close a goal, the prover (`explore.rs`) conjectures fresh lemmas the way
QuickSpec / Hipster do — **theory exploration**: it enumerates well-typed terms over the goal's
operations (within its prelude closure, in the first-order list fragment), tests each on a fixed battery
of inputs, and **buckets terms by equal results** — terms that agree on every test are conjectured
equal. The survivors are then **proved by induction** (the same Layer A machinery, recursively) before
being assumed. Testing is only a *filter*; soundness still comes from the proof, so a conjecture that
passes the tests but isn't a theorem is rejected when its induction fails — a false goal can never be
closed. Enumeration and the test battery are fixed (no RNG), so exploration is deterministic and its
certificates re-check (principle 5).

To stay sound *and* fast, discovered lemmas are added one at a time and the goal is retried with a
**minimal** axiom set (catalog + a single discovered lemma); piling every conjecture into one query
overwhelms the solver's quantifier instantiation even when a small subset closes instantly. Proofs are
memoized, so a shared lemma is discharged once. Demonstrated live: `reverse(append(reverse(xs), ys)) =
append(reverse(ys), xs)` — which needs `reverse_append` (catalogued) **and** reverse-involution (not
catalogued) — is **UNKNOWN under the catalog alone but PROVED once exploration discovers the involution
lemma**; the whole proof tree (the goal plus every discovered and catalog lemma's base/step) re-checks.

Conjectures are ranked smallest-first and **capped** (the caller proves each by induction, so the cap
bounds cost; raising it globally multiplies that cost on every goal — a measured 4× regression). To reach
a lemma the cap would truncate, exploration is **relevance-guided**: the smallest-cap base is kept exactly
(never reordered or dropped — a goal that closes within it is unaffected), and a few conjectures from
*beyond* the cap are **promoted** because they share a non-trivial operator skeleton
(`reverse(append(_,_))`, …) with the terms the goal equates, so they can fire there as a rewrite. Promotion
runs only after the base is exhausted (the caller early-stops), so it costs nothing on common goals;
soundness is unchanged because a promoted conjecture is still proved before it is assumed.

### Induction over user-defined recursive bodies

The same machinery proves laws about a **user-defined** recursive function, not just the built-in list
ops. Supply the function as `self` (a body); the prover encodes that body as its own `define-fun-rec
self` — the body branches with a boolean `case` on `null(xs)` and recurses via `self`/`apply(self, …)`
(the language has no native `cons`/`nil` patterns) — and the induction discharges it exactly as it does
`reverse`/`append`. Demonstrated live: a user-defined recursive `length` is **proved to distribute over
`append`** (`self(append(xs, ys)) = self(xs) + self(ys)`); a false law over the same `self` (e.g.
`self(xs) = 0`) is correctly NOT-PROVED. The recursive function may **return a list**, not just a
scalar: `self`'s SMT return sort is inferred from its base arm (a base case of `nil` ⇒ `Lst`), so a
cons-recursive map (`self = \xs -> case null(xs) of true -> nil | false -> cons(2*head xs, self(tail
xs))`) is **proved length-preserving** (`length(self xs) = length xs`). A **two-list-parameter** `self`
is also handled — induction is on the first list, the second carried as a spectator — so a user-defined
recursive `append` is **proved length-additive** (`length(self xs ys) = length xs + length ys`). Scope:
the recursion is on the first list parameter, with at most one additional spectator parameter; three or
more parameters are out of fragment and reported UNSUPPORTED.

### Folds

`foldr(f, z, xs)` and `foldl(f, z, xs)` are each encoded as a `define-fun-rec` over one global
uninterpreted binary `foldfn`, so a fold law is proved for **every** `f`. `foldr` discharges with the
ordinary induction hypothesis; `foldl` threads its accumulator, so for fold laws the step additionally
asserts the hypothesis **generalized over the non-induction variables** (`forall others. P(t, others)`),
which the solver instantiates at the changed accumulator. Demonstrated live: both `foldr` and `foldl`
are **proved to distribute over `append`** (`fold(f, z, append(xs, ys)) = …`), each certificate
re-checking. A law about a user-defined `self` gets the same generalized hypothesis (2026-10-17): a
recursion may change a spectator at its recursive call — `pointwise_mod` tails BOTH lists — and the
ground hypothesis at the fixed free constants misses the instance the step needs; the carried-spectator
cases still close by the ground instance, measured. Surface-parsed laws and bodies (curried
application spines, bare literals) are normalized at the backend's entry, as the first-order prover
always did — until they were, every surface-authored cons-recursive body read UNSUPPORTED here.

`map`/`filter` exploration (the SMT backend models their function argument as an uninterpreted symbol
and rarely discharges such laws even when handed the lemma) remains future work.

## Numeric induction (recursive functions over the integers)

A law about a function that recurses on an INTEGER — `gcd`, `isqrt`, `mod_pow`, a `nat`-result
contract on Euclid (the integer-tier module, 2026-10-17) — is outside both rungs above: no
`define-fun` may be recursive, and the structural backend inducts over lists. `prove` and
`check-refinement` then try **strong induction over the body's own well-founded measure** — the
argument position every self-call shrinks, with its floor, exactly as the termination checker
identifies it (`terminate::induction_measure`). The encoding supplies everything the solver would
otherwise have to invent: `self` is an **uninterpreted** function (never a recursive definition z3
must unfold on its own); the induction hypothesis is the law itself, quantified over primed copies
of the law's variables and restricted to measures in `[floor, m)`; the defining equation is
**instantiated explicitly** at every `self` application the law makes and once more at every recursive
call inside those (two levels), as `(self t…) = body[params := t…]` with the body's own `self` calls
left uninterpreted; then the negated law is checked. **UNSAT ⇒ PROVED by numeric induction on `m`.**
SAT is **not** a refutation — away from the instantiated points the uninterpreted `self` is
under-constrained, so a model may be spurious — and is reported UNKNOWN; neither direction can
mis-verdict. A law whose `self` applications all put a LITERAL at the measure position (`count(0) = 0`)
needs no hypothesis: the instantiated equation alone decides it (unfolding). The rung is a **ladder**:
unfolding alone is tried FIRST (the instantiated equations without any hypothesis — measured, that
is what proves `gcd(a, b) = gcd(b, a)`: with the quantified hypothesis present z3 answers unknown,
without it unsat), then the hypothesis is added. A law that applies `self` at **several measures**
(commutativity, associativity, Pascal's rule) inducts on the **sum** of the quantified nat variables
its measure arguments mention — well-founded over the nats, and the hypothesis then covers every
application whose measure arguments add up to less; a variable not nat-constrained refuses, stated.
Out of scope: `fn_ref`s in the body beyond their certified contracts (the composing rung below).
Measured on the integer-tier records: `count(n) = n`, `2·Σn = n(n+1)`,
`isqrt`'s floor bound and `0 ≤ mod_pow < m` prove; `gcd ≥ 0`, `isqrt ≥ 0`, `next_pow2 ≥ n` prove;
the binomial's non-negativity and the digit-sum bound do not close (nonlinear steps); the false laws
tried stay UNKNOWN.

**Lists in this rung, and the structural measure (2026-10-17, the integer-tier module's fifth
follow-up).** Two shapes fell between the rungs: a recursion over an INTEGER that builds or reads a
list (`pad_to xs n` counts `n` down while consing — its length law inducts on `n`, which the
structural backend cannot do, and lists were out of this rung's fragment), and a structural
recursion with a NUMERIC contract (`from_digits : nat → List nat → nat`'s `result ≥ 0` — a
`head` in the step, no numeric measure). So this rung lowers lists — the structural backend's
`Lst = nil | cons(Int, Lst)` datatype with `length`/`append`/`reverse` as the solver's own recursive
definitions and `cons`/`head`/`tail`/`null` as its constructors, selectors and tester — while the
**first-order prover keeps refusing them**, so a plain list law still reaches the structural backend
first (that ordering is what keeps every earlier list proof on its proven path). The measure may now
be **structural**: when every recursive call tails the same list parameter the termination checker
reports that position (`Measure::structural`), and the induction is over it. Its hypothesis is NOT
the quantified form with a guard `length(m') < length(m)`: over the recursive `length` definition
that guard is exactly what z3 cannot discharge (measured on `from_digits`: the obligation closes in
10 ms without the guard and times out with it). It is stated as explicit **instances** at each
tail-descending recursive call — `P[vars := the call's arguments]`, guarded by the descended list
being a cons (on `nil` the selector `tl` names an unconstrained value and the instance would be
unsound) and by the domain facts at the instance — the form in which the checker proved the
descent, and exactly the strong structural induction principle instantiated. The declared element
domain is stated too: a law variable fed to a `List nat` parameter is asserted `all_nat` (every
element `≥ 0`), the list analogue of a `nat` parameter's `≥ 0` — `from_digits ≥ 0` is FALSE over
Int-element lists (`ds = [−1]`) and SOUND over its declared domain, and the certificate shows which.
The ladder on the CLI and in `check-refinement` runs this rung after a structural induction that
ended UNKNOWN as well as after one that refused. Measured: `pad_to`'s `length(self(xs, n)) = n`
PROVED by induction on `n`; `from_digits ≥ 0` SOUND by induction on the structure of `ds`; the false
strengthening `≥ 1` and the Int-element variant stay UNKNOWN; the first-order prover still reports a
list law UNSUPPORTED.

**Contracts instantiated, and a law decided from the function's own contract (2026-10-17, the
sixth follow-up).** A callee's certified contract entered an obligation as quantified axioms only.
Two measurements moved it: `isqrt(n·n) = n` is a consequence of `isqrt`'s proved floor bounds
`r² ≤ x < (r+1)²`, and over the quantified form z3 answers *unknown* (nonlinear arithmetic under
quantifiers) while the same facts stated at the one application the law makes — `x := n·n` — are
unsat in 10 ms; and a checker record's `case ext_gcd(a, b) of (g, (x, y)) ⇒ a·x + b·y = g` applies
its callee inside the BODY, in parameter scope, where no instance could reach. So (1) every
contract fact — posts, and laws whose own `self` application fixes all their variables — is stated
as an INSTANCE at each application the obligation makes, beside the quantified axiom: the
obligation's own applications, plus the inlined body's applications at every `self` application
the goal makes (`let` over the parameters, the induction rung's unfolding discipline); (2) the
contract ladder: an obligation over linked callees z3 cannot decide with the quantified axioms is
retried with the instances alone (consequences of the axioms — weaker, so as sound); (3) the
**self-contract rung**, the last on the ladder in `prove`: a law outside every induction
principle is decided from the function's OWN certified contract — its proved postconditions and
laws, re-derived by the linked view exactly as a callee's are (without `--records`, a one-record
view of the record and body in hand), with `self` uninterpreted under that contract, instantiated
at the law's applications, and the law's variables fed to `nat` parameters domain-qualified.
Nothing unproved enters, so the rung is as sound as composition by contract; a satisfiable
negation is UNKNOWN. Measured: `isqrt(n·n) = n` PROVED from the contract; the checker law
`bezout_holds` proves on the callee's domain (`a, b ≥ 0`, the nat-qualified law) and stays UNKNOWN
over all ints — ext_gcd's post is certified for nats, and the prover will not extend it; the
false `isqrt(n·n) = n + 1` stays UNKNOWN (never REFUTED).

**Induction through a callee's definition (2026-10-17, the seventh follow-up).** A checker record
is not recursive — `digits_roundtrip = \b n -> from_digits(b, digits(b, n)) = n` — yet its law is
an induction: on `n`, through `digits`' own recursion (`digits(b, n) = cons(n mod b, digits(b,
n div b))`) and `from_digits`' structure. Neither callee's CONTRACT says it (`digits` has none;
`from_digits ≥ 0` does not relate the two); their DEFINITIONS do. The rung inlines `self` (it is
non-recursive, as the first-order prover does), lowers each linked callee's body over fresh
parameter names with its own recursion as the callee's symbol, and instantiates the defining
equation at every application the goal's `self` applications make (the body's applications,
`let`-bound over `self`'s parameters) and one level deeper at the recursive calls inside — the
numeric rung's discipline. The induction variable is the law variable that reaches a recursive
callee's measure position (`terminate::induction_measure` on the CALLEE's body — one analysis,
one more verdict); the hypothesis is an **instance** at that callee's recursive-call argument
(`n div b`), guarded by the measure decrease, measured once more against the quantified form:
the instance is unsat in 14 ms, the quantifier times out — and so does the callee's quantified
`≥ 0` contract axiom, so the contract ladder runs here too (quantified, then instances only).
Callees stay uninterpreted otherwise; SAT is UNKNOWN. Measured: `digits_roundtrip` PROVED on
`n` through `digits`; the false `… = n + 1` stays UNKNOWN. The rung is the last on `prove`'s
ladder, after the self-contract rung.

**The Euclidean rung, the hypothesis as instances, and the function's own facts (2026-10-18, the
eighth follow-up).** Three measurements on the integer tier's open laws, each a rung. (1) **The
Euclidean rung** (`tooling/validator/src/euclid.rs`): every modular identity the records state —
the sum and product congruences, `gcd` dividing its arguments, a digit sum bounded by the number
— is a consequence of one fact, the uniqueness of Euclidean division (`m ≠ 0 ∧ x = q·m + r ∧
0 ≤ r < |m| ⇒ x div m = q ∧ x mod m = r`), which z3 4.13, z3 4.16 and cvc5 all prove in exactly
that form (z3 4.16 answers unknown on the `m > 0` form and on `(x + m) mod m = x mod m`) while
none of them proves a congruence directly: `mod` by a variable is nonlinear and the solver does not
find the quotient. So the rung computes it: every `(mod t m)`/`(div t m)` in a certificate (`m` not
a literal) has `t` read as a polynomial over its atoms, each atom replaced by its own decomposition
`(div y m)·m + (mod y m)`, and `t = P + m·Q` split — the lemma INSTANTIATED at `x = t, q = Q +
(P div m), r = P mod m` says `t mod m = P mod m`, the ring homomorphism ℤ → ℤ/mℤ at exactly the
goal's terms; a nested residue `(mod X m)` of a compound `X` is lifted one occurrence per step
through `(mod X m) = X − m·(X div m)` so both sides reach one canonical residue polynomial; a
variable the certificate also divides by another modulus is first rewritten by that
decomposition (`a = (a div b)·b + (a mod b)` before reducing modulo `g` — Euclid's divisibility
step). The lemma is discharged by the solver first (cached per solver); each instance keeps the
lemma's premises, so a wrong witness is vacuous, never unsound, and SAT keeps its meaning. A retry
on UNKNOWN in every ladder (first-order, numeric and callee induction, the self-contract rung,
refinements); the goal's terms get the cross-modulus variants, hypotheses' terms their plain
instance only (measured: the full set closes Euclid's divisibility in 18 s, the pruned one in
0.1 s). (2) **The hypothesis as instances**: the numeric-induction ladder gains a third rung — the
hypothesis stated at each recursive call the unfoldings reach, each instance bound to the call's
arguments in goal scope and guarded by the measure decrease and the domain facts — after the
quantified form; the digit-sum bound and `digit_sum ≥ 0` close there, with no lemma (the
quantified hypothesis over `div`/`mod` terms leaves z3 at unknown — the pattern measured on every
rung before). (3) **The function's own certified facts in its inductions**: a step that needs a
proved fact about `self` — `gcd` divides both of its arguments only because `gcd ≥ 1` is certified;
without it the deepest, unconstrained application can be 0 and `mod` by it says nothing — gets
the record's contract (its proved posts and laws, as the linked view re-derives them)
instantiated at every `self` application the certificate states. Derived in PHASES so that
nothing assumes itself (`certify::derive_self_facts`): laws by the plain rungs; then posts with
those laws as facts (`binomial ≥ 0` needs its law `k > n ⇒ C(n, k) = 0`); then the remaining laws
with the posts and the phase-1 laws — and `check-refinement`, `certify` and the linked view all
read this one derivation, so a dependent never sees a fact its callee's own report does not show;
`prove`'s own retry never installs the law under proof. Two measured limits, recorded: a goal whose
two sides meet at residues the solver forms only through equations (an unfolding equating a
`self` application with a `mod` term — the fast-exponentiation step `pow(b, 2e) = pow(b, e)²
mod m`) is out of reach — substituting those equations into the goal was built and measured, and
the instance set it produces (108 for Euclid's divisibility) leaves z3 at unknown on certificates
it otherwise closes in 0.1 s: a chain of nonlinear premises is a proof to check stepwise, not one
query; and a witness that is a combination of a HYPOTHESIS (the Bézout identity's `q = −y − (a mod
m)·(x div m) + …` for `a·x ≡ 1`) is one the engine does not search for (measured by hand: with that
one instance z3 closes the modular-inverse law instantly). Also found: the generative checker
SKIPS a case where the law's evaluation errors (`gcd(0, 0) = 0`, then `0 mod 0`), so the unguarded
divisibility law read EXHAUSTIVE while the prover's model with the modulus 0 was honest; the law
is now guarded at `(0, 0)`.

**The stepwise checker (2026-10-18, the ninth follow-up).** The rung after the one-query
Euclidean rung, in every ladder (`euclid::retry_stepwise`), for a goal whose residues the two
sides reach only through the certificate's equations — the fast-exponentiation step. Measured
first: each Euclidean instance's PREMISE (a ring identity over the solver's `div`/`mod` axioms)
is unsat in 20 ms when checked with nothing but the declarations and `m ≠ 0`, and unknown beside
the hypotheses; and with every premise verified the final query was still unknown, because `mod`
by a variable and products of uninterpreted terms are nonlinear whether or not the proof needs
their semantics. So: (1) the instances are generated with the certificate's equations (an
unfolding's `A = E`, a hypothesis's conclusion, the branches of an `ite`) substituted into the
goal — exact matches to depth 3, then application atoms matched loosely (compound arithmetic
arguments may differ: `e − 1 = 2·(e div 2)` holds in the odd case), the order being the
measurement (29 exact instances closed the doubling law, 192 mixed ones did not); (2) each
premise is its own query (2 s each, 60 s in all; an unverified one is left out, stated); (3) the
uniqueness lemma and the idempotence lemma `(x mod m) mod m = x mod m` are discharged once per
solver; (4) the FINAL query states the hypotheses, the verified conclusions as `m ≠ 0 ⇒ …` facts,
the idempotence lemma at every residue, and the negated goal, with `mod`/`div` by a non-literal
modulus and products of two or more non-literal factors ABSTRACTED to uninterpreted
`umod`/`udiv`/`umul` — congruence plus linear arithmetic, decidable, answered in milliseconds
(unsat with the right instances; SAT, a model and not a timeout, without — which is what made
the last gap findable). Sound: the abstraction is a sound over-approximation (a model of the
original interprets the symbols as the real operations), every identity a conclusion rests on
was verified with the real operations in its own query, nothing is assumed that a query did not
discharge. The certificate is the final query, the premise script (every `(check-sat)` must
answer unsat) and the two lemmas. The callee rung, for its part, now states the hypothesis at
EVERY recursive callee's descent the induction variable reaches, and records the body's callee
applications at the hypothesis's application so the callees' laws are instantiated there (the
naive power's doubling law at `e div 2`). Measured: `pow(b, 2e) ≡ pow(b, e)² (mod m)` PROVED by
numeric induction (156 premises), and `mod_pow(b, e, m) = pow_mod_naive(b, e, m)` PROVED by
induction through both callees' definitions (251 instances, 250 premises); the false doubling
`… + 1` stays UNKNOWN.

**The witness rung (2026-10-29, the tenth follow-up).** The quotient witness that is a combination
of a HYPOTHESIS (above: the Bézout identity for `a·x ≡ 1`) is searched for, in a bounded space.
Every equation the certificate's hypotheses state (through implications, conjunctions and both
branches of an `ite`) is a polynomial identity over the chain's atoms; oriented at a monomial of
degree ≥ 2 with unit coefficient it is a rewrite rule — the Bézout identity `g = r·x + m·y` with
lead `r·x`; a residue equation `(mod T m) = R` through the exact identity `T = R + m·(div T m)`
with lead `T`'s monomial (`mod_inverse`'s own contract at `(m1, m2)` gives `m1·inv → (1 mod m2)
+ m2·(div (m1·inv) m2)`). When the chain's residue polynomial is stuck (nothing left to lift), a
monomial containing a lead — each lead atom itself or its residue, the residue UN-LIFTED through
`(mod y m) = y − m·(div y m)` to expose the atom — is rewritten by the rule and the chain
continues; the quotient that comes out is the hypothesis-derived one. Such an instance's premise
holds MODULO the rule's hypothesis, so the instance records the assertion it used and the
stepwise checker verifies the premise beside exactly that assertion and the certificate's small
facts (preconditions, a `case`'s tester, domain bounds — never an unfolding or a quantified
axiom). Two companions: a residue of ANOTHER modulus that is a polynomial multiple of the chain's
(`m1·m2` against `m1`, the Chinese-remainder shape) is lifted through its own identity, its `n`
part a multiple of `m`; and the witness rung's chains continue one substitution per premise (a
lift with the lifted term's atoms bare, else one atom decomposed) — the CRT chain's lift and
three decompositions as one premise are past 40 s for z3, one at a time milliseconds. Soundness
is unchanged (a rule only proposes a term; every premise is checked; an undischarged one leaves
its instance out or vacuous), and so are the plain chains' instance sets and timings: the
witness rung's instances come after the plain ones, are a further one-query retry, and have their
own 20 s budget stepwise. Measured: `mod_inverse`'s `Just(x) ⇒ a·x ≡ 1 (mod m)` SOUND, `crt2`'s
`Just(x) ⇒ x ≡ a1 (mod m1) ∧ x ≡ a2 (mod m2)` SOUND through `mod_inverse`'s contract (stepwise),
and the two checker laws on those domains first-order from the contracts at the point of use.
Same evening, the quotient lift: a quotient atom `(div X n)` of another modulus is lifted to its
quotient polynomial when the rules reduce `X`'s residue part modulo `n` to zero (`X = n·Q + 0`,
so `(div X n) = Q` by uniqueness) — `lcm`'s `(a·b) div g` is `g·(a div g)·(b div g)` by `gcd`'s
certified divisibility, and `g·(a div g)` is `a` by the same facts read the other way, so
`lcm(a, b) mod a = 0` on the guarded law (the unguarded one asks `0 mod 0` at `a = 0` and is
unprovable as stated); a residue atom may be a rule's degree-one lead (`(mod a g) → 0`), and the
goal's `ite` in arithmetic position is pushed to its branches as a variant.

## Semantic equivalence

`nl-validator equiv` decides whether two functions compute the same thing — `∀x. f(x) = g(x)` over the
unbounded domain — the operable form of *semantic equivalence vs hash equivalence* (two records can be
hash-different yet behaviorally identical). Functions of **any arity ≥ 1** are supported (the prover quantifies over several variables, inducting on
one and treating the rest as free).

A **normalization fast path** ([Canonical normalization](#canonical-normalization)) runs first: if the two
bodies share a canonical normal form — equal up to α-renaming, AC ordering of commutative operators
(`add(a,b) ≡ add(b,a)`), constant folding, and identity elimination — they are equivalent, decided
structurally with **no solver**. This also settles many cases where *both* sides recurse.

Otherwise it reuses the property prover by **inlining**, introducing no new encoding for the common cases:
with both sides non-recursive it builds `eq(f(x…), g(x…))` (operations stay visible to lemma discovery);
when one side recurses it becomes `self` (a `define-fun-rec`) and the other is inlined; and when **both**
recurse, both bodies are emitted as `define-fun-rec`s and `∀p0 ps…. f(p0, ps…) = g(p0, ps…)` is discharged by
**structural induction over the leading list parameter**, with **every remaining** parameter (any count —
arity is not capped) threaded through both functions as a **spectator** — declared free in the goal and
**∀-quantified in the induction hypothesis**, so both a *carried* spectator (append's second list, unchanged)
and a *descending* one (zipWith's, tailed each step) close. The induction **stride** is `lcm(stride_f,
stride_g)`: when both strides are read off the AST it is targeted directly in a single attempt (`lcm ≤ 60`,
so 3-vs-5 = 15 closes), and when a stride can't be read the prover *searches* `k = 1..12` — so
recursions that misalign by a constant stride (length-by-1 vs length-by-2, 2-vs-3, 3-vs-5) still close, and
when the bare step stalls the prover draws on the curated **list-algebra lemma catalog** (`append_nil`,
`append_assoc`, … — each proved by its own induction before being assumed), so a both-recursive step that
needs such a lemma closes too. The base cases double as refutation: a *satisfiable* base case is a concrete
short list (with concrete spectators) on which the two functions differ — a clean **DISTINCT**.

Verdicts: **EQUIVALENT** (the law is proved, or the normal forms are equal), **DISTINCT** (a counterexample —
from the first-order solver or a refuting base case), **UNKNOWN** (a non-closing induction is *not* a
refutation, so it is never reported as DISTINCT — e.g. two both-recursive functions that neither normalize
alike, align within stride 6, nor close via a catalog lemma), or **UNSUPPORTED** (nullary, mismatched arity,
a non-list leading parameter, a higher-order parameter, or recursive functions of arity > 2). Proved live:
`\xs. reverse(reverse xs) ≡ \xs. xs` and `\a b. add(a,b) ≡ \a b. add(b,a)` (the latter without a solver), two
list-sums written `add(head, self(tail))` vs `sub(self(tail), neg(head))`; `double ≢ \n. n+1` is DISTINCT,
`sum ≢ length` is DISTINCT at `[2]`. The node exposes this as `POST /v0/equiv`.

**Arity > 2 is supported.** The spectator machinery is not limited to one extra parameter: *every*
non-leading parameter is threaded through both functions and ∀-quantified in the induction hypothesis, so
the generalized IH closes carried, descending, and concrete-unfold spectators at any arity. E.g. an arity-3
`interleave3` (and arity-4 `interleave4`) built with nested `cons` vs one built with `append` of a concrete
prefix — equal only by unfolding `append` each step — is PROVED in-house; a distinct arity-3 pair is
refuted by a base case. Two tail-accumulators that thread the head element into *different* accumulators
(both `= a + b + sum(xs)`) also PROVE, via **accumulator-collapse lemma discovery**: for a function with
≥ 2 Int accumulators the prover conjectures and proves (by its own induction) `g(xs, a, b) = g(xs, 0, a+b)`,
which canonicalizes the interchangeable accumulators; asserting it for both sides lets the two-recursive IH
bridge the rest. The lemma's e-matching trigger must be the *plain* application `g(x, a, b)` — a trigger
carrying an interpreted `+` is silently dropped by z3, so the phrasing matters. (A cvc5 `--quant-ind`
fallback was investigated for the arity > 2 gap and reverted: with the in-house arity cap lifted, cvc5
proved nothing the in-house prover + normalization don't already.)

`nl-validator cluster <dir>` lifts this to a whole record set — behavioral-equivalence **classes** with a
canonical representative. To stay tractable it buckets functions by a coarse **signature shape** (arity +
coarse parameter/result types, type variables as wildcards) so only same-shape functions are ever
compared, then runs a union-find proving equivalence pairwise within each bucket (skipping pairs already
merged). The canonical representative is the lexicographically smallest content-address. This is the
deduplication-beyond-byte-identity that principle 2 calls for. Worked: a set with `\n. add(n,n)`,
`\n. mul(2,n)`, `\n. mul(3,n)` clusters the first two together and leaves the tripling distinct. Scope
follows equivalence (any arity ≥ 1 with one side non-recursive, **plus both-recursive pairs of arity ≤ 2**),
and cost within a shape
bucket of size *k* is up to O(*k*²) solver calls — the bucketing keeps that from being O(*n*²) over the
whole set.

### Canonical normalization

`nl-validator normalize <body>` rewrites a body-expression AST to a **canonical normal form** via
meaning-preserving rewrites, so functions reconcilable by those rewrites share one canonical artifact (and
one `expr_` content-address) — a step beyond merely *picking* a representative. The rewrites: **α-renaming**
of bound variables to positional names; **AC ordering** of associative+commutative operators
(`add`/`mul`/`and`/`or`/`xor`) — flatten across nesting, fold literals, drop the identity element, sort the
operands — so `add(a,b)` and `add(b,a)` coincide; **commutative ordering** of `eq`/`neq`; **constant
folding** of the total `Int`/`Bool` operators (`div`/`mod` are left alone, to avoid a divide-by-zero
rewrite); **identity elimination** (`add(x,0) → x`, `mul(x,1) → x`, …) but **not** *absorbing* elements
(`mul(x,0) → 0` is unsound under a non-terminating `x`); **idempotence** for `and`/`or`/`min`/`max`;
**subtraction-as-addition** (`a-b → a+(-b)`, negation distributed over sums); **negation-normal form**
(De Morgan + comparison negation); **involution** (`neg∘neg`, `not∘not`), `id(x) → x`, literal `nat → int`;
and **polynomial normalization** — `add`/`mul` are read as a polynomial `Σ cᵢ·monomialᵢ + c₀` over their
atoms, products of sums are **expanded**, and like monomials combined, so `x+x ≡ 2·x`, `(a+1)·(a−1) ≡
a·a−1` (difference of squares), and `2·(a+b) ≡ 2·a+2·b` share a normal form. Each rewrite preserves
meaning, so equal normal forms imply equivalence; the polynomial form additionally **never drops an atom**
— a monomial that cancels is dropped only when its atoms survive in other monomials (the `−a+a` of
`(a+1)(a−1)` cancels because `a` lives on in `a²`); when an atom would survive nowhere (`x+(−x)`, `a·b−a·b`)
the rewrite aborts, keeping every operand — so it is sound by construction, not only by the value-level
property test. A few **sound list rewrites** also apply — `reverse(reverse x) → x`, `reverse(nil) → nil`,
`append(x, nil) → x` / `append(nil, x) → x` (each retains every subterm; a *selector* like
`head(cons h t) → h` is excluded, as it drops a field). And **sound string rewrites**
(spec/expressiveness.md phase 1): `str_concat` spines are flattened, empty-string literals dropped (the
identity), adjacent literals folded, and the spine **right-nested** — concatenation is associative but
NOT commutative, so there is no operand sorting — which makes every association and literal-split of
one concatenation share a normal form (`("a" ++ s) ++ "b" ≡ "a" ++ (s ++ "b")`, solver-free);
`str_length`/`str_contains` fold on literals. With this, `normalize` is a **decision procedure
for the integer polynomial fragment**; for the list fragment it decides the sound-local equalities (so
`reverse(reverse xs) ≡ xs` is solver-free) but the induction-requiring list laws remain a normalizer
(*unequal* normal forms say nothing). This backs `equiv`'s
solver-free fast path — `double-via-add ≡ double-via-mul`, `(a+1)(a−1) ≡ a·a−1`, and a both-recursive
`2*head` vs `head+head` sum now close with no solver — and is deterministic (principle 5), so a body has
exactly one normal form.

## Composition metadata

`nl-validator compose <f1> <f2> …` derives the metadata of a sequential pipeline (each stage applied to
the previous stage's result) from the stages' own signatures — the operable answer to *composition
opacity*: a pipeline of well-described leaves is otherwise itself undescribed. It checks **type
composability** (stage `i`'s result type must fit stage `i+1`'s parameter type, structurally, with
type variables as wildcards) and propagates: **effects** = the union of every stage's effects;
**capabilities** = the union; **termination** = `always` only if every stage is `always`, else
`unknown`; **complexity** = **precise** when every stage carries the v0.3 `cost` metadata, else a coarse
upper bound (the maximum stage class, or `unknown` if any is unrecognized). Composability is decided
structurally/coarsely, but the composite's input/output **types** are computed **precisely** by threading
type variables through the pipeline (fresh-instantiate each stage, unify each result with the next
parameter), so `wrap : a → List a ; head : List b → b` composes to the exact `a → a`, not the imprecise
`a → b`. Worked: `reverse ; length` → `List a → nat`, effects `[]`, terminates `always`; `length ; reverse`
is **not composable** (a `nat` result can't feed a `List` parameter). **Stages may be multi-argument**: the
threaded value feeds each stage's *first* parameter, and a stage's remaining parameters become **auxiliary
inputs of the composite**, so `f : a → b ; g : (b, c) → d` composes to `(a, c) → d`; the reported composite
is `(input, aux…) → output`, a unary pipeline being the no-auxiliaries case and a nullary stage having
nothing to thread (non-composable). Complexity is measured in the size of the primary/threaded input,
auxiliaries held constant (the single-variable `cost` model).

**Precise complexity (`cost` metadata).** A stage's `signature.cost` declares its `time` class, the
`measure` it counts (`size` or `value`), and its `output_size` relation to the input (`constant`,
`preserving`, `bounded`, `quadratic`, `cubic`). The composer threads the value's size through the pipeline
as a polynomial degree `d` in the input `n`: a stage costing `O(m^t · (log m)^l)` on a size-`Θ(n^d)` input
costs `O(n^{t·d} · (log n)^l)`, and its `output_size` updates `d` (constant → 0, preserving/bounded → `d`,
quadratic → `2d`, cubic → `3d`); the composite is the max term. This is **sound under expansion**, which
the coarse max is not — an `n`-to-`n²` stage feeding `O(m²)` work is `O(n⁴)`, which `max(O(n²), O(n²))`
misses — and it *tightens* collapse pipelines (after a constant-size output, `d = 0`, so a size-measured
downstream cost is `O(1)`). The size-collapse shortcut is kept sound by `measure`: a **value**-measured
stage (cost tracks a number's magnitude, not a structural size, e.g. `length ; factorial`) can't substitute,
so the whole composite falls back to the coarse bound rather than wrongly claiming `O(1)`. The
`cost-basis` line of `compose`'s output records which path was taken.

## Effect enforcement

A function record *declares* its effects (`signature.effects`, the closed ten-effect vocabulary:
`fs.read`/`fs.write`/`net.read`/`net.write`/`alloc`/`time`/`random`/`io.console`/`process.spawn`/
`panic`). Effect **enforcement** makes that declaration a capability the runtime checks, not just
metadata. The evaluator runs against a *granted* effect set; the effectful builtins — `print`
(`io.console`), `rand` (`random`), `now` (`time`), `panic` (`panic`), and the **real-I/O** ones
`read_file`/`write_file` (`fs.read`/`fs.write`), `http_get`/`http_post` (`net.read`/`net.write`; both
`http://` and `https://`, the latter over TLS), the general **`http`** (`http(method, url, headers,
body) → {status, body}` — the effect is decided by the METHOD, `net.read` for GET/HEAD and
`net.write` for every other verb, so one builtin covers the whole verb surface without widening the
read-only grant; the effects walker refines a literal method to the side actually performed and
treats a dynamic one as both; the `{status, body}` record result is what a mutating workflow
verifies against), its header-preserving sibling **`http_full`** (same call, same method-decided
effect — the response headers survive as a canonical `Map string string`: names lowercased,
values OWS-trimmed, duplicates comma-joined — so `Location`-driven workflows are expressible and
redirect-following stays in-language), `spawn` (`process.spawn`), and `replicate` (`alloc` — allocate a list of
`n` copies, the heap-allocating builtin with no external I/O) — gate on it, and each performed effect is
appended to a structured **trace** (principle 9: an AI-ingestible record of what the body did). Adding
an effect kind is just an entry in `builtin_effect`; enforcement, tracing, and inference follow
automatically. Two GW6 additions on top of the grant set: a **net grant may be host-scoped**
(`net.write@api.example.com` — enforced at the effect boundary where the URL is known; the bare
grant still means any host), and an `http` header value — or, since 2026-10-06, its request
**body** (an API that takes a credential in its payload: RDS's `MasterUserPassword`, any
"set password" call) — may carry a **`{{secret:NAME}}` placeholder** substituted from
operator-supplied `--secret NAME=VALUE` values only inside the live effect — the trace records the
placeholder (the symbolic body), never the credential, so replay needs no secrets
(spec/agent-loop.md §Scope has the doctrine). The OUTBOUND mirror (2026-10-17, the
aws-ecosystem-poc redaction rule): `eval --redact VALUE=PLACEHOLDER` rewrites every occurrence of
an observed value — an account id, a resource ARN — in the trace's details and results and in the
printed result, after the live effect and before anything is recorded, so a service-assigned
identity never enters an observation; an equal-length placeholder keeps length-dependent
computations consistent, and `run` on the record minted from the redacted trace is the
consistency check (replay must reproduce the redacted result — it does whenever the body treats
the value opaquely). GW17 (2026-07-13) extended the scope grammar past
the host: a grant scope may carry a **path prefix** (`net.write@api.example.com/v0/things`) and
**fs grants scope by path** the same way (`fs.read@/data`), matched SEGMENT-ALIGNED at the effect
boundary (`/v0` covers `/v0/things`, never `/v0things`; query/fragment/port stripped) — one rule
for host, host+path, and fs scoping, and the legacy `http_get`/`http_post`/`read_file`/`write_file`
builtins carry scopes too. A grant may also be **trust-gated per function**
(`--grant-certified` on `respond`/`orchestrate --verify`): it counts only for a target certified
by a certifier the operator's policy trusts, decided per candidate and recorded as an auditable
`grants` transcript step (spec/agent-loop.md §Scope).

**Record / replay (principle 5).** Every trace entry records its `result` (what the builtin
returned), so a run is **replayable**: `nl-validator eval … --replay <trace>` makes the effectful
builtins return their recorded results in order instead of performing real I/O — the same body
reproduces deterministically, and the trace is sufficient to re-run it. So `read_file` reads a real
file on a live run (and is gated by `fs.read`), but returns the recorded contents under `--replay`
without touching the filesystem; `write_file` writes for real (gated by `fs.write`) but is a no-op on
replay; `http_get`/`spawn` fetch / run live but return recorded results on replay. Live: capture with
`--trace-out <file>`, then re-run with `--replay <file>`. Demonstrated: a real `http_get` of
`http://example.com` recorded, then replayed with no grant and no network → same body.

- `nl-validator run <record> --records <dir>` grants exactly the record's declared
  `signature.effects`. A body that performs an effect it didn't declare is rejected at eval time — so
  a record that *under-declares* its effects fails its own examples. (Pure records declare `[]`,
  perform nothing, and are unaffected.)
- `nl-validator eval <body> --grant <effect>…` runs a standalone body with an explicit grant; an
  ungranted effect is rejected, and the trace of granted effects is printed.

Determinism (principle 5): `rand` draws from a fixed-seeded per-evaluation PRNG — same body, same
sequence — so an effectful run is as replayable as a pure one, and the trace *is* the replay log.
Worked example: [`greet.v0.2.json`](examples/greet.v0.2.json) (`\msg -> print(msg)` :
`string -> unit`, declaring `effects: ["io.console"]`) runs clean under `run`; the same body under
`eval` is **rejected** without `--grant io.console` and emits a one-event trace with it.

**Replay-checkable examples (GW12).** An effectful record's worked examples used to be checkable
only by re-running them live — grants, secrets, and a reachable service — so a commons consumer had
to take them on faith. An example may now carry a **`trace` reference** (`trc_…`,
[trace.schema.json](trace.schema.json) — the same first-class trace artifact an `observed` claim
uses): the recorded observations of one real run of that example, published alongside the record.
`run` then checks such an example by **replay** — the trace resolves through the same link map that
resolves `fn_ref`s, every effect is served from the record, **no grants, no secrets, no live
service**, and the trace must be consumed exactly (a leftover or mismatched observation fails the
example rather than confirming a prefix). The honest scope is the observed-claim one: a replayed
PASS means the documented result follows deterministically from the recorded observations, which
are the *publisher's* testimony. Pure examples never need a trace and are unaffected. The OpenAPI
adapter emits these automatically: its `--verify-against` live gate runs each example exactly once,
requires the live result to equal the documented one, and attaches the observed trace — so a
description-generated record lands in the commons with examples anyone can check offline.

**Static inference.** `nl-validator check-effects <record> --body <body> [--records <dir>]` is the
verification counterpart: it infers a body's effects *without running it* by walking the AST for the
effectful builtins it names, folding in the **declared effects of any `fn_ref` callee** resolved from
`--records`, and reports **SOUND** (inferred ⊆ declared), **UNDER-DECLARED** (the body performs an
effect the record omits — exit 1, caught before execution), or **UNVERIFIABLE** (the body applies a
*free* external callee, or references a `fn_ref` callee not resolvable without `--records` — so the
inferred set is only a lower bound; a *bound* parameter applied directly is effect-polymorphic, not
opaque). A function's *own* effects are what it performs directly;
a higher-order *argument's* effects belong to the caller (effect polymorphism), so `map`'s declared
`[]` is SOUND even though `map(f, xs)` runs `f` — but a concrete `fn_ref` the body itself references
*does* contribute its declared effects. Worked: `greet` → SOUND `[io.console]`; `double` → SOUND `[]`;
the `print` body against a no-effects record → UNDER-DECLARED; a body applying `greet` by `fn_ref` →
UNVERIFIABLE bare, SOUND with `--records` (its `io.console` folded in).

**Scope (v0.1, honest).** Ten effectful builtins exercise **all ten** effect kinds, each recorded for
replay. Nine perform real I/O; the tenth, `replicate` (`alloc`), allocates a list of `n` copies on the
heap — the one effect kind with no external I/O, so it is fully deterministic and replays identically
(the trace records only the requested size; a negative size yields `[]`). Net and process are gated
**off by default** (must be granted). The net client speaks both `http://` and `https://` (TLS via
rustls + the ring provider, verified against the Mozilla webpki roots), and transparently de-chunks
`Transfer-Encoding: chunked` responses so the caller sees a clean body with no chunk-size markers.
