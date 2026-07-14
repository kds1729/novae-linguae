//! The canonical builtin type artifacts and the v0.2 builtin↔ref fold
//! (spec/type-expression.schema.json: "v0.2+ will likely fold these into 'ref' once the
//! primitives have stable hashes in the commons" — these tests pin those stable hashes and the
//! interchange semantics).

use nl_validator::{
    canonical_builtin_type_address, canonical_builtin_type_targets, check_type_well_formed,
    fold_canonical_type_refs, hash_artifact_with_kind, nat_param_positions, type_mentions_float,
    ArtifactKind, BUILTIN_TYPE_NAMES,
};
use serde_json::json;

/// A ref node to a builtin's canonical type artifact.
fn canon_ref(name: &str) -> serde_json::Value {
    json!({ "kind": "ref", "target": canonical_builtin_type_address(name).unwrap() })
}

#[test]
fn canonical_addresses_are_the_builtin_nodes_own_type_hashes() {
    // The canonical artifact for a builtin is the builtin node ITSELF — its address is therefore
    // recomputable anywhere, which is what makes the fold decidable without a store lookup.
    for name in BUILTIN_TYPE_NAMES {
        let node = json!({ "kind": "builtin", "name": name });
        let addr = hash_artifact_with_kind(&node, ArtifactKind::Type).unwrap();
        assert_eq!(canonical_builtin_type_address(name).unwrap(), addr);
        assert_eq!(canonical_builtin_type_targets().get(&addr).copied(), Some(name));
        // The artifact content is a well-formed type on its own (what the node's gate checks).
        check_type_well_formed(&node).unwrap();
    }
    assert_eq!(canonical_builtin_type_targets().len(), BUILTIN_TYPE_NAMES.len());
    assert!(canonical_builtin_type_address("NotABuiltin").is_none());
}

#[test]
fn canonical_addresses_are_pinned() {
    // Regression pin: these addresses are published commons artifacts — they must never move.
    // (JCS of {"kind":"builtin","name":…} → BLAKE3-256 → type_<hex>.)
    assert_eq!(
        canonical_builtin_type_address("int").unwrap(),
        "type_52f7ad9092dd7d22b8da25c5e95b90b69ddf4512cd636706ee9ca665b4ff54cb"
    );
    assert_eq!(
        canonical_builtin_type_address("Json").unwrap(),
        "type_abc8af97d1ba996b2513e016b2142c8068dae7e026f671eb8f7661cb2c920ec2"
    );
}

#[test]
fn fold_rewrites_canonical_refs_and_leaves_the_rest() {
    let ty = json!({ "kind": "forall", "vars": ["a"], "body": { "kind": "fn",
        "params": [
            canon_ref("int"),
            { "kind": "apply", "ctor": canon_ref("List"), "args": [ { "kind": "var", "name": "a" } ] },
            { "kind": "ref", "target": format!("type_{}", "ab".repeat(32)) } ],
        "result": canon_ref("bool") } });
    let folded = fold_canonical_type_refs(&ty);
    assert_eq!(folded.pointer("/body/params/0"), Some(&json!({ "kind": "builtin", "name": "int" })));
    assert_eq!(
        folded.pointer("/body/params/1/ctor"),
        Some(&json!({ "kind": "builtin", "name": "List" }))
    );
    // A non-canonical ref (a user-defined type) stays an opaque ref.
    assert_eq!(
        folded.pointer("/body/params/2/kind").and_then(|k| k.as_str()),
        Some("ref")
    );
    assert_eq!(folded.pointer("/body/result"), Some(&json!({ "kind": "builtin", "name": "bool" })));
    // A tree with no canonical refs folds to itself.
    assert_eq!(fold_canonical_type_refs(&folded), folded);
}

#[test]
fn float_guard_trips_through_a_canonical_ref() {
    // The soundness-relevant site: a float spelled as a canonical ref must refuse Int-theory
    // proving exactly like the builtin spelling.
    let hidden = json!({ "kind": "fn", "params": [canon_ref("float")],
                         "result": { "kind": "builtin", "name": "string" } });
    assert!(type_mentions_float(&hidden));
    let int_ref = json!({ "kind": "fn", "params": [canon_ref("int")], "result": canon_ref("int") });
    assert!(!type_mentions_float(&int_ref));
}

#[test]
fn nat_positions_recognized_through_a_canonical_ref() {
    // A ref-spelled `nat` parameter licenses the same guarded numeric descent as the builtin.
    let record = json!({ "signature": { "type": { "kind": "fn",
        "params": [ { "kind": "builtin", "name": "string" }, canon_ref("nat") ],
        "result": { "kind": "builtin", "name": "int" } } } });
    let positions: Vec<usize> = nat_param_positions(&record).into_iter().collect();
    assert_eq!(positions, vec![1]);
}
