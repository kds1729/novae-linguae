# Glossary — Declarative Infrastructure Provisioning on Novae Linguae

Companion to the concept summary (dictated 2026-11-10) and `decisions.md`. Terms are listed by the layer they
belong to, then alphabetically within it. Decision numbers refer to `decisions.md`. Entries marked
**provisional** rest on a decision the user accepted as "seems correct, model still under development".

Every object below that is a record is a content-addressed Novae Linguae record. Nothing is ever removed;
a later record supersedes an earlier one.

## The metaphor

The model is a restaurant. The **chef** makes **dishes** from **ingredients** and writes their **recipes**.
The **menu** lists what has been made. A diner places an order through the **provisioning form**, advised by the
**consigliere**; the order names a **plate**. The **kitchen** cooks the order and hands back a **receipt**.

## Layer 1 — Nova Lingua / Nova Locutio (what already exists)

**Attestation.** A signed Nova Locutio assertion about a record: that an operation is certified, that a
dish's test provisioning passed or failed, that a publisher is who they claim. Attestations are what the menu
view reads to decide a dish's state (decision 4).

**Check-plan.** NL's pre-effect symbolic plan checker. Verdicts: PLAN-SOUND, REJECTED (with provenance),
UNVERIFIABLE. Used by the provisioning command's planner and by scrub. Never silently passed.

**Content address.** The hash identity of a record. Dishes reference components, plates reference dishes and
their profile, receipts reference plates — all by content address, so meaning cannot change underneath.

**Kind.** A Nova Lingua record TYPE describing one sort of infrastructure thing (`relational-database`,
`network`, `object-store`, `container-service`). Its fields are the kind's **attributes**, each typed
(enumeration, nat, bool, …). Extending the vocabulary = publishing a new kind, which supersedes but never
invalidates the old one. "One concept, one symbol": no synonyms (decision 14).

**Ready state.** A state meaning a resource exists AND is usable (a database RUNNABLE, a cluster RUNNING, a
certificate ACTIVE), and attribute states (a peering established). NL's world-state vocabulary today has only
`exists` and `absent`; `ready` and attribute states are a named layer-1 gap this module needs closed
(deferred.md §10).

**Requires / ensures.** NL's world-state refinements over class+key resources. The dependency vocabulary of
recipes; the planner orders work from them, reversed for scrub. Ingestion yields them empty: authoring them
is the recipe author's job.

**Secret.** A scalar type whose values no record, output, trace or receipt will serialize. An operation that
takes or returns one is certified like any other; the type is what keeps the value out of every record. Where
a cloud resource accepts a credential only as a supplied value, the kitchen generates it, stores it, sets it and
discards it within one run, under this type. Not yet in NL: a named layer-1 gap (deferred.md §10; decision 31).

**Trust policy.** The receiver's local rules for which publishers and attestations count. Governs who may
publish a kind, a dish, or a recipe, and which dishes a menu view shows as offered (decisions 4, 11).

## Layer 2 — the provisioning mechanism

**Constraint.** A predicate (NL refinement) over a kind's attributes, e.g. `engine = postgres ∧ major ≥ 15`.
Needs carry constraints; so do profiles (decision 15).

**Coverage check.** The provisioning command's first step. For each need: evaluate (need predicate ∧ profile predicate for that
kind) on the `provides` of the dish of the need's serving; then confirm every wire is bound to compatible
ports and every sharing rule the profile states for a kind holds on the plate. Mechanical — it is
`check-refinement` with the dish's provides as subject (decisions 1, 14, 15, 18, 28).

**Dish.** A fully formed, selectable unit of infrastructure. Everything that affects behavior, topology, or
cost is baked in; its only inputs are names, placements (cloud project, regions, name-service zone) and wiring. Declares typed **inputs**, typed **outputs**, and
**provides** (a value of its kind). Dishes compose recursively: a dish's recipe may reference other dishes
by content address as components, down to operations at the leaves (decision 17). A dish is "a function
record with a cloud on the other end" (decision 21).
**Composition depth** is the author's choice: parts that are always tested together and do not vary
separately belong inside one composite dish; parts expected to be exchanged one at a time belong side by
side on a plate, each meeting its own need. A dish has no adjustable settings; a variation is a new dish
(see **variation chain** under cross-cutting terms, and
[`convention-over-configuration.md`](convention-over-configuration.md)).

