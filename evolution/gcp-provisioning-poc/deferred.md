# Deferred — Declarative Infrastructure Provisioning on Novae Linguae

Companion to the concept summary and `glossary.md`. Each entry is a question the concept depends on but
does not yet answer, with what is known, what was considered, and what would reopen it. Decision numbers
refer to the discussion log (`decisions.md`, kept outside the repo). Nothing here blocks the discussion
of the mechanism; several entries block implementation.

Status: **open** (no position taken), **leaning** (a candidate is favored but not decided), or
**knob** (the answer is a policy setting whose default is unset).

---

## 1. The human-readable version label and compliance reports — open

**Deferred 2026-11-10** (decision 12). The dictated doc attached an x.x.x label to an infrastructure version
(the needs list) "only when a release first uses it", and said compliance reports name needs only.

**Why deferred.** The label was meant to identify what a receipt contained — the state of a whole
environment at one point in time. But that state is needs list + plate + instance + receipt (including
any waiver), and the same needs list is built many ways and many times. A label on the needs list alone
is ambiguous; the user: "not sure version is even worth having at this point".

**Candidates recorded.**
- Label the needs list (stable, meaningful to an auditor, but not a state).
- Label the receipt (a true point-in-time state, but one per build, so labels multiply).
- A new snapshot object binding needs list + plate + the receipts built from them.

**Also carried here.** What a compliance report reads (needs only, or needs + waivers + receipt), and what
"first use by a release" means once releases simply name their needs list by content address (decision 26).

