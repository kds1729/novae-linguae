# Proposal 02 — observe the effect-free DELETE, and narrow the read-only rule to match its intent

- **Status:** accepted — applied as proposed (maintainer decision 2026-08-20).
- **Module:** [`gcp-sdk-poc`](../README.md)
- **Addresses:** [finding 13](../findings.md#13-finding-11s-refusal-makes-path-parameterised-delete-unreachable)
- **Resolution:** applied in `b0271de` as the opt-in `--observe-absent-delete` (requires
  `--verify-against`): the probe-then-delete mechanism exactly as specified below — probe GET at
  the absent name must answer non-2xx (a 2xx refuses loudly before any DELETE is issued), then
  the DELETE runs once and the example records what this service answered, trace-attached,
  offline-replayable. A DELETE documenting its 404 keeps its ordinary spec-derived example.
  Adapter tests 66 → 71, including the probe-finds-something refusal with the survivor verified
  undeleted. The caveats stand as stated: it is a flag, not a default; the gate prints each
  observed status (the write-shaped requests are visible, not silent); and the record carries
  the service's own answer rather than an assumed 404.
- **Blast radius:** recovers **109** path-parameterised DELETEs across four APIs, currently
  ungeneratable by any route. No existing record changes.
- **Measured:** live against Cloud Storage, and the mechanism demonstrates itself (below).

## The change

Let the observation gate run a path-parameterised DELETE **at the absent name**, and record what
the service returns — an ordinary observation, trace-attached and offline-replayable, exactly like
every other observed example.

The rule that currently forbids it — *an observation must not create or destroy state during
ingestion* — is right. But the gate enforces it by refusing the **verb**, and that is a proxy. What
the principle actually means is *no call that can change state*, and those two are not the same
predicate.

## Why the rule does not apply to this call

`_example_for` fills the path parameter with `gw7-absent-x`, "a name no test writes". A DELETE there
is the verb's **effect-free** case: applied where nothing is present it changes nothing and reports
absence.

Idempotency is how this case comes to notice — DELETE is *the* idempotent verb — but effect-freeness
is the property doing the work, and the two come apart: `PUT` at an absent name is equally idempotent
and **creates**. The boundary belongs at effect-freeness.

So the call the gate is refusing is one that provably has no effect. Measured live against Cloud
Storage, on a name deliberately never created:

```
GET    /b/nl-gw7-absent-probe-never-created -> 404      (probe: it is absent)
DELETE /b/nl-gw7-absent-probe-never-created -> 404      (the effect-free case)
GET    /b/nl-gw7-absent-probe-never-created -> 404      (re-probe: nothing changed)
```

No state created, none destroyed, and the service reported exactly the outcome the absent-name
convention was built to elicit.

## The mechanism: probe, then delete

`gw7-absent-x` is a *convention*, not a guarantee — if something did exist at that name, the
observation would destroy it. So absence becomes a **checked precondition** rather than an
assumption:

1. `GET` the path at the absent name. Require a non-2xx.
2. Only then, `DELETE` it, and record the observation.
3. The recorded example is the DELETE's real status, with its trace attached.

Both calls are safe, and step 1 turns the one risk into something the gate verifies. If the probe
answers 2xx — something is there — the gate refuses and says so, which is the honest outcome.

The three-call sequence above is that mechanism, run by hand.

## What it recovers, and why it beats the alternatives

All **109** ungeneratable DELETEs, with no per-operation operator input: no expected-status flag, no
binding, nothing to supply. Finding 13 lists two other directions; this is better than both.

| | evidence | operator burden |
|---|---|---|
| Operator-supplied expected status | unverified testimony | a value per operation |
| Assert an undocumented 404 | an invented claim | none, but unfaithful |
| **Observe the effect-free DELETE** | **a real observation, trace-attached, replayable** | **none** |

The faithfulness objection that made the second direction uncomfortable does not apply here. Nothing
is asserted that the description did not say — the description licenses the *shape*, and the
observation supplies the *value*. That is the established split, the same one schema-derived
projections already use.

## The second-order benefit: teardown becomes composable

An idempotent DELETE means `ensures absent` holds **regardless of prior state**. A plan can always
append a teardown step, and [`check-plan`](../../../spec/world-state.md) can discharge it without
knowing whether the resource was ever created. Recording idempotency is not only about coverage; it
is what makes teardown safe to compose — and without it a corpus can provision and never release.

## Caveats, stated rather than discovered

- **It should be opt-in.** Even an effect-free DELETE is a write-shaped request against a live
  service, and an operator may reasonably want ingestion to issue none. A flag, not a default.
- **Audit noise is real.** 109 DELETEs appear in an audit log even when every one answers 404. Worth
  saying out loud before someone finds them.
- **Services differ.** Some answer 404 to a delete-absent, some 204, some 200. That is fine and is
  rather the point: the observation records what *this* service did, and the record says so.
- **The probe costs a call.** Two requests per operation instead of one, which for 109 operations is
  219 rather than 109. Cheap for the guarantee it buys.

## Settled here — not asked

Per [`evolution/TEMPLATE.md`](../../TEMPLATE.md), a question a measurement or a precedent settles is
the author's to resolve. Two of the three this proposal originally asked were.

**The criterion is effect-freeness, not idempotency — settled by measurement.** An earlier draft
leaned on the word *idempotent*, and that is not the property doing the work. `PUT` at an absent
name is idempotent too — repeat it, get the same result — but it **creates**. What makes the DELETE
case admissible is that at an absent name it changes *nothing at all*. Drawing the boundary at
effect-freeness admits DELETE-at-absent and excludes `PUT`/`PATCH` cleanly, so the scope question
answers itself and the proposal's language is corrected throughout: idempotency is how the case was
noticed, effect-freeness is why it is safe.

**The probe is mandatory — settled by precedent.** "Verified by default" is the project's existing
posture, and an optional safety check on a destructive verb is not a safety check. Letting an
operator assert absence and skip the `GET` reintroduces exactly the risk the probe removes, for a
saving of one request.

## The one question that needs you

**Is *no call that can change state* the right narrowing of the read-only rule?**

The argument for is above: the verb-level rule provably over-refuses, and it costs 109 operations.

The argument against deserves stating plainly, because it is not weak. A verb-level rule is
**auditable by anyone**: *"ingestion never issues a DELETE"* is checkable from a request log, by
someone who trusts nothing about the implementation. *"Ingestion never issues a call that changes
state"* is only ever as good as the probe logic. That trades an externally-verifiable guarantee for
a broader one that requires trusting this adapter — and for a project built on content-addressing,
offline replay, and *the store stays untrusted*, that is a real cost rather than a technicality.

My recommendation is to narrow it: 109 unreachable operations is a steep price for an audit property
that the opt-in flag largely preserves anyway, since an operator who wants the strong guarantee
simply does not pass the flag. But this is a judgement between two goods, not a measurement, and it
is the only thing in this proposal that genuinely blocks.