**Generation.** A counter within a lineage: the n-th environment provisioned for that lineage. Every resource
name in an instance includes the generation, so the next generation can stand beside the current one in the
same project and region without a name collision (decision 29).

**Instance.** Where an environment is provisioned and what everything in it is called. A frozen record holding:
the plate it was generated for (by content address; its entries are keyed by that plate's servings); the
lineage and generation; per serving, the placement (cloud project, as many regions as the dish's provides
declares, a name-service zone where the dish asks for one); and per serving a literal name for every name
input of its dish, down through every component, with the recipe's fixed internal naming already applied.
Every name the kitchen sends to the cloud is in this record; the provisioning command derives none and
verifies each component name against its recipe. Written by the instance generator from the provisioning
form; used for exactly one provisioning run (decision 29).

**Kitchen.** The executor. Runs a fully resolved, verified plan — never anything less — and writes the
receipt. Eventual consistency is an explicit **wait obligation**: probe plus time limit.

**Lineage.** The identity of an environment across its successive generations ("the production environment
of product X"). The system knows a lineage only through the receipts of its generations; the instance
generator reads the latest to learn the last generation (decisions 9, 29).

**Menu.** Not a document: a trust-filtered VIEW over the dish, recipe, and attestation records on a node.
"Offered" = published by a trusted publisher with a passing test-provisioning attestation. The dated change log
is the publication stream itself. Dish states: **offered** (selectable), **deprecated** (selectable with a
warning; a better dish exists), **retired** (not selectable for new work; old plates still provision it) — each a
choice, published as a state record — and **unprovisionable** (a fact: a failed test-provisioning attestation, superseded
by a later passing one). Retired and unprovisionable stay distinct: one is a decision, the other evidence. Who
may deprecate or retire, and how long deprecated lasts, are policy. Retiring never removes a record
(decision 4).

**Need.** One entry in a needs list. Four parts: a **name** (local handle, the target of wires); a **kind**;
**constraints** over that kind; **wires** naming abstract roles toward other needs (`lives-in →
api-network`, `reads-from → primary-db`). A need is a range; a dish is a point. Needs never mention
profiles or ports (decisions 14, 15, 18).

**Needs list** (the doc's *infrastructure version*). An immutable, standalone record of what the software
needs: a set of needs with their constraints and wires. No inheritance between needs lists; any two can
be diffed. Profile-independent. *Naming note:* the doc calls this the "infrastructure version"; the
human-readable x.x.x label that went with it is deferred, and "version" is overloaded, so this glossary
says **needs list** (decisions 12, 13, 15).

**Made-from annotation.** An optional, separate record saying "needs list X was copied from needs list Y",
with free text explaining why. Not inside the needs list (that would change its address), creates no
dependency, never read by the mechanism; written automatically by the configurator. It tells a reader which
of the many possible diffs is the meaningful one.

**Operation** (ingredient). One cloud API call as a verified Novae Linguae function record. The leaf of
every recipe. Produced today by the OpenAPI adapter.

**Plate.** The frozen, content-addressed selection that covers one needs list for one profile. Records: its
**servings** (each one dish, provisioned once); the need → serving mapping, every need to exactly one serving;
the **profile** by content address; the resolved wires (dish output → dish input, type-checked when the plate
freezes); and for any dish chosen by override, that it was an explicit pick with a reason (decisions 15, 18,
23, 28).

**Planner.** The provisioning command's second step. Produces the order of work (and which steps may run in parallel) from the
recipes' requires/ensures, then verifies it with check-plan. New machinery: check-plan verifies an order it
is given and does not produce one.

**Soft hint.** Learned "this usually comes before that" knowledge, typically suggested by a failed run's
trace (a "network not found" error implies a missing `requires`). A hint may guide the planner's ordering
and may be approved by a person into a declared dependency. It never gates: a hint never turns UNVERIFIABLE
into PLAN-SOUND.

**Port.** A typed input or output on a dish. Wires bind an output port to an input port.

**Provisioning command** (the verb is *provision*). The function that creates an environment. Inputs: a
needs list, a plate, an instance — all by content address. Steps: coverage check → planner (check-plan) →
kitchen → receipt. If the plate is missing or pins an unprovisionable dish, provisioning stops and names it.
The provisioning command never searches for substitutes and never follows replaced-by pointers.

**Provides.** A dish's declaration of what it is: its top-level `ensures` over its kind, as a concrete value
(`ensures relational-database(engine = postgres, major = 15, ha = true, regions = 2, …)`). The same
world-state vocabulary recipes use for dependencies, so coverage and planning read one language. The thing a
need's predicate is evaluated against. Together with
the dish's ports and attestations, it is all an agent reads to decide whether a dish satisfies a need
(decision 22). A dish provides a value of exactly one kind; whether, for example, address translation is an
attribute of a network or a kind of its own is settled by the kind vocabulary, never by the coverage check
(decision 28).

**Receipt.** A standalone record of what one environment actually got: plate, instance, per serving WHICH
DISH ACTUALLY STANDS THERE (the pinned one, a substitute, or nothing after a partial provisioning) with the recipe
run for it, per need which serving covers it, outputs (secrets only as references, which the **secret
reference** port type makes so), optional traces. The plate is what an environment is
meant to have; the receipt is what it actually has. The system's ONLY knowledge of any
environment is the sum of its receipts (decision 9). A **scrub receipt** supersedes a provisioning receipt; a
**partial scrub receipt** records what is confirmed gone after a failed scrub (decisions 24, 25).

**Recipe.** A standalone record of how a dish is made: ordered create steps, delete steps, dependencies
(requires/ensures), **create-only settings**, and the dish's internal wiring (component output → component
input, frozen at authoring). Points at its dish. A dish can gain new recipes as the cloud API changes.

**Create-only settings.** Parameters the cloud API accepts only at creation (region, storage type,
encryption key). Changing one requires delete + recreate. Academic for cattle; decisive for pets
(decision 7).

**Scrub.** Teardown of one environment. One input: a provisioning receipt by content address. Asks policy, plans
the deletes in reverse dependency order, verifies with check-plan, runs, probes each resource gone within
its wait obligation, writes a scrub receipt. Stops-and-names on failure AND is idempotently retryable
(decisions 24, 25).

**Secret reference.** A record type (store, name, version) naming a secret held in the cloud's own secret
service. A credential-bearing dish output and the input that consumes it both have this type, so the plate-time
wiring check refuses a credential flowing into a plain-string port or a literal flowing into a credential port.
The secret itself is a resource created by the dish that produces it, named by the instance and deleted by
scrub like any other; there is no secret-store dish. The consuming operation passes the reference to the cloud,
which resolves it when the resource runs. A new generation creates fresh secrets; an addition to a pet wires to
the references in the pet's latest receipt. Dish authors prefer identity-based access over shared secrets where
the cloud offers it (decision 31).

**Selector.** The deterministic tool that composes a plate (new or replacement). Per need, the candidate set
is mechanical (offered dishes satisfying need ∧ profile predicates with ports fitting the wires); it ranks
by trust policy and tie-breaks by publication order; it consumes consigliere preferences as inputs. Same
inputs → same plate (decision 23).

**Serving.** One dish, provisioned once, on a plate. Every need is covered by exactly one serving. Two needs
pinned to the same dish are two servings of that dish and two separate resources (a primary and an analytics
database, both the same managed-database dish). Several needs may be covered by one serving when the profile
permits sharing for that kind (one database instance serving two services in a low-cost development profile;
forbidden in production by a profile rule on the plate). A need that no single dish satisfies is remedied by
a chef composing a dish or by the needs-list author splitting the need, never by the mechanism (decision 28).

**Test provisioning.** A provisioning run made to produce evidence rather than an environment for use: the provision-and-scrub of
a newly composed dish, the verification of a new recipe against its dish, and the periodic re-provisioning that
renews a dish's provisionable-as-of attestation. A dish is not offered without a passing one (decisions 19, 21).
Test provisioning runs always keep their traces regardless of trace-retention policy: there the trace is the evidence
that separates "the cloud retired this" from "our credential expired".

**Provisionable as of.** Whether a dish can still be provisioned is dated evidence, not a permanent property: a content
address preserves a dish's description forever, not the cloud's willingness to accept it. Renewed by test
provisioning runs; the cadence is policy. Where it runs, what it
costs, and under what account: not yet decided.

**Wire.** A reference between needs ("this service needs that database"). In a needs list it names an
abstract role. In a plate it is resolved to dish output → dish input, type-checked. Inside a dish the same
binding is frozen at authoring. By provisioning time every wire is a binding between pinned records; at kitchen time
a real value (a connection endpoint) flows along it. Wiring is the only cross-dish structure in the model
(decisions 6, 18).

## Layer 3 — policy ("what may a run do?")

**Add-only.** A general per-environment policy setting: the run may create, never delete or mutate what
exists. Locked on permanently for pets; on for cattle during a cutover window; on or delete-with-approval
for shared infrastructure; off for disposable dev (decision 8).

**Circuit breaker.** Trips when the planner derives a forbidden delete — typically because a create-only
setting differs in an add-only environment. Provisioning stops on the first such delete; it is not a
quantity threshold (decision 10).

**Protected scrub.** The policy item that protects WHAT can be deleted (decision 11).

**Trust in authors.** Which publishers' dishes, recipes, and kinds a menu view honors (decision 11).

**Who may delete.** The policy item naming the principals that may run scrub or any deleting plan.

**Trace retention.** Whether and how long receipts' optional traces are kept (test provisioning runs are exempt and
always keep theirs).

**Plate-composition approval.** Whether composing or overriding a plate for a given profile needs a named
approval (none for a disposable dev profile; required for production).

## Layer 4 — the profile realm ("what to choose")

**Chef.** The dish composer: finds components in the menu, composes a new dish (components by content address
plus operations, with internal wiring), writes its recipe, submits it for test provisioning and publication. Any
entity may be a chef; agents are the intended chefs. The chef is trusted for nothing — the mechanism's
verification makes a dish real (decisions 19–21).

**Consigliere** (plate recommendation advisor). Knows compatibility (whether Postgres 13 and 14 are
interchangeable for a need) and cost. Only recommends: its advice enters the selector as content-addressed
preference rules, never as decisions. Reads a release's **SBOM** to suggest needs. Never operates in
layers 1–2 (decisions 23, 26). Design points:
- **Knowledge and reasoning are separate**, so the reasoner can be swapped (a ranked list first, an agent
  later) without touching the knowledge.
- **Knowledge splits into shared and private.** Shared = vendor-level facts anyone faces ("this managed
  Postgres supports an in-place 13 → 14 major upgrade"), publishable to the commons under each receiver's
  trust policy. Private = claims about one organization's own software, kept local.
- **Its output is one object: candidates per need, never a draft plate.** Each candidate carries the dish,
  its coverage of the need, its backing (attestations with who and when, evidence; anything from AI
  reasoning labelled unattested), warnings (deprecated, distrusted), and the ranking reason. Responsibility
  for the choice stays with the author; there is a per-need decision record; the author's own policy can
  act on each candidate ("attested dishes only").
- Consulted whenever a plate is composed, not only when a dish is unprovisionable.

**Compatibility claim.** One unit of the consigliere's knowledge, as its own content-addressed record ("our
software runs correctly on Postgres 14"; "Postgres 13 and 14 are interchangeable for a relational-database
need"). Trust in a claim is expressed with NL's existing attestation verbs on its address (vouches-for,
trusts-claims-about within a domain); retraction, recency and the commons apply unchanged. No new claim kind
or verb is introduced.

**Provisioning form.** Where an order is placed: pick what to run (a release, resolved to its needs list, or a needs
list directly), what kind (a profile), and which lineage this is the next generation of; the plate is looked
up, the instance written by the instance generator. Kept
separate from the configurator because different groups may operate them.

**Scrub form.** The provisioning form's counterpart: pick an environment (by its latest receipt) and scrub it, within
what policy allows. Any expiry-driven or automatic scrub is an external agent calling this form, out of
scope.

**Instance generator.** The tool, called from the provisioning form, that writes the instance record: it reads
the plate's servings and their dishes' name and placement inputs, learns the lineage's last generation from
its latest receipt, applies the organization's naming and placement rules (policy, layer 3, with content from
layer 5) and each recipe's fixed internal naming, and writes out every literal. Its rules are policy; the
record it produces has a shape fixed by the mechanism (decision 29).

