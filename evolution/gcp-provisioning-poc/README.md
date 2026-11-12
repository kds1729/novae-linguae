# Declarative infrastructure provisioning on Novae Linguae — project charter

- **Status:** exploring (concept and requirements phase; nothing implemented, nothing provisioned)
- **Author:** Keith Sprochi <kds1729@gmail.com>
- **Dates:** chartered 2026-11-10, last updated 2026-11-12
- **Scope:** a new evolution module. Reads against `evolution/gcp-sdk-poc`, `evolution/aws-sdk-poc`,
  `evolution/aws-ecosystem-poc` (certified operations, `check-plan`, world-state refinements, probes),
  `spec/world-state.md`, `spec/trust-model.md`, `spec/commons.md`, and the composite-record machinery
  (`compose`, `assemble`).
- **Provenance:** the concept was dictated by the author on 2026-11-10 and refined in discussion the same
  day; the vocabulary is in [`glossary.md`](glossary.md), the open questions in [`deferred.md`](deferred.md).
  Target cloud: Google Cloud Platform. No cloud account is bound yet.
- **Resolution:** —

## 1. Purpose

Make the construction and teardown of an entire cloud infrastructure a **selection** problem rather than a
**generation** problem. An orchestration agent is given a description of what a system needs, selects
pre-verified, fully formed parts from a shared library, has the composition checked mechanically, and
provisions the result to a specified end state. Nothing is generated from rules at provisioning time, nothing is
guessed, and every part that reaches the library has been verified before it can be chosen.

The long-range goal is a library large enough, assembled by many agents and people over time, that an
entire data center can be provisioned from one description with one command, and torn down the same way.

## 2. Why on Novae Linguae

The project already has the parts this needs, and this module uses them rather than rebuilding them:

- **Operations are verified function records.** The OpenAPI and Smithy adapters compile cloud API
  operations into certified records today; the AWS modules provisioned and tore down a two-region
  ecosystem through them.
- **Plans are checked before they run.** `check-plan` decides PLAN-SOUND, REJECTED, or UNVERIFIABLE over
  requires/ensures world-state refinements, and probes spot-verify assumptions against the live cloud.
- **Records compose and are content-addressed.** Composite records reference their parts by hash; an
  address can never change meaning underneath a consumer.
- **Trust is a policy, not an assumption.** Signed attestations and the receiver's local trust policy
  decide which publishers and certifications count.
- **The vocabulary is a type system.** Kinds of infrastructure are Nova Lingua record types; needs are
  refinements over them; coverage is predicate evaluation the validator already performs.

The founding rule of the project applies unchanged: an agent reads the **record**, never a body of text.

### 2.1 Why selection, not generation

A generation-based provisioner, one that stamps environments from rules in code and a mutable bill of
materials, fails as a record of what exists in predictable ways, and each failure names a property this
design must have:

| Generation-based failure | Property here |
|---|---|
| The record is edited forward in place; there is no "as of" | Plates and receipts are frozen and content-addressed |
| What an environment gets is computed at run time by walking an inheritance tree; an edit high up silently changes everything below | A plate is a stored selection; needs lists have no inheritance |
| Values are filled in late by token substitution, so some final values are never in the record | Dish inputs are only names and wiring; outputs land in the receipt |
| Provisioning is create-only and additive, so the record diverges from the cloud | Environments are re-provisioned, not changed; pets are add-only by policy and receipts record what each actually got |
| One shape per resource type | One plate shape for everything |
| Needs, provision, and per-environment exceptions mixed in one place | Needs list, plate, and instance are separate records |
| The only true snapshot is deleted after a retention period | Records are never deleted |

## 3. Scope

**In scope**

- Provisioning a cloud infrastructure to a declared end state from a needs list, a plate, and an instance.
- Tearing one down (**scrub**) from its receipt.
- The library: dishes, recipes, kinds, their publication, test provisioning, and trust-filtered menu view.
- The tools that choose: the provisioning form, the needs-list configurator, the consigliere, and the chef.
- The policy layer that answers "what may a run do here?"

