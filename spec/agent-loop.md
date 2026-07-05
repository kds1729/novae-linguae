# The agent loop (v0.2)

*Status: v0.2, implemented in [`tooling/validator`](../tooling/validator/src/) —
`respond.rs` (`respond_to_request` / `verify_claim`) over `interp.rs`, exposed by
`nl-validator respond` and `nl-validator verify-claim`.*

This is where **Nova Locutio becomes actionable**. Messages were already validated, signed, encrypted,
stored, and discoverable — but nothing *consumed* one to drive behavior. The agent loop closes that
gap: an agent answers a `request` by **running** the requested computation and replying with a signed
`assert` whose claim *is* the result, and any receiver **re-runs** that claim to confirm it. It joins
the two languages — a Nova Locutio message drives a Nova Lingua evaluation (`spec/evaluation.md`) over
the shared commons (`spec/commons.md`) — and makes principles 1 (self-describing), 3 (verified by
default), 4 (assemble, don't write), and 6/7 (signed, no privileged party) operational end to end.

## The loop

```
requester                         responder                        any receiver
   │  request (apply target args)    │                                  │
   ├────────────────────────────────►│  resolve target body + fn_refs   │
   │                                  │  run it on the args (interp.rs)  │
   │         assert (predicate claim) │                                  │
   │◄────────────────────────────────┤                                  │
   │                                  │           assert ────────────────►  re-run the claim
   │                                  │                                  │  CONFIRMED / REFUTED
```

1. **Request.** A signed `request` ([`message.v0.2.schema.json`](message.v0.2.schema.json)) whose
   body is `action: "apply"` over a commons `target` (a `fn_…` content-address) and `args` — an
   array of **value-expressions** ([`value-expression.schema.json`](value-expression.schema.json)),
   so a higher-order argument is a `fn_ref` to a commons function, not an inline blob.
2. **Resolve + run.** The responder builds the address→body link map from its commons view (the same
   one [`run --records`](evaluation.md) uses), resolves the target's `body_hash` to its body, and
   **evaluates** it on the args. `fn_ref` arguments resolve through the same map, so composites run —
   e.g. `apply map (double, [1,2,3])` resolves and runs `double` by address (principle 4).
3. **Assert.** The responder emits a signed `assert` whose `predicate` claim
   ([`claim-expression.schema.json`](claim-expression.schema.json) →
   [`predicate-expression.schema.json`](predicate-expression.schema.json)) is the computed equation

   ```
   eq( <target>(arg0, arg1, …), <result> )
   ```

   — the target applied (an `app` op by content-address) to the request's args (each carried as a
   `lit`), equated to the produced `result` value-expression. The reply is addressed `to` the
   requester and threaded by `in_reply_to` to the request's hash. `subject` is the target.
4. **Verify.** Because the claim is an ordinary predicate over a content-addressed function, **any**
   receiver re-runs it: resolve the claim's functions from the commons and evaluate. A `request` is
   not needed to verify — the `assert` is self-contained. Verification is re-execution, so no one has
   to trust the responder (principles 3, 6, 7). A tampered result re-runs false and is refuted.

## CLI

```bash
# Responder: answer a request by running the target and emitting a signed assert.
nl-validator respond spec/examples/request.v0.2.json \
  --records spec/examples/ --seed <responder-seed> [--timestamp <iso8601>]

# Receiver: re-run an assert's claim against the commons. Exit 0 = CONFIRMED, 1 = REFUTED.
nl-validator verify-claim spec/examples/assert-result.v0.2.json --records spec/examples/
```

Worked example: [`request.v0.2.json`](examples/request.v0.2.json) asks the responder to apply
[`map`](examples/map.v0.2.json) to (`double` by `fn_ref`, `[1,2,3]`).
[`respond`](../tooling/validator/src/respond.rs) produces
[`assert-result.v0.2.json`](examples/assert-result.v0.2.json), whose claim is
`eq( map(double, [1,2,3]), [2,4,6] )`; `verify-claim` re-runs it to **CONFIRMED**. Both messages carry
real hashes and Ed25519 signatures that `nl-validator verify` passes; the request is signed by the
example "claude" identity (`did:nova:ea9b…505e`), the assert by an example responder identity.

## Beyond `apply`: validate and query

