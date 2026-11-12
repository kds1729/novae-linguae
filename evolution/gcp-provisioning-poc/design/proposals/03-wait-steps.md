# Proposal 03 — the wait obligation as a plan step

- **Status:** proposed (design phase, 2026-11-12). Built at the start of phase 3 on the author's go.
- **Module:** [`gcp-provisioning-poc`](../../README.md)
- **Addresses:** deferred item 10, gap 3 (decision 34); charter R16; `spec/world-state.md` "Deliberately out
  of v0.1: mid-plan probing".
- **Changes:** `plan.schema.json` `steps[]` gains a second step form, the wait step; `check-plan` treats it as
  ensuring `(resource, ready)` (or `(resource, absent)` for scrub); the kitchen (`run` over a plan) executes
  it as a paced poll with a limit.
- **Blast radius:** additive. Plans without wait steps are unchanged.

## The change

A **wait step** is a plan step declared at plan time:

```json
{ "kind": "wait",
  "resource": { "class": "sql-instance", "key": [ { "kind": "string", "value": "webshop-g7-primary-db" } ] },
  "until": "ready",
  "probe": { "target": "fn_…", "args": [ … ] },
  "interval_seconds": 15,
  "limit_seconds": 900 }
```

- `check-plan` treats it symbolically as `ensures (resource, until)` and, before that, `requires (resource,
  exists)` for `until = ready` (nothing to wait on otherwise). It is a step like any other in the sequence.
- The kitchen executes it by calling `probe` every `interval_seconds` and reading the class's readiness
  field (proposal 01) until the declared value is observed, or until `limit_seconds` have elapsed, which
  fails the step as `wait-timeout` — a named failure that stops the run.
- A wait step is the **only** observation inside a plan. It reads exactly one declared resource's state. The
  rule that observation probes verify the initial world (the assumptions) stays intact; the "different
  discipline" the specification warns about is confined to this one declared step shape, whose effect on
  the symbolic state is exactly its declared `ensures`.
- The `RetryAfter` finding of `evolution/aws-ecosystem-poc` applies: where the probe's answer carries a
  retry hint, the kitchen may use it in place of `interval_seconds`; the limit stands.

## Why

Eventual consistency is the ordinary case in cloud provisioning. Without a declared wait, either the kitchen
waits on its own (an undeclared behaviour, with an undeclared timeout, invisible to the checker) or the plan
runs ahead and fails at run time. Both make a timeout a silent event. Declaring the wait at plan time keeps
the plan literal and complete (design test, decision 16): the checker sees the readiness the later steps
rely on, and a timeout at run time is a failure with a name.

## Driver case

Worked example: a wait on `sql-instance(webshop-g7-primary-db)` until RUNNABLE, inserted by the planner
between the instance create and the database user create; limit 900 s from the recipe's readiness
declaration. For scrub, a wait until `absent` after each delete, within the same limit.
