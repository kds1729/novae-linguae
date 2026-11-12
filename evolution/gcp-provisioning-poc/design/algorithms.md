# The algorithms, stated precisely

Design phase (phase 2) of the gcp-provisioning-poc, 2026-11-12. This document states what each part of
the mechanism computes, from which inputs, and how it fails, precisely enough that the prototype (phase 3)
can be checked against it. Record shapes are in [`schemas/`](schemas/); the vocabulary is the module's
[`glossary.md`](../glossary.md); decision numbers refer to the discussion log kept outside the repository.
Everything here is layer 2 (mechanism) except where a step says "policy answers", which is layer 3, and the
instance generator's rules, which are layer 3 with organization content.

Terms used throughout:

- **resolve(a)** fetches the record at content address `a` from the node, hash-verified; a missing or
  mismatched record is a named failure, never a guess.
- **canonical(t)** is the JCS form of a type expression; two port types are equal when their canonical
  forms are byte-equal (builtins folded to their canonical type artifacts as spec/type-expression.schema.json
  defines).
- **holds(p, v)** evaluates predicate `p` (over a kind's attribute names) on the record value `v`: it is
  `check-refinement` with `v` as the subject and the kind's attributes as the parameter environment. It
  answers true, false, or UNDECIDED; UNDECIDED is treated as false by coverage and named as such.
- **latest(x)** of a set of records about one subject orders them by their `finished` or `timestamp` and
  takes the last; ties are broken by content address, so the choice is deterministic.

---

## 1. The menu view: which dishes are offered

**Inputs.** A node; the receiver's trust policy `P` (spec/trust-model.md; the reference policy engine's
declaration, evaluated over the node's attestation graph).

**Output.** For every dish `d` on the node, one of: `offered`, `deprecated` (offered with a warning),
`retired`, `unprovisionable`, `unattested`, `untrusted-publisher`.

**Procedure.**

1. `publisher(d)` is the signer of the publication that stored `d`. If `P` does not derive it as trusted,
   the state is `untrusted-publisher`. Stop.
2. `A(d)` is the set of provisioning attestations whose `dish` is `d`, signed by an attester `P` derives as
   trusted, and not older than `P`'s freshness bound for attestations (an item-8 knob). If `A(d)` is empty,
   the state is `unattested`. Stop.
3. `a = latest(A(d))`. If `a.result = fail` and `a.cause = dish-fault`, the state is `unprovisionable`.
   Stop. (An environment fault is no verdict; the previous trusted attestation stands, so remove
   environment-fault attestations from `A(d)` before taking `latest`.)
4. `S(d)` is the set of `dish-state` statements about `d` whose signer `P` trusts for that act (an item-8
   knob names who may deprecate or retire). If `latest(S(d)).state = retired`, the state is `retired`; if
   `deprecated`, the state is `deprecated`.
5. Otherwise the state is `offered`.

A node with no trusted roots in `P` makes every dish `untrusted-publisher`; the configurator and the
selector report "no trusted publishers or attesters are configured" (decision 32).

The same procedure, without steps 2 and 3, gives the offered kinds and the usable recipes: a kind is usable
when its publisher is trusted; a recipe is usable exactly when its dish is offered.

---

## 2. The coverage check

**Inputs.** A plate `L` (resolved), its needs list `N` and profile `F` (resolved from `L`), and for each
serving its dish `D(s)`: `resolve(s.dish)` for a provisioned serving, or the dish recorded for serving
`s.carried.serving` in `resolve(s.carried.receipt)` for a carried one.

**Output.** `COVERED`, or a list of named failures. Any failure stops the provisioning command (R17).

**Procedure.**

1. **Every need once.** The multiset of `L.coverage[].need` equals the set of `N.needs[].name` with each
   name exactly once; every `L.coverage[].serving` names a serving in `L.servings`. Otherwise fail
   `need-uncovered(n)` or `need-covered-twice(n)` or `unknown-serving(s)`.
2. **Kind.** For each coverage entry `(n, s)`: `D(s).of_kind = n.of_kind`. Otherwise fail `kind-mismatch(n, s)`.
3. **Predicates.** For each `(n, s)`: `holds(n.constraints, D(s).provides)` and, if `F.constraints` has an
   entry for `n.of_kind`, `holds(F.constraints[n.of_kind].predicate, D(s).provides)`. Otherwise fail
   `need-not-satisfied(n, s, which)`; an UNDECIDED evaluation fails as `need-undecided(n, s)`.
4. **Sharing.** For each serving `s` covering more than one need: `F.sharing` has an entry for the kind with
   `allowed = true`. Otherwise fail `sharing-not-permitted(s, kind)`.
5. **Wires, both ends.** For each need `n` and each of its wires `(role, to)`: at least one entry `w` in
   `L.wires` with `w.need = n.name`, `w.role = role`, `w.to = to` (a role such as `reads-from` may carry
   several values, an endpoint and a credential, so one needs-list wire may resolve to several port
   bindings). None fails `wire-unbound(n, role)`. For each such `w`: `w.input.serving` is the serving covering `n`;
   `w.output.serving` is the serving covering `to`; `w.input.port` is a wire input of `D(w.input.serving)`;
   `w.output.port` is an output of `D(w.output.serving)`; and `canonical(type of input port) =
   canonical(type of output port)`. Otherwise fail `wire-mistyped(n, role)` or `wire-misrouted(n, role)`.
6. **Every input fed.** For each serving `s` and each wire input `i` of `D(s)`: exactly one `L.wires` entry
   has `input = (s, i)`. Otherwise fail `input-unfed(s, i)` or `input-fed-twice(s, i)`. An input port of a
   carried serving counts as fed by its receipt.
7. **Carried servings.** For each carried serving: `resolve(s.carried.receipt)` is a provisioning receipt whose
   serving `s.carried.serving` has status `provisioned` or `carried`, and the latest `held-by` statement
   about it (if any) does not name a receipt other than the one this run will write. Otherwise fail
   `carried-not-standing(s)` or `carried-held-elsewhere(s)`.

Coverage reads `provides`, ports and attestations only; never a recipe (decision 22).

---

## 3. The selector

**Inputs.** A needs list `N`; a profile `F`; the menu view under the operator's policy `P` (section 1); a set
of **preference rules** `R`, each a content-addressed ordering rule contributed by the consigliere
(decision 23); a set of **overrides** `O`, each `(need, dish, reason, by)`; optionally a receipt `C` to carry
from (decision 35) with the set of needs to carry.

**Output.** A plate, or a named failure `no-candidate(n)` / `override-not-candidate(n)` / `wires-unfittable(n)`.
Same inputs give the same plate (R22).

**Procedure.**

1. **Order the needs.** Topologically by their wires (a need comes after every need it wires to); ties
   broken by need name. Wires cannot be cyclic in a valid needs list; a cycle fails `wire-cycle`.
2. **Carried needs first.** For each need to carry from `C`: the serving in `C` covering it becomes a carried
   serving; its dish is read from `C`. It still passes step 3's predicate test, so a carried resource that no
   longer satisfies the need fails here, before any plate exists.
3. **Candidates per need.** For each remaining need `n`, in order:
   `cand(n) = { d offered or deprecated under P : d.of_kind = n.of_kind ∧ holds(n.constraints, d.provides) ∧
   holds(F.constraints[kind], d.provides) ∧ ports fit }`, where *ports fit* means: for each wire `(role, to)`
   of `n`, every wire input port of `d` whose type equals an output port of the serving already chosen
   for `to` is bound to it (one needs-list wire may bind several ports); after all wires of `n` are
   processed, every wire input of `d` is bound exactly once (a dish with an input no wire of `n` feeds, or
   with two outputs of the same type competing for one input, is not a candidate). Empty `cand(n)` fails `no-candidate(n)`.
4. **Sharing.** If `F.sharing` permits the kind and a rule in `R` asks to share when permitted, and an
   already chosen serving's dish is in `cand(n)` with the same constraints already satisfied, map `n` to that
   serving instead of creating one. Otherwise every need gets its own serving.
5. **Rank.** Order `cand(n)` by: (a) `P`'s trust derivation (more trusted attesters first, per the policy's
   own scoring); (b) the rules in `R`, applied in the order `R` lists them, each a total preorder over
   dishes (lower cost, same product family, prefer successor per a succession statement); (c) publication
   order, earliest first; (d) content address. Take the first. `deprecated` dishes rank after `offered` ones
   regardless of (a)–(c).