**Out of scope**

- Software deployment, runtime configuration, loading data, warming caches, and every other
  non-deterministic step. The infrastructure is handed over empty.
- Pet migration (moving traffic from an irreplaceable environment to its replacement): an operational
  concern in the family of blue/green and canaries.
- Importing, observing, or reasoning about infrastructure that existed before this system.
- Secrets the organization already holds (a third-party key, a licence): runtime configuration, written into
  the environment after provisioning by the organization's own deployment tooling.
- Any one organization's conventions (layer 5).

**Fit.** The design suits organizations whose stored data can be copied or re-created outside this
system, because every change to an environment is a new environment beside the old one. Where production
data lives in resources that are not practical to re-provision, the fit is weaker; the pet exception
(principle 5) exists for that case. The design also has an upkeep cost that falls on its community: every
time the cloud's API changes underneath the library, the affected dishes stop provisioning and somebody has to
refresh the catalog (re-ingest the API description, re-author the affected variation chains, test-provision and
publish the successors). The mechanism makes that work enumerable and mechanical; it does not make it free. A
shared library of fixed parts stays usable only as long as its community refreshes it.

**Parallel solutions.** Adjustable settings on dishes and in-place change of resources are reasonable
things to want and are not part of this solution, because each gives up a property the design was chosen
for (section 2.1). Anyone who wants them is welcome to propose a parallel solution, for example a fork of
this module; a fork and the shared project can coexist and can exchange fixed dishes. The reasoning is in
[`convention-over-configuration.md`](convention-over-configuration.md).

## 4. Principles

These are the module's load-bearing walls. Everything in §6 derives from them.

1. **Never act on assumptions.** Where a choice is missing or ambiguous the system stops and names what is
   missing. It never guesses, substitutes, or follows replaced-by pointers.
2. **Selection is deterministic.** Selection is a tool the agent calls, never something it reasons out.
   All selection finishes before any execution starts. The kitchen runs only a fully resolved plan.
3. **Mechanism versus policy.** Generic mechanism is shared. Policy and organization specifics are
   configurable layers above it.
4. **Use Novae Linguae wherever it already supports the need.**
5. **Environments are cattle, not pets.** A change means provisioning a new environment and scrubbing the
   old. Pets are the exception, a policy fact, and are only ever added to.
6. **Reproduce the outcome, not the procedure.** What must be re-provisioned is infrastructure that meets the
   same needs, not the same API calls.
7. **The author is trusted for nothing.** A part is real when the mechanism has verified it, not when
   its author says so. This is what lets agents compose freely.
8. **The design test.** Every shape question is settled by asking what an agent must hold, 100 % accurate
   and in hand, at the moment it does the work.

## 5. The model in one paragraph

The model is a restaurant (full vocabulary: [`glossary.md`](glossary.md)). **Operations** are the
ingredients: one cloud API call each, verified. A **dish** is a fully formed unit with every behavioral
choice baked in and only names and wiring as inputs; dishes compose recursively from dishes and
operations, down to a whole data center. A **recipe** says how a dish is made. The **menu** is the
trust-filtered view of what is offered. A **needs list** says what the software needs, as typed
constraints and wires, independent of profile. A **profile** says what an organization wants of an
environment of a given kind, as constraints by kind. A **plate** freezes the choice of dishes that covers
one needs list for one profile, as **servings**, each a dish provisioned once. An **instance** says where and
under what names, every name written out, for one generation of an environment's lineage. The **provisioning command**
provisions and writes a **receipt**; **scrub** tears down from one. The **chef** composes dishes, the **consigliere** advises,
the **provisioning form** takes the order. All of these are content-addressed records.

## 6. Requirements

Numbered for reference. "Shall" marks a requirement; "may" marks a permitted behaviour. Each cites the
discussion decision it rests on where one exists.

### 6.1 Records and the library

- **R1.** Every object in §5 shall be a content-addressed Novae Linguae record. No record is ever deleted;
  state changes are later records that supersede.
