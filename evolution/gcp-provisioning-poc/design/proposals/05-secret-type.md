# Proposal 05 — a `secret` scalar type that nothing serializes

- **Status:** proposed (design phase, 2026-11-12). Built at the start of phase 3 on the author's go.
- **Module:** [`gcp-provisioning-poc`](../../README.md)
- **Addresses:** deferred item 10, gap 5 (decisions 31, 34); charter R18.
- **Changes:** `type-expression.schema.json` builtin enum gains `secret` (with its canonical type artifact,
  like the other builtins); `value-expression.schema.json` gains NO corresponding value kind, by design; the
  evaluator refuses to serialize a `secret` value into any record, example, trace entry or receipt and
  records it as the placeholder `{{secret}}` where a trace would otherwise carry it (the existing
  `{{secret:NAME}}` convention of `spec/trace.schema.json`, generalized); `typecheck` refuses a `secret` in
  any position other than an argument to a parameter of type `secret`, a `let` binding, or the result of a
  generator builtin.
- **Blast radius:** additive to the type vocabulary. No existing record uses the name.

## The change

A scalar type `secret` whose inhabitants exist only inside a run:

- **Introduction.** A builtin `gen_secret(length)` (effect `random`) produces one; an operation whose
  description marks a parameter as a credential is ingested with that parameter typed `secret`.
- **Use.** A `secret` may be passed to a parameter of type `secret` and bound by `let`. Nothing else: no
  comparison, no concatenation, no field of a record, no element of a list, no return from a function (a
  function returning `secret` is ill-typed). This is what makes "the kitchen discards it" a property the
  type checker enforces rather than a promise the kitchen keeps.
- **Serialization.** There is no value-expression kind for it, so no example, receipt, plan or claim can
  carry one; a trace records the placeholder. A record that tries to carry a `secret` is schema-invalid
  before it is ever hashed.
- **References.** What flows between dishes and lands in receipts is a **secret reference** (decision 31), an
  ordinary record type `{store: string, name: string, version: string}` naming an entry in the cloud's
  secret service. The reference is not secret and needs no special type.

## Why

Where the cloud offers identity-based access, no secret exists and this type is unused. Where a resource
accepts a credential only as a supplied value (a database user's password), a value must pass through the
kitchen once: generated, written to the secret service, set on the resource. Without a type, "it was never
written anywhere" is an audit claim about the kitchen's code; with the type, it is a consequence of the
record schemas and the type checker, checkable by anyone who reads the records (principle 7 applied to the
mechanism itself).

## Driver case

Worked example, serving `primary-db`: `gen_secret(32)` → `secrets.addVersion(secret-name, value: secret)` and
`users.insert(instance, user, password: secret)`; the receipt carries the output `credential` as a
secret-reference record naming the secret and its version; the trace carries `{{secret}}` at both call
sites. The container service receives the reference and the platform resolves it at start.