6. **Overrides.** If `O` names `n`: the named dish must be in `cand(n)`, else fail `override-not-candidate(n)`;
   it is taken instead of the ranked first, and `(n, serving, reason, by)` is recorded in `overrides`.
7. **Freeze.** Serving ids are `s1, s2, …` in need order; wires are written out as `(need, role, to, output
   port, input port)`; the cleanup manifest is every serving of `C` neither carried nor replaced; the plate
   is hashed.

Preference rules live in the inputs; the selector contains no taste of its own (R23).

---

## 4. The instance generator

**Inputs.** A plate `L`; a lineage `ℓ`; the organization's **naming rule** `name(ℓ, g, need, input) → string`
and **placement rule** `place(ℓ, g, F, kind) → placement` (policy, layer 3, content from layer 5); the node.

**Output.** An instance record, or `lineage-receipt-unreadable`.

**Procedure.**

1. `g = 1 + generation of latest(provisioning receipts on the node with lineage ℓ)`, or `1` if none. The
   generator reads receipts only (decision 9).
2. For each provisioned serving `s` (carried servings get no entry):
   a. `placement(s) = place(ℓ, g, F, D(s).of_kind)`, restricted to the placement inputs `D(s)` declares; a
      `regions` input receives exactly `count` regions.
   b. For each top-level name input `i` of `D(s)`: `names[[i]] = name(ℓ, g, needs covered by s, i)`. The
      naming rule MUST include `g` in every name (decision 29) and MUST produce a value the cloud accepts
      for the resource class; both are checked by the generator against the rule's own declaration.
   c. Recursively, for each component `c` of `D(s)`'s recipe and each of its name inputs `j` bound by
      `derive(from, suffix)`: `names[[c, j]] = names[[from]] + suffix`, and so on down the composition.
