//! Delegation-chain verification — the capability verifier the trust model (`spec/trust-model.md`)
//! calls for. Authorization in Novae Linguae is by **possession of a signed capability**, not by
//! identity, and there is **no central authority** (principles 6, 7): a receiver authorizes an action
//! requiring `cap:X` only if the presenter can exhibit a chain of signed `delegate` tokens back to a
//! root the receiver recognizes *per its own local trust policy*.
//!
//! This module verifies that chain. A `delegate` message (spec/message.v0.2.schema.json) is a signed
//! envelope where `from` is the granter, `to` is the grantee (`null` = bearer), and the body carries
//! the `capability`, an optional `expires_at`, and optional `conditions`. Given a pool of such tokens,
//! a set of recognized root DIDs, and the presenter + required capability, [`verify_delegation_chain`]
//! finds a valid chain or explains why none exists. It checks four things, exactly as the trust model
//! requires of a receiver:
//!
//! - **Signatures** — every token on the chain verifies against its granter's `did:nova` key.
//! - **Chain to a root** — the chain terminates at a DID in the recognized-roots set (a root is
//!   self-authorizing for any capability — that is what "recognized" means).
//! - **Attenuation** — each granter must itself hold a capability covering what it delegated, so no
//!   one can grant broader than they hold (`cap:fs/read/home` covers `cap:fs/read/home/projects`, not
//!   the reverse). Enforced structurally by recursing on the *delegated* capability.
//! - **Expiry** — a token whose `expires_at` precedes the verification instant `at` is skipped.
//!
//! Conditions are free-text policy hooks (e.g. "valid only below /home/projects"); this verifier
//! cannot evaluate arbitrary conditions, so it **collects** every condition along the chain into the
//! verdict for the caller's policy layer to enforce, rather than silently accepting or rejecting them.

use serde_json::Value as J;
use std::collections::{BTreeSet, HashMap};

/// One signed delegation step on a verified chain, ordered root-most first.
#[derive(Debug, Clone, PartialEq)]
pub struct ChainLink {
    pub granter: String,
    /// The grantee DID, or `None` for a bearer token (`to: null`) that authorizes any presenter.
    pub grantee: Option<String>,
    pub capability: String,
    pub expires_at: Option<String>,
    pub conditions: Vec<String>,
    /// The delegate message's content-address (`hash`), when present.
    pub message_hash: Option<String>,
}

/// The outcome of [`verify_delegation_chain`].
#[derive(Debug, Clone)]
pub struct ChainVerdict {
    pub authorized: bool,
    pub reason: String,
    /// The verified chain, root-most first, grantee-most last. Empty when the presenter is itself a
    /// recognized root (self-authorizing, no delegation needed).
    pub chain: Vec<ChainLink>,
    /// Every `condition` carried by a link on the chain — for the caller's policy layer to enforce.
    pub conditions: Vec<String>,
}

/// Does `broader` cover `narrower`? Capabilities are `cap:`-prefixed, `/`-segmented paths; a capability
/// covers another iff its segments are a (non-strict) prefix — so `cap:fs/read` covers `cap:fs/read`
/// and `cap:fs/read/home`, but not `cap:fs/write` or the broader `cap:fs`.
pub fn capability_covers(broader: &str, narrower: &str) -> bool {
    let strip = |c: &str| c.strip_prefix("cap:").unwrap_or(c).to_string();
    let b = strip(broader);
    let n = strip(narrower);
    let bseg: Vec<&str> = b.split('/').collect();
    let nseg: Vec<&str> = n.split('/').collect();
    nseg.len() >= bseg.len() && nseg[..bseg.len()] == bseg[..]
}

/// `true` if `expires_at` precedes the verification instant `at`. Both are compared lexicographically,
/// which is chronological for RFC 3339 timestamps normalized to UTC (`Z`) at the same precision — the
/// form the schema's `date-time` produces. With no clock (`at: None`) or no expiry, the token lives.
fn is_expired(expires_at: Option<&str>, at: Option<&str>) -> bool {
    matches!((expires_at, at), (Some(exp), Some(now)) if now > exp)
}

fn link_capability(d: &J) -> Option<&str> {
    d.pointer("/body/capability").and_then(|c| c.as_str())
}

