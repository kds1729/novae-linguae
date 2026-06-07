# Evaluation & type checking (v0.1)

*Status: v0.1, implemented in [`tooling/validator`](../tooling/validator/src/) — `interp.rs`
(evaluator) and `typecheck.rs` (type checker), exposed by `nl-validator eval` / `run` / `typecheck`
/ `check-properties --body`.*

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

**Scope (v0.1, honest).** Integers are 128-bit and `nat` is a non-negative `int`. Effects are not
modelled — a body that would perform I/O is outside this pure evaluator. The evaluator does not enforce
exhaustiveness or types; those are the checker's job.

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
— still example-bound, not a proof (a generative property-testing engine is the next rung).