3. Hash and publish the instance.

---

## 5. The provisioning command

**Inputs.** Three content addresses: a needs list `N`, a plate `L`, an instance `I` (R13).

**Output.** A provisioning receipt, published; and, for each carried serving, a `held-by` statement. Or a
named stop before any effect.

**Procedure.**

1. **Resolve and bind.** `resolve` all three; `L.needs_list = N` and `I.plate = L`, else stop
   `inputs-mismatched`. Resolve every serving's dish and recipe, recursively through components, and every
   operation record and readiness probe. Ask policy "what may this run do here?" for `(I.lineage)`; the
   answer is kept for step 6.
2. **Coverage** (section 2). Any failure stops and names it.
3. **Instance check.** For every provisioned serving and every name input reachable through the composition,
   `I` has exactly one name at that path, and every derived name equals its derivation from the top-level
   name. Every placement input is supplied. Otherwise stop `instance-incomplete(s, path)` or
   `instance-derivation-mismatch(s, path)`.
4. **Carried servings stand.** For each carried serving, probe every resource listed under it in its receipt:
   each must answer `exists` (or `ready` where the receipt says `ready`). Any `absent` stops
   `carried-absent(s, resource)`; an inconclusive probe (an access fault) stops `carried-unobservable(s)`.
5. **Expand.** Flatten each provisioned serving into steps: its recipe's operations, plus each component's
   steps recursively, with the component's `bind` substituted for the component's inputs. Every binding
   that grounds at plan time (`lit`, `input`, `input-at`, `derive`, `json` of those) is replaced by its
   literal; the rest (`wire`, `component-output`, `operation-output`, `secret`) stay symbolic. Each step's
   refinements are grounded: a key part that does not ground stops `key-not-literal(step, part)`.
6. **Plan** (section 6). The planner returns a plan with waves and wait steps. If the policy answer forbids
   deletes and the plan contains a retire step (it never does for a provisioning run), stop
   `forbidden-delete`: the circuit breaker (R26).
