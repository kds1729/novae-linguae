# Design (phase 2)

The design phase of the gcp-provisioning-poc, begun 2026-11-12 when the author declared the concept plan fully
realized. It produces four things the charter's phase 2 asks for: the record schemas, the algorithms stated
precisely, the five layer-1 specification proposals, and the glossary's web service hand-traced from needs
list to receipt with every record written out. Nothing here is implemented; phase 3 (the prototype against
the fake service) starts only on the author's go.

| Where | What |
|---|---|
| [`schemas/`](schemas/) | JSON Schemas for the ten record kinds the mechanism adds (kind, dish, recipe, needs list, profile, plate, instance, receipt, provisioning attestation, statement) and the proposed plan extension; `common.schema.json` holds the shared definitions. |
| [`algorithms.md`](algorithms.md) | The menu view, the coverage check, the selector, the instance generator, the provisioning command, the planner, scrub, test provisioning and re-provisioning, each with inputs, outputs, procedure and named failures. |
| [`proposals/`](proposals/) | The five changes to the language the mechanism needs (deferred item 10, decision 34): a `ready` state, a planner, wait steps, authored refinements in the recipe, a `secret` type. Written in the module's proposal format; built at the start of phase 3. |
| [`worked-example/`](worked-example/) | The web service of the glossary, every record written out under `records/`, the hand trace in `trace.md`, the address table in `addresses.md`, and `build.py`, which writes, hashes, signs and checks them. |

## How the records fit the language

Every object the charter names is a content-addressed record (R1). The design adds ten address prefixes
(`kind_`, `dish_`, `rcp_`, `needs_`, `prof_`, `plate_`, `inst_`, `rcpt_`, `patt_`, `stmt_`) to the nine the
specification has; the extension is additive and is the first thing phase 3 proposes to `spec/`. Three choices
made here, beyond the concept decisions:

- **A kind is a named record type.** Decision 14 said a kind is a Nova Lingua record type. The design wraps the
  type expression in a `kind_` record with a name, so two kinds with the same attributes and different names are
  different kinds ("one concept, one symbol" is nominal), while coverage still evaluates predicates over the
  record type's fields with `check-refinement`.
- **A provisioning attestation is a signed record of its own**, modelled on the certification record rather
  than on the `verified` claim, because it carries fields a claim cannot (harness plate, instance, result,
  cause class, receipts). Decision 32 had asked for this to be confirmed in the design phase; the answer is the
  certification precedent.
- **Reference types carry names.** A port type such as `network-ref` carries the resource's name beside its
  link, so a recipe can state `requires psa-connection(wire network.name)` and the planner can ground it from
  the instance. This is how one dish declares a dependency on another dish's resource without reading its
  recipe, and it is the AWS ecosystem module's "names, not placeholders" applied to wiring.

Three names in the record schemas differ in spelling from the glossary because the type-expression schema
fixes field names as snake_case: `encryption_at_rest`, `private_service_access`, `min_instances`.

## What the worked example shows

The needs list has four needs; the production profile adds failover and two regions to the database and a warm
instance to the service; the menu offers five dishes, two of which satisfy the database need. The selector's
candidate sets are checked, the plate's four servings and five port bindings pass coverage, the instance writes
out every name including the derived ones, the planner produces four waves with one wait step (the database
must be RUNNABLE before its user and the service are created), the plan rests on no assumptions, the receipt
records every serving's outputs with the credential as a secret reference, and scrub plans the deletes in
reverse. Each of the five dishes has a passing, signed test provisioning: a harness plate, a test instance, a
receipt, a scrub receipt and an attestation. `build.py` recomputes every address and verifies every signature.

The operation records are stand-ins for the OpenAPI adapter's output (the real Google Cloud API operations are
named in `addresses.md`); in phase 4 they are replaced by adapter records and the recipes change only in the
addresses they cite. The attester is a throwaway design-phase key.