The responder dispatches on the message: it also handles a `request` to **validate** and a **query**.

- **`request` / `validate`** — *"is this target sound?"* The responder resolves the target's record
  and body, **typechecks** the body against the declared `signature.type` and **runs** its worked
  examples (the same checks as `nl-validator typecheck` / `run`), and replies with an `assert` whose
  claim is `verified` (subject verified *by* the responder's DID) when both pass, or a `reject`
  (`code: constraint_violated`, with the reason; `unknown_target` if it can't resolve it). This is
  validation-as-a-service: the verdict is re-execution, signed and attributable. Worked example:
  [`request-validate.json`](examples/request-validate.v0.2.json) → [`assert-verified.json`](examples/assert-verified.v0.2.json)
  (validate `double` → `verified`).

- **`propose`** — *"would you do this?"* A proposal invites action but allows refusal. The responder
  verifies it can fulfil the `apply` (resolve + test-run the target on the proposed args) and replies
  `commit` — an `apply` commitment to run it — or `reject` with a reason. Worked example:
  [`propose.json`](examples/propose.v0.2.json) (apply `double` to `[21]`) → [`commit-apply.json`](examples/commit-apply.v0.2.json).
  Acting on the commitment (execute → assert) reuses the `apply` path above, so a full
  `query → propose → commit → assert` chain composes from these handlers, threaded by `in_reply_to`.

- **`query`** — *"what do you have that matches?"* The responder searches its records for those
  matching the query `pattern` (`effects` / `intent_tags` as containment, `terminates` as equality;
  `signature_type` matching is deferred) and replies with an `ack` carrying the sorted matching
  content-addresses. This is discovery over Nova Locutio — the precondition for principle 4 (assemble
  from what exists). Worked example: [`query.json`](examples/query.v0.2.json) (effects `io.console`) →
  [`ack-query.json`](examples/ack-query.v0.2.json) (the one match: `greet`).

```bash
nl-validator respond spec/examples/request-validate.v0.2.json --records spec/examples/ --seed <s>  # -> assert verified / reject
nl-validator respond spec/examples/query.v0.2.json            --records spec/examples/ --seed <s>  # -> ack with matches
```

## Autonomous orchestration

`nl-validator orchestrate --records <dir> --intent <tag> --arg <value> --seed <s>` drives the whole
conversation end to end: the orchestrator **discovers** a function by intent (`query` → `ack`),
**proposes** applying it (`propose` → `commit`), the committer **fulfils** it (`commit` → `assert`),
and the orchestrator **verifies** the result by re-running the claim. The agent never names the
function — it finds one (principle 4, made autonomous). Every message is signed and threaded; the run
prints the transcript and exits non-zero unless every stage's claim is CONFIRMED. **Each `--intent` is
a pipeline stage** — the result of one feeds the next — so the orchestrator *composes* multiple
discovered functions. Worked: a single `--intent arithmetic` over `[21]` discovers `double` and
confirms `double(21) = 42` (five messages); `--intent arithmetic --intent arithmetic` discovers and
composes `double` twice, confirming `double(double(21)) = 84` (ten messages).

### Verified orchestration (`--verify`)

`nl-validator orchestrate --verify [--policy <p> --attestation <a>…]` folds verification into the loop —
the project's thesis in one autonomous run: **discover** functions by intent (a query returns a *set*),
keep only those whose **signature fits the application** — arity *and* parameter types must accept the
arguments (a binary function is no candidate for a unary apply; a function over lists is no candidate for
an integer argument), with polymorphic type variables unified consistently across the parameters — and a
**higher-order argument is checked too**: a `fn_ref` passed where a function is expected is resolved and
its own signature unified against the expected function type, so a wrongly-shaped (e.g. unary) function
can't be slipped into a higher-order slot (a `fn_ref` the node can't resolve is rejected, since it can't
be type-checked) — **rank the survivors by trust** and use the most-trusted —
the receiver's *local* policy over its own attestation
graph (no central authority — principle 7), preferring higher aggregate confidence, then more
vertex-disjoint paths, then more distinct attesters; if none is trusted the run aborts before any
function is touched. (This replaces a naive "take matches[0]": discovery returns candidates, and *which*
to use is the consumer's trust decision, not the order they came back in.) It then **proves** the chosen
function's own
declared property over the unbounded domain (don't trust the record's claim — re-prove it with the SMT
+ induction + lemma-discovery engine), then **apply** it and **re-verify** the result by re-running
(principle 3). The transcript gains `trust` and `prove` steps between `ack` and `propose`. Worked: with
a trusted root vouching for `double`, `--verify --intent arithmetic` over `[21]` discovers `double`,
confirms it trusted, proves its `doubles` property, applies it, and re-verifies `21 → 42` (CONFIRMED);
drop the vouching attestation and the same run ABORTS at the trust gate.

### Over a live node (`--node`)

The same loop — plain or `--verify --require-certified` — runs against a **remote commons** instead
of a local directory: `nl-validator orchestrate --node https://<node> --intent <tag> --arg <v> --seed
<s> [--publish]`. Discovery goes through the node's `POST /v0/query`; the candidates' records,
bodies, and `fn_ref` helpers are fetched by content-address (a bounded reference-closure walk) and
**every fetched artifact is re-hashed locally** — it must equal the address it was requested by, so
the store stays untrusted infrastructure (principle 7): a lying or corrupted node can only *fail* a
run, never spoof a function into it. `--publish` sends the final signed `assert` back through the
node's verify-then-store gate, making the result claim a public commons artifact. The receiving half
is symmetric: `nl-validator verify-claim msg_<hash> --node https://<node>` lets a third party who
knows *only an address and a node URL* fetch the claim and everything it references (all
hash-verified) and re-run it — verification is re-execution, across the network. Worked, against the
production node: `--node https://nl.1105software.com --intent parse` over `"id,21,ok"` discovered
three candidates, chose `double_second_field`, certified it, applied it, CONFIRMED `42`, and
published the assert; an independent `verify-claim msg_5d22cb… --node …` then re-ran it to CONFIRMED.

**Cost (measured, deliberately unoptimized):** the remote loop adds ~1.5 s over local — 7 sequential
HTTPS requests (1 query + 6 artifact fetches) at ~210 ms each, dominated by per-request TCP+TLS
handshakes (the client sends `Connection: close`). Acceptable for discovery-then-run. If it ever
matters, the designed remedies in order: a **local content-addressed cache** (artifacts are immutable
— same address, same bytes, forever — so caching is staleness-free by construction and makes repeat
runs local-speed), connection reuse across the closure walk, and parallel artifact fetches. None
implemented until a real workload makes the 1.5 s substantial.

## Scope (v0.2, honest)

- **The inbound speech acts are wired.** Beyond `apply`/`validate`/`query`/`propose`, the responder
  also handles: the `store` request action (verify the inline payload's content-address →
  `ack`/`reject`); **acting on a received `commit`** (fulfil an `apply` commitment — resolve + run the
  function → `assert` the result, closing `propose → commit → assert`); and `delegate` / `retract`
  (acknowledged). The loop is driven end to end by `nl-validator orchestrate` (above), and
  `apply`/`propose` are **capability-gated**: a target whose record declares required
  `signature.capabilities` is fulfilled only if the sender is authorized, else `not_authorized`. With
  no recognized roots configured the gate is possession-only (the request must list the capability in
  `constraints.capabilities`); configured with a `TrustPolicy` (recognized roots + a `delegate` token
  pool) it switches to **chain-verified** — the sender must exhibit a valid signed `delegate` chain
  back to a recognized root, checked by `verify_delegation_chain` (signatures, attenuation, expiry,
  conditions; see `spec/trust-model.md` and `nl-validator verify-delegation`). Listing the string no
  longer suffices.
- **Pure targets.** The target must be a body the v0.1 evaluator handles (`spec/evaluation.md`):
  effects are not modelled, so an effectful target is out of scope. An unresolvable target or args
  that don't decode are an honest error, never a silent empty assert.
- **`predicate` claims.** The responder emits — and `verify-claim` re-runs — a `predicate` claim. The
  `satisfies` / `verified` claim kinds are descriptive and not re-run here.
- **Example-exact, not proven.** A CONFIRMED verdict means the claim's equation evaluated true on the
  concrete values asserted. It is a re-execution of *that* computation, not a proof over all inputs
  (that is the generative property-testing engine, still the next rung — see the project README).