**What any answer must cope with** (from a worked timeline of one product's development):
- Needs lists are created during development, before any release that will use them exists; the only
  software identity nearby is a commit id.
- A list's identity exists before anyone knows it matters; it can become the mainline or a released list
  later WITHOUT changing. Any label meaning "official" is attached after the fact.
- Most lists are never released (abandoned experiments, daily mainline steps); they still need a way to be
  referred to while in use, and should not look like release numbers.
- Merging two engineers' independent changes yields a list that never ran anywhere before.
- Parallel release lines make lists a TREE, not a line: one logical change (a hotfix) applied to two
  starting points yields two different lists.
- "Newer" does not mean "contains more": a hotfix list created later can lack what the mainline has, so a
  label that sorts must not imply containment.
- Many releases share one list (a code-only fix); one list serves development builds with only a commit id.
- A released list can stay in service for years for a single regulated customer.

**Earlier-recorded candidate** (superseded by today's "undecided", kept as one option): the x.x.x label is
arbitrary, chosen by a person when a release is created; only lists a release uses ever get one; a release
that reuses an unchanged list reuses its number; compliance reports name needs only; disaster-recovery
records name the list AND the plate, since rebuilding elsewhere needs what was actually built.

**Reopens when:** an auditor-facing requirement is stated, or disaster-recovery needs a single name for
"what to rebuild".

## 2. Pet migration — open (operational)

**Deferred 2026-11-10** (decision 9, user clarification). How traffic and responsibility move from a pet to
its cattle replacement: in the family of blue/green deployments and canaries. Not a modelling question —
pets are a policy fact (add-only, permanent), their pre-existing state is never observed or imported, and
the system knows of a pet only what its own receipts added. Out of the concept's scope the way deployments
and runtime settings are.

**Reopens when:** a real pet is in hand and a replacement plate is buildable.

## 3. The test build — open; blocks implementation

**Raised 2026-11-10.** A dish is offered only after a passing test build (decisions 19, 21). A test build
provisions real cloud resources and scrubs them.

**Undecided.** Where it runs; what it costs and who pays; which cloud account and identity it uses; whether
a composite dish re-tests its already-tested components or only its new wiring; how often an offered dish
is re-tested as the cloud API drifts (the recipe-versioning hook in the glossary's **Recipe** entry).

**Constraint on record.** The user's 2026-08-20 cloud policy was AWS-only on the personal account and no
further GCP (the earlier GCP work ran on a work identity's billing). This exercise targets GCP. The account
question is to be settled when the plan reaches implementation, not before (user, 2026-11-10: "don't get
ahead of yourself").

## 4. Instance generation by policy — open

**Raised 2026-11-10.** The GO form says the instance (project, region, names) is generated by policy. No
shape yet: the naming rule, the placement rule, how an instance relates to a profile (does "production"
imply a project?), whether an instance may be reused across builds of the same needs list, and how the
instance's names reach recipes (they are the one input a dish accepts besides wiring).

**Candidate split.** The RECORD shape (project, region, names, DNS zone) is mechanism; the RULES that generate
one (naming conventions, region defaults, project allocation) are organization policy, i.e. layer 5 content
configured through layer 3. That keeps the mechanism free of any one company's naming.

## 5. Secrets as references — open

**Raised 2026-11-10.** Receipts carry secrets only as references. Undecided: which secret store the
reference names; how the kitchen resolves a reference when wiring one dish's secret output (a database
password) into another dish's input; whether the store is itself a dish; what the receipt records so a
rebuild can re-wire without ever holding the secret.

## 6. Publisher trust for dishes, recipes, and kinds — leaning

**Raised 2026-11-10** (decisions 4, 11, 14). The menu view is trust-filtered under the receiver's local
trust policy, which NL already has for function records. **Leaning:** reuse it unchanged — a kind, a dish,
a recipe, and a build attestation are records like any other. Undecided: whether publishing a KIND should
require a higher trust tier than publishing a dish, since every need and dish downstream depends on it;
and what the default policy of a fresh node is.

## 7. One dish per need — provisional

**Decision 1, 2026-11-10,** accepted as "seems correct, model still under development". Coverage assumes
each need is covered by exactly one dish and a plate is that need → dish mapping. Cases that could break
it: a need that only a pair of dishes covers together; two needs covered by one composite dish (the Lego
story makes this likely — a `vpc-2subnets-nat` dish covers "a network" and "a NAT" at once). **Candidate
resolution:** composition absorbs it — if two needs are always met together, the chef makes one dish and
the needs list has one need. To be checked against real needs lists.

**Counter-evidence on record:** the earlier discussion treated "one dish covers several needs" as normal
(a shared database instance serving two services' database needs is the cheap dev-profile shape). If that
stands, coverage becomes many-needs-to-one-dish and the plate maps each need to a dish that may be shared.
Still mechanical; the one-per-need rule would simply be dropped.

## 8. Policy knobs with no default — knob

Each is a layer-3 setting whose existence is decided and whose default is not:
- Automatic vs human-triggered scrub retry after a partial scrub (decision 25).
- Trace retention period.
- Add-only for shared infrastructure: add-only, or delete-with-approval (decision 8).
- Who may delete, by principal.
- The approximate-rebuild waiver: who may sign, and whether a waiver expires.
- Plate-composition approval per profile (none for a dev profile, required for production).
- Test-build cadence (how often an offered dish's buildable-as-of attestation is renewed).
- Who may deprecate or retire a dish, and how long deprecated lasts before retired.

## 9. Recipe drift and re-testing — open

**Implied by the glossary's Recipe entry.** "A dish can gain new recipes as the cloud API changes." Undecided:
who notices the drift (a failed scheduled test build? the OpenAPI adapter re-ingesting a changed spec?),
whether a new recipe needs its own test build before it replaces the old one for GO, and whether a dish
with two live recipes is one dish or two.

## 10. Layer-1 gaps this module needs closed — open; blocks the design phase

**Raised 2026-11-10.** Four things the mechanism assumes of NL that NL does not yet have:
- **A `ready` state and attribute states** in the world-state vocabulary (today `exists`/`absent` only). Most
  real dependencies are on readiness (a database RUNNABLE, a certificate ACTIVE, a peering established).
- **A planner.** `check-plan` verifies an order; nothing produces one. Topological order from
  requires/ensures with parallel waves.
- **Mid-plan waits.** Long-running operations and "wait until DNS resolves" are not modelled; the wait
  obligation (probe + time limit) has to be a plan step, stated at plan time, so a plan with waits can still
  be PLAN-SOUND and a timeout at run time is a named failure.
- **Authored refinements.** Ingested operations carry empty requires/ensures; recipes need them authored,
  and the soft-hint path (a failed trace suggests one, a person approves it) needs a home.

Each is a `spec/` or validator change with its own proposal; none is specific to this module.

## 11. Pets: a "move" operation and the cleanup manifest — open

**Raised 2026-11-10.** Today's decisions give pets the rules (add-only, circuit breaker, knowledge = own
receipts) but no named operation. The earlier discussion had one, worth recording:
- A **move** takes a pet's LATEST RECEIPT and a new plate, and builds every dish the plate has that the
  receipt does not. It compares against the receipt, not the old plate, because after a partial build or a
  substitution the old plate describes dishes that are not there.
- It never deletes. Whatever the receipt has that the new plate no longer needs stays in place and is listed
  in a **cleanup manifest**; cleanup is done separately, later, and each item cleaned should update the
  receipt so receipts stay accurate.
- A dish that holds data and must be replaced is the circuit breaker's canonical case: stop, never destroy.
- Related: **adopting a surviving resource** (scrub everything but the database, then GO a new environment
  around it). Needs scrub exclusions as a policy setting and a GO that adopts rather than builds, which
  breaks "GO builds from nothing" and is a mechanism change; receipts would then include a dish built by
  another environment's build.

**Reopens when:** the first pet exists under this system.

---

## Removed from the open list

- **SBOM format** (dictated still-open item). Resolved 2026-11-10, decision 26: the SBOM is the release's own
  bill of materials, an external format the consigliere consumes to suggest needs. Not ours to define.
- **Exact shape of a need.** Settled 2026-11-10, decisions 14–18; see the glossary's **Need** entry.
- **How an equivalent rebuild's new plate is composed, and by whom.** Settled 2026-11-10, decision 23: the
  deterministic selector, with consigliere preferences as inputs and a bounded override.
- **The scrub function.** Settled 2026-11-10, decisions 24–25.