- **R2.** A kind shall be a Nova Lingua record type whose fields are typed attributes. A dish's `provides`
  shall be a value of exactly one kind. A need's constraints shall be a predicate over its kind. A dish or need
  that uses an undefined attribute or a wrongly typed value shall fail validation at publish time.
- **R3.** A dish shall declare typed inputs and typed outputs. Its only inputs are names, placements (cloud
  project, regions, name-service zone) and wires.
- **R4.** A dish shall name exactly one recipe by content address; a changed recipe is a new dish. The
  recipe may reference other dishes by content address as components. Internal wiring
  between components shall be fixed at authoring time and carried in the recipe. A referenced dish is
  never modified.
- **R5.** A dish shall not be offered until a test provisioning has produced a passing attestation. A failed test
  provisioning shall mark the dish unprovisionable until a later passing one supersedes it. Test provisioning runs shall always
  keep their traces.
- **R6.** The menu shall be a view over records on a node, filtered by the receiver's trust policy on two
  axes: offered means published by a publisher the receiver trusts AND carrying a passing provisioning
  attestation from an attester the receiver trusts. The mechanism shall record both identities. A node with no
  trusted roots configured shall offer nothing and shall name that as the reason. The menu's change log is
  the publication stream.
- **R7.** Retiring or deprecating a dish shall be a published state record and shall remove nothing.

### 6.2 Needs, profiles, plates

- **R8.** A needs list shall be immutable, self-contained, and free of profile names and port names. Any
  two needs lists shall be diffable.
- **R9.** A need shall have a local name, a kind, constraints over that kind, and wires naming abstract
  roles toward other needs.
- **R10.** A profile shall be a content-addressed record of constraints by kind.
- **R11.** A plate shall pin, by content address, its needs list, its profile, and its servings. A serving
  is one dish, provisioned once. Every need shall map to exactly one serving; several needs may map to the
  same serving where the profile permits sharing for that kind. The plate shall record every wire resolved
  to a typed dish-output → dish-input binding. Resolution shall be type-checked when the plate freezes.
- **R12.** A plate may record that a dish was chosen by explicit override, with a reason. An override
  shall only choose from the mechanical candidate set.

### 6.3 Provisioning

- **R13.** The provisioning command shall take exactly three inputs, each by content address: a needs list, a plate, an
  instance. It shall derive everything else from records those reference. It shall refuse an instance that
  was not generated for the pinned plate.
- **R13a.** An instance shall be a frozen record naming the plate it was generated for, a lineage (the
  identity of an environment across its successive generations) and a generation, and holding, per serving,
  its placement and a literal name for every name input of its dish down through every component. The
  provisioning command shall derive no name and shall verify that every component name equals the fixed
  derivation in its recipe.
- **R13b.** The rules that generate an instance (naming convention, project allocation, region defaults)
  shall be policy; the record shape shall be mechanism. The generator shall learn a lineage's last
  generation from the lineage's latest receipt. An instance shall serve exactly one provisioning run.
- **R14.** Coverage shall be mechanical: for each need, the conjunction of the need's predicate and the
  profile's predicate for that kind, evaluated on the provides of the dish of the need's serving; every wire
  bound to compatible ports; and every sharing rule the profile states for a kind satisfied by the plate.
- **R15.** The planner shall produce the order of work from the recipes' requires/ensures and verify it
  with `check-plan`. A plan that is not PLAN-SOUND shall not run. UNVERIFIABLE is reported, never passed.
  Learned ordering hints may guide the planner but shall never make an unverified plan count as sound.
- **R16.** Eventual consistency shall be an explicit wait obligation: a probe plus a time limit.
- **R17.** If the plate is missing or pins an unprovisionable dish, provisioning shall stop and name it. The
  provisioning command shall never search for a substitute.
