//! Tree-walking evaluator for the Nova Lingua body-expression AST
//! (spec/body-expression.schema.json). This is the language's missing semantic core: it **executes**
//! a body. Given a function record's body and an example's arguments it computes a result, so the
//! worked `examples[]` become runnable tests rather than unchecked assertions, and `properties[]`
//! that reference `map`/`filter`/`fold`/`compose` can be verified by *running* rather than deferred.
//!
//! Values are the value-expression AST (spec/value-expression.schema.json). The evaluator is a
//! call-by-value lambda calculus with: lexical closures, currying / partial application, `let`,
//! `case` over the four pattern kinds, record field projection, and a small total builtin library
//! (arithmetic, comparison, booleans, lists, and the higher-order `map`/`filter`/`foldl`/`foldr`/
//! `compose`/`apply`). `if` is absent by design — it is `case` on a `bool` (principle 8).
//!
//! Scope: this is a reference evaluator for clarity, matching the v0.1 body schema. Integers are
//! i128 (the big-int string form is accepted but must fit); `int` and `nat` share the `Int`
//! representation (a `nat` is a non-negative `int`), so example checking compares values, not kind
//! tags. `field`/record and `tuple` are supported. Effects are not modelled — bodies that would
//! perform I/O are out of scope for this pure evaluator.

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine;
use serde_json::{json, Value as J};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

type Env = BTreeMap<String, Val>;

// Composition / linking: a scoped address → body-AST map. When set, applying a `fn_ref` value resolves
// the target (a function content-address, or a body's own `expr_` address) to its body and runs it, so
// records compose end-to-end — e.g. running map's example `map(<fn_ref double>, [1,2,3])` executes the
// referenced `double` (commons/linking, principle 4: assemble from existing records). Thread-local so
// the evaluator's signatures stay unchanged; set for the duration of a linked run, then cleared.
thread_local! {
    static RESOLVER: RefCell<HashMap<String, J>> = RefCell::new(HashMap::new());
}

/// Install the link map (address → body AST) for `fn_ref` resolution during evaluation.
pub fn set_resolver(map: HashMap<String, J>) {
    RESOLVER.with(|r| *r.borrow_mut() = map);
}

/// Clear the link map.
pub fn clear_resolver() {
    RESOLVER.with(|r| r.borrow_mut().clear());
}

fn resolve_fn_ref(addr: &str) -> Option<J> {
    RESOLVER.with(|r| r.borrow().get(addr).cloned())
}

/// Look up any artifact the installed link map carries by content-address (bodies AND trace
/// artifacts — `build_link_map` indexes both). Public so `run_examples` can resolve an example's
/// `trace` reference through the same map the run already installed.
pub fn resolver_lookup(addr: &str) -> Option<J> {
    RESOLVER.with(|r| r.borrow().get(addr).cloned())
}

// Effect enforcement: a scoped capability sandbox. Effectful builtins (`print` → io.console, `rand`
// → random) gate on a *granted* effect set and append to a structured trace — so a body may only
// perform effects its grant permits (e.g. a function record's declared `signature.effects`), and the
// trace is a replayable record of what it did (principles 5 + 9). Pure bodies touch none of this.
thread_local! {
    static EFFECTS: RefCell<EffectState> = RefCell::new(EffectState::new());
}

struct EffectState {
    granted: std::collections::HashSet<String>,
    trace: Vec<J>,
    /// A recorded trace to replay: effectful builtins consume their recorded result in order instead
    /// of performing real I/O, so a run reproduces deterministically (principle 5). `None` = live.
    replay: Option<std::collections::VecDeque<J>>,
    rng: u64,
    /// Operator-supplied named secrets. A `{{secret:NAME}}` placeholder in an `http` header value is
    /// substituted from here at the effect boundary — the secret value never exists as a language
    /// value, never enters a record, and never enters the trace (the trace keeps the placeholder).
    secrets: std::collections::HashMap<String, String>,
    /// Operator-supplied OAuth2 client-credentials identities (GW13). A `{{oauth:NAME}}` placeholder
    /// in an `http` header value resolves to a live access token: fetched from the identity's token
    /// endpoint with its client credentials INSIDE the live effect boundary, cached per evaluation.
    /// Like a secret, the token never exists as a language value, never enters a record, and never
    /// enters the trace — replay needs no identity at all.
    oauth: std::collections::HashMap<String, OAuthConfig>,
    /// Per-evaluation token cache: one token fetch per identity per evaluation.
    oauth_tokens: std::collections::HashMap<String, String>,
}

/// An OAuth2 **client-credentials** identity (`--oauth NAME=token_url|client_id|client_secret`).
/// The one grant type that is pure machine-to-machine: no browser, no user, no redirect — the
/// token endpoint exchanges the client credentials for a bearer token directly, which is why it is
/// the only OAuth2 flow inside the effect-boundary credentials doctrine (the others need an
/// interactive principal the boundary cannot supply).
#[derive(Clone)]
pub struct OAuthConfig {
    pub token_url: String,
    pub client_id: String,
    pub client_secret: String,
}

impl EffectState {
    fn new() -> Self {
        EffectState {
            granted: std::collections::HashSet::new(),
            trace: Vec::new(),
            replay: None,
            rng: 0x1234_5678_9abc_def0,
            secrets: std::collections::HashMap::new(),
            oauth: std::collections::HashMap::new(),
            oauth_tokens: std::collections::HashMap::new(),
        }
    }
}

/// Install the granted effect set for the next evaluation; resets the trace and the effect PRNG so a
/// run is reproducible. An effectful builtin not in this set is rejected at eval time.
pub fn set_effect_grants<I: IntoIterator<Item = String>>(granted: I) {
    EFFECTS.with(|e| {
        let mut e = e.borrow_mut();
        e.granted = granted.into_iter().collect();
        e.trace.clear();
        e.rng = 0x1234_5678_9abc_def0;
    });
}

/// The currently installed effect-grant set (operator-declared; empty = pure-only). Lets the
/// responder's static effect gate report *which* effects a target needs beyond what the operator
/// granted, before the sandbox would reject the run anyway.
pub fn current_effect_grants() -> std::collections::BTreeSet<String> {
    EFFECTS.with(|e| e.borrow().granted.iter().cloned().collect())
}

/// Install a recorded effect trace to REPLAY: effectful builtins return their recorded results in
/// order without performing real I/O, so an effectful run reproduces deterministically (principle 5;
/// the trace is sufficient to re-run — principle 9). `entries` is the trace from a prior live run.
pub fn set_effect_replay(entries: Vec<J>) {
    EFFECTS.with(|e| e.borrow_mut().replay = Some(entries.into_iter().collect()));
}

/// Leave replay mode (back to live). Pairs with [`set_effect_replay`] so a caller that installed a
/// recorded trace for one evaluation (e.g. verifying an `observed` claim) doesn't leave the thread
/// replaying into the next.
pub fn clear_effect_replay() {
    EFFECTS.with(|e| e.borrow_mut().replay = None);
}

/// How many recorded entries the installed replay trace still holds (`None` = not in replay mode).
/// A verifier uses this to require that a claim's computation consumed its trace EXACTLY — leftover
/// entries mean the trace does not correspond to this computation.
pub fn effect_replay_remaining() -> Option<usize> {
    EFFECTS.with(|e| e.borrow().replay.as_ref().map(|q| q.len()))
}

/// Install the operator's named secrets (`--secret NAME=VALUE`). Credentials are effect-boundary
/// configuration, not data: an `http` header value may carry a `{{secret:NAME}}` placeholder, and
/// the substitution happens only inside the live effect — symbolic form in the trace, real value on
/// the wire.
pub fn set_effect_secrets<I: IntoIterator<Item = (String, String)>>(secrets: I) {
    EFFECTS.with(|e| e.borrow_mut().secrets = secrets.into_iter().collect());
}

/// Install the operator's OAuth2 client-credentials identities (`--oauth NAME=token_url|id|secret`).
/// Clears the per-evaluation token cache — a fresh evaluation authenticates afresh.
pub fn set_effect_oauth<I: IntoIterator<Item = (String, OAuthConfig)>>(identities: I) {
    EFFECTS.with(|e| {
        let mut e = e.borrow_mut();
        e.oauth = identities.into_iter().collect();
        e.oauth_tokens.clear();
    });
}

/// Resolve a `{{oauth:NAME}}` placeholder to a live access token: the cached one, or a fresh
/// client-credentials exchange against the identity's token endpoint (RFC 6749 §4.4 —
/// `grant_type=client_credentials`, form-encoded, `access_token` out of the JSON response).
/// The token-endpoint round-trip is credential MACHINERY, not body semantics: it happens only
/// inside a live effect (replay never gets here), it is not a traced observation (exactly like
/// the TLS handshake under a net effect), and the operator supplied the endpoint explicitly —
/// `--oauth` IS the opt-in for that fetch.
fn oauth_token(name: &str) -> Result<String> {
    // Short borrows only: the token fetch must not hold the sandbox RefCell across real I/O.
    let cached = EFFECTS.with(|e| e.borrow().oauth_tokens.get(name).cloned());
    if let Some(tok) = cached {
        return Ok(tok);
    }
    let cfg = EFFECTS.with(|e| e.borrow().oauth.get(name).cloned());
    let Some(cfg) = cfg else {
        bail!("oauth identity `{name}` is not supplied (pass --oauth {name}=token_url|client_id|client_secret)");
    };
    let form = format!(
        "grant_type=client_credentials&client_id={}&client_secret={}",
        pct_encode(&cfg.client_id),
        pct_encode(&cfg.client_secret)
    );
    let headers = vec![("Content-Type".to_string(), "application/x-www-form-urlencoded".to_string())];
    let (status, body) = http_roundtrip("POST", &cfg.token_url, &headers, Some(&form))
        .with_context(|| format!("oauth token exchange for `{name}` at {}", cfg.token_url))?;
    if status != 200 {
        bail!("oauth token endpoint for `{name}` answered {status}: {}", &body[..body.len().min(200)]);
    }
    let parsed: J = serde_json::from_str(&body)
        .map_err(|e| anyhow!("oauth token endpoint for `{name}` returned non-JSON: {e}"))?;
    let token = parsed
        .get("access_token")
        .and_then(|t| t.as_str())
        .ok_or_else(|| anyhow!("oauth token response for `{name}` has no string `access_token`"))?
        .to_string();
    EFFECTS.with(|e| {
        e.borrow_mut().oauth_tokens.insert(name.to_string(), token.clone());
    });
    Ok(token)
}

/// Substitute every `{{oauth:NAME}}` placeholder in `value` with a live access token (see
/// [`oauth_token`]). Same honesty as secrets: an unsupplied identity is refused by name.
fn substitute_oauth(value: &str) -> Result<String> {
    const MARK: &str = "{{oauth:";
    if !value.contains(MARK) {
        return Ok(value.to_string());
    }
    let mut out = String::new();
    let mut rest = value;
    while let Some(start) = rest.find(MARK) {
        out.push_str(&rest[..start]);
        let after = &rest[start + MARK.len()..];
        let Some(end) = after.find("}}") else {
            bail!("unterminated {{{{oauth:...}}}} placeholder in header value");
        };
        out.push_str(&oauth_token(&after[..end])?);
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

/// Substitute every `{{secret:NAME}}` placeholder in `value` from the installed secrets. A
/// placeholder naming a secret the operator did not supply is an error (the honest refusal —
/// sending the placeholder text as a credential would be a silent auth failure).
fn substitute_secrets(value: &str) -> Result<String> {
    if !value.contains("{{secret:") {
        return Ok(value.to_string());
    }
    EFFECTS.with(|e| {
        let e = e.borrow();
        let mut out = String::new();
        let mut rest = value;
        while let Some(start) = rest.find("{{secret:") {
            out.push_str(&rest[..start]);
            let after = &rest[start + "{{secret:".len()..];
            let Some(end) = after.find("}}") else {
                bail!("unterminated {{{{secret:...}}}} placeholder in header value");
            };
            let name = &after[..end];
            match e.secrets.get(name) {
                Some(v) => out.push_str(v),
                None => bail!("secret `{name}` is not supplied (pass --secret {name}=...)"),
            }
            rest = &after[end + 2..];
        }
        out.push_str(rest);
        Ok(out)
    })
}

/// Reset the effect sandbox (empty grant, empty trace, live mode).
pub fn clear_effects() {
    EFFECTS.with(|e| *e.borrow_mut() = EffectState::new());
}

/// Drain the structured effect trace collected during evaluation (principle 9: AI-ingestible trace).
/// Each entry is `{effect, detail, result}` — `result` is what the builtin returned, enabling replay.
pub fn take_effect_trace() -> Vec<J> {
    EFFECTS.with(|e| std::mem::take(&mut e.borrow_mut().trace))
}

/// Run an effectful operation. In **replay** mode it returns the next recorded result for `effect`
/// (no real I/O — deterministic re-execution); otherwise it gates on the granted set (enforcement),
/// runs `live` (the real side effect), records `{effect, detail, result}`, and returns the result.
/// The sandbox borrow is released before `live` runs, so `live` may itself touch the sandbox (e.g.
/// the rand PRNG).
fn effect_op(effect: &str, detail: J, live: impl FnOnce() -> Result<Val>) -> Result<Val> {
    effect_op_at(effect, None, detail, live)
}

/// Whether one grant string permits `effect` at `scope`. A grant is `effect[@scope-prefix]`:
/// the bare form permits the effect anywhere; a scoped form permits it only where the runtime
/// scope extends the grant's scope SEGMENT-ALIGNED (`net.write@api.example.com` covers
/// `api.example.com/v0/things` but `net.write@api.example.com/v0` does not cover
/// `api.example.com/v0things`). The runtime scope is `host[/path]` for net effects and the file
/// path for fs effects, so one rule carries host-scoped, path-scoped, and fs-path-scoped grants.
fn grant_permits(grant: &str, effect: &str, scope: Option<&str>) -> bool {
    match grant.split_once('@') {
        None => grant == effect,
        Some((base, pat)) => {
            let pat = pat.strip_suffix('/').unwrap_or(pat);
            base == effect
                && scope.is_some_and(|s| {
                    s == pat || (s.starts_with(pat) && s.as_bytes().get(pat.len()) == Some(&b'/'))
                })
        }
    }
}

/// `effect_op` with an optional SCOPE (the URL `host/path` of a net effect, or the file path of an
/// fs effect). A scoped effect is permitted by the bare grant (`net.write` — anywhere) or by a
/// grant whose scope prefix matches segment-aligned (`net.write@api.example.com` — any path on
/// that host; `net.write@api.example.com/v0/things` — only under that path; `fs.read@/data` —
/// only under that directory). A scoped grant alone does NOT satisfy a different host or path.
fn effect_op_at(effect: &str, scope: Option<&str>, detail: J, live: impl FnOnce() -> Result<Val>) -> Result<Val> {
    enum Mode {
        Replay(J),
        ReplayExhausted,
        Live,
    }
    let mode = EFFECTS.with(|e| -> Result<Mode> {
        let mut e = e.borrow_mut();
        if let Some(q) = e.replay.as_mut() {
            return Ok(q.pop_front().map_or(Mode::ReplayExhausted, Mode::Replay));
        }
        let allowed = e.granted.iter().any(|g| grant_permits(g, effect, scope));
        if !allowed {
            match scope {
                Some(s) => bail!("ungranted effect `{effect}` at `{s}`: the body performed it, but no grant covers it — a grant is `{effect}` (anywhere), `{effect}@host` (net: any path on the host), or a scope prefix like `{effect}@{s}` (pass --grant …)"),
                None => bail!("ungranted effect `{effect}`: the body performed it, but it is not in the granted capability set (declare it in signature.effects or pass --grant {effect})"),
            }
        }
        Ok(Mode::Live)
    })?;
    match mode {
        Mode::Replay(entry) => {
            let recorded = entry.get("effect").and_then(|x| x.as_str()).unwrap_or_default();
            if recorded != effect {
                bail!("replay mismatch: body performed `{effect}` but the trace recorded `{recorded}`");
            }
            match entry.get("result") {
                Some(r) => decode_value(r),
                None => Ok(Val::Unit),
            }
        }
        Mode::ReplayExhausted => bail!("replay log exhausted while performing effect `{effect}`"),
        Mode::Live => {
            let result = live()?;
            EFFECTS.with(|e| {
                e.borrow_mut().trace.push(json!({ "effect": effect, "detail": detail, "result": encode_value(&result) }));
            });
            Ok(result)
        }
    }
}

/// Deterministic per-evaluation PRNG draw in `[0, bound)` for the `rand` effect.
fn effect_rand(bound: i128) -> Result<i128> {
    if bound <= 0 {
        bail!("rand bound must be positive, got {bound}");
    }
    let r = EFFECTS.with(|e| {
        let mut e = e.borrow_mut();
        let mut x = e.rng | 1;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        e.rng = x;
        x.wrapping_mul(0x2545f4914f6cdd1d)
    });
    Ok((r % bound as u64) as i128)
}

/// A runtime value. Mirrors the value-expression kinds, plus the two callable forms (`Closure`,
/// `Builtin`) that only exist at runtime.
#[derive(Clone, Debug)]
pub enum Val {
    Bool(bool),
    Int(i128),
    Float(f64),
    Str(String),
    Bytes(Vec<u8>),
    Unit,
    List(Vec<Val>),
    Tuple(Vec<Val>),
    Record(BTreeMap<String, Val>),
    Variant(String, Option<Box<Val>>),
    /// A finite map with string keys (`Map string a`, spec/expressiveness.md phase 2). BTreeMap
    /// gives deterministic (sorted-by-key) iteration and encoding — canonical form for free.
    Map(BTreeMap<String, Val>),
    FnRef(String),
    Closure { params: Vec<String>, body: Rc<J>, env: Env },
    /// A self-recursive function value: like `Closure`, but every application first re-binds
    /// `self_name` to the *whole* function in the environment, so the body can call back into it.
    /// Binding the full function (never a partially-applied remainder) keeps `self` correct even
    /// when the recursive function is itself partially applied.
    RecClosure { self_name: String, params: Vec<String>, body: Rc<J>, env: Env },
    Builtin { name: String, arity: usize, applied: Vec<Val> },
}

// ---------------------------------------------------------------------------
// Value (de)serialization: value-expression AST <-> Val.
// ---------------------------------------------------------------------------

fn parse_int(v: &J) -> Result<i128> {
    if let Some(i) = v.as_i64() {
        return Ok(i as i128);
    }
    if let Some(u) = v.as_u64() {
        return Ok(u as i128);
    }
    if let Some(s) = v.as_str() {
        return s.parse::<i128>().map_err(|e| anyhow!("integer literal {s:?}: {e}"));
    }
    bail!("not an integer literal: {v}")
}

/// Decode a value-expression AST node into a runtime `Val`.
pub fn decode_value(v: &J) -> Result<Val> {
    let kind = v.get("kind").and_then(|k| k.as_str()).ok_or_else(|| anyhow!("value missing kind: {v}"))?;
    Ok(match kind {
        "bool" => Val::Bool(v["value"].as_bool().ok_or_else(|| anyhow!("bool value"))?),
        "int" | "nat" => Val::Int(parse_int(&v["value"])?),
        "float" => Val::Float(v["value"].as_f64().ok_or_else(|| anyhow!("float value"))?),
        "string" => Val::Str(v["value"].as_str().ok_or_else(|| anyhow!("string value"))?.to_string()),
        "bytes" => {
            let s = v["value"].as_str().ok_or_else(|| anyhow!("bytes value"))?;
            Val::Bytes(base64::engine::general_purpose::STANDARD.decode(s).map_err(|e| anyhow!("base64: {e}"))?)
        }
        "unit" => Val::Unit,
        "list" => Val::List(decode_seq(&v["elems"])?),
        "tuple" => Val::Tuple(decode_seq(&v["elems"])?),
        "record" => {
            let mut m = BTreeMap::new();
            for f in v["fields"].as_array().ok_or_else(|| anyhow!("record fields"))? {
                let name = f["name"].as_str().ok_or_else(|| anyhow!("field name"))?.to_string();
                m.insert(name, decode_value(&f["value"])?);
            }
            Val::Record(m)
        }
        "variant" => {
            let tag = v["tag"].as_str().ok_or_else(|| anyhow!("variant tag"))?.to_string();
            let payload = match v.get("payload") {
                Some(p) => Some(Box::new(decode_value(p)?)),
                None => None,
            };
            Val::Variant(tag, payload)
        }
        "fn_ref" => Val::FnRef(v["target"].as_str().ok_or_else(|| anyhow!("fn_ref target"))?.to_string()),
        "map" => {
            let mut m = BTreeMap::new();
            for e in v["entries"].as_array().ok_or_else(|| anyhow!("map entries"))? {
                let key = e["key"].as_str().ok_or_else(|| anyhow!("map key"))?.to_string();
                if m.insert(key.clone(), decode_value(&e["value"])?).is_some() {
                    bail!("map key {key:?} appears more than once");
                }
            }
            Val::Map(m)
        }
        other => bail!("unknown value kind: {other}"),
    })
}

fn decode_seq(v: &J) -> Result<Vec<Val>> {
    v.as_array().ok_or_else(|| anyhow!("expected an array of values"))?.iter().map(decode_value).collect()
}

/// Encode a runtime `Val` back into a value-expression AST node (for `eval`'s output). Integers are
/// emitted as `int`; callables and `fn_ref` are emitted in an informational form.
pub fn encode_value(v: &Val) -> J {
    match v {
        Val::Bool(b) => json!({ "kind": "bool", "value": b }),
        Val::Int(i) => {
            // JSON numbers are exact only below 2^53 (spec/canonical-serialization.md); stringify above.
            if i.unsigned_abs() < (1u128 << 53) {
                json!({ "kind": "int", "value": *i as i64 })
            } else {
                json!({ "kind": "int", "value": i.to_string() })
            }
        }
        Val::Float(f) => json!({ "kind": "float", "value": f }),
        Val::Str(s) => json!({ "kind": "string", "value": s }),
        Val::Bytes(b) => json!({ "kind": "bytes", "value": base64::engine::general_purpose::STANDARD.encode(b) }),
        Val::Unit => json!({ "kind": "unit" }),
        Val::List(xs) => json!({ "kind": "list", "elems": xs.iter().map(encode_value).collect::<Vec<_>>() }),
        Val::Tuple(xs) => json!({ "kind": "tuple", "elems": xs.iter().map(encode_value).collect::<Vec<_>>() }),
        Val::Record(m) => json!({
            "kind": "record",
            "fields": m.iter().map(|(k, v)| json!({ "name": k, "value": encode_value(v) })).collect::<Vec<_>>()
        }),
        Val::Variant(tag, payload) => match payload {
            Some(p) => json!({ "kind": "variant", "tag": tag, "payload": encode_value(p) }),
            None => json!({ "kind": "variant", "tag": tag }),
        },
        Val::FnRef(t) => json!({ "kind": "fn_ref", "target": t }),
        // BTreeMap iterates sorted by key, so the encoding is canonical by construction.
        Val::Map(m) => json!({
            "kind": "map",
            "entries": m.iter().map(|(k, v)| json!({ "key": k, "value": encode_value(v) })).collect::<Vec<_>>()
        }),
        Val::Closure { params, .. } | Val::RecClosure { params, .. } => {
            json!({ "kind": "function", "params": params.len() })
        }
        Val::Builtin { name, arity, applied } => {
            json!({ "kind": "function", "builtin": name, "remaining": arity - applied.len() })
        }
    }
}

/// Structural equality (the semantics of the `eq` builtin and `lit` patterns).
pub fn val_eq(a: &Val, b: &Val) -> bool {
    match (a, b) {
        (Val::Bool(x), Val::Bool(y)) => x == y,
        (Val::Int(x), Val::Int(y)) => x == y,
        (Val::Float(x), Val::Float(y)) => x == y,
        (Val::Str(x), Val::Str(y)) => x == y,
        (Val::Bytes(x), Val::Bytes(y)) => x == y,
        (Val::Unit, Val::Unit) => true,
        (Val::List(x), Val::List(y)) | (Val::Tuple(x), Val::Tuple(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| val_eq(p, q))
        }
        (Val::Record(x), Val::Record(y)) | (Val::Map(x), Val::Map(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| val_eq(v, w)))
        }
        (Val::Variant(t1, p1), Val::Variant(t2, p2)) => {
            t1 == t2
                && match (p1, p2) {
                    (None, None) => true,
                    (Some(a), Some(b)) => val_eq(a, b),
                    _ => false,
                }
        }
        (Val::FnRef(x), Val::FnRef(y)) => x == y,
        _ => false, // closures/builtins are not comparable
    }
}

// ---------------------------------------------------------------------------
// Evaluation.
// ---------------------------------------------------------------------------

fn as_int(v: &Val) -> Result<i128> {
    match v {
        Val::Int(i) => Ok(*i),
        _ => bail!("expected an integer, got {}", encode_value(v)),
    }
}

/// Coerce an `Int` or `Float` to `f64` (for mixed-numeric arithmetic / comparison).
fn as_f64n(v: &Val) -> Result<f64> {
    match v {
        Val::Int(i) => Ok(*i as f64),
        Val::Float(f) => Ok(*f),
        _ => bail!("expected a number, got {}", encode_value(v)),
    }
}

fn as_str(v: &Val) -> Result<String> {
    match v {
        Val::Str(s) => Ok(s.clone()),
        _ => bail!("expected a string, got {}", encode_value(v)),
    }
}

fn as_str_list(v: &Val) -> Result<Vec<String>> {
    match v {
        Val::List(xs) => xs.iter().map(as_str).collect(),
        _ => bail!("expected a list of strings, got {}", encode_value(v)),
    }
}

/// The host component of an `http://` / `https://` URL (see [`url_scope`] for the full
/// `host[/path]` grant scope built on top of it).
pub(crate) fn url_host(url: &str) -> Result<String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .ok_or_else(|| anyhow!("only http:// and https:// URLs are supported: {url}"))?;
    let authority = rest.split('/').next().unwrap_or(rest);
    let host = authority.rsplit_once(':').map_or(authority, |(h, _)| h);
    Ok(host.to_string())
}