/// Verify that `grantee` is authorized to wield `required`, by exhibiting a signed delegation chain
/// from a recognized `root` down to `grantee`. Returns a [`ChainVerdict`] that is either authorized
/// (with the verified chain and accumulated conditions) or not (with a reason).
pub fn verify_delegation_chain(
    required: &str,
    grantee: &str,
    delegations: &[J],
    roots: &BTreeSet<String>,
    at: Option<&str>,
) -> ChainVerdict {
    // Memoize per (capability, holder): authorization is a property of that pair alone, independent of
    // who asked — so a result, once computed, is reusable, and an in-progress entry signals a cycle.
    #[derive(Clone)]
    enum State {
        InProgress,
        Done(Option<Vec<ChainLink>>),
    }
    let mut memo: HashMap<(String, String), State> = HashMap::new();

    fn walk(
        required: &str,
        holder: &str,
        delegations: &[J],
        roots: &BTreeSet<String>,
        at: Option<&str>,
        memo: &mut HashMap<(String, String), State>,
    ) -> Option<Vec<ChainLink>> {
        if roots.contains(holder) {
            return Some(vec![]); // a recognized root is self-authorizing for any capability
        }
        let key = (required.to_string(), holder.to_string());
        match memo.get(&key) {
            Some(State::Done(r)) => return r.clone(),
            Some(State::InProgress) => return None, // cycle: this holder is already on the stack
            None => {}
        }
        memo.insert(key.clone(), State::InProgress);

        let mut found = None;
        for d in delegations {
            let Some(granter) = d.get("from").and_then(|f| f.as_str()) else { continue };
            let Some(cap) = link_capability(d) else { continue };
            // Grantee match: an explicit `to` must equal the holder; `to: null` is a bearer token that
            // matches any holder.
            let to = d.get("to").and_then(|t| t.as_str());
            let matches = match to {
                Some(t) => t == holder,
                None => true,
            };
            if !matches {
                continue;
            }
            if !capability_covers(cap, required) {
                continue;
            }
            let expires = d.pointer("/body/expires_at").and_then(|e| e.as_str());
            if is_expired(expires, at) {
                continue;
            }
            // The token must be authentically signed by its granter.
            if crate::verify_signature(d).is_err() {
                continue;
            }
            // Attenuation: the granter must itself hold a capability covering what it delegated. Recurse
            // on `cap` (not `required`) so no link can widen the grant beyond its own authority.
            if let Some(mut sub) = walk(cap, granter, delegations, roots, at, memo) {
                let conditions = d
                    .pointer("/body/conditions")
                    .and_then(|c| c.as_array())
                    .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                    .unwrap_or_default();
                sub.push(ChainLink {
                    granter: granter.to_string(),
                    grantee: to.map(String::from),
                    capability: cap.to_string(),
                    expires_at: expires.map(String::from),
                    conditions,
                    message_hash: d.get("hash").and_then(|h| h.as_str()).map(String::from),
                });
                found = Some(sub);
                break;
            }
        }
        memo.insert(key, State::Done(found.clone()));
        found
    }

    match walk(required, grantee, delegations, roots, at, &mut memo) {
        Some(chain) => {
            let conditions: Vec<String> =
                chain.iter().flat_map(|l| l.conditions.iter().cloned()).collect();
            let reason = if chain.is_empty() {
                format!("`{grantee}` is itself a recognized root")
            } else {
                let root = &chain[0].granter;
                format!(
                    "valid chain of {} delegation(s) from recognized root `{root}` to `{grantee}` for `{required}`",
                    chain.len()
                )
            };
            ChainVerdict { authorized: true, reason, chain, conditions }
        }
        None => ChainVerdict {
            authorized: false,
            reason: format!(
                "no signed delegation chain authorizes `{grantee}` to wield `{required}` back to a recognized root"
            ),
            chain: vec![],
            conditions: vec![],
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{did_nova_from_pubkey, signing_key_from_seed};
    use serde_json::json;

    fn did(seed: &str) -> String {
        did_nova_from_pubkey(&signing_key_from_seed(seed).verifying_key())
    }

    /// Build a signed `delegate` token: `granter_seed`'s identity grants `capability` to `to`
    /// (`None` = bearer), with optional expiry/conditions.
    fn delegate(
        granter_seed: &str,
        to: Option<&str>,
        capability: &str,
        expires_at: Option<&str>,
        conditions: &[&str],
    ) -> J {
        let key = signing_key_from_seed(granter_seed);
        let mut env = json!({
            "schema_version": "0.2.0",
            "kind": "delegate",
            "to": to,
            "in_reply_to": null,
            "body": {
                "capability": capability,
                "expires_at": expires_at,
                "conditions": conditions,
            }
        });
        crate::sign_message(&mut env, &key).unwrap();
        env
    }

    #[test]
    fn capability_covering_is_prefix_on_segments() {
        assert!(capability_covers("cap:fs/read", "cap:fs/read"));
        assert!(capability_covers("cap:fs/read", "cap:fs/read/home"));
        assert!(capability_covers("cap:fs/read/home", "cap:fs/read/home/projects"));
        assert!(!capability_covers("cap:fs/read/home", "cap:fs/read"));
        assert!(!capability_covers("cap:fs/read", "cap:fs/write"));
        assert!(!capability_covers("cap:fs/reading", "cap:fs/read")); // segment-wise, not substring
    }

    #[test]
    fn direct_grant_from_root_authorizes() {
        let (root, alice) = (did("root"), did("alice"));
        let roots = BTreeSet::from([root.clone()]);
        let tokens = vec![delegate("root", Some(&alice), "cap:apply/double", None, &[])];
        let v = verify_delegation_chain("cap:apply/double", &alice, &tokens, &roots, None);
        assert!(v.authorized, "{}", v.reason);
        assert_eq!(v.chain.len(), 1);
        assert_eq!(v.chain[0].granter, root);
    }

    #[test]
    fn multi_hop_attenuated_chain_authorizes_covered_action() {
        // root → alice (cap:fs/read/home) → bob (cap:fs/read/home/projects), narrowing each hop.
        let (root, alice, bob) = (did("root"), did("alice"), did("bob"));
        let roots = BTreeSet::from([root.clone()]);
        let tokens = vec![
            delegate("root", Some(&alice), "cap:fs/read/home", None, &[]),
            delegate("alice", Some(&bob), "cap:fs/read/home/projects", None, &["below /home/projects"]),
        ];
        // bob may read a path the leaf covers.
        let v = verify_delegation_chain("cap:fs/read/home/projects/x", &bob, &tokens, &roots, None);
        assert!(v.authorized, "{}", v.reason);
        assert_eq!(v.chain.len(), 2);
        assert_eq!(v.chain[0].granter, root); // root-most first
        assert_eq!(v.conditions, vec!["below /home/projects".to_string()]);
    }

    #[test]
    fn over_broad_redelegation_is_rejected() {
        // alice holds only cap:fs/read/home/projects but tries to grant bob the broader cap:fs/read.
        let (root, alice, bob) = (did("root"), did("alice"), did("bob"));
        let roots = BTreeSet::from([root]);
        let tokens = vec![
            delegate("root", Some(&alice), "cap:fs/read/home/projects", None, &[]),
            delegate("alice", Some(&bob), "cap:fs/read", None, &[]),
        ];
        let v = verify_delegation_chain("cap:fs/read", &bob, &tokens, &roots, None);
        assert!(!v.authorized, "attenuation must forbid widening a grant");
    }

    #[test]
    fn chain_not_reaching_a_recognized_root_is_rejected() {
        // alice grants bob, but alice's own grant traces to `stranger`, not a recognized root.
        let (alice, bob) = (did("alice"), did("bob"));
        let roots = BTreeSet::from([did("root")]);
        let tokens = vec![
            delegate("stranger", Some(&alice), "cap:apply/double", None, &[]),
            delegate("alice", Some(&bob), "cap:apply/double", None, &[]),
        ];
        let v = verify_delegation_chain("cap:apply/double", &bob, &tokens, &roots, None);
        assert!(!v.authorized);
    }

    #[test]
    fn tampered_token_signature_fails() {
        let (root, alice) = (did("root"), did("alice"));
        let roots = BTreeSet::from([root]);
        let mut tok = delegate("root", Some(&alice), "cap:apply/double", None, &[]);
        // Tamper with the granted capability after signing.
        tok["body"]["capability"] = json!("cap:apply/everything");
        let v = verify_delegation_chain("cap:apply/everything", &alice, &[tok], &roots, None);
        assert!(!v.authorized, "a token whose body was altered after signing must not verify");
    }

    #[test]
    fn expired_token_is_skipped() {
        let (root, alice) = (did("root"), did("alice"));
        let roots = BTreeSet::from([root]);
        let tokens = vec![delegate("root", Some(&alice), "cap:apply/double", Some("2020-01-01T00:00:00Z"), &[])];
        // Verifying "now" (a later instant) finds the token expired.
        let v = verify_delegation_chain("cap:apply/double", &alice, &tokens, &roots, Some("2026-06-08T00:00:00Z"));
        assert!(!v.authorized, "an expired token must not authorize");
        // Verifying at an instant before expiry, it holds.
        let v2 = verify_delegation_chain("cap:apply/double", &alice, &tokens, &roots, Some("2019-06-01T00:00:00Z"));
        assert!(v2.authorized, "{}", v2.reason);
    }

    #[test]
    fn bearer_token_authorizes_any_presenter() {
        let root = did("root");
        let roots = BTreeSet::from([root]);
        let tokens = vec![delegate("root", None, "cap:apply/double", None, &[])]; // to: null
        let v = verify_delegation_chain("cap:apply/double", &did("anyone"), &tokens, &roots, None);
        assert!(v.authorized, "a bearer token authorizes whoever presents it");
    }

    #[test]
    fn cyclic_delegations_terminate_unauthorized() {
        // alice ↔ bob delegate to each other, neither rooted: must terminate, not loop.
        let (alice, bob) = (did("alice"), did("bob"));
        let roots = BTreeSet::from([did("root")]);
        let tokens = vec![
            delegate("alice", Some(&bob), "cap:apply/double", None, &[]),
            delegate("bob", Some(&alice), "cap:apply/double", None, &[]),
        ];
        let v = verify_delegation_chain("cap:apply/double", &bob, &tokens, &roots, None);
        assert!(!v.authorized);
    }
}
