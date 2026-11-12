# Convention over configuration

A guide for the gcp-provisioning-poc, written 2026-11-12. It explains two consequences of the module's
design philosophy, the alternatives that were considered, and why the design is the way it is. Decision
numbers refer to the discussion log (`decisions.md`, kept outside the repository), as in the module's other
documents ([`README.md`](README.md) is the charter, [`glossary.md`](glossary.md) the vocabulary,
[`deferred.md`](deferred.md) the open questions).

Project terms used below are explained in Appendix A. The parts of the current design that the
discussion refers to are summarized in Appendix B.

## 1. Summary

This design provisions cloud infrastructure by selecting ready-made parts (dishes) and composing them,
rather than by generating infrastructure from adjustable rules. The settings of a dish are fixed when it
is authored; choosing a dish is the way to choose its settings. Along the same lines, an environment is
not changed in place once it is provisioned; a change is made by provisioning a replacement and retiring the old
one. This is the sense in which the design prefers convention over configuration.

On 2026-11-12 the user described two consequences of that preference, so that they are understood and
written down. Both are "all or nothing" in character.

1. **Fixed parts lead to many similar copies.** If a team wants a Redis cache that expires keys, and the
   closest existing part is a complete platform whose Redis keeps keys indefinitely, there is no single
   setting to change. The team authors a new Redis part and a new version of each part that contains it.
   One setting difference becomes a set of new records.
2. **A change to production is made by provisioning a new production beside the current one.** There is
   no in-place change, so even a small adjustment goes through full provisioning and a switch of traffic.

Two ways of easing those consequences were considered: giving dishes adjustable settings, and having the
provisioning command compare the new selection with what was provisioned before and act only on the difference. Both
were set aside, because each gives up one of the properties the design was chosen for. Section 4.1
explains which.

The design stays as it is, and this note is clear about what that costs. Anyone who wants adjustable
settings or in-place change is welcome to propose a parallel solution, for example a fork of this project;
it will not be part of this one, and the two can coexist. The rest of this note explains the reasoning
behind each consequence, what follows from the design, and how the design handles the points the
discussion raised.

## 2. Consequence 1: fixed parts lead to many similar copies

### 2.1 The consequence as described

A team wants a Redis cache that expires keys. The closest existing dish is a complete platform whose Redis
part keeps keys indefinitely. Because a dish's settings are fixed, there is no single setting to change.
The team authors a new Redis dish with the desired setting, and then a new version of each dish that
contains it, up to and including the complete platform. One setting difference becomes a set of new
records, where an adjustable setting would have been a single edit. The pattern repeats at every level of
composition.

### 2.2 The alternative that was considered

