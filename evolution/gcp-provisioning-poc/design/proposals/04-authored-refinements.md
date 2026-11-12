# Proposal 04 — authored world refinements in the recipe, and the proposed-refinement path

- **Status:** proposed (design phase, 2026-11-12). Built at the start of phase 3 on the author's go.
- **Module:** [`gcp-provisioning-poc`](../../README.md)
- **Addresses:** deferred item 10, gap 4 (decision 34); `spec/world-state.md` "Deliberately out of v0.1:
  ingestion … world refinements are authored".
- **Changes:** `check-plan` accepts, per step, refinements supplied beside the step (a plan-local overlay
  keyed by the step's position), in addition to those on the target record; the key-part forms `arg`
  (positional) and `body-field` over a positional argument are added for that overlay; a `proposed-refinement`
  statement kind (this module's `statement` record) carries a suggested refinement with its trace evidence.
- **Blast radius:** additive. Records' own refinements keep their meaning; a plan without an overlay decides
  as before.

## The change

Ingested operations carry empty world refinements (the gcp-sdk-poc measurement: 81 of 81). Recipes need
them. Three rules:

1. **Refinements live in the recipe, keyed by operation.** A recipe step is an operation plus its authored
   `requires`/`ensures`, with keys over the step's own bindings (`arg i`, `body-field` of a JSON-bound
   argument, or a literal). A recipe is therefore self-contained, and two recipes may author different
   refinements for the same operation without touching the operation's record. When the recipe is expanded
   for a run, the step's refinements are grounded to literals and handed to `check-plan` as the step's
   overlay.
2. **Test provisioning verifies them.** A passing test provisioning (decision 36) shows every step
   succeeding when its `requires` held and every `ensures` observed afterwards (by the resources recorded in
   the receipt and the scrub's probes). An authored refinement is therefore priced like the operation
   itself: by attestation, under the receiver's trust policy. A reusable library of refinement records may
   exist for chefs to copy from; it is a convenience, not a source of truth.
3. **The proposed-refinement path.** When a run fails and its trace shows the cause (a "network not found"
   answer to a create), the mechanism may publish a `proposed-refinement` statement: the operation, the
   suggested `requires`, the trace as evidence. It is a suggestion in the publication stream. It becomes
   authored only when a chef adopts it into a recipe, which is then test-provisioned; the mechanism never
   promotes a hint by itself, and a hint never makes an UNVERIFIABLE plan PLAN-SOUND.

## Why

The design test: at the moment the plan checker reads a step, it needs that step's contract on the world.
Putting it in the operation record would require re-publishing an ingested record for every recipe's view
of it and would make the record's address depend on an author's judgment rather than on the API description
it was ingested from. Putting it in the recipe keeps the operation objective (what the description says) and
the refinement attributable (what this chef declares, tested by this attester).

## Driver case

Worked example: ten operation steps, each with authored refinements; for example the database user
create requires `(sql-instance(name), ready)` and ensures `(sql-user(instance, user), exists)`. None of
these is in the operation records; all of them are in the recipes; all of them are exercised by the test
provisioning of each dish.
