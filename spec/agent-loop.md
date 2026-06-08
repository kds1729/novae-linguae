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

## Scope (v0.2, honest)

- **`apply` only.** The responder handles `action: "apply"`. `store` / `validate` requests, and the
  other speech acts (propose/commit/delegate/…), are separate flows not driven here.
- **Pure targets.** The target must be a body the v0.1 evaluator handles (`spec/evaluation.md`):
  effects are not modelled, so an effectful target is out of scope. An unresolvable target or args
  that don't decode are an honest error, never a silent empty assert.
- **`predicate` claims.** The responder emits — and `verify-claim` re-runs — a `predicate` claim. The
  `satisfies` / `verified` claim kinds are descriptive and not re-run here.
- **Example-exact, not proven.** A CONFIRMED verdict means the claim's equation evaluated true on the
  concrete values asserted. It is a re-execution of *that* computation, not a proof over all inputs
  (that is the generative property-testing engine, still the next rung — see the project README).