A dish could declare typed settings (for example, "eviction policy, one of: keep indefinitely,
least-recently-used, by time-to-live"), with the value supplied when the plate is chosen. What the dish
declares it provides would then depend on the chosen value. A composite dish would either fix each part's
settings or expose them as its own.

Why it was set aside. The design's dish is a part whose behavior is known in full from its record; a
setting supplied later means part of that behavior is not in the record until the plate is made. A dish
with settings is also, in effect, a small family of dishes, and the family is only as verified as its
members: either every member goes through test provisioning, in which case the library holds those
members anyway, or some members are provisioned for the first time in a real environment, which relies on
the type declaration rather than on a test. The coverage check would also change character, from evaluating a predicate on a
known value to finding a value that satisfies it. None of these is fatal, but together they move the
design away from the properties it was chosen for.

### 2.3 What follows from keeping fixed parts

**The duplication is smaller than it first appears.** Composites refer to their parts by content address.
When one part changes, only the chain of records from that part up to the top-level composite is new: one
record per level, each a short list of addresses plus the connections between them. Every part that did
not change is shared as it was. Storage is not where the cost lands.

**The cost lands on verification.** Each new record in that chain is a new dish and goes through its own
test provisioning, which takes cloud resources, time and money. This is the price of fixed settings. It can also
be read as a benefit: a Redis that keeps keys and a Redis that expires them behave differently, and
verifying them separately is reasonable. What test provisioning needs to exercise, and how its cost is kept
in proportion, is covered by the module's deferred-questions document.

**Convenience can live in the tooling rather than the model.** "Author this dish with one part changed" is
something the chef can do mechanically: take the chain of records from the changed part to the top,
produce the new records, submit each for test provisioning. The library still holds only fixed dishes, and the
person or agent asking for the change receives a new plate rather than an edit.

**Selection copes well with variations.** A need is a predicate, and a predicate such as "eviction policy
is one of: least-recently-used, by time-to-live" matches every fixed dish in that set. Fixed dishes are
what keep coverage a simple evaluation.

**Composition depth is a choice.** A composite dish is a good fit for parts that are always tested
together and do not vary separately. If the parts of a "complete platform" are expected to be exchanged
one at a time, the platform may be better expressed as a plate, where each need is met by its own dish
and one can be exchanged without touching the others. The example above may be a case where a plate would
have served better than a deeply nested dish.

**Forks and the shared project can coexist.** A fork that adds adjustable settings produces records of a
shape this project's coverage check does not read, so they simply are not selectable here, with no
special handling. A fork can still contribute fixed dishes back to the shared menu, since a fixed dish is
a fixed dish regardless of how it was authored.

## 3. Consequence 2: a change to production is a new production beside the current one

### 3.1 The consequence as described

Because environments are treated as replaceable, a change to production means provisioning the new version
as a second complete environment beside the current one, switching traffic to it, and retiring the old
one (a blue/green change). There is no in-place change, however small the adjustment.

### 3.2 The alternative that was considered

The provisioning command could read the environment's latest receipt, match each need by its role across the old and new plates,
and for each role keep the resource, change it in place where the cloud provider allows, or replace it
(provision the new one, reconnect, retire the old one).

Why it was set aside. It is in-place change, which the design has deliberately left out. It also asks the
provisioning command to reason about the current state of an environment, and so far the design has avoided that: it acts on a
plate, and the system's knowledge of an environment is simply its own receipts. Reconciling against
recorded state is a workable approach, used widely elsewhere, but it brings back the forward-edited
record that the charter's section 2.1 set out to avoid.

### 3.3 What follows from provisioning a new production each time

**Whole-environment blue/green is already the charter's position.** The charter's out-of-scope list
includes loading data and moving traffic from an old environment to its replacement; environments are
handed over empty. Moving data and traffic is the organization's responsibility, with its own tooling.

**Fit.** The design is a good fit for organizations whose stored data can be copied or re-created outside
this system. Where production data lives in resources that are not practical to re-provision, the fit is
weaker, and it would help readers for the charter to say so.

**Double capacity during the switch** is part of the cost. While the old and new production both exist,
both are paid for.

**Resource names need to differ per generation.** When the next version of production is provisioned beside the
current one in the same cloud project and region, the two cannot share resource names, or the provider
will refuse the second at the first name collision. Resource naming therefore includes the generation;
the module's deferred-questions document carries this requirement under resource naming.

**The traffic switch sits above the environment.** The provisioning command does not switch traffic. The switch happens outside
this system, in the name service or in a load balancer above both environments. If a load balancer is
itself a dish inside the environment, the new environment gets its own, and the switch happens above
both. Writing this down avoids the expectation that the provisioning command performs it.

### 3.4 Environments that are only added to

The charter keeps one exception to "no in-place change": a pet, an environment that is not practical to
re-provision and is therefore only ever added to. This note does not change that. What it means in practice:

- In a pet, a setting is not changed once set, since change is not addition. The available move is to add
  a new resource beside the old one and have the application switch to it; the switch itself is outside
  this system's scope, as it is for any environment.
- Removals in a pet happen only with policy approval, so a pet tends to accumulate resources over time.
  That is the expected shape of a pet, and the reason the charter treats pets as the exception rather
  than the rule.
- Provisioning a pet's next generation uses per-generation resource names, as any environment does.
- The policy items that serve pets, the "add-only" setting, the safety stop on removals, and the rule for
  what counts as a change rather than an addition, are described in the charter and the glossary.

## 4. Points that apply to both consequences

### 4.1 Why the two alternatives were set aside together

The two alternatives share a lineage: settings supplied at deployment time, and reconciliation of a
desired state against a recorded state. These are the two central ideas of the configuration-management
approach to provisioning, and they work well in that setting. The charter's section 2.1 chose a different
set of properties for this project, frozen content-addressed records with no late-filled values and no
forward edits, and the two alternatives each trade one of those away. Setting them aside keeps the
project's chosen properties intact; it is not a judgment of the other approach.

### 4.2 The design stays as it is; alternatives are welcome as parallel solutions

Dishes stay fixed and environments stay replaceable. Some people will prefer to change one setting rather
than compose a new plate, and the project does not try to serve that preference, because doing so would
give up the properties it was chosen for. Anyone who wants adjustable settings or in-place change is
welcome to propose a parallel solution, for example a fork of this project. It will not be part of this
one, but the two can coexist: the fork adds what it needs, the shared project keeps its shape, and fixed
dishes can flow between them.

The project's stated audience is agents rather than people, and agents are the intended authors of
dishes. Authoring a new chain of records is a small task for an agent, so the inconvenience of fixed
settings falls lightly on the intended user.

### 4.3 A note on wording

Project documents explain the design by its properties and do not name or evaluate other tools or
products. The charter's section 2.1 is written that way, and this note follows it.

## 5. How it is, and why

- **A dish's settings are fixed when it is authored.** Its inputs are names and connections to other
  dishes. This keeps a dish's behavior fully in its record and keeps coverage a simple evaluation. (In
  line with decisions 1, 2, 17, 18 and 22.)
- **Resources are created and retired, not changed in place.** The provisioning command creates; the teardown command retires. This keeps
  every record frozen and content-addressed, with no forward edits. (Charter principle 5.)