/// The grant SCOPE of a net effect: `host` for a bare-path URL, `host/path` otherwise (query and
/// fragment stripped, port dropped like [`url_host`]). This is what a scoped net grant
/// (`net.write@host[/path/prefix]`) is checked against, segment-aligned (see `grant_permits`).
pub(crate) fn url_scope(url: &str) -> Result<String> {
    let host = url_host(url)?;
    let rest = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://")).unwrap_or(url);
    let path = match rest.split_once('/') {
        Some((_, p)) => p.split(['?', '#']).next().unwrap_or(""),
        None => "",
    };
    let path = path.trim_end_matches('/');
    if path.is_empty() {
        Ok(host)
    } else {
        Ok(format!("{host}/{path}"))
    }
}

/// RFC 3986 strict percent-encoding: unreserved characters (ALPHA / DIGIT / - . _ ~) pass, every
/// other UTF-8 byte becomes %XX uppercase hex. Backs the `url_encode` builtin (GW10 — raw
/// concatenation of caller data into a URL is unsound) and the oauth client-credentials form body.
pub(crate) fn pct_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// A minimal, dependency-free HTTP/1.1 request over a raw TCP socket — `https://` via
/// rustls, `http://` plaintext. Returns `(status, body)`; `extra_headers` are emitted after the
/// standard ones. Backs `http_get`/`http_post` (which drop the status, their historical shape)
/// and the general `http` builtin (which surfaces it as `{status, body}` — the piece a mutating
/// workflow verifies against). The gating + record/replay live in `effect_op`.
pub(crate) fn http_roundtrip(
    method: &str,
    url: &str,
    extra_headers: &[(String, String)],
    body: Option<&str>,
) -> Result<(i128, String)> {
    http_roundtrip_full(method, url, extra_headers, body).map(|(s, _, b)| (s, b))
}