7. **Check.** `check-plan` on the plan with its assumptions; every assumption is probed; the verdict must be
   PLAN-SOUND with every assumption OBSERVED. UNVERIFIABLE, REJECTED or PROBE-REFUTED stops with the
   checker's own naming (R15). The plan is published as a `plan_` artifact.
8. **Kitchen.** Execute waves in order; within a wave, steps may run concurrently. For an operation step:
   render the remaining bindings from earlier outputs, call the operation through the ordinary effect
   boundary, record the trace entry, and extract the declared outputs by path. For a wait step: call the
   probe at the declared interval until `ready_when` holds, or until `limit_seconds` elapse, which fails the
   step as `wait-timeout(step, resource)`. A `secret`-typed value is generated when first needed and
   discarded when the run ends; it is never written anywhere.
9. **Stop on failure.** The first failed step stops the run (principle 1): no later wave starts; steps in
   the same wave finish or fail on their own. The receipt records every serving as `provisioned`, `failed`
   (with the step and the cloud's answer) or `not-started`.
10. **Receipt.** Write and publish the receipt with status per serving, outputs per port (secret references
    only, by type), the ground resources each serving ensured (with their states), the plan, the cleanup
    manifest copied from the plate, and traces where policy retains them. For each carried serving publish
    a `held-by` statement naming the earlier receipt and serving and this receipt.

The command never searches for a substitute, never follows `supersedes`, never reads a receipt it was not
given, and never computes a difference between a plate and a receipt (R17, R31; decision 35).

---

## 6. The planner

**Inputs.** The expanded steps `T` of section 5 step 5 (each with grounded refinements); the readiness
declarations of every recipe involved; the set `W` of plate wires (each a kitchen-time data dependency from
an output-producing step to an input-consuming step).

**Output.** A plan: `assume[]` and `waves[][]`, where each wave is a list of steps, each an operation step
`(target, args)` or a wait step `(resource, until: ready, probe, args, limit_seconds)`; and the dependency
graph it was built from. Deterministic: ties broken by `(serving id, step id)`.

**Procedure.**

1. **Resources.** For every step `t` and every refinement `r` of `t`, the ground resource `g(r)` is its class
   plus its literal key values.
2. **Satisfaction.** A requires `(g, exists)` is satisfied by an ensures `(g, exists)` or `(g, ready)`; a
   requires `(g, ready)` only by an ensures `(g, ready)`; a requires `(g, absent)` by an ensures `(g, absent)`.
3. **Edges.** For each step `t` and each requires `r` of `t`: let `E(r)` be the steps whose ensures satisfy
   `r`. If `E(r)` has exactly one step `e`, add edge `e → t`. If it has several, fail `ambiguous-ensures(r)`
   (two steps claim the same resource; a malformed recipe). If it is empty and `r.state = ready` and some
   step `e'` ensures `(g, exists)`: create a wait step `w` for `g` from the readiness declaration of `g`'s
   class (fail `no-readiness-declaration(class)` if none), add edges `e' → w → t`, and record that `w`
   ensures `(g, ready)`. If `E(r)` is empty otherwise: add `r` to `assume[]` (the initial world the plan
   rests on; section 5 step 7 insists every assumption is probed).
4. **Data edges.** For each wire in `W` and each kitchen-time binding: an edge from the producing step to the
   consuming step.
5. **Waves.** Kahn's algorithm over the edges; wave `k` holds every step all of whose predecessors are in
   waves `< k`; within a wave, steps are sorted by `(serving id, step id)`. A cycle fails `dependency-cycle`.
6. **Soft hints** (glossary) may add edges in step 4 and never satisfy a requires in step 3.
7. The plan's `steps` are the waves flattened in order (a sequence `check-plan` can execute symbolically);
   the wave structure is kept beside it for the kitchen.

The planner never decides soundness; `check-plan` does (R15). For scrub (section 7) the same procedure runs
over the retire steps, whose refinements are the deletes' own requires/ensures, so dependents are deleted
before what they depend on by construction.

---

## 7. Scrub

**Inputs.** One content address: a provisioning receipt `K` (R19). The scrub form supplies it (R19a).

**Output.** A scrub receipt superseding `K`, or a partial scrub receipt (R20).

**Procedure.**

1. Resolve `K`, its plate, instance, dishes and recipes. Ask policy "what may this run do here?" for
   `K.lineage`; if the answer forbids deletes (add-only; a pet), stop `forbidden-delete` before planning.
   Protected-scrub items and "who may delete" are answered here too.
2. **What to delete.** Every serving of `K` with status `provisioned` whose latest `held-by` statement does
   not name another receipt. Carried servings (`status = carried`) are left alone: they belong to the
   receipt they were carried from, which is where they are scrubbed. Servings a `cleanup-done` statement
   already covers are skipped.
3. **Expand** the retire steps of those servings' recipes, recursively through components, binding names and
   placements from `K.instance` and outputs from `K`'s recorded outputs.
4. **Plan** (section 6) over the retire steps, **check** with `check-plan`, probing assumptions; stop on
   anything but PLAN-SOUND with every assumption OBSERVED.
5. **Kitchen**, as in section 5 step 8; a delete step is followed by a wait step for `(g, absent)` within the
   class's `limit_seconds`, the probe's not-found answer meaning absent (the probe convention of
   spec/world-state.md).
6. **Receipt.** If every resource is confirmed absent: a `scrub` receipt with every serving `scrubbed`,
   superseding `K`. Otherwise a `partial-scrub` receipt naming each resource `absent` or `exists`
   (remaining) with its probe trace, superseding `K`; re-running scrub against `K` is legal and idempotent:
   step 5 probes first and skips what is already absent (decision 25).

---

## 8. Test provisioning

**Inputs.** A submitted dish `d` with its recipe; the chef's harness plate `H` (whose servings are `d` and
offered dishes for each of `d`'s wire inputs; `H.needs_list` is a needs list the chef writes for the
harness, one need per serving); the attester's policy and account.

