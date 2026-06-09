//! Equivalence clustering — group commons functions into behavioral-equivalence classes and name a
//! canonical representative for each. This is the second half of the "semantic equivalence vs hash
//! equivalence" open problem: [`crate::equiv`] *proves* two functions equal, and this lifts that to a
//! whole record set, so behaviorally-identical functions with different content-addresses collapse into
//! one class (deduplication beyond byte-identity — principle 2).
//!
//! To keep it tractable, candidates are first bucketed by a coarse **signature shape** (arity + coarse
//! parameter/result types, type variables as wildcards): only same-shape functions are ever compared,
//! and within a bucket a union-find runs `prove_equivalent` pairwise (skipping pairs already merged).
//! The canonical representative is the lexicographically smallest content-address in a class.
//!
//! Scope follows [`crate::equiv`] (v0.1): unary functions, at least one side of a pair non-recursive —
//! so two mutually-recursive same-shape functions stay separate classes (we can't yet prove them equal),
//! and only functions whose body this node holds participate. Cost within a shape bucket of size k is up
//! to O(k²) solver calls; the shape bucketing is what keeps that from being O(n²) over the whole set.

use serde_json::Value as J;
use std::collections::HashMap;

use crate::{prove_equivalent, EquivVerdict};

/// A coarse, hashable shape string for a type-expression (arity + structure; type variables → `_`,
/// `int`/`nat` unified as `Num`). Two functions are compared only if their shapes match.
fn type_shape(t: &J) -> String {
    let t = if t.get("kind").and_then(|k| k.as_str()) == Some("forall") {
        t.get("body").unwrap_or(t)
    } else {
        t
    };
    match t.get("kind").and_then(|k| k.as_str()) {
        Some("fn") => {
            let params: Vec<String> =
                t.get("params").and_then(|p| p.as_array()).map(|a| a.iter().map(type_shape).collect()).unwrap_or_default();
            let result = t.get("result").map(type_shape).unwrap_or_else(|| "?".into());
            format!("({})->{result}", params.join(","))
        }
        Some("builtin") => match t.get("name").and_then(|n| n.as_str()) {
            Some("int") | Some("nat") => "Num".into(),
            Some(other) => other.into(),
            None => "?".into(),
        },
        Some("apply") if t.pointer("/ctor/name").and_then(|n| n.as_str()) == Some("List") => {
            let e = t.get("args").and_then(|a| a.as_array()).and_then(|a| a.first()).map(type_shape).unwrap_or_else(|| "_".into());
            format!("List[{e}]")
        }
        Some("var") => "_".into(),
        _ => "?".into(),
    }
}

fn record_shape(record: &J) -> String {
    record.pointer("/signature/type").map(type_shape).unwrap_or_else(|| "?".into())
}

fn find(parent: &mut [usize], mut i: usize) -> usize {
    while parent[i] != i {
        parent[i] = parent[parent[i]]; // path halving
        i = parent[i];
    }
    i
}

/// Cluster `items` (`(content-address, record, body)`) into behavioral-equivalence classes. Functions
/// are compared only within a shape bucket; a missing body (`None`) can't be proved equal to anything, so
/// it stays a singleton. Returns the classes, each sorted (canonical rep first), ordered by their rep.
pub fn cluster(items: &[(String, J, Option<J>)], solver: &str) -> Vec<Vec<String>> {
    let n = items.len();
    let mut parent: Vec<usize> = (0..n).collect();

    let mut by_shape: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, (_, rec, _)) in items.iter().enumerate() {
        by_shape.entry(record_shape(rec)).or_default().push(i);
    }
    for group in by_shape.values() {
        for a in 0..group.len() {
            for b in (a + 1)..group.len() {
                let (i, j) = (group[a], group[b]);
                if find(&mut parent, i) == find(&mut parent, j) {
                    continue; // already merged transitively
                }
                if let (Some(bi), Some(bj)) = (&items[i].2, &items[j].2) {
                    if matches!(prove_equivalent(bi, bj, solver), EquivVerdict::Equivalent(_)) {
                        let (ri, rj) = (find(&mut parent, i), find(&mut parent, j));
                        parent[ri] = rj;
                    }
                }
            }
        }
    }

    // Gather members per class root.
    let mut classes: HashMap<usize, Vec<String>> = HashMap::new();
    for i in 0..n {
        let r = find(&mut parent, i);
        classes.entry(r).or_default().push(items[i].0.clone());
    }
    let mut out: Vec<Vec<String>> = classes
        .into_values()
        .map(|mut members| {
            members.sort(); // canonical representative = lexicographically smallest address
            members
        })
        .collect();
    out.sort(); // deterministic order, by each class's representative
    out
}

