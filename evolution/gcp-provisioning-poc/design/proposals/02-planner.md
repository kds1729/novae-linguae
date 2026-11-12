# Proposal 02 — a deterministic planner that `check-plan` grades

- **Status:** proposed (design phase, 2026-11-12). Built at the start of phase 3 on the author's go.
- **Module:** [`gcp-provisioning-poc`](../../README.md)
- **Addresses:** deferred item 10, gap 2 (decision 34); charter R15.
- **Changes:** a new validator command, `nl-validator plan`, producing a `plan_` artifact from a set of
  steps with grounded world refinements; `plan.schema.json` gains an optional `waves` field (the wave
  structure beside the flat `steps`). `check-plan` is unchanged.
- **Blast radius:** additive. Existing plans have no `waves` and decide exactly as before.

## The change

`check-plan` verifies an order it is given; nothing in the language produces one. The planner does:

- **Input:** a set of steps, each `(id, target fn_, args, refinements)` with every world key already
  grounded to literals; the readiness declarations of the classes involved (proposal 01); optional data
  edges (producer → consumer) and optional soft-hint edges.
- **Output:** a plan `{assume, steps, waves}`: `assume` lists every requires no step satisfies; `waves`
  groups steps that may run concurrently; `steps` is the waves flattened, the sequence `check-plan`
  executes symbolically.
- **Procedure:** the one stated in [`../algorithms.md`](../algorithms.md) section 6: satisfaction edges from
  ensures to requires (with `ready` implying `exists`); a wait step inserted where a `ready` requirement is
  met only by an `exists`; data edges; Kahn's algorithm into waves; ties broken by `(serving id, step id)`;
  two steps ensuring the same resource is an error (`ambiguous-ensures`); a cycle is an error.
- **Determinism:** the same inputs give byte-identical plans, so a plan is a reproducible content-addressed
  artifact and two parties can compare addresses instead of plans.

The planner never decides soundness. Its output goes to `check-plan`, whose verdict is the only one that
licenses a run. A soft hint is an edge, never a satisfaction.

## Why

The module's provisioning command must run only a fully resolved, verified plan (principle 2, R15). Without a
planner, the order would be hand-written in each recipe, which makes the order an assumption of the author
rather than a consequence of the declared contracts, and makes composition fragile: a composite's steps
interleave with its components' steps in an order no author wrote. Deriving the order from requires/ensures
is what lets dishes compose freely (decision 17) while every run is still checked.

## Driver case

Worked example: four servings expand to ten operation steps, and the planner inserts one wait step. It
produces six waves: the network, the secret and the bucket in parallel; the peering address range and the
secret version; the private-service connection; the database instance; the wait until the instance is
RUNNABLE; the replica, the database user and the container service in parallel. `check-plan` grades the flat
sequence PLAN-SOUND with an empty assumption list (`worked-example/addresses.md`, planner output).

## Honest grading

A plan is a claim about the declared contracts, never about the world (spec/world-state.md). The planner
adds nothing to that: it arranges declarations; `check-plan` checks them; probes test the assumptions; the
run tests the declarations.
