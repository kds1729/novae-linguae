# Proposal 01 — emit the declared request `Content-Type`

- **Status:** proposed
- **Module:** [`gcp-sdk-poc`](../README.md)
- **Addresses:** [finding 1](../findings.md#1-the-request-content-type-is-dropped)
- **Blast radius:** re-addresses every body-carrying record. Needs a maintainer decision.
- **Spike:** measured against the full test suite; the complete diff is inline below.

## The change

An operation with a `requestBody` compiles to an `http` call whose header argument is `map_empty`, so
no `Content-Type` is sent and the record is rejected by any service requiring one — while reporting
`certify=OK`. Multipart bodies already emit the header (with the spec-time boundary); only the
non-multipart path is missing it.

Two edits in `tooling/nl-ingest-openapi/openapi_ingest.py`, against `76fc6ba`:

1. Capture the declared media type where `content` is already read (beside the `mp_types`
   computation, ~line 650). Data-driven, never inferred:
   - exactly one non-multipart declared type → that is the `Content-Type`;
   - more than one → note and send none, because the description has not said which to use;
   - multipart-only → unchanged.
2. Emit it as an `elif` beside the existing multipart injection (~line 721).

```diff
@@ def build_operation(spec, base_url, path, verb, op, shared_params, global_securi
     body_spec = None
     mp_parts, mp_ct, mp_boundary = [], None, None
+    body_ct = None
     if "requestBody" in op:
         body_spec = deref(spec, op["requestBody"])
         if not isinstance(body_spec, dict):
             return ("skip", op_id, "unresolvable requestBody $ref (external or dangling)")
         content = body_spec.get("content") or {}
         mp_types = sorted(ct for ct in content if ct.startswith("multipart/"))
+        # The DECLARED request media type, for the `Content-Type` request header. Data-driven, never
+        # a blanket `application/json` guess: exactly one non-multipart type is unambiguous, while
+        # several are a choice the description leaves open — note that and send none. (Multipart
+        # carries its own Content-Type with the boundary; see mp_ct below.)
+        declared_cts = [ct for ct in sorted(content) if not ct.startswith("multipart/")]
+        if len(declared_cts) == 1:
+            body_ct = declared_cts[0]
+        elif len(declared_cts) > 1:
+            offered = ", ".join(f"`{c}`" for c in declared_cts)
+            notes.append(f"request `Content-Type` omitted — the body declares {offered} and the "
+                         f"description does not say which one to send")
         if content and len(mp_types) == len(content):
@@
     if mp_ct:
         headers = curried_app(b_var("map_put"), s_lit("Content-Type"),
                               s_lit(f"{mp_ct}; boundary={mp_boundary}"), headers)
+    elif body_ct:
+        # A request body without its declared Content-Type is rejected by real services, so a
+        # record generated without this header does not execute. Symmetric with the multipart case
+        # directly above, which has always emitted the header.
+        headers = curried_app(b_var("map_put"), s_lit("Content-Type"), s_lit(body_ct), headers)
 
     if has_body:
         body_arg = b_var("body")
```

## The consequence needing a decision: content re-addressing

Every body-carrying operation's body AST changes, so `body_hash` changes, so the record `hash`
changes. Any already-published record for such an operation acquires a new content address.

This is the same class of concern that led `http_full` to be introduced as a *second* builtin rather
than changing `http`'s result shape, which is why this is a proposal and not a PR. The
counter-argument is that the affected records do not currently work — what would be re-addressed is a
set of artifacts that cannot perform their documented call. But the sequencing, and whether a version
boundary or migration note is wanted, is not the contributor's call to make.

## Measured collateral

Full suite on the spike, 49 tests. **One failure**, and it is substantive rather than hash churn:

```
FAIL: test_header_projection_alpha_equivalent_to_hand_authored
AssertionError: 'expr_c850bada0ff3032b…' != 'expr_cc4ddae9d869c8ef…'
```

`createThingLocation` projects the `Location` header of **POST `createThing`**, which carries a
`requestBody` — so the embedded POST gains the header and its canonical normal form no longer
coincides with hand-authored `spec/examples/body-create-thing.json`. Restoring the GW16
α-equivalence result means regenerating that fixture (and its record's `body_hash`) to include the
header. That is a documented faithfulness artifact, so it should be an explicit, reviewed change
rather than a side effect.

Everything else passes, notably:

- `test_faithful_to_hand_authored_gw6_records` — unaffected; it compares only the bodyless verbs
  (`getItemStatus`, `deleteItem`), exactly as its own comment scopes it.
- `test_multipart_compiles_to_a_deterministic_form` — the multipart path is untouched.
- `test_every_record_certifies` — the added header certifies cleanly throughout.

## Independent corroboration

Cloud Storage v1 was ingested twice: once with this change, and once through the unpatched adapter
followed by an out-of-band repair script that rewrote each body AST, recomputed the hashes and
re-certified. The two agree **exactly**:

```
records: 81
body_hash differences vs the post-hoc-repaired records: 0
bodies carrying Content-Type: 31          (30 application/json + 1 application/octet-stream)
ambiguous multi-declared-type notes: 0
```

Two independent implementations — an in-adapter emitter and an after-the-fact rewriter — converging
on byte-identical content addresses is good evidence the injection rule above is the right one. It
also means the repair step disappears downstream with no change to the resulting corpus.

## Questions only a maintainer can answer

1. Is the re-addressing acceptable now, or should it ride a schema/version boundary?
2. Should the multi-declared-type case note-and-omit (proposed), or refuse the operation outright?
3. Regenerating `spec/examples/body-create-thing.json` to restore the GW16 α-equivalence result — in
   this change, or as its own?
