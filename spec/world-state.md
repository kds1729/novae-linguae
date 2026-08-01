# World-state refinements and plan checking (v0.1)

The fourth open question of [`evolution/gcp-sdk-poc`](../evolution/gcp-sdk-poc/): an effectful
record's real contract is about the **world**, not its arguments. `PUT /items/{name}` guarantees
the item exists afterward; `DELETE` guarantees it is gone; a cloud operation *requires a VPC and
guarantees a subnet*. Value refinements (`pre`/`post` over parameters and `result`) cannot say any
of this — and without it, an agent cannot check a multi-step plan before performing any effect.
This spec adds the smallest vocabulary that can: two refinement kinds over an abstract resource
state, and a symbolic plan checker that discharges them **before anything runs**.

## The vocabulary

A function record's `signature.refinements[]` may carry, alongside the value refinements,
**world-state refinements** ([`function-record.v0.2.schema.json`](function-record.v0.2.schema.json)
`$defs/world_refinement`):

```json
{ "kind": "requires", "resource": { "class": "item", "key": [ { "kind": "var", "name": "name" } ] }, "state": "exists" }
{ "kind": "ensures",  "resource": { "class": "item", "key": [ { "kind": "var", "name": "name" } ] }, "state": "absent" }
```

- **`requires`** — what must be true of the external system for the call's documented success
  behavior; **`ensures`** — what the call leaves true.
- A **resource** is a `class` (free lowercase-kebab convention, like intent tags — `item`,
  `bucket`, `vpc`, `subnet`) plus a **key**: parameters of the function (by name — the body's
  lambda binders, the same convention `check-refinement` uses for value refinements), literal
  values, and/or **body fields**. Instantiating the key at a call's actual arguments **grounds**
  the resource: `item(name)` applied at `name = "widget"` is the ground resource
  `item("widget")`. An empty key names a class-wide singleton.
- A **`body-field` key part** (`{ "kind": "body-field", "param": "body", "field": "name" }`) reads
  a top-level scalar field of a JSON request-body parameter — earned by
  [`evolution/gcp-sdk-poc` finding 9](../evolution/gcp-sdk-poc/findings.md): the REST creation
  idiom names the new resource *inside* the body (9 of 9 GCS creates), so without this a create's
  `ensures` — exactly the clause a lifecycle plan needs — was inexpressible and the correct plan
  could only come back unverifiable. It grounds at **plan-check time from the step's literal
  argument**: the checker parses the plan's own concrete data, never runtime values, so
  decidability is untouched — and a grounded `bucket(body.name)` is the *same* resource as a later
  step's parameter-keyed `bucket(bucket)`, which is what lets the create discharge the read. A
  body argument that is not a JSON object carrying the field is a malformed plan/declaration pair:
  an **error**, never a verdict (the resource the call is declared to affect would be unnamed).
- **`state`** is `exists` or `absent` — deliberately the whole v0.1 state vocabulary. It covers
  the driver cases (create/verify/delete lifecycles, requires-a-VPC/guarantees-a-subnet); richer
  state (attributes, quantities, ownership) is future vocabulary, earned by a driver, not
  anticipated.

## Plan checking (`nl-validator check-plan`)

A **plan** is a sequence of concrete applications, plus what the planner is willing to assume
about the initial world:

```json
{
  "assume": [
    { "resource": { "class": "item", "key": [ { "kind": "lit", "value": { "kind": "string", "value": "widget" } } ] },
      "state": "absent" }
  ],
  "steps": [
    { "target": "fn_…", "args": [ { "kind": "string", "value": "http://…" }, { "kind": "string", "value": "widget" }, … ] },
    …
  ]
}
```

`check-plan --plan <file> (--records <dir> | --node <url>)` resolves each step's record (and body,
for the parameter names), instantiates its world refinements at the step's arguments, and
**symbolically executes** the sequence over a ground-resource state map:

- a step's `requires` must be satisfied by the current symbolic state — established by an earlier
  step's `ensures`, or by a stated assumption. A requirement the state **contradicts** rejects the
  plan (`REJECTED`, exit 1, naming the step, the resource, and where the contradicting state came
  from). A requirement the state says **nothing** about is collected as `UNKNOWN` — the plan is
  `UNVERIFIABLE`, never silently passed (state the missing assumption or reorder the plan);
- a step's `ensures` updates the symbolic state (later ensures overwrite earlier ones — the world
  moves);
- a plan whose every requirement discharges is `PLAN-SOUND`.

Ground resources compare by class + canonical key values, so `item("widget")` and `item("gone")`
never collide. Steps' argument counts are checked against the record's parameters; value
refinements (`pre`/`post`) are out of the plan checker's scope (they are `check-refinement`'s,
per record).

## Honest grading — what is verified, what is testimony

The plan check verifies **the plan against the declarations**: given what the records declare
about the world, the sequence is consistent, before any effect is performed. It never verifies
**the declarations against the world** — a `requires`/`ensures` is the record author's stated
contract, exactly as trustworthy as its author, priced through the attestation/certification
machinery like every other claim, and exposed by the effectful run itself when false (the
create → verify → delete exit gates are precisely such contracts holding live). `certify` treats
world refinements as declarations outside the SMT checker's scope (`check-refinement` reports
them as checked-elsewhere, the `inv` precedent); they never silently pass as proved.

## Deliberately out of v0.1

- **Observation probes** — mapping a resource state to the read-only call that would confirm it
  live (`item(x) exists` ⇝ `GET /items/{x}` → 200). This is what would let a checked plan be
  *spot-verified* against the real world before running; it needs a probe vocabulary and belongs
  to a future increment with a driver.
- **Plans as commons artifacts** — a plan riding in a Nova Locutio `propose`/`commit` body so the
  *checked plan itself* is content-addressed and signed. v0.1 keeps the plan a local input file.
- **Ingestion** — API descriptions carry no pre/postconditions (the gcp-sdk-poc measurement:
  81 of 81 records with empty refinements), so the OpenAPI adapter honestly derives none. World
  refinements are authored, or come from richer future description formats.
- **Richer state** than `exists`/`absent`, conditional contracts (a `DELETE`'s 404-vs-204 split),
  and quantified resources ("some bucket") — future vocabulary, driver-gated.
