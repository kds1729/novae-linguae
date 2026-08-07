# Proposal 02 — observe the idempotent DELETE, and narrow the read-only rule to match its intent

- **Status:** proposed
- **Module:** [`gcp-sdk-poc`](../README.md)
- **Addresses:** [finding 13](../findings.md#13-finding-11s-refusal-makes-path-parameterised-delete-unreachable)
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

`_example_for` fills the path parameter with `gw7-absent-x`, "a name no test writes". A DELETE
there is **DELETE's idempotent case**: applied to a resource that is not present it changes nothing
and reports absence. That is not an awkward edge of the verb, it is the defining property of it.

So the call the gate is refusing is one that provably has no effect. Measured live against Cloud
Storage, on a name deliberately never created:

```
GET    /b/nl-gw7-absent-probe-never-created -> 404      (probe: it is absent)
DELETE /b/nl-gw7-absent-probe-never-created -> 404      (the idempotent case)
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
| **Observe the idempotent DELETE** | **a real observation, trace-attached, replayable** | **none** |

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

## Questions only a maintainer can answer

1. Is *no call that can change state* the right narrowing of the read-only rule, or does the
   verb-level refusal earn its conservatism even where it provably over-refuses?
2. Should the probe be mandatory, or should an operator be able to assert absence and skip it?
3. `PUT`/`PATCH` at an absent name are also idempotent-ish in principle and are refused by the same
   rule — deliberately out of scope here, since neither is safe in the way DELETE is (both may
   *create*). Worth confirming that boundary is where you want it.
