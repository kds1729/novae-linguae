# Proposal 01 — a third built-in world state: `ready`

- **Status:** proposed (design phase, 2026-11-12). Built at the start of phase 3 on the author's go.
- **Module:** [`gcp-provisioning-poc`](../../README.md)
- **Addresses:** deferred item 10, gap 1 (decision 34); `spec/world-state.md` "Deliberately out of v0.1:
  richer state".
- **Changes:** `spec/world-state.md`; `function-record.v0.2.schema.json` `$defs/world_refinement.state` and
  `plan.schema.json` `assume[].state` gain the value `ready`; `check-plan`'s symbolic state and probes.
- **Blast radius:** additive. Every existing record and plan remains valid; `exists`/`absent` plans decide
  exactly as before.

## The change

The world-state vocabulary gains a third built-in state, `ready`, with one axiom: **`ready` implies
`exists`.** A resource that is `ready` exists and is usable: a database instance whose status is RUNNABLE, a
certificate that is ACTIVE, a network peering that is ESTABLISHED.

Satisfaction in the plan checker becomes: a `requires (g, exists)` is discharged by an `ensures (g, exists)`
or `(g, ready)`; a `requires (g, ready)` only by an `ensures (g, ready)`; `absent` as today. An `ensures
(g, ready)` is written by a step that observes readiness (a wait step, proposal 03) or by an operation whose
documented success already means usable (rare; stated by its author).

Probing: a probe answers `ready` when its read succeeds (2xx, so `exists`) **and** the class's declared
readiness field has the declared value; `exists` when the read succeeds and the field does not; `absent` on
404; inconclusive otherwise. The declaration of which read operation, which field and which value is per
resource class and lives with the recipes that use the class (this module's recipe record carries it under
`readiness`; a `spec/` home for shared declarations is a later, driver-gated step).

## Why

Most real dependencies in provisioning are on readiness, not existence. The module's worked example has a
container service that reads from a managed database: the database's create call returns while the instance
is still being provisioned; a service started against it fails to connect. With only `exists`/`absent`, the
plan checker would pass a plan that provisions the service as soon as the create call returns, and the run
would fail at the service, or worse, succeed on one day and fail on another depending on timing. The design
test (decision 16) says the planner must hold "the database is usable" as a fact it can order on. `ready` is
that fact.

## Why not attribute states

The discussion also recorded "attribute states" (a peering established, a replica caught up). Every such
case examined is the readiness of a resource of its own class: the peering connection is a resource; the
replica is a resource. One built-in state with a class-declared observation covers them without a general
attribute vocabulary, which would be anticipated rather than earned.

## Driver case

Worked example, serving `primary-db` (a managed Postgres instance): the create operation ensures
`(sql-instance(name), exists)`; the service's create step requires `(sql-instance(name), ready)`; the
planner inserts a wait step whose probe is the instance read operation with `ready_when state = RUNNABLE`;
the plan is PLAN-SOUND with the wait, and UNVERIFIABLE without it, which is the correct verdict.

## Honest grading

A `ready` ensured by a wait step is an observation made during the run, so a plan containing wait steps is
PLAN-SOUND *conditionally on the waits completing within their limits*; a timeout is a named failure of the
wait step at run time, never a silent pass (proposal 03).