- **A variation is a new chain of records from the changed part up to the top-level composite.**
  Unchanged parts are shared by content address. The cost is test provisioning per new record, not storage,
  and the separate verification is the point.
- **Making variations convenient is a job for the chef.** The chef may author "this dish with one part
  changed" mechanically; the model itself does not change.
- **Composition depth is the author's choice.** Parts that are always tested together belong in a
  composite dish; parts expected to be exchanged one at a time belong side by side on a plate.
- **A change to production is a new production beside the current one.** Resource names include the
  generation. The traffic switch happens above both environments and is outside the provisioning command's scope. Double
  capacity during the switch is part of the cost.
- **The design fits organizations whose stored data can be copied or re-created outside this system.**
  Where production data lives in resources that are not practical to re-provision, the fit is weaker, and the
  pet exception exists for that case.
- **Environments that are only added to remain the one exception**, as the charter describes.
- **Project documents explain the design by its properties and do not name or evaluate other tools.**

## 6. If you want it otherwise

Adjustable settings and in-place change are reasonable things to want, and widely used elsewhere. They
are not part of this solution, because each gives up a property this design was chosen for. Anyone who
wants them is welcome to propose a parallel solution, for example a fork of this project. A fork and the
shared project can coexist: the fork's records with adjustable settings are simply not selectable here,
and fixed dishes can be contributed back to the shared menu from either side.

## 7. Where this appears in the module's other documents

The statement of fit (section 3.3) and the note on parallel solutions (section 6) are in the charter's
scope section. The glossary carries the guidance on composition depth under **Dish** and the term
**variation chain** for the chain of records from a changed part up to the top-level composite. The
per-generation naming requirement is under instance generation in the deferred-questions document.

## Appendix A. Terms

- **Dish**: a complete, ready-made piece of cloud infrastructure, for example "a Redis cache with a given
  memory limit and a given key-eviction policy". The settings that affect how it behaves, how it is laid
  out, and what it costs are fixed when it is authored. Its inputs are names and connections to other
  dishes.
- **Recipe**: the record of how a dish is provisioned: the cloud operations to run, in order, and the operations
  that retire it.
- **Composite dish**: a dish composed of other dishes, for example "a complete web application platform"
  containing a network dish, a database dish and a cache dish. A composite refers to its parts by their
  content address (a cryptographic hash of their content), so a part is shared by reference rather than
  copied. A published dish is not modified; a new version is a new record with a new address.
- **Need**: one requirement of a piece of software, stated as a category plus constraints, for example
  "a cache that expires keys". A **needs list** is all the needs of one version of the software.
- **Coverage**: the check that a chosen dish satisfies a need. Each dish declares what it provides as a
  typed value; each need is a predicate over that type; coverage is evaluating the predicate on the value.
- **Plate**: the list of dishes chosen to satisfy one needs list, frozen once chosen.
- **Receipt**: the record of what one environment actually received when a plate was provisioned.
- **Environment**: one provisioned copy of a plate, for example "production" or "the test environment".
- **Pet**: an environment that is not practical to re-provision, so it is only ever added to. **Cattle**: an
  environment that is replaced rather than changed in place.
- **Blue/green**: a way of changing production. The new version is provisioned as a second, complete environment
  beside the current one; traffic is switched to the new one; the old one is retired.
- **Chef**: the tool, agent, or person that composes new dishes and submits them for verification.
- **Test provisioning**: provisioning a newly authored dish once, in the cloud, to show that it can be
  provisioned. A dish is offered for selection after test provisioning passes.
- **Provisioning command** (called GO in the charter): the command that provisions an environment from a plate. **Teardown command** (called
  scrub in the charter): the command that retires one.
- **Menu**: the set of dishes offered for selection: published, passed test provisioning, from a trusted publisher.

## Appendix B. What the current design says

The project charter and the decisions log contain the following, which the discussion refers to.

- A dish's behavioral settings are fixed when it is authored (the dish definition in Appendix A).
- Provisioning an environment is selection rather than generation. The agent chooses dishes from the menu;
  when nothing fits, it stops and names what is missing rather than substituting or searching elsewhere.
  (Charter principles 1 and 2; decision 2.)
- Environments are replaced rather than changed in place. Pets are the one exception: a matter of policy
  rather than a type in the model, and only ever added to. (Charter principle 5; decisions 8 and 9.)
- Composites refer to their parts by content address and are not modified after publication.
  (Decision 17.)
- An agent decides whether to use a dish by reading its declared properties and attestations rather than
  its recipe or any prose about it. (Decision 22.)
- A dish becomes usable when the mechanism has verified it, independently of who authored it. (Charter
  principle 7.)
- The charter's section 2.1 explains why selection was chosen over generation, by listing properties of
  generation-based provisioning and the property this design has instead. Two of those properties matter
  here: records are frozen and content-addressed, so there is always an exact "as of"; and values are
  not filled in late, so every final value is in the record.
