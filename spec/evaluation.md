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
`head` `tail` `length` `null` `append` `reverse`), tuples (`fst` `snd`), `id`, and the higher-order
`map` `filter` `foldl` `foldr` `compose` `apply`. `eq`/structural equality is the semantics of the
`lit` pattern.

**Composition.** Applying a `fn_ref` value resolves its target (a function content-address, or a
body's own `expr_` address) against a link map and runs the referenced body — so records assemble and
run end-to-end (principle 4). `nl-validator run --records <dir>` builds the link map from a directory
and resolves both a record's `body_hash` and its `fn_ref` arguments.

**Scope (v0.1, honest).** Integers are 128-bit and `nat` is a non-negative `int`. Most builtins are
pure; the effectful ones (`print` → `io.console`, `rand` → `random`, `now` → `time`, `panic` →
`panic`) are gated by a capability sandbox (see **Effect enforcement** below). The evaluator does not enforce exhaustiveness or types;
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

## Termination checking

Every record declares `signature.terminates` (`always` / `conditional` / `never` / `unknown`), but it was
only *propagated* (a composite is `always` only if every stage is), never *checked* — a record could claim
`always` for a body that loops. `nl-validator check-termination <record> --body <body>` verifies a declared
`always` **structurally**, with no solver. Over the first-order fragment (the arithmetic/boolean/comparison
builtins plus `head`/`tail`/`cons`/`null`/`length`/`append`/`reverse`) a body provably halts when it is
**non-recursive**, or **structurally recursive**: every `self`-call's recursion argument is `tail^k(p)`
(`k ≥ 1`) of one fixed parameter `p` — a list is a finite inductive structure and `tail` strictly shrinks
it, so the recursion is well-founded and halts (normally, or with an error at `nil`). The analysis is
**sound and conservative**: a recursion whose argument is not a strict structural descent, `self`-calls
descending on different parameters, or any **higher-order / opaque** application (`map`/`filter`/`fold`,
applying a parameter or an `fn_ref`, whose callee's termination is unseen) is `Unknown` — never a false
`always`, the same honesty stance `check-effects` takes for opaque callees. Verdicts: **SOUND** (declared
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
`length`/`append`/`reverse` are `O(n)`): a **non-recursive** body is `O(1)` or `O(n)` (a finite AST over data
that stays `O(n)`); a **structural recursion** is solved as a recurrence `T(n) = a·T(n−k) + w`, where `a` is
the branching factor (the number of `self`-calls on the worst-case execution path — `case` arms are mutually
exclusive, so `filter` stays `O(n)` rather than reading as exponential), `k` the descent, and `w` the per-step
non-recursive work: one self-call with `O(1)` work → `O(n)`, one with `O(n)` work (an `append` of the
recursive result — naive `reverse`) → `O(n²)`, two-or-more constant-descent calls → **exponential** (a sound
upper bound: naive `fib`), and a **halving** descent (`div(p, c)`) → `O(log n)` / `O(n log n)`. Because a
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
connective operand → a bool), samples `N` inputs (default 100), runs the body, and reports per
property:

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
REFUTED is replayable. Generation ranges over the inferred *type*, ignoring refinements and
preconditions; an input the body rejects at runtime is a **skipped** case, never a counterexample, so
a partial function's domain gaps don't manufacture false refutations. A CONSISTENT/UNVERIFIABLE law
that quantifies only over first-order data (e.g. `map`'s `forall xs. eq(map(id, xs), xs)`, which the
example path cannot reach because the worked examples bind `xs` to the wrong shape) becomes a real
HELD over hundreds of generated inputs (or EXHAUSTIVE when its domain is small). For a large domain
it remains a sampled search rather than a proof — but it finds counterexamples the examples never
would, and over a small domain it *is* a proof.

## SMT proof (the unbounded rung)

`nl-validator prove <record> --body <body>` discharges a `forall` law over the **unbounded** domain,
the rung above sampling and bounded enumeration. Each property and the function body are translated to
**SMT-LIB 2** (the `Int`/`Bool` fragment: arithmetic, comparisons, boolean connectives, `let`, boolean
`case` → `ite`, and `self` inlined as a `define-fun`); the tool asserts the **negation** of the law and
asks a solver (z3 by default, anything that speaks SMT-LIB on `--solver`) whether it is satisfiable:

- **PROVED** — the solver returns `unsat`: no counterexample exists *anywhere*, a real proof over all
  inputs (not just a sampled range). E.g. `double`'s `forall n. eq(self(n), add(n,n))` is proved for
  every integer; a four-variable commutativity law that the bounded enumerator can't cover is proved
  outright.
- **REFUTED** — `sat`: the solver's model is a concrete counterexample (e.g. `n = 0`); exit 1.
- **UNKNOWN** — the solver gave up.
- **UNSUPPORTED** — the property or body is outside the `Int`/`Bool` fragment (lists, higher-order
  arguments, recursion, opaque callees). Reported honestly, never silently "proved" — the same
  boundary the generative checker draws as UNGENERATABLE.

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
`unsat` ⇒ PROVED by induction.** `map`/`filter` take at most one function/predicate, modelled as `id`
or a single uninterpreted symbol — so `forall f xs. length(map(f, xs)) = length(xs)` is proved with `f`
*uninterpreted* (i.e. for every f). The base and step scripts together are the re-checkable certificate.

### Lemma discovery

Where one unfold of the definitions plus the IH does not close the step — a law that needs an auxiliary
lemma, classically `reverse(reverse(xs)) = xs` — the prover does not give up. It selects relevant lemmas
from a **curated catalog** of standard list-algebra laws (`lemmas.rs`: `append_nil`, `append_assoc`,
`reverse_append`, `length_append`, `map_append`, `filter_append`), **proves each one by induction first**
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
re-checking.

`map`/`filter` exploration (the SMT backend models their function argument as an uninterpreted symbol
and rarely discharges such laws even when handed the lemma) remains future work.

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
**structural induction over the leading list parameter**, with one **spectator** parameter (arity ≤ 2)
threaded through both functions — declared free in the goal and **∀-quantified in the induction hypothesis**,
so both a *carried* spectator (append's second list, unchanged) and a *descending* one (zipWith's, tailed
each step) close. The induction **stride is searched** (`k = 1..6`, targeted at `lcm(stride_f, stride_g)`) so
recursions that misalign by a small constant stride (length-by-1 vs length-by-2, or 2-vs-3) still close, and
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
refuted by a base case. What stays UNKNOWN at higher arity is a step needing a lemma the generalized IH
doesn't supply — e.g. two tail-accumulators threading the head element into *different* accumulators (both
`= a + b + sum(xs)`), which needs an accumulator-invariance lemma. (A cvc5 `--quant-ind` fallback was
investigated for the arity > 2 gap and reverted: with the in-house arity cap lifted, cvc5 proved nothing
the in-house prover + normalization don't already, and failed the same residuals.)

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
`head(cons h t) → h` is excluded, as it drops a field). With this, `normalize` is a **decision procedure
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
`http://` and `https://`, the latter over TLS), `spawn` (`process.spawn`), and `replicate` (`alloc` — allocate a list of
`n` copies, the heap-allocating builtin with no external I/O) — gate on it, and each performed effect is
appended to a structured **trace** (principle 9: an AI-ingestible record of what the body did). Adding
an effect kind is just an entry in `builtin_effect`; enforcement, tracing, and inference follow
automatically.

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