/// The header-preserving client behind `http_full` (GW14): the same round-trip, but the response
/// headers survive decoding as a canonical map — names lowercased, OWS-trimmed values, duplicates
/// comma-joined in arrival order (RFC 7230 §3.2.2's list rule; `Set-Cookie` gets the same treatment,
/// the honest reference-grade choice). Redirects are NOT followed — a 3xx is returned as-is with its
/// `location` header, so redirect-following is in-language code, not builtin machinery.
pub(crate) fn http_roundtrip_full(
    method: &str,
    url: &str,
    extra_headers: &[(String, String)],
    body: Option<&str>,
) -> Result<(i128, BTreeMap<String, String>, String)> {
    use std::net::TcpStream;
    use std::time::Duration;

    // Scheme: https:// goes over TLS (rustls + ring + Mozilla webpki roots); http:// is plaintext.
    let (tls, rest, default_port) = if let Some(r) = url.strip_prefix("https://") {
        (true, r, 443u16)
    } else if let Some(r) = url.strip_prefix("http://") {
        (false, r, 80u16)
    } else {
        bail!("only http:// and https:// URLs are supported: {url}");
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().map_err(|_| anyhow!("bad port in {url}"))?),
        None => (authority, default_port),
    };

    let tcp = TcpStream::connect((host, port)).map_err(|e| anyhow!("connect {host}:{port}: {e}"))?;
    let _ = tcp.set_read_timeout(Some(Duration::from_secs(15)));
    let _ = tcp.set_write_timeout(Some(Duration::from_secs(15)));
    let payload = body.unwrap_or("");
    let mut extras = String::new();
    for (name, value) in extra_headers {
        if name.contains(['\r', '\n', ':']) || value.contains(['\r', '\n']) {
            bail!("invalid header {name:?} (control characters / colon in name are not allowed)");
        }
        extras.push_str(&format!("{name}: {value}\r\n"));
    }
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: nl-validator\r\nAccept: */*\r\nConnection: close\r\n{extras}Content-Length: {}\r\n\r\n{payload}",
        payload.len()
    );

    let raw = if tls {
        tls_roundtrip(host, tcp, req.as_bytes())?
    } else {
        plain_roundtrip(tcp, req.as_bytes())?
    };
    decode_http_response_parts(&raw)
}

/// The historical body-only client behind `http_get`/`http_post`.
pub(crate) fn http_request(method: &str, url: &str, body: Option<&str>) -> Result<String> {
    http_roundtrip(method, url, &[], body).map(|(_, b)| b)
}

/// Read a stream to EOF, tolerating an unclean close (a server that drops the connection without a
/// graceful shutdown is the norm with `Connection: close`, and TLS surfaces it as `UnexpectedEof`).
fn read_to_close<R: std::io::Read>(mut r: R) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    match r.read_to_end(&mut buf) {
        Ok(_) => Ok(buf),
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Ok(buf),
        Err(e) => Err(anyhow!("reading response: {e}")),
    }
}

fn plain_roundtrip(mut stream: std::net::TcpStream, req: &[u8]) -> Result<Vec<u8>> {
    use std::io::Write;
    stream.write_all(req).map_err(|e| anyhow!("writing request: {e}"))?;
    read_to_close(stream)
}

fn tls_roundtrip(host: &str, tcp: std::net::TcpStream, req: &[u8]) -> Result<Vec<u8>> {
    use std::io::Write;
    use std::sync::Arc;
    let roots = rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|e| anyhow!("tls config: {e}"))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    let server_name = rustls::pki_types::ServerName::try_from(host.to_string())
        .map_err(|e| anyhow!("invalid TLS server name {host}: {e}"))?;
    let conn = rustls::ClientConnection::new(Arc::new(config), server_name)
        .map_err(|e| anyhow!("tls setup: {e}"))?;
    let mut stream = rustls::StreamOwned::new(conn, tcp);
    stream.write_all(req).map_err(|e| anyhow!("writing request: {e}"))?;
    read_to_close(stream)
}

/// Split an HTTP/1.1 response into its status code and body, de-chunking a `Transfer-Encoding:
/// chunked` payload so the caller never sees chunk-size markers. Body as a (lossy) UTF-8 string.
/// The `http`/`http_get`/`http_post` shape — headers dropped.
#[cfg(test)]
fn decode_http_response_full(raw: &[u8]) -> Result<(i128, String)> {
    decode_http_response_parts(raw).map(|(s, _, b)| (s, b))
}

/// The full decode: status, headers, body. Header names are lowercased and values OWS-trimmed
/// (the canonical form — lowercase names are what `str_lt`'s code-point order sorts sanely, and
/// HTTP names are case-insensitive); duplicate headers are comma-joined in arrival order.
fn decode_http_response_parts(raw: &[u8]) -> Result<(i128, BTreeMap<String, String>, String)> {
    let status = {
        let line_end = raw.windows(2).position(|w| w == b"\r\n").unwrap_or(raw.len());
        let line = String::from_utf8_lossy(&raw[..line_end]);
        // "HTTP/1.1 200 OK" — the second token is the status code; 0 if unparseable.
        line.split_whitespace().nth(1).and_then(|s| s.parse::<i128>().ok()).unwrap_or(0)
    };
    let idx = raw.windows(4).position(|w| w == b"\r\n\r\n");
    let (header_bytes, mut body): (&[u8], &[u8]) = match idx {
        Some(i) => (&raw[..i], &raw[i + 4..]),
        None => return Ok((status, BTreeMap::new(), String::from_utf8_lossy(raw).into_owned())),
    };
    let mut headers: BTreeMap<String, String> = BTreeMap::new();
    // skip(1): the status line is not a header field.
    for line in String::from_utf8_lossy(header_bytes).lines().skip(1) {
        let Some((name, value)) = line.split_once(':') else { continue };
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim();
        if name.is_empty() {
            continue;
        }
        match headers.get_mut(&name) {
            Some(prev) => {
                prev.push_str(", ");
                prev.push_str(value);
            }
            None => {
                headers.insert(name, value.to_string());
            }
        }
    }
    let chunked = headers.get("transfer-encoding").map(|v| v.to_ascii_lowercase().contains("chunked")).unwrap_or(false);
    if chunked {
        let decoded = dechunk(body)?;
        return Ok((status, headers, String::from_utf8_lossy(&decoded).into_owned()));
    }
    // A defensive nicety: some servers still emit a stray trailing CRLF.
    if body.ends_with(b"\r\n") {
        body = &body[..body.len() - 2];
    }
    Ok((status, headers, String::from_utf8_lossy(body).into_owned()))
}

/// Decode an HTTP/1.1 chunked transfer body: a sequence of `<hex-size>[;ext]CRLF<data>CRLF`, ending at a
/// zero-size chunk. Trailers after the final chunk are ignored.
fn dechunk(body: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < body.len() {
        let nl = body[i..]
            .windows(2)
            .position(|w| w == b"\r\n")
            .ok_or_else(|| anyhow!("malformed chunked body: missing chunk-size CRLF"))?;
        let line = &body[i..i + nl];
        let hex_end = line.iter().position(|&b| b == b';').unwrap_or(line.len());
        let hex = std::str::from_utf8(&line[..hex_end]).map_err(|_| anyhow!("non-UTF-8 chunk size"))?.trim();
        let size = usize::from_str_radix(hex, 16).map_err(|_| anyhow!("bad chunk size: {hex:?}"))?;
        i += nl + 2;
        if size == 0 {
            break; // last chunk; ignore any trailers
        }
        if i + size > body.len() {
            bail!("chunked body truncated: declared {size} bytes, {} remain", body.len() - i);
        }
        out.extend_from_slice(&body[i..i + size]);
        i += size;
        // each chunk's data is terminated by CRLF
        if body.get(i..i + 2) == Some(b"\r\n".as_slice()) {
            i += 2;
        } else {
            break;
        }
    }
    Ok(out)
}

fn as_bool(v: &Val) -> Result<bool> {
    match v {
        Val::Bool(b) => Ok(*b),
        _ => bail!("expected a bool, got {}", encode_value(v)),
    }
}

fn as_list(v: &Val) -> Result<Vec<Val>> {
    match v {
        Val::List(xs) => Ok(xs.clone()),
        _ => bail!("expected a list, got {}", encode_value(v)),
    }
}

fn as_map(v: &Val) -> Result<BTreeMap<String, Val>> {
    match v {
        Val::Map(m) => Ok(m.clone()),
        _ => bail!("expected a map, got {}", encode_value(v)),
    }
}

// ---------------------------------------------------------------------------
// JSON-as-data (spec/expressiveness.md phase 3): the `Json` sum type — JNull | JBool bool |
// JNum int/float | JStr string | JList (List Json) | JObj (Map string Json) — is an ordinary
// variant tree over the existing value system; these two conversions are the only new machinery.
// ---------------------------------------------------------------------------

/// A parsed `serde_json` tree as a `Json` variant value.
fn json_to_val(j: &J) -> Result<Val> {
    Ok(match j {
        J::Null => Val::Variant("JNull".to_string(), None),
        J::Bool(b) => Val::Variant("JBool".to_string(), Some(Box::new(Val::Bool(*b)))),
        J::Number(n) => {
            let inner = if let Some(i) = n.as_i64() {
                Val::Int(i as i128)
            } else if let Some(u) = n.as_u64() {
                Val::Int(u as i128)
            } else if let Some(f) = n.as_f64() {
                Val::Float(f)
            } else {
                bail!("unrepresentable JSON number: {n}")
            };
            Val::Variant("JNum".to_string(), Some(Box::new(inner)))
        }
        J::String(s) => Val::Variant("JStr".to_string(), Some(Box::new(Val::Str(s.clone())))),
        J::Array(xs) => Val::Variant(
            "JList".to_string(),
            Some(Box::new(Val::List(xs.iter().map(json_to_val).collect::<Result<Vec<_>>>()?))),
        ),
        J::Object(m) => {
            // serde_json already resolves duplicate keys last-wins; BTreeMap makes the order canonical.
            let mut out = BTreeMap::new();
            for (k, v) in m {
                out.insert(k.clone(), json_to_val(v)?);
            }
            Val::Variant("JObj".to_string(), Some(Box::new(Val::Map(out))))
        }
    })
}

/// A `Json` variant value back as a `serde_json` tree. Errors on a value that isn't Json-shaped
/// (partial like `head`, on inputs a `Json`-typed program can't produce) and on integers outside
/// the JSON-representable i64/u64 range.
fn val_to_json(v: &Val) -> Result<J> {
    let Val::Variant(tag, payload) = v else {
        bail!("render_json expects a Json value, got {}", encode_value(v));
    };
    Ok(match (tag.as_str(), payload) {
        ("JNull", None) => J::Null,
        ("JBool", Some(p)) => match &**p {
            Val::Bool(b) => json!(b),
            other => bail!("JBool payload must be a bool, got {}", encode_value(other)),
        },
        ("JNum", Some(p)) => match &**p {
            Val::Int(i) => {
                let i = i64::try_from(*i).map_err(|_| anyhow!("JNum integer out of JSON range: {i}"))?;
                json!(i)
            }
            Val::Float(f) => serde_json::Number::from_f64(*f)
                .map(J::Number)
                .ok_or_else(|| anyhow!("JNum float is not finite"))?,
            other => bail!("JNum payload must be a number, got {}", encode_value(other)),
        },
        ("JStr", Some(p)) => match &**p {
            Val::Str(s) => json!(s),
            other => bail!("JStr payload must be a string, got {}", encode_value(other)),
        },
        ("JList", Some(p)) => match &**p {
            Val::List(xs) => J::Array(xs.iter().map(val_to_json).collect::<Result<Vec<_>>>()?),
            other => bail!("JList payload must be a list, got {}", encode_value(other)),
        },
        ("JObj", Some(p)) => match &**p {
            Val::Map(m) => {
                let mut out = serde_json::Map::new();
                for (k, v) in m {
                    out.insert(k.clone(), val_to_json(v)?);
                }
                J::Object(out)
            }
            other => bail!("JObj payload must be a map, got {}", encode_value(other)),
        },
        _ => bail!("not a Json value: variant `{tag}`"),
    })
}

/// Builtin arity, or `None` if `name` is not a builtin. `nil` is handled separately (a nullary value).
fn builtin_arity(name: &str) -> Option<usize> {
    Some(match name {
        "neg" | "abs" | "not" | "id" | "head" | "tail" | "last" | "init" | "length" | "null"
        | "reverse" | "fst" | "snd" | "str_length" | "str_lower" | "url_encode" | "to_string"
        | "to_float" | "parse_int"
        | "map_size" | "map_keys" | "parse_json" | "render_json" | "print" | "rand"
        | "now" | "panic" | "read_file" | "http_get" => 1,
        "add" | "sub" | "mul" | "div" | "mod" | "eq" | "neq" | "lt" | "le" | "gt" | "ge" | "and"
        | "or" | "xor" | "cons" | "append" | "concat" | "map" | "filter" | "min" | "max"
        | "str_concat" | "str_contains" | "str_lt" | "str_split" | "str_join"
        | "map_get" | "map_del"
        | "apply" | "write_file" | "http_post" | "spawn" | "replicate" => 2,
        "foldl" | "foldr" | "compose" | "map_put" => 3,
        "http" | "http_full" => 4,
        _ => return None,
    })
}

/// The effect an effectful builtin performs, or `None` if pure. Mirrors the gating in `run_builtin`.
pub fn builtin_effect(name: &str) -> Option<&'static str> {
    match name {
        "print" => Some("io.console"),
        "rand" => Some("random"),
        "now" => Some("time"),
        "panic" => Some("panic"),
        "read_file" => Some("fs.read"),
        "write_file" => Some("fs.write"),
        "http_get" => Some("net.read"),
        "http_post" => Some("net.write"),
        // The general requests' effect depends on their METHOD argument at runtime (net.read for
        // GET/HEAD, net.write otherwise); net.write is the conservative static answer here, and the
        // effects walker refines it when the method is a literal (see effects.rs).
        "http" | "http_full" => Some("net.write"),
        "spawn" => Some("process.spawn"),
        "replicate" => Some("alloc"),
        _ => None,
    }
}

/// Whether `name` is a known builtin (incl. the nullary constants `nil`/`map_empty`) — i.e. its
/// meaning and effects are known statically.
pub fn is_builtin(name: &str) -> bool {
    name == "nil" || name == "map_empty" || builtin_arity(name).is_some()
}

/// Resolve a `var` name: lexical environment first, then the builtin library, then the nullary
/// constants (`nil`, `map_empty`).
fn resolve_var(name: &str, env: &Env) -> Result<Val> {
    if let Some(v) = env.get(name) {
        return Ok(v.clone());
    }
    if name == "nil" {
        return Ok(Val::List(vec![]));
    }
    if name == "map_empty" {
        return Ok(Val::Map(BTreeMap::new()));
    }
    if let Some(arity) = builtin_arity(name) {
        return Ok(Val::Builtin { name: name.to_string(), arity, applied: vec![] });
    }
    bail!("unbound variable: {name}")
}

/// Evaluate a body-expression AST node in an environment.
pub fn eval(expr: &J, env: &Env) -> Result<Val> {
    let kind = expr.get("kind").and_then(|k| k.as_str()).ok_or_else(|| anyhow!("expr missing kind: {expr}"))?;
    match kind {
        "var" => resolve_var(expr["name"].as_str().ok_or_else(|| anyhow!("var name"))?, env),
        "lit" => decode_value(&expr["value"]),
        "lambda" => {
            let params = expr["params"]
                .as_array()
                .ok_or_else(|| anyhow!("lambda params"))?
                .iter()
                .map(|p| p["name"].as_str().map(String::from).ok_or_else(|| anyhow!("param name")))
                .collect::<Result<Vec<_>>>()?;
            Ok(Val::Closure { params, body: Rc::new(expr["body"].clone()), env: env.clone() })
        }
        "app" => {
            let f = eval(&expr["fn"], env)?;
            let args = expr["args"]
                .as_array()
                .ok_or_else(|| anyhow!("app args"))?
                .iter()
                .map(|a| eval(a, env))
                .collect::<Result<Vec<_>>>()?;
            apply(f, args)
        }
        "let" => {
            let name = expr["name"].as_str().ok_or_else(|| anyhow!("let name"))?.to_string();
            let bound = eval(&expr["value"], env)?;
            let mut env2 = env.clone();
            env2.insert(name, bound);
            eval(&expr["body"], &env2)
        }
        "case" => {
            let scrutinee = eval(&expr["scrutinee"], env)?;
            for arm in expr["arms"].as_array().ok_or_else(|| anyhow!("case arms"))? {
                if let Some(binds) = match_pattern(&arm["pattern"], &scrutinee)? {
                    let mut env2 = env.clone();
                    env2.extend(binds);
                    return eval(&arm["body"], &env2);
                }
            }
            bail!("non-exhaustive case: no arm matched {}", encode_value(&scrutinee))
        }
        "field" => {
            let record = eval(&expr["record"], env)?;
            let name = expr["name"].as_str().ok_or_else(|| anyhow!("field name"))?;
            match record {
                Val::Record(m) => m.get(name).cloned().ok_or_else(|| anyhow!("no field {name} on record")),
                other => bail!("field projection on a non-record: {}", encode_value(&other)),
            }
        }
        "variant" => {
            // Variant construction with a *computed* payload (`Just(a / b)`): the tag is fixed, the payload
            // is an expression evaluated in the current environment. (The `lit` path constructs only
            // constant variants; this is the body-expression form.)
            let tag = expr["tag"].as_str().ok_or_else(|| anyhow!("variant tag"))?.to_string();
            let payload = match expr.get("payload") {
                Some(p) => Some(Box::new(eval(p, env)?)),
                None => None,
            };
            Ok(Val::Variant(tag, payload))
        }
        "tuple" => {
            // Tuple construction with computed elements (`(f a, g b)`) — the body-expression form
            // (the `lit` path builds only constant tuples). Destructured by a `tuple` pattern in a
            // `case`, or the 2-tuple `fst`/`snd` builtins.
            let elems = expr["elems"]
                .as_array()
                .ok_or_else(|| anyhow!("tuple elems"))?
                .iter()
                .map(|e| eval(e, env))
                .collect::<Result<Vec<_>>>()?;
            Ok(Val::Tuple(elems))
        }
        other => bail!("unknown expression kind: {other}"),
    }
}

/// Match a pattern against a value; `Some(bindings)` on success (possibly empty), `None` on mismatch.
fn match_pattern(pat: &J, v: &Val) -> Result<Option<Env>> {
    let kind = pat.get("kind").and_then(|k| k.as_str()).ok_or_else(|| anyhow!("pattern missing kind"))?;
    Ok(match kind {
        "wildcard" => Some(Env::new()),
        "bind" => {
            let name = pat["name"].as_str().ok_or_else(|| anyhow!("bind name"))?.to_string();
            let mut e = Env::new();
            e.insert(name, v.clone());
            Some(e)
        }
        "lit" => {
            if val_eq(&decode_value(&pat["value"])?, v) {
                Some(Env::new())
            } else {
                None
            }
        }
        "variant" => {
            let tag = pat["tag"].as_str().ok_or_else(|| anyhow!("variant tag"))?;
            match v {
                Val::Variant(vtag, payload) if vtag == tag => match (pat.get("payload"), payload) {
                    (None, _) => Some(Env::new()),
                    (Some(pp), Some(pv)) => match_pattern(pp, pv)?,
                    (Some(_), None) => None,
                },
                _ => None,
            }
        }
        "tuple" => {
            // Destructure a tuple positionally: `(x, y)` binds each element. Matches only a tuple of
            // the same arity; sub-patterns match left to right and their bindings union (disjoint by
            // construction — the surface forbids repeated binders).
            let pats = pat["elems"].as_array().ok_or_else(|| anyhow!("tuple pattern elems"))?;
            match v {
                Val::Tuple(vs) if vs.len() == pats.len() => {
                    let mut e = Env::new();
                    for (p, sub) in pats.iter().zip(vs.iter()) {
                        match match_pattern(p, sub)? {
                            Some(b) => e.extend(b),
                            None => return Ok(None),
                        }
                    }
                    Some(e)
                }
                _ => None,
            }
        }
        other => bail!("unknown pattern kind: {other}"),
    })
}

/// Apply a callable to arguments, supporting currying (too few args → a partial application) and
/// over-application (extra args applied to the result).
pub fn apply(f: Val, mut args: Vec<Val>) -> Result<Val> {
    if args.is_empty() {
        return Ok(f);
    }
    match f {
        Val::Closure { params, body, env } => {
            if args.len() < params.len() {
                let mut env2 = env.clone();
                for (p, a) in params.iter().zip(args.iter()) {
                    env2.insert(p.clone(), a.clone());
                }
                Ok(Val::Closure { params: params[args.len()..].to_vec(), body, env: env2 })
            } else {
                let mut env2 = env;
                for (p, a) in params.iter().zip(args.iter()) {
                    env2.insert(p.clone(), a.clone());
                }
                let result = eval(&body, &env2)?;
                let extra = args.split_off(params.len());
                apply(result, extra)
            }
        }
        Val::RecClosure { self_name, params, body, env } => {
            // Re-bind `self` to the whole function, then evaluate exactly as a closure would. The
            // resulting closure's env carries `self`, so currying/partial application stay recursive.
            let mut env0 = env.clone();
            env0.insert(
                self_name.clone(),
                Val::RecClosure { self_name: self_name.clone(), params: params.clone(), body: body.clone(), env },
            );
            apply(Val::Closure { params, body, env: env0 }, args)
        }
        Val::Builtin { name, arity, mut applied } => {
            applied.append(&mut args);
            if applied.len() < arity {
                Ok(Val::Builtin { name, arity, applied })
            } else {
                let rest = applied.split_off(arity);
                let result = run_builtin(&name, applied)?;
                apply(result, rest)
            }
        }
        Val::FnRef(addr) => match resolve_fn_ref(&addr) {
            // Composition: resolve the referenced record's body and apply it (see RESOLVER above).
            // Use `eval_recursive_body` (not bare `eval`) so a RECURSIVE referenced function binds
            // `self` and recurses — otherwise a `self`-recursive commons function applied by address
            // (e.g. an agent-loop `apply` whose target recurses, re-run by `verify_claim`) errors on
            // the first self-call. Non-recursive bodies are unaffected (the RecClosure never re-binds).
            Some(body) => apply(eval_recursive_body(&body)?, args),
            None => bail!("cannot apply unresolved fn_ref {addr} (run with --records to link it)"),
        },
        other => bail!("cannot apply a non-function value: {}", encode_value(&other)),
    }
}

fn run_builtin(name: &str, a: Vec<Val>) -> Result<Val> {
    // Arithmetic stays exact on two ints; if either operand is a float, promote to f64 (so `number`
    // bodies from TS/JS, which carry floats, run). Comparison always compares numerically.
    let num2 = |fi: fn(i128, i128) -> i128, ff: fn(f64, f64) -> f64| -> Result<Val> {
        Ok(match (&a[0], &a[1]) {
            (Val::Int(x), Val::Int(y)) => Val::Int(fi(*x, *y)),
            _ => Val::Float(ff(as_f64n(&a[0])?, as_f64n(&a[1])?)),
        })
    };
    let numcmp = |f: fn(f64, f64) -> bool| -> Result<Val> { Ok(Val::Bool(f(as_f64n(&a[0])?, as_f64n(&a[1])?))) };
    Ok(match name {
        "add" => num2(|x, y| x + y, |x, y| x + y)?,
        "sub" => num2(|x, y| x - y, |x, y| x - y)?,
        "mul" => num2(|x, y| x * y, |x, y| x * y)?,
        "div" => match (&a[0], &a[1]) {
            (Val::Int(x), Val::Int(y)) => {
                if *y == 0 {
                    bail!("division by zero");
                }
                Val::Int(x.div_euclid(*y))
            }
            // Partial at zero exactly like the int form — Infinity/NaN are unrepresentable in
            // canonical JCS, so a zero divisor is an error, never a non-finite value (GW5).
            _ => {
                let (x, y) = (as_f64n(&a[0])?, as_f64n(&a[1])?);
                if y == 0.0 {
                    bail!("division by zero");
                }
                Val::Float(x / y)
            }
        },
        "mod" => match (&a[0], &a[1]) {
            (Val::Int(x), Val::Int(y)) => {
                if *y == 0 {
                    bail!("modulo by zero");
                }
                Val::Int(x.rem_euclid(*y))
            }
            _ => {
                let (x, y) = (as_f64n(&a[0])?, as_f64n(&a[1])?);
                if y == 0.0 {
                    bail!("modulo by zero");
                }
                Val::Float(x % y)
            }
        },
        "neg" => match &a[0] {
            Val::Int(i) => Val::Int(-i),
            v => Val::Float(-as_f64n(v)?),
        },
        "abs" => match &a[0] {
            Val::Int(i) => Val::Int(i.abs()),
            v => Val::Float(as_f64n(v)?.abs()),
        },
        "min" => num2(std::cmp::min, f64::min)?,
        "max" => num2(std::cmp::max, f64::max)?,
        "eq" => Val::Bool(val_eq(&a[0], &a[1])),
        "neq" => Val::Bool(!val_eq(&a[0], &a[1])),
        "lt" => numcmp(|x, y| x < y)?,
        "le" => numcmp(|x, y| x <= y)?,
        "gt" => numcmp(|x, y| x > y)?,
        "ge" => numcmp(|x, y| x >= y)?,
        "and" => Val::Bool(as_bool(&a[0])? && as_bool(&a[1])?),
        "or" => Val::Bool(as_bool(&a[0])? || as_bool(&a[1])?),
        "xor" => Val::Bool(as_bool(&a[0])? ^ as_bool(&a[1])?),
        "not" => Val::Bool(!as_bool(&a[0])?),
        "id" => a.into_iter().next().unwrap(),
        "fst" => match &a[0] {
            Val::Tuple(xs) if !xs.is_empty() => xs[0].clone(),
            other => bail!("fst on a non-tuple: {}", encode_value(other)),
        },
        "snd" => match &a[0] {
            Val::Tuple(xs) if xs.len() >= 2 => xs[1].clone(),
            other => bail!("snd on a non-pair: {}", encode_value(other)),
        },
        "cons" => {
            let mut xs = as_list(&a[1])?;
            xs.insert(0, a[0].clone());
            Val::List(xs)
        }
        "head" => as_list(&a[0])?.into_iter().next().ok_or_else(|| anyhow!("head of empty list"))?,
        "tail" => {
            let xs = as_list(&a[0])?;
            if xs.is_empty() {
                bail!("tail of empty list");
            }
            Val::List(xs[1..].to_vec())
        }
        "last" => as_list(&a[0])?.into_iter().next_back().ok_or_else(|| anyhow!("last of empty list"))?,
        "init" => {
            let xs = as_list(&a[0])?;
            if xs.is_empty() {
                bail!("init of empty list");
            }
            Val::List(xs[..xs.len() - 1].to_vec())
        }
        "length" => Val::Int(as_list(&a[0])?.len() as i128),
        "null" => Val::Bool(as_list(&a[0])?.is_empty()),
        "reverse" => {
            let mut xs = as_list(&a[0])?;
            xs.reverse();
            Val::List(xs)
        }
        "append" | "concat" => {
            let mut xs = as_list(&a[0])?;
            xs.extend(as_list(&a[1])?);
            Val::List(xs)
        }
        // String operations (spec/expressiveness.md phase 1). All pure and total; pattern/separator
        // arguments come FIRST so a partial application is a reusable predicate/splitter/joiner.
        "str_concat" => {
            let mut s = as_str(&a[0])?;
            s.push_str(&as_str(&a[1])?);
            Val::Str(s)
        }
        // Unicode scalar values, not bytes or graphemes — exact and platform-independent.
        "str_length" => Val::Int(as_str(&a[0])?.chars().count() as i128),
        "str_contains" => {
            let needle = as_str(&a[0])?;
            Val::Bool(as_str(&a[1])?.contains(needle.as_str()))
        }
        // Strict lexicographic order over Unicode scalar values — the SAME order canonical map keys
        // use (map_keys / check-value), so sorting with str_lt agrees with the core's one ordering.
        // Deliberately NOT a collation (locale-free, deterministic); Rust's str < is exactly this.
        "str_lt" => Val::Bool(as_str(&a[0])? < as_str(&a[1])?),
        // Unicode DEFAULT (untailored) lowercase mapping — deterministic and locale-independent
        // (no Turkish-i tailoring). GW4 pulled it for case-insensitive grouping/sorting.
        "str_lower" => Val::Str(as_str(&a[0])?.to_lowercase()),
        // RFC 3986 percent-encoding: unreserved characters (ALPHA / DIGIT / - . _ ~) pass through,
        // every other UTF-8 BYTE becomes %XX (uppercase hex). Total and deterministic — the
        // strictest form, safe in any URL component. GW10 pulled it: a query string built by
        // str_concat over a raw value is UNSOUND (a space or `&` changes the request).
        "url_encode" => Val::Str(pct_encode(&as_str(&a[0])?)),
        "str_split" => {
            let sep = as_str(&a[0])?;
            let s = as_str(&a[1])?;
            let parts: Vec<Val> = if sep.is_empty() {
                // Empty separator: one singleton string per Unicode scalar value.
                s.chars().map(|c| Val::Str(c.to_string())).collect()
            } else {
                // Keeps empties ("a,,b" by "," -> ["a","","b"]); separator absent -> [s].
                s.split(sep.as_str()).map(|p| Val::Str(p.to_string())).collect()
            };
            Val::List(parts)
        }
        "str_join" => {
            let sep = as_str(&a[0])?;
            Val::Str(as_str_list(&a[1])?.join(&sep))
        }
        // One rendering concept over both numeric types (GW5): canonical decimal for int, the
        // JCS / ECMAScript Number-to-String rendering for float — the SAME rendering the
        // hashing layer's canonicalizer emits, so to_string(3.0) = "3", to_string(3.25) = "3.25".
        // Non-finite floats (only reachable via arithmetic overflow) are refused, not rendered.
        "to_string" => match &a[0] {
            Val::Float(f) => {
                if !f.is_finite() {
                    bail!("to_string on a non-finite float");
                }
                Val::Str(serde_jcs::to_string(&J::from(*f)).map_err(|e| anyhow!("float rendering: {e}"))?)
            }
            v => Val::Str(as_int(v)?.to_string()),
        },
        // Total int -> float widening (GW5); IEEE nearest-even beyond 2^53 — deterministic.
        "to_float" => Val::Float(as_int(&a[0])? as f64),
        // Map operations (spec/expressiveness.md phase 2). String keys; key argument FIRST (like the
        // string ops' pattern-first order); all total — an absent key is None/no-op, never an error.
        "map_put" => {
            let key = as_str(&a[0])?;
            let mut m = as_map(&a[2])?;
            m.insert(key, a[1].clone());
            Val::Map(m)
        }
        "map_get" => {
            let key = as_str(&a[0])?;
            match as_map(&a[1])?.remove(&key) {
                Some(v) => Val::Variant("Just".to_string(), Some(Box::new(v))),
                None => Val::Variant("None".to_string(), None),
            }
        }
        "map_del" => {
            let key = as_str(&a[0])?;
            let mut m = as_map(&a[1])?;
            m.remove(&key);
            Val::Map(m)
        }
        "map_size" => Val::Int(as_map(&a[0])?.len() as i128),
        // BTreeMap iterates sorted, so the key list is deterministic (principle 5).
        "map_keys" => Val::List(as_map(&a[0])?.into_keys().map(Val::Str).collect()),
        // JSON-as-data (spec/expressiveness.md phase 3): the language's own canonical form becomes
        // manipulable from inside. parse_json is total via Maybe; render_json emits the JCS-canonical
        // text, so render_json of a parse_json IS canonicalization.
        "parse_json" => match serde_json::from_str::<J>(&as_str(&a[0])?).ok().as_ref().map(json_to_val) {
            Some(Ok(v)) => Val::Variant("Just".to_string(), Some(Box::new(v))),
            _ => Val::Variant("None".to_string(), None),
        },
        "render_json" => {
            let tree = val_to_json(&a[0])?;
            let bytes = crate::canonicalize(&tree)?;
            Val::Str(String::from_utf8(bytes).map_err(|e| anyhow!("canonical JSON is not UTF-8: {e}"))?)
        }
        "parse_int" => {
            // Accepts exactly the canonical decimal rendering (optional leading `-`, no leading
            // zeros, no `-0`, no whitespace/`+`); everything else, incl. overflow, is None — the
            // totality-via-Maybe pattern that replaces `error`.
            let s = as_str(&a[0])?;
            let digits = s.strip_prefix('-').unwrap_or(&s);
            let canonical = !digits.is_empty()
                && digits.chars().all(|c| c.is_ascii_digit())
                && (digits == "0" || !digits.starts_with('0'))
                && !(s.starts_with('-') && digits == "0");
            match if canonical { s.parse::<i128>().ok() } else { None } {
                Some(i) => Val::Variant("Just".to_string(), Some(Box::new(Val::Int(i)))),
                None => Val::Variant("None".to_string(), None),
            }
        }
        "map" => {
            let f = a[0].clone();
            let out = as_list(&a[1])?
                .into_iter()
                .map(|x| apply(f.clone(), vec![x]))
                .collect::<Result<Vec<_>>>()?;
            Val::List(out)
        }
        "filter" => {
            let p = a[0].clone();
            let mut out = vec![];
            for x in as_list(&a[1])? {
                if as_bool(&apply(p.clone(), vec![x.clone()])?)? {
                    out.push(x);
                }
            }
            Val::List(out)
        }
        "foldl" => {
            let f = a[0].clone();
            let mut acc = a[1].clone();
            for x in as_list(&a[2])? {
                acc = apply(f.clone(), vec![acc, x])?;
            }
            acc
        }
        "foldr" => {
            let f = a[0].clone();
            let init = a[1].clone();
            let xs = as_list(&a[2])?;
            let mut acc = init;
            for x in xs.into_iter().rev() {
                acc = apply(f.clone(), vec![x, acc])?;
            }
            acc
        }
        "compose" => {
            // compose(f, g, x) = f (g x)
            let inner = apply(a[1].clone(), vec![a[2].clone()])?;
            apply(a[0].clone(), vec![inner])?
        }
        "apply" => apply(a[0].clone(), vec![a[1].clone()])?,
        // Effectful builtins — gated by the capability sandbox, recorded, and replayable (EFFECTS).
        "print" => effect_op("io.console", encode_value(&a[0]), || Ok(Val::Unit))?,
        "rand" => {
            let n = as_int(&a[0])?;
            effect_op("random", json!({ "bound": n.to_string() }), || Ok(Val::Int(effect_rand(n)?)))?
        }
        "now" => effect_op("time", json!({}), || Ok(Val::Int(0)))?,
        "panic" => {
            effect_op("panic", encode_value(&a[0]), || Ok(Val::Unit))?;
            bail!("panic: {}", encode_value(&a[0]));
        }
        "read_file" => {
            // fs.read: read a real file's contents (live), or the recorded contents (replay).
            // The file path is the grant scope, so `fs.read@/data` confines reads to a directory.
            let path = as_str(&a[0])?;
            let scope = path.clone();
            effect_op_at("fs.read", Some(&scope), json!({ "path": path.as_str() }), move || {
                std::fs::read_to_string(&path).map(Val::Str).map_err(|e| anyhow!("read_file {path}: {e}"))
            })?
        }
        "write_file" => {
            // fs.write: write a real file (live), or a no-op returning unit (replay).
            // The file path is the grant scope, so `fs.write@/out` confines writes to a directory.
            let path = as_str(&a[0])?;
            let contents = as_str(&a[1])?;
            let scope = path.clone();
            effect_op_at("fs.write", Some(&scope), json!({ "path": path.as_str() }), move || {
                std::fs::write(&path, &contents).map(|_| Val::Unit).map_err(|e| anyhow!("write_file {path}: {e}"))
            })?
        }
        "http_get" => {
            // net.read: a real http:// GET (live), or the recorded body (replay). The URL's
            // host/path is the grant scope, same as the general `http` builtin.
            let url = as_str(&a[0])?;
            let scope = url_scope(&url)?;
            effect_op_at("net.read", Some(&scope), json!({ "url": url.as_str() }), move || {
                http_request("GET", &url, None).map(Val::Str)
            })?
        }
        "http_post" => {
            // net.write: a real http:// POST (live), or the recorded response (replay). Scoped
            // like http_get.
            let url = as_str(&a[0])?;
            let body = as_str(&a[1])?;
            let scope = url_scope(&url)?;
            effect_op_at("net.write", Some(&scope), json!({ "url": url.as_str() }), move || {
                http_request("POST", &url, Some(&body)).map(Val::Str)
            })?
        }
        "http" | "http_full" => {
            // The general request (GW6): http(method, url, headers, body) -> {status, body}.
            // net.read for GET/HEAD, net.write for every other method — decided by the METHOD, so a
            // mutating call is gated by the mutating grant even through this one builtin. The grant
            // check is scoped by host and path (net.write@host[/path]). Header values may carry {{secret:NAME}}
            // placeholders, substituted from the operator's secrets only inside the live effect —
            // the trace detail records the SYMBOLIC headers, so a credential never enters the trace
            // (and replay needs no secrets at all: the response is replayed from the record).
            // `http_full` (GW14) is the same request with the response headers surviving into the
            // result — {status, headers, body} — which is what makes `Location`-driven workflows
            // (server-assigned identity, redirects) expressible in-language.
            let want_headers = name == "http_full";
            let method = as_str(&a[0])?.to_ascii_uppercase();
            let url = as_str(&a[1])?;
            let headers = as_map(&a[2])?;
            let body = as_str(&a[3])?;
            let effect = if method == "GET" || method == "HEAD" { "net.read" } else { "net.write" };
            let scope = url_scope(&url)?;
            let mut symbolic: Vec<(String, String)> = Vec::new();
            for (k, v) in &headers {
                symbolic.push((k.clone(), as_str(v)?));
            }
            let detail = json!({
                "method": method,
                "url": url.as_str(),
                "headers": symbolic.iter().map(|(k, v)| json!({ "name": k, "value": v })).collect::<Vec<_>>(),
                "body": body.as_str(),
            });
            effect_op_at(effect, Some(&scope), detail, move || {
                let mut real = Vec::with_capacity(symbolic.len());
                for (k, v) in &symbolic {
                    // Secrets first, then oauth: both placeholder families resolve only inside
                    // the live effect — the trace keeps the symbolic form for both.
                    real.push((k.clone(), substitute_oauth(&substitute_secrets(v)?)?));
                }
                let (status, resp_headers, resp_body) = http_roundtrip_full(&method, &url, &real, Some(&body))?;
                let mut rec = BTreeMap::new();
                rec.insert("status".to_string(), Val::Int(status));
                if want_headers {
                    rec.insert(
                        "headers".to_string(),
                        Val::Map(resp_headers.into_iter().map(|(k, v)| (k, Val::Str(v))).collect()),
                    );
                }
                rec.insert("body".to_string(), Val::Str(resp_body));
                Ok(Val::Record(rec))
            })?
        }
        "spawn" => {
            // process.spawn: run a real subprocess and return its stdout (live), or recorded (replay).
            let cmd = as_str(&a[0])?;
            let args = as_str_list(&a[1])?;
            effect_op("process.spawn", json!({ "cmd": cmd.as_str(), "args": args }), move || {
                let out = std::process::Command::new(&cmd)
                    .args(&args)
                    .output()
                    .map_err(|e| anyhow!("spawn {cmd}: {e}"))?;
                Ok(Val::Str(String::from_utf8_lossy(&out.stdout).into_owned()))
            })?
        }
        "replicate" => {
            // alloc: allocate a list of `n` copies of `x` on the heap. The canonical heap-allocating
            // builtin — the one effect kind with no external I/O, so it is fully deterministic and
            // replays identically (the trace records only the requested size). Negative n yields [].
            let n = as_int(&a[0])?;
            let x = a[1].clone();
            effect_op("alloc", json!({ "size": n.to_string() }), move || {
                let count = if n < 0 { 0usize } else { n as usize };
                Ok(Val::List(std::iter::repeat(x).take(count).collect()))
            })?
        }
        other => bail!("unknown builtin: {other}"),
    })
}

// ---------------------------------------------------------------------------
// Top-level entry points used by the CLI (`eval` / `run`).
// ---------------------------------------------------------------------------

/// Evaluate a body AST, then apply it to the given argument values. Returns the resulting value AST.
/// Evaluate a (typically record) body to its function value, binding `self` to the function itself
/// so a self-recursive body can call back into it. A lambda becomes a `RecClosure`; any other body
/// is returned unchanged (nothing to recurse into).
pub fn eval_recursive_body(body: &J) -> Result<Val> {
    Ok(match eval(body, &Env::new())? {
        Val::Closure { params, body, env } => {
            Val::RecClosure { self_name: "self".to_string(), params, body, env }
        }
        other => other,
    })
}

pub fn eval_body(body: &J, args: &[J]) -> Result<J> {
    let f = eval_recursive_body(body)?;
    let argv = args.iter().map(decode_value).collect::<Result<Vec<_>>>()?;
    Ok(encode_value(&apply(f, argv)?))
}

/// Outcome of running one worked example through the body.
#[derive(Debug)]
pub struct ExampleRun {
    pub index: usize,
    pub passed: bool,
    pub got: J,
    pub expected: J,
    pub error: Option<String>,
}

/// Run every `examples[]` of a function record through its `body`: bind the example's args, evaluate
/// the body, and compare to the example's claimed `result`. This is what makes the examples executable.
///
/// An example carrying a `trace` reference (`trc_…`, spec/trace.schema.json) is run by **replay**:
/// the recorded observations are resolved through the installed resolver (the same link map that
/// resolves `fn_ref`s — `build_link_map` indexes trace artifacts) and every effect is served from
/// the record — no grants, no secrets, no live service — with the trace required to be consumed
/// exactly. That is what lets a commons consumer check an *effectful* record's examples offline
/// (the record-level counterpart of an `observed` claim, same honest scope: the trace is the
/// publisher's testimony about what the world said). An example without a `trace` runs live under
/// whatever grants are installed, exactly as before.
pub fn run_examples(record: &J, body: &J) -> Result<Vec<ExampleRun>> {
    let f = eval_recursive_body(body)?;
    let examples = record.get("examples").and_then(|e| e.as_array()).cloned().unwrap_or_default();
    let mut out = vec![];
    for (index, ex) in examples.iter().enumerate() {
        let args = ex.get("args").and_then(|a| a.as_array()).cloned().unwrap_or_default();
        let expected_j = ex.get("result").cloned().unwrap_or(J::Null);
        let run = (|| -> Result<(bool, J)> {
            // Decode everything BEFORE installing replay, so no error path can leave the thread
            // replaying into the next example.
            let argv = args.iter().map(decode_value).collect::<Result<Vec<_>>>()?;
            let expected = decode_value(&expected_j)?;
            let replaying = match ex.get("trace").and_then(|t| t.as_str()) {
                Some(trace_addr) => {
                    let trace = resolver_lookup(trace_addr).ok_or_else(|| {
                        anyhow!("example {index} references trace `{trace_addr}` which is not in the provided records — without the recorded observations the effectful example cannot be replayed")
                    })?;
                    let ops = trace
                        .get("ops")
                        .and_then(|o| o.as_array())
                        .ok_or_else(|| anyhow!("trace `{trace_addr}` has no `ops` array"))?
                        .clone();
                    set_effect_replay(ops);
                    true
                }
                None => false,
            };
            let got = apply(f.clone(), argv);
            let leftover = effect_replay_remaining().unwrap_or(0);
            if replaying {
                clear_effect_replay();
            }
            let got = got?;
            if replaying && leftover > 0 {
                bail!("example {index}'s trace was not fully consumed ({leftover} recorded observation{} left over) — the trace does not correspond to this example", if leftover == 1 { "" } else { "s" });
            }
            Ok((val_eq(&got, &expected), encode_value(&got)))
        })();
        match run {
            Ok((passed, got)) => out.push(ExampleRun { index, passed, got, expected: expected_j, error: None }),
            Err(e) => out.push(ExampleRun {
                index,
                passed: false,
                got: J::Null,
                expected: expected_j,
                error: Some(format!("{e:#}")),
            }),
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Run-backed property verification (predicate-expression AST, spec/predicate-expression.schema.json).
//
// The static property checker (eval.rs) honestly marks any law needing to *re-apply a function*
// (`map`/`filter`/`fold`/`compose`/`apply`/the function-under-test `self`) or a quantifier as
// UNVERIFIABLE. With an executable body in hand, those become decidable: `self` is the running
// function, the higher-order ops are the builtins above, and a `forall` ranges over the worked
// examples' arguments (the examples ARE the test inputs). So `forall n. eq(self(n), add(n, n))` is
// now actually checked, per example — CONSISTENT instead of UNVERIFIABLE. Still example-bound, so not
// a proof: a CONSISTENT verdict means "ran true on every example and false on none".
// ---------------------------------------------------------------------------

use crate::Verdict;

fn decode_pred_lit(v: &J) -> Option<Val> {
    match v {
        J::Bool(b) => Some(Val::Bool(*b)),
        J::Number(n) => n.as_i64().map(|i| Val::Int(i as i128)).or_else(|| n.as_f64().map(Val::Float)),
        J::String(s) => Some(Val::Str(s.clone())),
        J::Null => Some(Val::Unit),
        _ => None,
    }
}

/// Evaluate a predicate-expression node. `None` == undecidable (unbound var, unknown op, or an
/// application that errors). `self_fn` is the executable function-under-test, bound to `self`.
fn eval_predicate(node: &J, env: &Env, self_fn: &Option<Val>) -> Option<Val> {
    let kind = node.get("kind")?.as_str()?;
    match kind {
        "var" => {
            let name = node.get("name")?.as_str()?;
            if name == "self" {
                self_fn.clone()
            } else {
                // Bound var first; otherwise a bare builtin/`nil` used as an argument (e.g. `id` in
                // `map(id, xs)`), so functor laws over known builtins become decidable.
                env.get(name).cloned().or_else(|| resolve_var(name, &Env::new()).ok())
            }
        }
        "lit" => {
            // A bare scalar JSON literal, or a structured value-expression payload (the schema
            // allows the latter for compound literals — lists, fn_refs, records, …).
            let v = node.get("value")?;
            decode_pred_lit(v).or_else(|| decode_value(v).ok())
        }
        "forall" | "exists" => {
            // Range the quantifier over THIS example: bind the bound vars positionally to arg0..argN.
            let mut env2 = env.clone();
            if let Some(vars) = node.get("vars").and_then(|v| v.as_array()) {
                for (i, var) in vars.iter().enumerate() {
                    if let (Some(name), Some(arg)) = (var.as_str(), env.get(&format!("arg{i}"))) {
                        env2.insert(name.to_string(), arg.clone());
                    }
                }
            }
            eval_predicate(node.get("body")?, &env2, self_fn)
        }
        "app" => {
            let op = node.get("op")?.as_str()?;
            let arg_nodes = node.get("args")?.as_array()?;
            let args: Option<Vec<Val>> = arg_nodes.iter().map(|a| eval_predicate(a, env, self_fn)).collect();
            let args = args?;
            match op {
                // Boolean connectives not in the builtin library.
                "implies" => match (&args[0], &args[1]) {
                    (Val::Bool(a), Val::Bool(b)) => Some(Val::Bool(!a || *b)),
                    _ => None,
                },
                "iff" => match (&args[0], &args[1]) {
                    (Val::Bool(a), Val::Bool(b)) => Some(Val::Bool(a == b)),
                    _ => None,
                },
                // A content-address op (`fn_…`/`expr_…`) is a commons function referenced by hash —
                // apply it as a `fn_ref` so the thread-local resolver links it (set during claim
                // verification, see `eval_claim`). If no resolver is installed the apply errors →
                // None → undecidable, so this never silently passes.
                _ if op.starts_with("fn_") || op.starts_with("expr_") => {
                    apply(Val::FnRef(op.to_string()), args).ok()
                }
                // Everything else — eq/neq/and/or/not, arithmetic, comparisons, list ops, and the
                // higher-order map/filter/fold/compose/apply — IS a builtin. Run it.
                _ => {
                    let f = resolve_var(op, &Env::new()).ok()?;
                    apply(f, args).ok()
                }
            }
        }
        _ => None,
    }
}

/// Verdict for one property across a record's examples, with the body available to run.
pub fn runtime_verdict(expr: &J, examples: &[J], self_fn: &Option<Val>) -> Verdict {
    let mut any_true = false;
    let mut any_false = false;
    for ex in examples {
        let mut env = Env::new();
        if let Some(r) = ex.get("result").and_then(|r| decode_value(r).ok()) {
            env.insert("result".to_string(), r);
        }
        if let Some(args) = ex.get("args").and_then(|a| a.as_array()) {
            for (i, a) in args.iter().enumerate() {
                if let Ok(v) = decode_value(a) {
                    env.insert(format!("arg{i}"), v);
                }
            }
        }
        match eval_predicate(expr, &env, self_fn) {
            Some(Val::Bool(true)) => any_true = true,
            Some(Val::Bool(false)) => any_false = true,
            _ => {}
        }
    }
    if any_false {
        Verdict::Contradicted
    } else if any_true {
        Verdict::Consistent
    } else {
        Verdict::Unverifiable
    }
}

/// Build the executable function-under-test from a body AST (for `self`), if it evaluates.
pub fn self_fn_from_body(body: &J) -> Option<Val> {
    eval_recursive_body(body).ok()
}

/// Evaluate a closed predicate-expression — a Nova Locutio `assert` claim — to a runtime value,
/// resolving any content-addressed function ops (`fn_…`/`expr_…`) through the installed resolver
/// (set via [`set_resolver`]). `None` if undecidable. Used by claim verification: the receiver
/// re-runs the claim instead of trusting the asserter (principle 3 — verification is re-execution).
pub fn eval_claim(expr: &J) -> Option<Val> {
    eval_predicate(expr, &Env::new(), &None)
}

/// Evaluate a predicate-expression node under explicit variable bindings, for the generative
/// property-testing engine (`proptest.rs`): `bindings` supplies the quantified variables' sampled
/// values and `self_fn` the function-under-test (`self`). `None` if undecidable on these inputs
/// (e.g. the input is outside the function's domain) — the caller treats that as a skipped case, not
/// a counterexample.
pub fn eval_predicate_env(
    node: &J,
    bindings: &BTreeMap<String, Val>,
    self_fn: &Option<Val>,
) -> Option<Val> {
    eval_predicate(node, bindings, self_fn)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn examples_dir() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spec/examples")
    }

    fn load(name: &str) -> J {
        serde_json::from_str(&std::fs::read_to_string(examples_dir().join(name)).unwrap()).unwrap()
    }

    fn nat(n: i128) -> J {
        json!({ "kind": "nat", "value": n as i64 })
    }

    #[test]
    fn double_runs_on_its_examples() {
        let record = load("double.v0.2.json");
        let body = load("body-double.json");
        let runs = run_examples(&record, &body).unwrap();
        assert_eq!(runs.len(), 3);
        assert!(runs.iter().all(|r| r.passed), "double should match all its worked examples");
        // double(5) == 10
        assert_eq!(eval_body(&body, &[nat(5)]).unwrap(), encode_value(&Val::Int(10)));
    }

    #[test]
    fn last_and_init_builtins() {
        let lst = json!({ "kind": "list", "elems": [nat(1), nat(2), nat(3)] });
        // last [1,2,3] == 3
        let last = json!({ "kind": "lambda", "params": [{ "name": "xs" }],
            "body": { "kind": "app", "fn": { "kind": "var", "name": "last" }, "args": [{ "kind": "var", "name": "xs" }] } });
        assert_eq!(eval_body(&last, &[lst.clone()]).unwrap(), encode_value(&Val::Int(3)));
        // init [1,2,3] == [1,2]
        let init = json!({ "kind": "lambda", "params": [{ "name": "xs" }],
            "body": { "kind": "app", "fn": { "kind": "var", "name": "init" }, "args": [{ "kind": "var", "name": "xs" }] } });
        assert_eq!(eval_body(&init, &[lst]).unwrap(),
                   encode_value(&Val::List(vec![Val::Int(1), Val::Int(2)])));
    }

    #[test]
    fn tuple_construction_and_destructure() {
        // \a b -> case (a + b, a - b) of (s, d) => s * d   ==  a^2 - b^2
        let body = json!({ "kind": "lambda", "params": [{ "name": "a" }, { "name": "b" }], "body": {
            "kind": "case",
            "scrutinee": { "kind": "tuple", "elems": [
                { "kind": "app", "fn": { "kind": "var", "name": "add" },
                  "args": [{ "kind": "var", "name": "a" }, { "kind": "var", "name": "b" }] },
                { "kind": "app", "fn": { "kind": "var", "name": "sub" },
                  "args": [{ "kind": "var", "name": "a" }, { "kind": "var", "name": "b" }] }] },
            "arms": [{ "pattern": { "kind": "tuple", "elems": [
                { "kind": "bind", "name": "s" }, { "kind": "bind", "name": "d" }] },
                "body": { "kind": "app", "fn": { "kind": "var", "name": "mul" },
                    "args": [{ "kind": "var", "name": "s" }, { "kind": "var", "name": "d" }] } }] } });
        // (5+3)*(5-3) = 16 = 25 - 9
        assert_eq!(eval_body(&body, &[nat(5), nat(3)]).unwrap(), encode_value(&Val::Int(16)));
        // A tuple RESULT round-trips through encode.
        let mk = json!({ "kind": "lambda", "params": [{ "name": "n" }], "body": {
            "kind": "tuple", "elems": [
                { "kind": "var", "name": "n" },
                { "kind": "app", "fn": { "kind": "var", "name": "neg" }, "args": [{ "kind": "var", "name": "n" }] }] } });
        assert_eq!(eval_body(&mk, &[nat(4)]).unwrap(),
                   encode_value(&Val::Tuple(vec![Val::Int(4), Val::Int(-4)])));
        // A wrong-arity tuple pattern does not match (non-exhaustive → error, not a false bind).
        let bad = json!({ "kind": "lambda", "params": [{ "name": "p" }], "body": {
            "kind": "case", "scrutinee": { "kind": "var", "name": "p" },
            "arms": [{ "pattern": { "kind": "tuple", "elems": [
                { "kind": "bind", "name": "x" }, { "kind": "bind", "name": "y" },
                { "kind": "bind", "name": "z" }] }, "body": { "kind": "var", "name": "x" } }] } });
        let two = json!({ "kind": "tuple", "elems": [nat(1), nat(2)] });
        assert!(eval_body(&bad, &[two]).is_err(), "a 3-arity pattern must not match a 2-tuple");
    }

    #[test]
    fn string_builtins() {
        let s = |v: &str| json!({ "kind": "string", "value": v });
        let run2 = |f: &str, a: J, b: J| {
            let l = json!({ "kind": "lambda", "params": [{ "name": "x" }, { "name": "y" }],
                "body": { "kind": "app", "fn": { "kind": "var", "name": f },
                          "args": [{ "kind": "var", "name": "x" }, { "kind": "var", "name": "y" }] } });
            eval_body(&l, &[a, b]).unwrap()
        };
        let run1 = |f: &str, a: J| {
            let l = json!({ "kind": "lambda", "params": [{ "name": "x" }],
                "body": { "kind": "app", "fn": { "kind": "var", "name": f }, "args": [{ "kind": "var", "name": "x" }] } });
            eval_body(&l, &[a]).unwrap()
        };
        assert_eq!(run2("str_concat", s("nova"), s(" lingua")), encode_value(&Val::Str("nova lingua".into())));
        // str_length counts Unicode scalar values, not bytes.
        assert_eq!(run1("str_length", s("héllo")), encode_value(&Val::Int(5)));
        assert_eq!(run1("str_length", s("")), encode_value(&Val::Int(0)));
        // str_contains is pattern-first; empty needle is true.
        assert_eq!(run2("str_contains", s("ing"), s("nova lingua")), encode_value(&Val::Bool(true)));
        assert_eq!(run2("str_contains", s("xyz"), s("nova lingua")), encode_value(&Val::Bool(false)));
        assert_eq!(run2("str_contains", s(""), s("anything")), encode_value(&Val::Bool(true)));
        // str_split is separator-first and keeps empties; absent separator -> [s]; empty separator -> scalars.
        let strs = |xs: &[&str]| Val::List(xs.iter().map(|x| Val::Str((*x).into())).collect());
        assert_eq!(run2("str_split", s(","), s("a,,b")), encode_value(&strs(&["a", "", "b"])));
        assert_eq!(run2("str_split", s(";"), s("a,b")), encode_value(&strs(&["a,b"])));
        assert_eq!(run2("str_split", s(""), s("héy")), encode_value(&strs(&["h", "é", "y"])));
        // str_join inverts a non-empty-separator split.
        let joined = run2("str_join", s(", "),
            json!({ "kind": "list", "elems": [s("a"), s("b"), s("c")] }));
        assert_eq!(joined, encode_value(&Val::Str("a, b, c".into())));
        // str_lt is strict lexicographic order over Unicode scalar values — the canonical map-key
        // order: "Z" < "a" (0x5A < 0x61), "a" < "ab" (prefix), irreflexive.
        assert_eq!(run2("str_lt", s("Z"), s("a")), encode_value(&Val::Bool(true)));
        assert_eq!(run2("str_lt", s("a"), s("ab")), encode_value(&Val::Bool(true)));
        assert_eq!(run2("str_lt", s("b"), s("ab")), encode_value(&Val::Bool(false)));
        assert_eq!(run2("str_lt", s("x"), s("x")), encode_value(&Val::Bool(false)));
        // str_lower is the Unicode default (untailored) lowercase mapping — deterministic, and the
        // full mapping (İ lowers to i + combining dot, two scalar values).
        assert_eq!(run1("str_lower", s("Nova LINGUA")), encode_value(&Val::Str("nova lingua".into())));
        assert_eq!(run1("str_lower", s("HÉLLO")), encode_value(&Val::Str("héllo".into())));
        assert_eq!(run1("str_lower", s("\u{130}")), encode_value(&Val::Str("i\u{307}".into())));
        // url_encode is RFC 3986 strict percent-encoding: unreserved passes, everything else —
        // including reserved characters and each UTF-8 byte of a multi-byte scalar — becomes %XX
        // (uppercase hex). Encoding an already-unreserved string is the identity.
        assert_eq!(run1("url_encode", s("hello world")), encode_value(&Val::Str("hello%20world".into())));
        assert_eq!(run1("url_encode", s("a&b=c/d?e")), encode_value(&Val::Str("a%26b%3Dc%2Fd%3Fe".into())));
        assert_eq!(run1("url_encode", s("AZaz09-._~")), encode_value(&Val::Str("AZaz09-._~".into())));
        assert_eq!(run1("url_encode", s("héy")), encode_value(&Val::Str("h%C3%A9y".into())));
        assert_eq!(run1("url_encode", s("100%")), encode_value(&Val::Str("100%25".into())));
        assert_eq!(run1("url_encode", s("")), encode_value(&Val::Str("".into())));
    }

    #[test]
    fn to_string_and_parse_int_round_trip() {
        let s = |v: &str| json!({ "kind": "string", "value": v });
        let run1 = |f: &str, a: J| {
            let l = json!({ "kind": "lambda", "params": [{ "name": "x" }],
                "body": { "kind": "app", "fn": { "kind": "var", "name": f }, "args": [{ "kind": "var", "name": "x" }] } });
            eval_body(&l, &[a]).unwrap()
        };
        let just = |i: i128| encode_value(&Val::Variant("Just".into(), Some(Box::new(Val::Int(i)))));
        let none = encode_value(&Val::Variant("None".into(), None));
        assert_eq!(run1("to_string", nat(42)), encode_value(&Val::Str("42".into())));
        assert_eq!(run1("to_string", json!({ "kind": "int", "value": -7 })), encode_value(&Val::Str("-7".into())));
        // parse_int accepts exactly canonical decimal (totality via Maybe — never an error).
        assert_eq!(run1("parse_int", s("42")), just(42));
        assert_eq!(run1("parse_int", s("-7")), just(-7));
        assert_eq!(run1("parse_int", s("0")), just(0));
        for bad in ["", "abc", "007", "-0", "+5", " 5", "5 ", "1.5", "99999999999999999999999999999999999999999"] {
            assert_eq!(run1("parse_int", s(bad)), none, "parse_int({bad:?}) must be None");
        }
    }

    #[test]
    fn float_report_primitives_gw5() {
        // to_float / numeric to_string / numeric div-mod — the GW5 pull.
        let fl = |v: f64| json!({ "kind": "float", "value": v });
        let run1 = |f: &str, a: J| {
            let l = json!({ "kind": "lambda", "params": [{ "name": "x" }],
                "body": { "kind": "app", "fn": { "kind": "var", "name": f }, "args": [{ "kind": "var", "name": "x" }] } });
            eval_body(&l, &[a])
        };
        let run2 = |f: &str, a: J, b: J| {
            let l = json!({ "kind": "lambda", "params": [{ "name": "x" }, { "name": "y" }],
                "body": { "kind": "app", "fn": { "kind": "var", "name": f },
                          "args": [{ "kind": "var", "name": "x" }, { "kind": "var", "name": "y" }] } });
            eval_body(&l, &[a, b])
        };
        // to_float widens exactly on small ints (IEEE nearest-even beyond 2^53 — deterministic).
        assert_eq!(run1("to_float", json!({ "kind": "int", "value": 3 })).unwrap(), encode_value(&Val::Float(3.0)));
        assert_eq!(run1("to_float", json!({ "kind": "int", "value": -2 })).unwrap(), encode_value(&Val::Float(-2.0)));
        // to_string on a float is the JCS / ECMAScript canonical rendering — whole floats have
        // no fraction ("3", not "3.0"), and it is the SAME rendering the hashing layer emits.
        assert_eq!(run1("to_string", fl(3.0)).unwrap(), encode_value(&Val::Str("3".into())));
        assert_eq!(run1("to_string", fl(3.25)).unwrap(), encode_value(&Val::Str("3.25".into())));
        assert_eq!(run1("to_string", fl(-0.5)).unwrap(), encode_value(&Val::Str("-0.5".into())));
        // Float division works and is partial at zero — Infinity/NaN are never produced.
        assert_eq!(run2("div", fl(6.5), fl(2.0)).unwrap(), encode_value(&Val::Float(3.25)));
        assert!(run2("div", fl(1.0), fl(0.0)).is_err(), "float div by zero must error, not yield inf");
        assert!(run2("mod", fl(1.0), fl(0.0)).is_err(), "float mod by zero must error, not yield NaN");
        // The GW5 mean shape: div (sum) (to_float (length)) over [1.0, 2.0, 4.0] = 7/3-ish -> exact 2.3333…?
        // Use an exact case: mean of [1.5, 2.5] = 2.
        let body = json!({ "kind": "lambda", "params": [{ "name": "xs" }], "body": {
            "kind": "app", "fn": { "kind": "var", "name": "div" }, "args": [
                { "kind": "app", "fn": { "kind": "var", "name": "foldl" },
                  "args": [{ "kind": "var", "name": "add" }, { "kind": "lit", "value": { "kind": "float", "value": 0.0 } }, { "kind": "var", "name": "xs" }] },
                { "kind": "app", "fn": { "kind": "var", "name": "to_float" },
                  "args": [{ "kind": "app", "fn": { "kind": "var", "name": "length" }, "args": [{ "kind": "var", "name": "xs" }] }] }] } });
        let xs = json!({ "kind": "list", "elems": [fl(1.5), fl(2.5)] });
        assert_eq!(eval_body(&body, &[xs]).unwrap(), encode_value(&Val::Float(2.0)));
    }

    #[test]
    fn extract_and_parse_field_runs() {
        // The GW1 shape (spec/expressiveness.md): split a textual payload on a separator, walk to a
        // field, parse it, and case on the Maybe — strings are now data, not just opaque carriers.
        // \s -> case parse_int(head(tail(str_split(",", s)))) of { Just(n) => n + n; None => 0 }
        let app = |f: &str, args: Vec<J>| json!({ "kind": "app", "fn": { "kind": "var", "name": f }, "args": args });
        let sep = json!({ "kind": "lit", "value": { "kind": "string", "value": "," } });
        let field = app("head", vec![app("tail", vec![app("str_split", vec![sep, json!({ "kind": "var", "name": "s" })])])]);
        let body = json!({ "kind": "lambda", "params": [{ "name": "s" }], "body": {
            "kind": "case",
            "scrutinee": app("parse_int", vec![field]),
            "arms": [
                { "pattern": { "kind": "variant", "tag": "Just", "payload": { "kind": "bind", "name": "n" } },
                  "body": app("add", vec![json!({ "kind": "var", "name": "n" }), json!({ "kind": "var", "name": "n" })]) },
                { "pattern": { "kind": "variant", "tag": "None" },
                  "body": { "kind": "lit", "value": { "kind": "int", "value": 0 } } }] } });
        let payload = json!({ "kind": "string", "value": "id,21,ok" });
        assert_eq!(eval_body(&body, &[payload]).unwrap(), encode_value(&Val::Int(42)));
        let junk = json!({ "kind": "string", "value": "id,notanum,ok" });
        assert_eq!(eval_body(&body, &[junk]).unwrap(), encode_value(&Val::Int(0)));
    }

    #[test]
    fn map_builtins() {
        // Build up from map_empty, read back, delete, and inspect — the config-lookup idiom
        // (spec/expressiveness.md phase 2). Key argument first, like the string ops.
        let app = |f: &str, args: Vec<J>| json!({ "kind": "app", "fn": { "kind": "var", "name": f }, "args": args });
        let s = |v: &str| json!({ "kind": "lit", "value": { "kind": "string", "value": v } });
        let i = |n: i64| json!({ "kind": "lit", "value": { "kind": "int", "value": n } });
        let built = app("map_put", vec![s("b"), i(2), app("map_put", vec![s("a"), i(1), json!({ "kind": "var", "name": "map_empty" })])]);
        let get = |k: &str, m: J| app("map_get", vec![s(k), m]);
        let body = json!({ "kind": "lambda", "params": [{ "name": "u" }], "body": get("b", built.clone()) });
        let just = |n: i128| encode_value(&Val::Variant("Just".into(), Some(Box::new(Val::Int(n)))));
        let none = encode_value(&Val::Variant("None".into(), None));
        assert_eq!(eval_body(&body, &[json!({ "kind": "unit" })]).unwrap(), just(2));
        // A missing key is None (total), and map_del removes.
        let body_missing = json!({ "kind": "lambda", "params": [{ "name": "u" }], "body": get("zz", built.clone()) });
        assert_eq!(eval_body(&body_missing, &[json!({ "kind": "unit" })]).unwrap(), none);
        let body_del = json!({ "kind": "lambda", "params": [{ "name": "u" }],
            "body": get("a", app("map_del", vec![s("a"), built.clone()])) });
        assert_eq!(eval_body(&body_del, &[json!({ "kind": "unit" })]).unwrap(), none);
        // size + keys (sorted — BTreeMap iteration is deterministic); put overwrites, not duplicates.
        let body_size = json!({ "kind": "lambda", "params": [{ "name": "u" }],
            "body": app("map_size", vec![app("map_put", vec![s("a"), i(9), built.clone()])]) });
        assert_eq!(eval_body(&body_size, &[json!({ "kind": "unit" })]).unwrap(), encode_value(&Val::Int(2)));
        let body_keys = json!({ "kind": "lambda", "params": [{ "name": "u" }], "body": app("map_keys", vec![built]) });
        assert_eq!(eval_body(&body_keys, &[json!({ "kind": "unit" })]).unwrap(),
                   encode_value(&Val::List(vec![Val::Str("a".into()), Val::Str("b".into())])));
    }

    #[test]
    fn map_value_decode_encode_round_trip() {
        // A map VALUE (e.g. an example argument) decodes, and re-encodes in canonical sorted form.
        let m = json!({ "kind": "map", "entries": [
            { "key": "a", "value": { "kind": "int", "value": 1 } },
            { "key": "b", "value": { "kind": "list", "elems": [{ "kind": "int", "value": 2 }] } }
        ] });
        let id = json!({ "kind": "lambda", "params": [{ "name": "m" }], "body": { "kind": "var", "name": "m" } });
        assert_eq!(eval_body(&id, &[m.clone()]).unwrap(), m);
        // Lookup into a passed-in map value.
        let body = json!({ "kind": "lambda", "params": [{ "name": "m" }],
            "body": { "kind": "app", "fn": { "kind": "var", "name": "map_get" },
                      "args": [{ "kind": "lit", "value": { "kind": "string", "value": "a" } }, { "kind": "var", "name": "m" }] } });
        assert_eq!(eval_body(&body, &[m]).unwrap(),
                   encode_value(&Val::Variant("Just".into(), Some(Box::new(Val::Int(1))))));
    }

    #[test]
    fn json_parse_and_render() {
        let app = |f: &str, args: Vec<J>| json!({ "kind": "app", "fn": { "kind": "var", "name": f }, "args": args });
        let slit = |v: &str| json!({ "kind": "lit", "value": { "kind": "string", "value": v } });
        let run_str = |f: &str, input: &str| {
            let body = json!({ "kind": "lambda", "params": [{ "name": "s" }],
                "body": app(f, vec![json!({ "kind": "var", "name": "s" })]) });
            eval_body(&body, &[json!({ "kind": "string", "value": input })]).unwrap()
        };
        // parse_json is total via Maybe.
        let none = encode_value(&Val::Variant("None".into(), None));
        assert_eq!(run_str("parse_json", "{not json"), none);
        // Every JSON shape lands in the Json variant tree.
        let parsed = run_str("parse_json", r#"{"b": [1, true, null], "a": "x"}"#);
        assert_eq!(parsed["kind"], "variant");
        assert_eq!(parsed["tag"], "Just");
        assert_eq!(parsed["payload"]["tag"], "JObj");
        // render_json(parse_json(s)) IS canonicalization: keys sorted, minimal whitespace (JCS).
        let round = json!({ "kind": "lambda", "params": [{ "name": "s" }], "body": {
            "kind": "case",
            "scrutinee": app("parse_json", vec![json!({ "kind": "var", "name": "s" })]),
            "arms": [
                { "pattern": { "kind": "variant", "tag": "Just", "payload": { "kind": "bind", "name": "j" } },
                  "body": app("render_json", vec![json!({ "kind": "var", "name": "j" })]) },
                { "pattern": { "kind": "variant", "tag": "None" }, "body": slit("invalid") }] } });
        let canon = eval_body(&round, &[json!({ "kind": "string", "value": "{ \"b\" : [1, true, null] , \"a\": \"x\" }" })]).unwrap();
        assert_eq!(canon, encode_value(&Val::Str(r#"{"a":"x","b":[1,true,null]}"#.into())));
    }

    #[test]
    fn json_field_projection_runs() {
        // The practical form of GW1 (spec/expressiveness.md phase 3): parse the payload as JSON and
        // PROJECT the field — no more splitting body text. Malformed input and missing/mistyped
        // fields all fall through to the default, totally.
        let app = |f: &str, args: Vec<J>| json!({ "kind": "app", "fn": { "kind": "var", "name": f }, "args": args });
        let dflt = json!({ "kind": "lit", "value": { "kind": "int", "value": 8080 } });
        let vpat = |tag: &str, inner: J| json!({ "kind": "variant", "tag": tag, "payload": inner });
        let bind = |n: &str| json!({ "kind": "bind", "name": n });
        // case parse_json s of { Just(JObj(m)) => case map_get "port" m of { Just(JNum(p)) => p; _ => 8080 }; _ => 8080 }
        let inner_case = json!({ "kind": "case",
            "scrutinee": app("map_get", vec![json!({ "kind": "lit", "value": { "kind": "string", "value": "port" } }),
                                             json!({ "kind": "var", "name": "m" })]),
            "arms": [
                { "pattern": vpat("Just", vpat("JNum", bind("p"))), "body": { "kind": "var", "name": "p" } },
                { "pattern": { "kind": "wildcard" }, "body": dflt.clone() }] });
        let body = json!({ "kind": "lambda", "params": [{ "name": "s" }], "body": {
            "kind": "case",
            "scrutinee": app("parse_json", vec![json!({ "kind": "var", "name": "s" })]),
            "arms": [
                { "pattern": vpat("Just", vpat("JObj", bind("m"))), "body": inner_case },
                { "pattern": { "kind": "wildcard" }, "body": dflt }] } });
        let run = |input: &str| eval_body(&body, &[json!({ "kind": "string", "value": input })]).unwrap();
        assert_eq!(run(r#"{"host": "h", "port": 9000}"#), encode_value(&Val::Int(9000)));
        assert_eq!(run(r#"{"host": "h"}"#), encode_value(&Val::Int(8080)));
        assert_eq!(run(r#"{"port": "not a number"}"#), encode_value(&Val::Int(8080)));
        assert_eq!(run("not json at all"), encode_value(&Val::Int(8080)));
    }

    #[test]
    fn reverse_via_last_and_init() {
        // The natural `cons (last xs) (self (init xs))` formulation of reverse — which a model reaches for
        // and which is algorithmically correct — now runs, because `last`/`init` are builtins.
        let body = json!({ "kind": "lambda", "params": [{ "name": "xs" }], "body": {
            "kind": "case",
            "scrutinee": { "kind": "app", "fn": { "kind": "var", "name": "null" }, "args": [{ "kind": "var", "name": "xs" }] },
            "arms": [
                { "pattern": { "kind": "lit", "value": { "kind": "bool", "value": true } },
                  "body": { "kind": "var", "name": "nil" } },
                { "pattern": { "kind": "lit", "value": { "kind": "bool", "value": false } }, "body": {
                    "kind": "app", "fn": { "kind": "var", "name": "cons" }, "args": [
                        { "kind": "app", "fn": { "kind": "var", "name": "last" }, "args": [{ "kind": "var", "name": "xs" }] },
                        { "kind": "app", "fn": { "kind": "var", "name": "self" }, "args": [
                            { "kind": "app", "fn": { "kind": "var", "name": "init" }, "args": [{ "kind": "var", "name": "xs" }] }] }] } }] } });
        let input = json!({ "kind": "list", "elems": [nat(1), nat(2), nat(3), nat(4)] });
        assert_eq!(eval_body(&body, &[input]).unwrap(),
                   encode_value(&Val::List(vec![Val::Int(4), Val::Int(3), Val::Int(2), Val::Int(1)])));
    }

    #[test]
    fn self_recursive_body_runs() {
        // `self` is bound to the function itself, so a self-recursive body evaluates and its examples pass.
        let record = load("length.json");
        let body = load("body-length.json");
        let runs = run_examples(&record, &body).unwrap();
        assert!(runs.iter().all(|r| r.passed), "recursive length should match all its worked examples");
        // length([10,20,30,40]) == 4 — exercises four levels of `self` recursion.
        assert_eq!(
            eval_body(&body, &[json!({ "kind": "list", "elems": [nat(10), nat(20), nat(30), nat(40)] })]).unwrap(),
            encode_value(&Val::Int(4))
        );
    }

    #[test]
    fn self_recursion_survives_partial_application() {
        // A recursive function partially applied (curried) must still recurse: `self` always rebinds the
        // WHOLE function, never the partially-applied remainder. `factorial` curries trivially (arity 1),
        // so apply it in two steps via the RecClosure path and check the deep recursion still terminates.
        let factorial = json!({
            "kind": "lambda", "params": [{ "name": "n" }],
            "body": { "kind": "case",
                "scrutinee": { "kind": "app", "fn": { "kind": "var", "name": "eq" },
                    "args": [{ "kind": "var", "name": "n" }, { "kind": "lit", "value": { "kind": "int", "value": 0 } }] },
                "arms": [
                    { "pattern": { "kind": "lit", "value": { "kind": "bool", "value": true } },
                      "body": { "kind": "lit", "value": { "kind": "int", "value": 1 } } },
                    { "pattern": { "kind": "lit", "value": { "kind": "bool", "value": false } },
                      "body": { "kind": "app", "fn": { "kind": "var", "name": "mul" }, "args": [
                          { "kind": "var", "name": "n" },
                          { "kind": "app", "fn": { "kind": "var", "name": "self" }, "args": [
                              { "kind": "app", "fn": { "kind": "var", "name": "sub" },
                                "args": [{ "kind": "var", "name": "n" }, { "kind": "lit", "value": { "kind": "int", "value": 1 } }] }] }] } },
                ] }
        });
        let f = eval_recursive_body(&factorial).unwrap();
        assert!(matches!(f, Val::RecClosure { .. }));
        assert!(val_eq(&apply(f, vec![Val::Int(5)]).unwrap(), &Val::Int(120)));
    }

    #[test]
    fn fn_ref_to_recursive_body_binds_self() {
        // A recursive function applied BY ADDRESS (fn_ref) must still bind `self` and recurse — this is
        // the path `verify_claim` takes when re-running an agent-loop `apply` whose target is recursive.
        // Before the fix the fn_ref body was evaluated with plain `eval` (a non-recursive Closure), so the
        // first `self`-call errored and the claim came back undecidable. `length([1,2,3]) == 3`.
        let length = json!({
            "kind": "lambda", "params": [{ "name": "xs" }],
            "body": { "kind": "case",
                "scrutinee": { "kind": "app", "fn": { "kind": "var", "name": "null" },
                    "args": [{ "kind": "var", "name": "xs" }] },
                "arms": [
                    { "pattern": { "kind": "lit", "value": { "kind": "bool", "value": true } },
                      "body": { "kind": "lit", "value": { "kind": "int", "value": 0 } } },
                    { "pattern": { "kind": "lit", "value": { "kind": "bool", "value": false } },
                      "body": { "kind": "app", "fn": { "kind": "var", "name": "add" }, "args": [
                          { "kind": "lit", "value": { "kind": "int", "value": 1 } },
                          { "kind": "app", "fn": { "kind": "var", "name": "self" }, "args": [
                              { "kind": "app", "fn": { "kind": "var", "name": "tail" },
                                "args": [{ "kind": "var", "name": "xs" }] }] }] } },
                ] }
        });
        set_resolver(HashMap::from([("fn_test_length".to_string(), length)]));
        let got = apply(
            Val::FnRef("fn_test_length".to_string()),
            vec![Val::List(vec![Val::Int(1), Val::Int(2), Val::Int(3)])],
        )
        .unwrap();
        clear_resolver();
        assert!(val_eq(&got, &Val::Int(3)));
    }

    #[test]
    fn is_zero_case_matching() {
        let body = load("body-is-zero.json");
        assert_eq!(eval_body(&body, &[nat(0)]).unwrap(), json!({ "kind": "bool", "value": true }));
        assert_eq!(eval_body(&body, &[nat(7)]).unwrap(), json!({ "kind": "bool", "value": false }));
    }

    #[test]
    fn detects_a_wrong_example() {
        let record = json!({
            "examples": [{ "args": [nat(2)], "result": nat(5) }]  // wrong: double(2) = 4, not 5
        });
        let body = load("body-double.json");
        let runs = run_examples(&record, &body).unwrap();
        assert!(!runs[0].passed);
        assert_eq!(runs[0].got, encode_value(&Val::Int(4)));
    }

    #[test]
    fn variant_construction_with_computed_payload() {
        // \a b -> case b == 0 of { true => None; false => Just(a / b) } — a safe-division returning Maybe.
        let body = json!({ "kind": "lambda", "params": [{ "name": "a" }, { "name": "b" }], "body": {
            "kind": "case",
            "scrutinee": { "kind": "app", "fn": { "kind": "var", "name": "eq" },
                "args": [{ "kind": "var", "name": "b" }, { "kind": "lit", "value": { "kind": "int", "value": 0 } }] },
            "arms": [
                { "pattern": { "kind": "lit", "value": { "kind": "bool", "value": true } },
                  "body": { "kind": "variant", "tag": "None" } },
                { "pattern": { "kind": "lit", "value": { "kind": "bool", "value": false } },
                  "body": { "kind": "variant", "tag": "Just",
                    "payload": { "kind": "app", "fn": { "kind": "var", "name": "div" },
                        "args": [{ "kind": "var", "name": "a" }, { "kind": "var", "name": "b" }] } } }] } });
        // safe_div(6, 2) = Just(3) — the payload is computed, not a constant.
        assert_eq!(
            eval_body(&body, &[nat(6), nat(2)]).unwrap(),
            json!({ "kind": "variant", "tag": "Just", "payload": { "kind": "int", "value": 3 } })
        );
        // safe_div(1, 0) = None.
        assert_eq!(eval_body(&body, &[nat(1), nat(0)]).unwrap(), json!({ "kind": "variant", "tag": "None" }));
    }

    #[test]
    fn higher_order_builtins() {
        // map(double, [1,2,3]) == [2,4,6] using a lambda for double.
        let dbl = json!({
            "kind": "lambda",
            "params": [{ "name": "n" }],
            "body": { "kind": "app", "fn": { "kind": "var", "name": "add" },
                      "args": [{ "kind": "var", "name": "n" }, { "kind": "var", "name": "n" }] }
        });
        let f = eval(&dbl, &Env::new()).unwrap();
        let xs = Val::List(vec![Val::Int(1), Val::Int(2), Val::Int(3)]);
        let mapped = apply(
            Val::Builtin { name: "map".into(), arity: 2, applied: vec![] },
            vec![f.clone(), xs.clone()],
        )
        .unwrap();
        assert!(val_eq(&mapped, &Val::List(vec![Val::Int(2), Val::Int(4), Val::Int(6)])));

        // foldl(add, 0, [1,2,3,4]) == 10
        let add = Val::Builtin { name: "add".into(), arity: 2, applied: vec![] };
        let sum = apply(
            Val::Builtin { name: "foldl".into(), arity: 3, applied: vec![] },
            vec![add, Val::Int(0), Val::List(vec![Val::Int(1), Val::Int(2), Val::Int(3), Val::Int(4)])],
        )
        .unwrap();
        assert!(val_eq(&sum, &Val::Int(10)));
    }

    #[test]
    fn currying_and_compose() {
        // compose(double, double)(3) == 12   (currying: compose applied to 2 of 3 args is a function)
        let dbl = eval(
            &json!({ "kind": "lambda", "params": [{ "name": "n" }],
                     "body": { "kind": "app", "fn": { "kind": "var", "name": "add" },
                               "args": [{ "kind": "var", "name": "n" }, { "kind": "var", "name": "n" }] } }),
            &Env::new(),
        )
        .unwrap();
        let compose = Val::Builtin { name: "compose".into(), arity: 3, applied: vec![] };
        let twice = apply(compose, vec![dbl.clone(), dbl]).unwrap(); // partial: a function
        let out = apply(twice, vec![Val::Int(3)]).unwrap();
        assert!(val_eq(&out, &Val::Int(12)));
    }

    #[test]
    fn composition_resolves_fn_ref_across_records() {
        // Link `double` by its real content-address, then run `\xs -> map(<fn_ref double>, xs)` on
        // [1,2,3]: the fn_ref resolves to double's committed body and runs -> [2,4,6]. Cross-record
        // composition with real data (principle 4: assemble from existing records).
        let double_rec = load("double.v0.2.json");
        let addr = double_rec["hash"].as_str().unwrap().to_string();
        let mut map = HashMap::new();
        map.insert(addr.clone(), load("body-double.json"));
        set_resolver(map);

        let body = json!({ "kind": "lambda", "params": [{ "name": "xs" }],
            "body": { "kind": "app", "fn": { "kind": "var", "name": "map" }, "args": [
                { "kind": "lit", "value": { "kind": "fn_ref", "target": addr } },
                { "kind": "var", "name": "xs" }] } });
        let xs = json!({ "kind": "list", "elems": [nat(1), nat(2), nat(3)] });
        let got = eval_body(&body, &[xs]).unwrap();
        clear_resolver();

        assert_eq!(got, json!({ "kind": "list", "elems": [
            { "kind": "int", "value": 2 }, { "kind": "int", "value": 4 }, { "kind": "int", "value": 6 }] }));

        // An unresolved fn_ref is an honest error (not a silent pass).
        assert!(eval_body(
            &json!({ "kind": "app", "fn": { "kind": "lit", "value": { "kind": "fn_ref", "target": "fn_deadbeef" } },
                     "args": [{ "kind": "lit", "value": nat(1) }] }),
            &[],
        )
        .is_err());
    }

    #[test]
    fn run_backed_property_verification() {
        // double's law `forall n. eq(self(n), add(n, n))` is UNVERIFIABLE statically (self + forall),
        // but with the runnable body it is actually checked over the examples -> CONSISTENT.
        let record = load("double.v0.2.json");
        let body = load("body-double.json");
        let examples: Vec<J> = record["examples"].as_array().unwrap().clone();
        let expr = &record["properties"][0]["expr"];
        let self_fn = self_fn_from_body(&body);
        assert_eq!(runtime_verdict(expr, &examples, &self_fn), Verdict::Consistent);

        // A body that does NOT satisfy the law (triple instead of double) is CONTRADICTED.
        let triple = json!({ "kind": "lambda", "params": [{ "name": "n" }],
            "body": { "kind": "app", "fn": { "kind": "var", "name": "mul" },
                      "args": [{ "kind": "var", "name": "n" }, { "kind": "lit", "value": nat(3) }] } });
        let wrong = self_fn_from_body(&triple);
        assert_eq!(runtime_verdict(expr, &examples, &wrong), Verdict::Contradicted);
    }

    #[test]
    fn let_and_field() {
        // let x = 4 in x ;  and  record field projection
        let e = json!({ "kind": "let", "name": "x", "value": { "kind": "lit", "value": nat(4) },
                        "body": { "kind": "var", "name": "x" } });
        assert!(val_eq(&eval(&e, &Env::new()).unwrap(), &Val::Int(4)));

        let rec = json!({ "kind": "lit", "value": { "kind": "record",
            "fields": [{ "name": "a", "value": nat(1) }, { "name": "b", "value": nat(2) }] } });
        let proj = json!({ "kind": "field", "record": rec, "name": "b" });
        assert!(val_eq(&eval(&proj, &Env::new()).unwrap(), &Val::Int(2)));
    }

    #[test]
    fn effect_enforcement_gates_print() {
        // \msg -> print(msg)
        let body = json!({ "kind": "lambda", "params": [{ "name": "msg" }],
            "body": { "kind": "app", "fn": { "kind": "var", "name": "print" },
                      "args": [{ "kind": "var", "name": "msg" }] } });
        let arg = json!({ "kind": "string", "value": "hi" });

        // Ungranted: the io.console effect is rejected at eval time.
        set_effect_grants(Vec::<String>::new());
        assert!(eval_body(&body, &[arg.clone()]).is_err(), "print must be rejected without io.console");
        clear_effects();

        // Granted: runs, returns unit, and the structured trace records the effect.
        set_effect_grants(vec!["io.console".to_string()]);
        let out = eval_body(&body, &[arg]).unwrap();
        let trace = take_effect_trace();
        clear_effects();
        assert_eq!(out, json!({ "kind": "unit" }));
        assert_eq!(trace.len(), 1);
        assert_eq!(trace[0]["effect"], "io.console");
        assert_eq!(trace[0]["detail"], json!({ "kind": "string", "value": "hi" }));
    }

    #[test]
    fn rand_is_deterministic_and_gated() {
        // \n -> rand(n)
        let body = json!({ "kind": "lambda", "params": [{ "name": "n" }],
            "body": { "kind": "app", "fn": { "kind": "var", "name": "rand" },
                      "args": [{ "kind": "var", "name": "n" }] } });
        let n = json!({ "kind": "nat", "value": 100 });

        set_effect_grants(vec!["random".to_string()]);
        let a = eval_body(&body, &[n.clone()]).unwrap();
        clear_effects();
        set_effect_grants(vec!["random".to_string()]);
        let b = eval_body(&body, &[n.clone()]).unwrap();
        clear_effects();
        assert_eq!(a, b, "rand must be deterministic across runs (same fixed seed)");
        let v = a["value"].as_i64().unwrap();
        assert!((0..100).contains(&v), "rand(100) in [0,100)");

        // Ungranted: random is rejected.
        set_effect_grants(Vec::<String>::new());
        assert!(eval_body(&body, &[n]).is_err(), "rand must be rejected without the random grant");
        clear_effects();
    }

    #[test]
    fn now_and_panic_are_gated_effects() {
        let unary = |op: &str| json!({ "kind": "lambda", "params": [{ "name": "x" }],
            "body": { "kind": "app", "fn": { "kind": "var", "name": op }, "args": [{ "kind": "var", "name": "x" }] } });

        // now → time: ungranted rejected; granted returns a fixed reading + traces `time`.
        set_effect_grants(Vec::<String>::new());
        assert!(eval_body(&unary("now"), &[nat(1)]).is_err());
        clear_effects();
        set_effect_grants(vec!["time".to_string()]);
        assert_eq!(eval_body(&unary("now"), &[nat(1)]).unwrap(), json!({ "kind": "int", "value": 0 }));
        let trace = take_effect_trace();
        clear_effects();
        assert_eq!(trace[0]["effect"], "time");

        // panic → panic: gated, and aborts even when granted.
        set_effect_grants(Vec::<String>::new());
        assert!(eval_body(&unary("panic"), &[nat(1)]).is_err()); // ungranted
        clear_effects();
        set_effect_grants(vec!["panic".to_string()]);
        assert!(eval_body(&unary("panic"), &[nat(1)]).is_err()); // granted but aborts
        clear_effects();
    }

    #[test]
    fn replicate_is_a_gated_alloc_effect() {
        // \n -> replicate(n, 7): ungranted rejected; granted allocates a list and traces `alloc`.
        let body = json!({ "kind": "lambda", "params": [{ "name": "n" }],
            "body": { "kind": "app", "fn": { "kind": "var", "name": "replicate" },
                "args": [{ "kind": "var", "name": "n" }, { "kind": "lit", "value": { "kind": "int", "value": 7 } }] } });

        // Ungranted: alloc is rejected at eval time.
        set_effect_grants(Vec::<String>::new());
        assert!(eval_body(&body, &[nat(3)]).is_err(), "replicate must be rejected without the alloc grant");
        clear_effects();

        // Granted: allocates [7,7,7] and records one `alloc` effect carrying the requested size.
        set_effect_grants(vec!["alloc".to_string()]);
        let got = eval_body(&body, &[nat(3)]).unwrap();
        assert_eq!(got, json!({ "kind": "list", "elems": [
            { "kind": "int", "value": 7 }, { "kind": "int", "value": 7 }, { "kind": "int", "value": 7 }] }));
        let trace = take_effect_trace();
        clear_effects();
        assert_eq!(trace[0]["effect"], "alloc");
        assert_eq!(trace[0]["detail"]["size"], "3");

        // Negative size allocates nothing but still performs the effect.
        set_effect_grants(vec!["alloc".to_string()]);
        let empty = eval_body(&body, &[json!({ "kind": "int", "value": -2 })]).unwrap();
        assert_eq!(empty, json!({ "kind": "list", "elems": [] }));
        clear_effects();
    }

    #[test]
    fn http_response_decoding_dechunks() {
        // A plain (non-chunked) response: status parsed off the status line, body returned
        // verbatim past the header separator.
        let plain = b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 5\r\n\r\nhello";
        assert_eq!(super::decode_http_response_full(plain).unwrap(), (200, "hello".to_string()));

        // A chunked response: "Wiki" + "pedia" + " in chunks." across three chunks, then a 0-chunk.
        let chunked = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n\
                        4\r\nWiki\r\n5\r\npedia\r\nE\r\n in\r\n\r\nchunks.\r\n0\r\n\r\n";
        assert_eq!(super::decode_http_response_full(chunked).unwrap(), (200, "Wikipedia in\r\n\r\nchunks.".to_string()));

        // Chunk-size extensions (after ';') are ignored; the size still governs.
        let ext = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3;foo=bar\r\nabc\r\n0\r\n\r\n";
        assert_eq!(super::decode_http_response_full(ext).unwrap(), (200, "abc".to_string()));

        // Header match is case-insensitive (HTTP header names/values are not case-sensitive here).
        let mixed = b"HTTP/1.1 200 OK\r\ntransfer-encoding: Chunked\r\n\r\n2\r\nhi\r\n0\r\n\r\n";
        assert_eq!(super::decode_http_response_full(mixed).unwrap(), (200, "hi".to_string()));

        // Non-200 statuses come through — the piece a mutating workflow verifies against.
        let created = b"HTTP/1.1 201 Created\r\nContent-Length: 0\r\n\r\n";
        assert_eq!(super::decode_http_response_full(created).unwrap(), (201, String::new()));
        let missing = b"HTTP/1.1 404 Not Found\r\n\r\ngone";
        assert_eq!(super::decode_http_response_full(missing).unwrap(), (404, "gone".to_string()));
    }

    #[test]
    fn general_http_builtin_gates_by_method_and_host_and_replays() {
        let s = |v: &str| json!({ "kind": "string", "value": v });
        // \m u h b -> http m u h b
        let http_body = json!({ "kind": "lambda",
            "params": [{ "name": "m" }, { "name": "u" }, { "name": "h" }, { "name": "b" }],
            "body": { "kind": "app", "fn": { "kind": "var", "name": "http" },
                      "args": [{ "kind": "var", "name": "m" }, { "kind": "var", "name": "u" },
                               { "kind": "var", "name": "h" }, { "kind": "var", "name": "b" }] } });
        let no_headers = json!({ "kind": "map", "entries": [] });

        // A mutating method under a net.read-only grant is rejected BEFORE any I/O — the method,
        // not the builtin name, decides the effect.
        set_effect_grants(vec!["net.read".to_string()]);
        let err = eval_body(&http_body, &[s("DELETE"), s("http://h.example/x"), no_headers.clone(), s("")])
            .unwrap_err()
            .to_string();
        assert!(err.contains("net.write"), "method decides the effect: {err}");
        clear_effects();

        // A host-scoped grant permits its host's effect check but not another host's. (Both die at
        // connect time for the granted host — the gate is what's under test, so assert the failure
        // is NOT the gate for the matching host and IS the gate for the other.)
        set_effect_grants(vec!["net.write@h.example".to_string()]);
        let gate_err = eval_body(&http_body, &[s("PUT"), s("http://other.example/x"), no_headers.clone(), s("")])
            .unwrap_err()
            .to_string();
        assert!(gate_err.contains("ungranted effect"), "other host must be gated: {gate_err}");
        assert!(gate_err.contains("other.example"), "gate names the offending host: {gate_err}");
        clear_effects();

        // Replay: the recorded response reproduces without any network or grants; the recorded
        // result is the {status, body} record.
        let resp = json!({ "kind": "record", "fields": [
            { "name": "body", "value": { "kind": "string", "value": "made" } },
            { "name": "status", "value": { "kind": "int", "value": 201 } }] });
        set_effect_replay(vec![json!({ "effect": "net.write", "detail": {}, "result": resp })]);
        let out = eval_body(&http_body, &[s("PUT"), s("http://h.example/x"), no_headers, s("{}")]).unwrap();
        assert_eq!(out, resp);
        clear_effects();
    }

    #[test]
    fn grant_scopes_match_segment_aligned() {
        use super::{grant_permits, url_scope};
        // Bare grant: anywhere, but only its own effect.
        assert!(grant_permits("net.read", "net.read", Some("h.example/v1/x")));
        assert!(!grant_permits("net.read", "net.write", Some("h.example/v1/x")));
        // Host-scoped: any path on the host, no other host.
        assert!(grant_permits("net.write@h.example", "net.write", Some("h.example")));
        assert!(grant_permits("net.write@h.example", "net.write", Some("h.example/v0/things")));
        assert!(!grant_permits("net.write@h.example", "net.write", Some("other.example/v0")));
        // Path-scoped: only under the path, segment-aligned — /v0 covers /v0/things, not /v0things.
        assert!(grant_permits("net.write@h.example/v0", "net.write", Some("h.example/v0")));
        assert!(grant_permits("net.write@h.example/v0", "net.write", Some("h.example/v0/things")));
        assert!(!grant_permits("net.write@h.example/v0", "net.write", Some("h.example/v0things")));
        assert!(!grant_permits("net.write@h.example/v0", "net.write", Some("h.example")));
        // A trailing slash on the grant is tolerated; a scoped grant never matches scopeless effects.
        assert!(grant_permits("net.write@h.example/v0/", "net.write", Some("h.example/v0/x")));
        assert!(!grant_permits("net.write@h.example", "net.write", None));
        // fs scoping: the file path is the scope.
        assert!(grant_permits("fs.read@/data", "fs.read", Some("/data/in.txt")));
        assert!(!grant_permits("fs.read@/data", "fs.read", Some("/database/in.txt")));

        // url_scope: host for a bare URL, host/path otherwise; query/fragment/port stripped.
        assert_eq!(url_scope("https://h.example").unwrap(), "h.example");
        assert_eq!(url_scope("https://h.example/").unwrap(), "h.example");
        assert_eq!(url_scope("http://h.example:8080/v0/things?page=2#f").unwrap(), "h.example/v0/things");
    }

    #[test]
    fn path_scoped_grant_gates_the_http_builtin() {
        let s = |v: &str| json!({ "kind": "string", "value": v });
        let http_body = json!({ "kind": "lambda",
            "params": [{ "name": "m" }, { "name": "u" }, { "name": "h" }, { "name": "b" }],
            "body": { "kind": "app", "fn": { "kind": "var", "name": "http" },
                      "args": [{ "kind": "var", "name": "m" }, { "kind": "var", "name": "u" },
                               { "kind": "var", "name": "h" }, { "kind": "var", "name": "b" }] } });
        let no_headers = json!({ "kind": "map", "entries": [] });

        // A path outside the granted prefix dies at the gate — before any I/O — and the error
        // names the offending scope.
        set_effect_grants(vec!["net.write@h.example/v0/things".to_string()]);
        let err = eval_body(&http_body, &[s("PUT"), s("http://h.example/v0/other"), no_headers.clone(), s("")])
            .unwrap_err()
            .to_string();
        assert!(err.contains("ungranted effect"), "outside the path prefix must be gated: {err}");
        assert!(err.contains("h.example/v0/other"), "gate names the offending scope: {err}");
        clear_effects();

        // Inside the prefix the gate passes (the failure is at connect time, not the gate).
        set_effect_grants(vec!["net.write@h.example/v0/things".to_string()]);
        let err = eval_body(&http_body, &[s("PUT"), s("http://h.example/v0/things/t1"), no_headers, s("")])
            .unwrap_err()
            .to_string();
        assert!(!err.contains("ungranted effect"), "inside the prefix the gate is not the failure: {err}");
        clear_effects();
    }

    #[test]
    fn http_response_header_decoding_is_canonical() {
        // Names lowercase, values OWS-trimmed; duplicates comma-join in arrival order.
        let raw = b"HTTP/1.1 302 Found\r\nLocation:  /v0/things/th_1 \r\nX-Tag: a\r\nx-tag: b\r\nContent-Length: 0\r\n\r\n";
        let (status, headers, body) = super::decode_http_response_parts(raw).unwrap();
        assert_eq!(status, 302);
        assert_eq!(body, "");
        assert_eq!(headers.get("location").map(String::as_str), Some("/v0/things/th_1"));
        assert_eq!(headers.get("x-tag").map(String::as_str), Some("a, b"));
        assert!(!headers.contains_key("Location"), "names are canonically lowercase");

        // A chunked response still surfaces its headers (and the de-chunked body).
        let chunked = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nETag: \"v7\"\r\n\r\n2\r\nhi\r\n0\r\n\r\n";
        let (status, headers, body) = super::decode_http_response_parts(chunked).unwrap();
        assert_eq!((status, body.as_str()), (200, "hi"));
        assert_eq!(headers.get("etag").map(String::as_str), Some("\"v7\""));

        // A header-less malformed response yields an empty map, not an error.
        let bare = b"HTTP/1.1 200 OK";
        let (_, headers, _) = super::decode_http_response_parts(bare).unwrap();
        assert!(headers.is_empty());
    }

    #[test]
    fn http_full_builtin_surfaces_headers_and_replays() {
        let s = |v: &str| json!({ "kind": "string", "value": v });
        // \m u h b -> http_full m u h b
        let body = json!({ "kind": "lambda",
            "params": [{ "name": "m" }, { "name": "u" }, { "name": "h" }, { "name": "b" }],
            "body": { "kind": "app", "fn": { "kind": "var", "name": "http_full" },
                      "args": [{ "kind": "var", "name": "m" }, { "kind": "var", "name": "u" },
                               { "kind": "var", "name": "h" }, { "kind": "var", "name": "b" }] } });
        let no_headers = json!({ "kind": "map", "entries": [] });

        // Same method-decided gate as `http`: a mutating method under net.read-only is rejected.
        set_effect_grants(vec!["net.read".to_string()]);
        let err = eval_body(&body, &[s("POST"), s("http://h.example/x"), no_headers.clone(), s("")])
            .unwrap_err()
            .to_string();
        assert!(err.contains("net.write"), "method decides the effect: {err}");
        clear_effects();

        // Live against a local listener: the response headers survive into the result record —
        // the Location a server-assigned-identity workflow projects from.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let mut req = Vec::new();
            loop {
                let n = std::io::Read::read(&mut conn, &mut buf).unwrap();
                req.extend_from_slice(&buf[..n]);
                if req.windows(4).any(|w| w == b"\r\n\r\n") || n == 0 {
                    break;
                }
            }
            std::io::Write::write_all(
                &mut conn,
                b"HTTP/1.1 201 Created\r\nLocation: /v0/things/th_9\r\nContent-Length: 2\r\n\r\nok",
            )
            .unwrap();
        });
        set_effect_grants(vec!["net.write@127.0.0.1".to_string()]);
        let out = eval_body(&body, &[s("POST"), s(&format!("http://{addr}/v0/things")), no_headers.clone(), s("{}")])
            .unwrap();
        assert_eq!(out.pointer("/fields/2/value/value"), Some(&json!(201)), "status field: {out}");
        let headers_entries = out.pointer("/fields/1/value/entries").and_then(|e| e.as_array()).unwrap().clone();
        let loc = headers_entries
            .iter()
            .find(|e| e["key"] == "location")
            .and_then(|e| e.pointer("/value/value"))
            .cloned();
        assert_eq!(loc, Some(json!("/v0/things/th_9")), "location header survives: {out}");
        let trace = take_effect_trace();
        clear_effects();
        assert_eq!(trace.len(), 1);
        assert_eq!(trace[0]["effect"], "net.write");

        // Replay: the recorded {status, headers, body} record reproduces grantless offline.
        let resp = trace[0]["result"].clone();
        set_effect_replay(vec![json!({ "effect": "net.write", "detail": {}, "result": resp })]);
        let replayed =
            eval_body(&body, &[s("POST"), s(&format!("http://{addr}/v0/things")), no_headers, s("{}")]).unwrap();
        assert_eq!(replayed, resp);
        clear_effects();
    }

    #[test]
    fn http_secret_placeholders_substitute_at_the_boundary_only() {
        // Substitution succeeds only for supplied secrets, and composes around literal text.
        super::set_effect_secrets(vec![("tok".to_string(), "s3cr3t".to_string())]);
        assert_eq!(super::substitute_secrets("Bearer {{secret:tok}}").unwrap(), "Bearer s3cr3t");
        assert_eq!(super::substitute_secrets("no placeholders").unwrap(), "no placeholders");
        let missing = super::substitute_secrets("{{secret:absent}}").unwrap_err().to_string();
        assert!(missing.contains("absent"), "{missing}");
        clear_effects();

        // The trace records the SYMBOLIC header value — a live `http` call's detail is built from
        // the pre-substitution headers, so the secret value never enters the trace. Exercised
        // against a real local listener that answers 204 with no body.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let mut req = Vec::new();
            loop {
                let n = std::io::Read::read(&mut conn, &mut buf).unwrap();
                req.extend_from_slice(&buf[..n]);
                if req.windows(4).any(|w| w == b"\r\n\r\n") || n == 0 {
                    break;
                }
            }
            std::io::Write::write_all(&mut conn, b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n").unwrap();
            String::from_utf8_lossy(&req).into_owned()
        });

        let s = |v: &str| json!({ "kind": "string", "value": v });
        let http_body = json!({ "kind": "lambda",
            "params": [{ "name": "m" }, { "name": "u" }, { "name": "h" }, { "name": "b" }],
            "body": { "kind": "app", "fn": { "kind": "var", "name": "http" },
                      "args": [{ "kind": "var", "name": "m" }, { "kind": "var", "name": "u" },
                               { "kind": "var", "name": "h" }, { "kind": "var", "name": "b" }] } });
        let headers = json!({ "kind": "map", "entries": [
            { "key": "Authorization", "value": { "kind": "string", "value": "Bearer {{secret:tok}}" } }] });
        set_effect_grants(vec!["net.write".to_string()]);
        super::set_effect_secrets(vec![("tok".to_string(), "s3cr3t".to_string())]);
        let url = format!("http://127.0.0.1:{}/x", addr.port());
        let out = eval_body(&http_body, &[s("POST"), s(&url), headers, s("payload")]).unwrap();
        assert_eq!(out.pointer("/fields/1/value/value").and_then(|v| v.as_i64()), Some(204));

        let trace = take_effect_trace();
        clear_effects();
        let trace_text = trace[0].to_string();
        assert!(trace_text.contains("{{secret:tok}}"), "trace keeps the placeholder: {trace_text}");
        assert!(!trace_text.contains("s3cr3t"), "the secret value must NOT enter the trace");

        // The wire saw the real value (the substitution happened inside the live effect).
        let wire = server.join().unwrap();
        assert!(wire.contains("Authorization: Bearer s3cr3t"), "wire request: {wire}");
        assert!(wire.contains("POST /x"), "wire request: {wire}");
    }

    #[test]
    fn fs_read_write_are_gated_and_replayable() {
        let path = std::env::temp_dir().join("nl-fs-roundtrip.txt");
        let path_s = path.to_str().unwrap().to_string();
        let s = |v: &str| json!({ "kind": "string", "value": v });

        let write_body = json!({ "kind": "lambda", "params": [{ "name": "p" }, { "name": "c" }],
            "body": { "kind": "app", "fn": { "kind": "var", "name": "write_file" },
                      "args": [{ "kind": "var", "name": "p" }, { "kind": "var", "name": "c" }] } });
        let read_body = json!({ "kind": "lambda", "params": [{ "name": "p" }],
            "body": { "kind": "app", "fn": { "kind": "var", "name": "read_file" }, "args": [{ "kind": "var", "name": "p" }] } });

        // Real write (gated fs.write).
        set_effect_grants(vec!["fs.write".to_string()]);
        eval_body(&write_body, &[s(&path_s), s("hello fs")]).unwrap();
        clear_effects();

        // Ungranted read → rejected; granted read → real contents, with a recorded `result`.
        set_effect_grants(Vec::<String>::new());
        assert!(eval_body(&read_body, &[s(&path_s)]).is_err());
        clear_effects();
        set_effect_grants(vec!["fs.read".to_string()]);
        assert_eq!(eval_body(&read_body, &[s(&path_s)]).unwrap(), s("hello fs"));
        let trace = take_effect_trace();
        clear_effects();
        assert_eq!(trace[0]["effect"], "fs.read");
        assert_eq!(trace[0]["result"], s("hello fs"));

        // REPLAY: reading a nonexistent path reproduces the recorded contents — no real I/O.
        set_effect_replay(trace);
        assert_eq!(eval_body(&read_body, &[s("/no/such/path")]).unwrap(), s("hello fs"));
        clear_effects();

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn spawn_and_net_effects_gate_trace_and_replay() {
        let s = |v: &str| json!({ "kind": "string", "value": v });

        // process.spawn: real `echo hi` -> "hi\n"; ungranted rejected; replay reproduces (no spawn).
        let spawn_body = json!({ "kind": "lambda", "params": [{ "name": "c" }, { "name": "a" }],
            "body": { "kind": "app", "fn": { "kind": "var", "name": "spawn" },
                      "args": [{ "kind": "var", "name": "c" }, { "kind": "var", "name": "a" }] } });
        let args = json!({ "kind": "list", "elems": [s("hi")] });
        set_effect_grants(Vec::<String>::new());
        assert!(eval_body(&spawn_body, &[s("echo"), args.clone()]).is_err());
        clear_effects();
        set_effect_grants(vec!["process.spawn".to_string()]);
        let out = eval_body(&spawn_body, &[s("echo"), args.clone()]).unwrap();
        let trace = take_effect_trace();
        clear_effects();
        assert_eq!(out, s("hi\n"));
        assert_eq!(trace[0]["effect"], "process.spawn");
        set_effect_replay(trace);
        assert_eq!(eval_body(&spawn_body, &[s("/no/such/cmd"), args]).unwrap(), s("hi\n"));
        clear_effects();

        // net.read: an unsupported scheme is rejected (http:// and https:// are the only ones);
        // replay returns the recorded body without any network (so http:// and https:// alike replay).
        let get_body = json!({ "kind": "lambda", "params": [{ "name": "u" }],
            "body": { "kind": "app", "fn": { "kind": "var", "name": "http_get" }, "args": [{ "kind": "var", "name": "u" }] } });
        set_effect_grants(vec!["net.read".to_string()]);
        assert!(eval_body(&get_body, &[s("ftp://example.com")]).is_err());
        clear_effects();
        set_effect_replay(vec![json!({ "effect": "net.read", "detail": { "url": "http://x" }, "result": s("BODY") })]);
        assert_eq!(eval_body(&get_body, &[s("http://anything")]).unwrap(), s("BODY"));
        clear_effects();
    }

    #[test]
    fn http_oauth_placeholder_exchanges_and_caches_at_the_boundary() {
        // GW13: `{{oauth:NAME}}` resolves to a live client-credentials token — fetched from the
        // identity's token endpoint inside the live effect, cached per evaluation, symbolic in the
        // trace, and never needed on replay. One local listener plays token endpoint AND api:
        // exactly ONE token exchange must occur across two api calls (the cache), so it accepts
        // three connections total.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let mut transcripts = Vec::new();
            for i in 0..3 {
                let (mut conn, _) = listener.accept().unwrap();
                let mut buf = [0u8; 4096];
                let mut req = Vec::new();
                loop {
                    let n = std::io::Read::read(&mut conn, &mut buf).unwrap();
                    req.extend_from_slice(&buf[..n]);
                    if n == 0 {
                        break;
                    }
                    // Headers seen; read the body per Content-Length if present.
                    if let Some(pos) = req.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&req[..pos]).into_owned();
                        let want: usize = head
                            .lines()
                            .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse().unwrap()))
                            .unwrap_or(0);
                        if req.len() >= pos + 4 + want {
                            break;
                        }
                    }
                }
                let text = String::from_utf8_lossy(&req).into_owned();
                let reply: &[u8] = if i == 0 {
                    b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 28\r\n\r\n{\"access_token\":\"tok-abc123\"}"
                } else {
                    b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n"
                };
                std::io::Write::write_all(&mut conn, reply).unwrap();
                transcripts.push(text);
            }
            transcripts
        });

        let s = |v: &str| json!({ "kind": "string", "value": v });
        let http_body = json!({ "kind": "lambda",
            "params": [{ "name": "m" }, { "name": "u" }, { "name": "h" }, { "name": "b" }],
            "body": { "kind": "app", "fn": { "kind": "var", "name": "http" },
                      "args": [{ "kind": "var", "name": "m" }, { "kind": "var", "name": "u" },
                               { "kind": "var", "name": "h" }, { "kind": "var", "name": "b" }] } });
        let headers = json!({ "kind": "map", "entries": [
            { "key": "Authorization", "value": { "kind": "string", "value": "Bearer {{oauth:svc}}" } }] });
        set_effect_grants(vec!["net.read".to_string()]);
        super::set_effect_oauth(vec![("svc".to_string(), super::OAuthConfig {
            token_url: format!("http://127.0.0.1:{}/token", addr.port()),
            client_id: "client&id".to_string(), // reserved chars must be form-encoded
            client_secret: "s=cr/t".to_string(),
        })]);
        let url = format!("http://127.0.0.1:{}/api", addr.port());
        let out = eval_body(&http_body, &[s("GET"), s(&url), headers.clone(), s("")]).unwrap();
        assert_eq!(out.pointer("/fields/1/value/value").and_then(|v| v.as_i64()), Some(204));
        // Second call: served from the token cache — no second exchange.
        let out2 = eval_body(&http_body, &[s("GET"), s(&url), headers, s("")]).unwrap();
        assert_eq!(out2.pointer("/fields/1/value/value").and_then(|v| v.as_i64()), Some(204));

        let trace = take_effect_trace();
        clear_effects();
        let trace_text = serde_json::Value::Array(trace.clone()).to_string();
        assert!(trace_text.contains("{{oauth:svc}}"), "trace keeps the placeholder: {trace_text}");
        assert!(!trace_text.contains("tok-abc123"), "the token must NOT enter the trace");
        assert_eq!(trace.len(), 2, "the token exchange is credential machinery, not a traced effect");

        let transcripts = server.join().unwrap();
        assert!(transcripts[0].contains("POST /token"), "{}", transcripts[0]);
        assert!(transcripts[0].contains("grant_type=client_credentials"), "{}", transcripts[0]);
        assert!(transcripts[0].contains("client_id=client%26id"), "form-encoded id: {}", transcripts[0]);
        assert!(transcripts[0].contains("client_secret=s%3Dcr%2Ft"), "form-encoded secret: {}", transcripts[0]);
        assert!(transcripts[1].contains("Authorization: Bearer tok-abc123"), "{}", transcripts[1]);
        assert!(transcripts[2].contains("Authorization: Bearer tok-abc123"), "{}", transcripts[2]);

        // Unsupplied identity: refused by name. Replay: the recorded result comes back with NO
        // identity installed at all (the trace is sufficient — credentials never needed again).
        let missing = super::substitute_oauth("{{oauth:absent}}").unwrap_err().to_string();
        assert!(missing.contains("absent"), "{missing}");
        set_effect_replay(trace);
        let replayed = eval_body(&http_body, &[s("GET"), s("http://gone.example/api"),
            json!({ "kind": "map", "entries": [] }), s("")]).unwrap();
        assert_eq!(replayed.pointer("/fields/1/value/value").and_then(|v| v.as_i64()), Some(204));
        clear_effects();
    }

    #[test]
    fn example_with_trace_replays_grantlessly() {
        // GW12: an effectful example carrying a `trace` reference runs by REPLAY through the
        // installed resolver — no grants, no live effect — so a consumer checks it offline.
        let s = |v: &str| json!({ "kind": "string", "value": v });
        let body = json!({ "kind": "lambda", "params": [{ "name": "msg" }],
            "body": { "kind": "app", "fn": { "kind": "var", "name": "print" },
                      "args": [{ "kind": "var", "name": "msg" }] } });
        let trace = json!({ "kind": "trace", "version": "0.1.0",
            "ops": [{ "effect": "io.console", "detail": s("hi"), "result": { "kind": "unit" } }] });
        let trc = crate::hash_artifact_with_kind(&trace, crate::ArtifactKind::Trace).unwrap();

        clear_effects(); // the consumer grants NOTHING
        set_resolver([(trc.clone(), trace)].into());

        // Without the trace reference the effectful example cannot run grantlessly…
        let live = json!({ "examples": [{ "args": [s("hi")], "result": { "kind": "unit" } }] });
        let runs = run_examples(&live, &body).unwrap();
        assert!(!runs[0].passed);
        assert!(runs[0].error.as_deref().unwrap_or("").contains("ungranted"), "{:?}", runs[0].error);

        // …with it, the example replays and passes.
        let rec = json!({ "examples": [{ "args": [s("hi")], "result": { "kind": "unit" }, "trace": trc.clone() }] });
        let runs = run_examples(&rec, &body).unwrap();
        assert!(runs[0].passed, "{:?}", runs[0]);

        // A trace with observations the example never used does not correspond to it.
        let fat = json!({ "kind": "trace", "version": "0.1.0",
            "ops": [{ "effect": "io.console", "detail": s("hi"), "result": { "kind": "unit" } },
                    { "effect": "io.console", "detail": s("stray"), "result": { "kind": "unit" } }] });
        let fat_addr = crate::hash_artifact_with_kind(&fat, crate::ArtifactKind::Trace).unwrap();
        set_resolver([(fat_addr.clone(), fat)].into());
        let rec = json!({ "examples": [{ "args": [s("hi")], "result": { "kind": "unit" }, "trace": fat_addr }] });
        let runs = run_examples(&rec, &body).unwrap();
        assert!(!runs[0].passed);
        assert!(runs[0].error.as_deref().unwrap_or("").contains("not fully consumed"), "{:?}", runs[0].error);

        // A missing trace is an honest per-example error naming the address, and a later pure
        // example still runs (no replay leaks across examples).
        let gone = "trc_".to_string() + &"0".repeat(64);
        let pure_body = json!({ "kind": "lambda", "params": [{ "name": "x" }],
            "body": { "kind": "var", "name": "x" } });
        let rec = json!({ "examples": [
            { "args": [s("hi")], "result": { "kind": "unit" }, "trace": gone },
            { "args": [s("ok")], "result": s("ok") } ] });
        let runs = run_examples(&rec, &pure_body).unwrap();
        assert!(runs[0].error.as_deref().unwrap_or("").contains("trc_"), "{:?}", runs[0].error);
        assert!(runs[1].passed, "{:?}", runs[1]);
        clear_resolver();
        clear_effects();
    }
}