- **R18.** The provisioning command shall write a receipt recording plate, instance, per serving which dish actually
  stands there with the recipe run for it, per need which serving covers it, outputs with secrets only as
  references, and optional traces. The port types make the last of these so: a credential-bearing output has
  the secret-reference type, and the secret scalar type keeps any value that passed through the kitchen out of
  every record.

### 6.4 Scrub

- **R19.** Scrub shall take one input, a provisioning receipt by content address, and derive deletes from the
  recipes in reverse dependency order, verified with `check-plan`.
- **R19a.** Scrub shall be ordered through a scrub form, the counterpart of the provisioning form, and shall run only
  within what policy allows for that environment.
- **R20.** Scrub shall probe each resource gone within its wait obligation and write a scrub receipt that
  supersedes the provisioning receipt. A failed scrub shall stop, name the failure, and write a partial scrub
  receipt. Re-running scrub against the same provisioning receipt shall be legal and idempotent.

### 6.5 Re-provisioning

- **R21.** The system shall grade a re-provisioning against needs as identical (every pinned dish still provisions),
  equivalent (a new plate meets every need), approximate (a need is not met exactly and a person has
  signed a waiver; the environment stays labeled approximate), or unprovisionable.
- **R22.** Equivalent re-provisionings shall be composed by a deterministic selector: candidates per need are the
  offered dishes satisfying need ∧ profile with ports fitting the wires; ranked by trust policy;
  tie-broken by publication order. Same inputs shall yield the same plate.
- **R23.** The consigliere's advice shall enter the selector only as content-addressed preference rules,
  never as decisions.

### 6.6 Policy

- **R24.** Before any run the mechanism shall ask policy one question, "what may this run do here?", and
  shall act only within the answer.
- **R25.** Add-only shall be a general per-environment policy setting. A pet is an environment where it is
  permanently on.
- **R26.** The circuit breaker shall trip when the planner derives a forbidden delete, typically because a
  create-only setting differs in an add-only environment. Provisioning stops on the first such delete.
- **R27.** Policy shall name who may delete, what is protected from scrub, how long traces are retained,
  and which publishers are trusted.

### 6.7 Actors

- **R28.** All layer-4 tools shall be usable by an agent or a person. The agent is the default operator for
  everything, including composing dishes and plates; people are the fallback and the override.
- **R29.** Signing a waiver for an approximate re-provisioning shall be reserved to a person.
- **R30.** An agent composing a dish or plate shall be bounded exactly as a person is: menu offerings
  only, no search, no substitution; it shall stop and name what is missing.

### 6.8 Knowledge boundary

- **R31.** The system's knowledge of any environment shall be exactly the sum of its own receipts. It shall
  never observe, import, or reason about pre-existing state. A collision with a pre-existing resource is
  refused by the cloud API and surfaces as a stopped, named run.

### 6.9 Releases

- **R32.** A software release record shall name its needs list by content address. Several releases may
  share one.
- **R33.** The consigliere may read a release's SBOM to suggest needs. The system shall not produce an
  SBOM; the receipt is the infrastructure inventory.

## 7. Architecture

Five layers; each depends only on the layers below it.

| # | Layer | Concern | Holds |
|---|---|---|---|
| 1 | **NL** | The library | operations, kinds (types), content addressing, `check-plan`, attestations, trust policy |
| 2 | **Provisioning mechanism** | Doing exactly what it is told | dishes, recipes, needs, profiles, plates, servings, instances, receipts, coverage check, planner, kitchen, selector, test provisioning, the provisioning command, scrub |
| 3 | **Policy** | What a run may do | add-only, circuit breaker, who may delete, protected scrub, trace retention, trust in authors |
| 4 | **Profile realm** | What to choose | provisioning form, scrub form, needs-list configurator, consigliere, chef |
| 5 | **Organization specifics** | How it fits one company | not part of this module |

Layer 2 never searches, substitutes, or chooses; layer 4 never executes. The only judgment in the system
sits in layer 4 and, for waivers, with a person.

### 7.1 Lifecycle of a dish

1. A **chef** (agent or person) finds components in the menu and composes a dish: components by content
   address plus operations, with internal wiring and a recipe.