**Infrastructure-version configurator.** The wizard for authoring a new needs list: start from an explicitly
chosen existing plate, keep its dishes for unchanged needs, explicitly pick dishes for new or changed
needs. (Composes plates from existing dishes; does not author dishes — that is the chef.)

**Profile.** A lasting, named kind of environment with an intent (minimal-cost dev, full production). A
content-addressed record of constraints BY KIND ("production: every relational-database has ha ∧
regions ≥ 2"). Plates pin their profile by content address (decision 15).

## Cross-cutting terms

**Actors.** All layer-4 tools are actor-agnostic. The AI orchestration agent does everything by default;
people are the fallback and the override. Exception: **waivers are human-only** (decisions 2, 3).

**Cattle / pets.** Cattle: environments that are replaced, never changed — a change means provisioning a new one and
scrubbing the old. Pets: environments that cannot yet be replaced; a policy fact (add-only, permanent), not a
type in the model. Pets are only ever added to; what existed before this system is never observed,
imported, or reasoned about (decisions 8, 9).

**Variation chain.** The records that are new when one part of a composite dish changes: the changed part
and one new version of each dish that contains it, up to the top-level composite. Parts that did not change
are shared by content address, so the cost of a variation is a test provisioning per new record, not storage
([`convention-over-configuration.md`](convention-over-configuration.md)).

**Pet migration.** An operational concern in the family of blue/green and canaries — how traffic and
responsibility move from old to new. Deferred.

**Re-provisioning levels** (measured against needs). **Identical:** every pinned dish still provisions. **Equivalent:** a
new plate with substituted dishes still meets every need. **Approximate:** a need is not met exactly; a
person signs a **waiver** and the environment stays labeled approximate. **Unprovisionable:** a need cannot
be met and nobody accepts a deviation.

**Release.** A software release. Its record names its needs list by content address; several releases may
share one. The x.x.x label and compliance-report semantics are deferred (decisions 12, 26).

**SBOM** (software bill of materials). The RELEASE's inventory of its own dependencies. An INPUT the
consigliere reads to suggest needs. Not produced by this system; the receipt is the infrastructure
inventory (decision 26).

**External agents.** Nothing in the design is coupled to software builds, source control, or schedules. Any
automation (a repository hook, an expiry timer) is an external agent calling the provisioning form, scrub form, or
configurator from outside, as a person would.

**The design test.** Shape questions are decided by asking what an agent needs at the moment it does the
work; all of it must be 100% accurate and in hand before the task can complete (decision 16).

## Worked example (GCP)

A needs list for a small web service:

```
need api-network
  kind: network
  constraints: private-service-access = true

need primary-db
  kind: relational-database
  constraints: engine = postgres, major >= 15, encryption-at-rest = true
  wires: lives-in -> api-network

need assets
  kind: object-store
  constraints: versioning = true, public-read = false

need api
  kind: container-service
  constraints: min-instances >= 1, max-concurrency >= 80
  wires: lives-in -> api-network, reads-from -> primary-db, reads-from -> assets
```

A production profile adds, by kind: `relational-database: ha = true, regions >= 2`.

Two dishes that cover `primary-db` in production (both satisfy need ∧ profile; the extra attribute on
the second is harmless because a need tests only the attributes it names):

```
dish cloudsql-pg15-ha-2r
  provides: kind = relational-database, engine = postgres, major = 15,
            ha = true, regions = 2, encryption-at-rest = true
  inputs:  name, network: ref network
  outputs: endpoint: connection-string

dish alloydb-pg15-ha-2r
  provides: … same …, columnar-engine = true
```

Two contributions that FAIL TO PUBLISH, because kinds are types: one spells `high-availability` and
`region-count` (unknown fields), another uses kind `database`, a string-valued `ha`, and a list-valued
`regions` (wrong type). Coverage never has to cope with them.

Composition (the Lego story): dish `vpc-2subnets` (three operations, internal wiring baked in) is published
on day 1. On day 15 another chef needs NAT as well, finds `vpc-2subnets`, and publishes `vpc-2subnets-nat`:
a recipe referencing the first dish by content address as a component plus one NAT-gateway operation wired
to the component's VPC output; provides `network, subnets = 2, nat = true`. Neither dish is ever modified.
