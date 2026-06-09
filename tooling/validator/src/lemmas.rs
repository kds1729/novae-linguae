//! Auxiliary-lemma catalog for the inductive prover ([`crate::induct`]) — Layer A of lemma discovery.
//!
//! Some list laws can't be closed by a single unfold + induction hypothesis; the classic case is
//! `reverse(reverse(xs)) = xs`, whose step needs the helper `reverse(append(as, bs)) =
//! append(reverse(bs), reverse(as))`, which itself rests on `append`'s associativity and right-identity.
//! Rather than have the solver invent these (it can't), we supply a small **curated catalog** of
//! standard equational lemmas over the list algebra. When the bare induction stalls, the prover selects
//! relevant catalog lemmas, **proves each one by induction** (recursively — they may need each other),
//! and re-runs the stalled obligation with the *proven* lemmas added as universally-quantified
//! assumptions. A lemma is assumed only after it is itself discharged, so this never manufactures a
//! false proof — it is exactly as honest as the bare engine, just able to reach further.
//!
//! This is the pragmatic, re-checkable v0.1 (mirroring `property_catalog.json` for properties and the
//! lexical embedder for search): a fixed catalog behind a selection seam. The generalizable follow-on
//! (Layer B) is *theory exploration* — conjecturing lemmas by enumerating small terms over the goal's
//! operations and testing them with the generative engine before proving the survivors. The catalog
//! function below is the seam where that discovery procedure drops in.

use serde_json::{json, Value as J};

/// A named candidate lemma, stated as a `forall` claim AST over the list algebra — the same AST shape
/// the inductive prover lowers to SMT-LIB.
pub struct Lemma {
    pub name: &'static str,
    pub stmt: J,
}

fn forall(vars: &[&str], body: J) -> J {
    json!({ "kind": "forall", "vars": vars, "body": body })
}
fn app(op: &str, args: Vec<J>) -> J {
    json!({ "kind": "app", "op": op, "args": args })
}
fn var(n: &str) -> J {
    json!({ "kind": "var", "name": n })
}
fn eq(a: J, b: J) -> J {
    app("eq", vec![a, b])
}

/// The standard list-algebra lemmas. Each is provable by structural induction; the cheap ones
/// (`append_nil`, `append_assoc`, `length_append`) need no further lemma, while `reverse_append` rests
/// on the first two — the recursive discharge in [`crate::induct`] resolves that dependency tree.
pub fn catalog() -> Vec<Lemma> {
    vec![
        // append(xs, nil) = xs  — right identity of append.
        Lemma {
            name: "append_nil",
            stmt: forall(&["xs"], eq(app("append", vec![var("xs"), var("nil")]), var("xs"))),
        },
        // append(append(xs, ys), zs) = append(xs, append(ys, zs))  — associativity.
        Lemma {
            name: "append_assoc",
            stmt: forall(
                &["xs", "ys", "zs"],
                eq(
                    app("append", vec![app("append", vec![var("xs"), var("ys")]), var("zs")]),
                    app("append", vec![var("xs"), app("append", vec![var("ys"), var("zs")])]),
                ),
            ),
        },
        // reverse(append(xs, ys)) = append(reverse(ys), reverse(xs))  — reverse distributes over append.
        Lemma {
            name: "reverse_append",
            stmt: forall(
                &["xs", "ys"],
                eq(
                    app("reverse", vec![app("append", vec![var("xs"), var("ys")])]),
                    app("append", vec![app("reverse", vec![var("ys")]), app("reverse", vec![var("xs")])]),
                ),
            ),
        },
        // length(append(xs, ys)) = add(length(xs), length(ys))
        Lemma {
            name: "length_append",
            stmt: forall(
                &["xs", "ys"],
                eq(
                    app("length", vec![app("append", vec![var("xs"), var("ys")])]),
                    app("add", vec![app("length", vec![var("xs")]), app("length", vec![var("ys")])]),
                ),
            ),
        },
        // map(f, append(xs, ys)) = append(map(f, xs), map(f, ys))  — map distributes over append.
        Lemma {
            name: "map_append",
            stmt: forall(
                &["f", "xs", "ys"],
                eq(
                    app("map", vec![var("f"), app("append", vec![var("xs"), var("ys")])]),
                    app(
                        "append",
                        vec![
                            app("map", vec![var("f"), var("xs")]),
                            app("map", vec![var("f"), var("ys")]),
                        ],
                    ),
                ),
            ),
        },
    ]
}