/// Cluster every function record in `dir` (resolving each one's body when the node holds it).
pub fn cluster_dir(dir: &std::path::Path, solver: &str) -> anyhow::Result<Vec<Vec<String>>> {
    let records = crate::build_record_map(dir)?;
    let link = crate::build_link_map(dir)?;
    let mut items: Vec<(String, J, Option<J>)> =
        records.into_iter().map(|(h, r)| (h.clone(), r, link.get(&h).cloned())).collect();
    items.sort_by(|a, b| a.0.cmp(&b.0)); // deterministic input order
    Ok(cluster(&items, solver))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn solver() -> Option<&'static str> {
        for s in ["z3", "cvc5"] {
            if std::process::Command::new(s).arg("--version").output().map(|o| o.status.success()).unwrap_or(false) {
                return Some(s);
            }
        }
        None
    }

    fn lam(p: &str, body: J) -> J {
        json!({ "kind": "lambda", "params": [{ "name": p }], "body": body })
    }
    fn bap(f: &str, args: J) -> J {
        json!({ "kind": "app", "fn": { "kind": "var", "name": f }, "args": args })
    }
    fn v(n: &str) -> J {
        json!({ "kind": "var", "name": n })
    }
    fn int(n: i64) -> J {
        json!({ "kind": "lit", "value": { "kind": "int", "value": n } })
    }
    fn num_to_num() -> J {
        json!({ "kind": "fn", "params": [{ "kind": "builtin", "name": "int" }], "result": { "kind": "builtin", "name": "int" } })
    }
    fn list_to_list() -> J {
        let la = json!({ "kind": "apply", "ctor": { "kind": "builtin", "name": "List" }, "args": [{ "kind": "var", "name": "a" }] });
        json!({ "kind": "forall", "vars": ["a"], "body": { "kind": "fn", "params": [la.clone()], "result": la } })
    }
    fn item(hash: &str, ty: J, body: J) -> (String, J, Option<J>) {
        (hash.to_string(), json!({ "hash": hash, "signature": { "type": ty } }), Some(body))
    }

    #[test]
    fn clusters_equivalent_functions_into_classes() {
        let Some(s) = solver() else { return };
        let h = |c: char| format!("fn_{}", c.to_string().repeat(64));
        let items = [
            item(&h('a'), num_to_num(), lam("n", bap("add", json!([v("n"), v("n")])))), // 2n
            item(&h('b'), num_to_num(), lam("m", bap("mul", json!([int(2), v("m")])))), // 2m
            item(&h('c'), num_to_num(), lam("n", bap("mul", json!([int(3), v("n")])))), // 3n (distinct)
            item(&h('d'), list_to_list(), lam("xs", bap("reverse", json!([bap("reverse", json!([v("xs")]))])))), // rev∘rev
            item(&h('e'), list_to_list(), lam("ys", v("ys"))), // identity
        ];
        let classes = cluster(&items, s);
        // Find each hash's class membership.
        let class_of = |x: &str| classes.iter().find(|c| c.contains(&x.to_string())).unwrap().clone();
        assert_eq!(class_of(&h('a')), class_of(&h('b')), "double-via-add ≡ double-via-mul");
        assert_eq!(class_of(&h('d')), class_of(&h('e')), "reverse∘reverse ≡ identity");
        assert_ne!(class_of(&h('a')), class_of(&h('c')), "triple is a distinct class");
        // Three classes: {a,b}, {c}, {d,e}.
        assert_eq!(classes.len(), 3, "{classes:?}");
        // Canonical rep is the smallest address in the class.
        let ab = class_of(&h('a'));
        assert_eq!(ab.first().unwrap(), &h('a'));
    }

    #[test]
    fn different_shapes_are_never_merged() {
        let Some(s) = solver() else { return };
        // A num→num and a List→List function never compare (different shape) — two singletons.
        let items = [
            item("fn_x", num_to_num(), lam("n", v("n"))),
            item("fn_y", list_to_list(), lam("xs", v("xs"))),
        ];
        let classes = cluster(&items, s);
        assert_eq!(classes.len(), 2);
    }
}