**Output.** A signed provisioning attestation.

**Procedure.**

1. Validate `d` and its recipe against the kind's attributes, the port types, and the recipe rules (every
   dish input referenced is declared; every output produced; every component input bound; `derive` only from
   this dish's name inputs).
2. `I = ` instance generator for `H`, lineage `test/<first 16 hex of d>`, with the attester's naming rule,
   which marks test names.
3. Run the provisioning command on `(H.needs_list, H, I)`. Retain the trace regardless of policy.
4. If it completed: run scrub on its receipt. `result = pass` iff the scrub receipt is a `scrub` receipt
   (every resource confirmed gone) and the provisioning receipt shows every serving `provisioned` with every
   declared output produced.
5. If it stopped or failed: classify. `dish-fault` when the failing step's answer was a client-side refusal
   of the request (a 4xx other than authentication, authorization, quota or rate limit) or a `wait-timeout`
   on a resource of `d`'s own recipe; `environment-fault` for authentication, authorization, quota, rate
   limiting, provider-side errors (5xx), network failures, or a failure inside a harness serving. Run scrub
   on whatever receipt exists.
6. Write the attestation: dish, recipe, harness plate, instance, result, cause and detail on failure,
   receipt, scrub receipt, trace, times; sign as the attester; publish.

A composite is tested whole by construction: its components are expanded like any serving's (decision 36).

---

## 9. Re-provisioning

**Identical.** A new instance for the same plate (next generation); the provisioning command runs. Nothing
else.

**Equivalent** (R21, R22). The provisioning command stopped at section 2 or because the menu view marks a
pinned dish `unprovisionable` or `retired`. The selector (section 3) runs with the same needs list and
profile; it re-chooses every need (the result may differ only where the menu changed, by determinism); a new
plate, a new instance, a new run.

**Approximate** (R21, R29). The selector failed `no-candidate(n)`. A person may sign a waiver naming `n` and a
relaxed predicate; the selector reruns with the waiver's predicate in place of `n.constraints`; the plate
records the override with the waiver as reason; the receipt's lineage is labeled approximate by a statement.
No agent may sign a waiver.

**Unprovisionable.** `no-candidate(n)` and no waiver. Stop and name `n`.
