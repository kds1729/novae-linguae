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
- **`app`** evaluates the function and arguments and applies.
- **`let`** binds monomorphically.
- **`case`** evaluates the scrutinee and tries arms in order over the four pattern kinds
  (`wildcard` / `bind` / `lit` / `variant`); a non-matching scrutinee is a runtime error
  (exhaustiveness is the checker's job, not the evaluator's). `if` does not exist — it is `case` on a
  `bool` (principle 8).
- **`field`** projects a record field.

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
- `let` is monomorphic (a deliberate simplification).
- A declared `forall` type's variables are **skolemized to rigid constants**, so the body must be
  genuinely polymorphic: `\x -> x` checks against `forall a. a → a`, but `\x -> add(x, x)` does not.
- Verdict: **WELL-TYPED** (with the type) or **ILL-TYPED** (with the mismatch, exit 1).

**Scope (v0.1, honest).** `nat` is normalized to `int` (no refinement-aware checking here);
sum/`variant` and `ref` (named-type-by-address) types are opaque, so `case` arms over them are checked
structurally with fresh payload types rather than resolved; refinements and effects are separate
concerns, not checked here.

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

Where one unfold of the definitions plus the IH does not close the step — a law that needs an auxiliary
lemma, classically `reverse(reverse(xs)) = xs` — the solver (run under a wall-clock timeout, so an
undecidable query can never hang) returns UNKNOWN. That is reported honestly, never as a false PROVED.
Proved live: `map(id, xs) = xs`, `length(map(f, xs)) = length(xs)`, `length(append(xs, ys)) =
length(xs) + length(ys)` — each by induction; `reverse∘reverse = id` → UNKNOWN. Lemma discovery,
`foldl`/`foldr`, and induction over user-defined recursive bodies (`self`) remain future work.

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