2. Layer 2 validates the composite against the kind's type, type-checks the wiring, and runs the **test
   provisioning** (provision, probe, scrub).
3. The dish is published with its provisioning attestation. The trust-filtered menu now offers it.
4. The cloud API drifts. The dish's next scheduled test provisioning fails and it becomes unprovisionable, a
   fact recorded by attestation. A chef authors a successor dish, with the same provides and a new recipe,
   which is test-provisioned and published like any dish; plates that pinned the old dish re-provision
   equivalently through the selector. The old dish is unchanged and remains resolvable.
5. The dish is deprecated, then retired, by published state records. Its record, and every plate that
   pinned it, remain resolvable forever.

### 7.2 Lifecycle of an environment

1. A needs list is authored (configurator, with the consigliere suggesting from the release's SBOM).
2. The selector composes a plate for a profile, with the consigliere's candidates per need as input; a person
   or agent may override within the candidate set. An optional made-from annotation records which needs list
   the new one was copied from.
3. The provisioning form generates the instance by policy, for the next generation of the environment's
   lineage. The provisioning command runs: coverage → `check-plan` → kitchen → receipt.
4. A change is a new needs list or plate, a new provisioning run, and a scrub of the old environment from its receipt,
   ordered through the scrub form.
5. If a pinned dish becomes unprovisionable, provisioning stops; the selector composes an equivalent plate; the
   provisioning command provisions it fresh.

## 8. Relationship to existing modules

- `gcp-sdk-poc` and `aws-sdk-poc` proved that cloud operations can be certified function records and that a
  live loop closes on real clouds. They supply the operation layer.
- `aws-ecosystem-poc` provisioned and tore down a two-region ecosystem through `check-plan`-verified plans
  and audited the teardown with the language's own probes. It is the closest precedent for the provisioning command and scrub,
  and its finding that *names, not placeholders, keep plans literal* is inherited by the instance design.
- The number tiers established the prover rungs this module's coverage check relies on
  (`check-refinement` over typed records), and the practice of cross-checking every validator change
  against every module.
- The stranger tests established the measure of agent-facing work: an unaided agent, given only a URL,
  succeeds or does not. This module will be measured the same way.

## 9. Open questions

Maintained in [`deferred.md`](deferred.md). The ones that block implementation, as opposed to discussion:
the test provisioning (where it runs, what it costs, under which cloud account) and the layer-1 gaps (a `ready` state, a planner, mid-plan waits).

## 10. Plan

This module advances in gated phases. A phase does not begin until the previous one's gate is met.

1. **Concept** (this charter, the glossary, the deferred list). *Gate:* the author declares the plan
   fully realized. **Current phase.**
2. **Design.** Record schemas for kind, dish, recipe, needs list, profile, plate, instance, receipt;
   the coverage and selection algorithms stated precisely; the test-provisioning question in
   `deferred.md` answered. *Gate:* a worked example (the glossary's web service) hand-traced from needs
   list to receipt with every record written out.
3. **Prototype against the fake service.** Kinds and dishes over `tooling/fake-service` operations;
   provisioning and scrub end to end with no cloud. *Gate:* the worked example provisions, scrubs, and audits clean.
4. **Live GCP slice.** One small needs list, one profile, real GCP operations through the OpenAPI
   adapter; test provisioning runs, the provisioning command, scrub, audited by probes. *Gate:* a stranger agent provisions and scrubs
   the slice unaided.

Nothing in phases 2–4 starts without the author's explicit go.

## 11. Documents

| File | Role |
|---|---|
| `README.md` | this charter and requirements document |
| [`glossary.md`](glossary.md) | the vocabulary, by layer, with the GCP worked example |
| [`deferred.md`](deferred.md) | open and provisional questions, with candidates and reopen conditions |
| [`convention-over-configuration.md`](convention-over-configuration.md) | guide: why dishes have fixed settings and environments are replaced rather than changed, and what that costs |
