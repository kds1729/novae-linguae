//! `nl-validator`: reference CLI for validating Novae Linguae artifacts.

use anyhow::{anyhow, bail, Result};
use clap::{Parser, Subcommand, ValueEnum};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// CLI-facing artifact-kind selector for `--kind` flags. Mirrors
/// `nl_validator::ArtifactKind`.
#[derive(Clone, Copy, Debug, ValueEnum)]
enum CliKind {
    /// Function record (top-level `fn_<hex>` hash, strips `hash` before hashing).
    FunctionRecord,
    /// Nova Locutio message (top-level `msg_<hex>` hash, strips `hash` and
    /// `signature` before hashing; Ed25519 signature covers the hash).
    Message,
    /// Body expression (top-level `expr_<hex>` hash, nothing stripped).
    Body,
    /// Weights pointer record (top-level `wgt_<hex>` hash, strips `hash` before hashing).
    Weights,
    /// Signed eval attestation (top-level `evl_<hex>` hash, strips `hash` and `signature`;
    /// Ed25519 signature covers the hash).
    EvalAttestation,
}

impl From<CliKind> for nl_validator::ArtifactKind {
    fn from(k: CliKind) -> Self {
        match k {
            CliKind::FunctionRecord => Self::FunctionRecord,
            CliKind::Message => Self::Message,
            CliKind::Body => Self::BodyExpression,
            CliKind::Weights => Self::Weights,
            CliKind::EvalAttestation => Self::EvalAttestation,
        }
    }
}

#[derive(Parser)]
#[command(
    name = "nl-validator",
    version,
    about = "Reference validator/canonicalizer/hasher for Novae Linguae artifacts"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Validate a JSON instance against a JSON Schema (draft 2020-12).
    Validate {
        /// Path to the JSON Schema (e.g. spec/function-record.schema.json)
        schema: PathBuf,
        /// Path to the JSON instance to validate
        record: PathBuf,
    },
    /// JCS-canonicalize a JSON document (RFC 8785). Writes canonical UTF-8 bytes
    /// to stdout with no trailing newline.
    Canonicalize {
        /// Path to the JSON document
        record: PathBuf,
    },
    /// Compute the content-hash of a Novae Linguae artifact. Auto-detects
    /// whether the input is a function record, a Nova Locutio message, or a
    /// body expression — pass `--kind` to override the detection. Strips the
    /// kind-appropriate fields per spec/canonical-serialization.md, JCS-
    /// canonicalizes, and BLAKE3-256 hashes. Prints `<prefix>_<hex>` to
    /// stdout followed by a newline.
    Hash {
        /// Path to the artifact
        record: PathBuf,
        /// Override the artifact-kind auto-detection.
        #[arg(long, value_enum)]
        kind: Option<CliKind>,
    },
    /// Verify an artifact end-to-end. For function records: hash check only.
    /// For Nova Locutio messages: hash check AND Ed25519 signature check.
    /// For body expressions: refused — body expressions don't carry a stored
    /// `hash` field to verify against (use `hash` to compute, compare
    /// externally). Exit code 0 on PASS, 1 on FAIL.
    Verify {
        /// Path to the artifact
        record: PathBuf,
        /// Override the artifact-kind auto-detection.
        #[arg(long, value_enum)]
        kind: Option<CliKind>,
    },
    /// Sign a Nova Locutio message with a deterministic Ed25519 key derived
    /// from the given seed. Sets `from` to the resulting did:nova: DID,
    /// recomputes `hash`, and writes a real Ed25519 signature into the
    /// `signature` field. Writes the signed message to stdout, or back to
    /// the input file when `--in-place` is given.
    Sign {
        /// Path to the message to sign
        record: PathBuf,
        /// Seed string used to derive the signing key deterministically.
        /// Same seed = same key = same signature.
        #[arg(long)]
        seed: String,
        /// Write the signed message back to the input file rather than to
        /// stdout.
        #[arg(long)]
        in_place: bool,
    },
    /// Run well-formedness checks on a Nova Lingua type expression. Catches
    /// what JSON Schema cannot express on its own: type-variable scoping
    /// (vars bound by an enclosing forall), rank-1 polymorphism (no nested
    /// forall), uniqueness of record fields and sum variant tags, and
    /// `apply.ctor` being an actual type constructor.
    CheckType {
        /// Path to the type-expression document
        record: PathBuf,
    },
    /// Run well-formedness checks on a Nova Lingua predicate expression.
    /// Catches what JSON Schema cannot express: arity of known built-in
    /// operators (not/1, and/2, eq/2, foldl/3, …). Unknown ops (content-
    /// address refs, scope variables) are not checked here.
    CheckPredicate {
        /// Path to the predicate-expression document
        record: PathBuf,
    },
    /// Run well-formedness checks on a Nova Lingua value expression. Catches
    /// what JSON Schema cannot express: record field name uniqueness.
    CheckValue {
        /// Path to the value-expression document
        record: PathBuf,
    },
    /// Run well-formedness checks on a Nova Lingua body expression. Catches
    /// what JSON Schema cannot express: lambda parameter name uniqueness, and
    /// literal value well-formedness (`lit.value` must satisfy check-value).
    CheckBody {
        /// Path to the body-expression document
        record: PathBuf,
    },
    /// Evaluate a function record's algebraic `properties[]` against its worked
    /// `examples[]`. Binds `result` and `arg0..argN` from each example and
    /// evaluates each property's predicate three-valued: CONTRADICTED (false on
    /// some example — exit 1), UNVERIFIABLE (needs runtime / re-applying an
    /// unknown function, e.g. map/filter/fold/compose, or a quantifier), or
    /// CONSISTENT (true on ≥1, false on none — not a proof). Exit 0 unless a
    /// property is contradicted.
    CheckProperties {
        /// Path to the function record
        record: PathBuf,
        /// Optional body-expression AST: verify properties by *running* (decides
        /// self / map / filter / fold / compose / apply and forall over the
        /// examples) instead of the static example evaluator.
        #[arg(long)]
        body: Option<PathBuf>,
        /// Additionally run a GENERATIVE pass: sample inputs for each property's quantified
        /// variables, run the body, and report HELD / REFUTED (with a shrunk counterexample) /
        /// UNGENERATABLE. A refuted property fails (exit 1). Deterministic (principle 5); most
        /// useful with --body.
        #[arg(long)]
        generate: bool,
        /// Cases to sample per property when --generate is set.
        #[arg(long, default_value_t = 100)]
        cases: usize,
    },
    /// Evaluate a Nova Lingua body-expression AST and apply it to zero or more
    /// argument values, printing the resulting value AST. This *executes* the
    /// body (a tree-walking evaluator over the v0.1 body schema: closures,
    /// currying, `case`, `let`, field projection, and a small builtin library
    /// incl. map/filter/fold/compose). See `spec/body-expression.schema.json`.
    Eval {
        /// Path to the body-expression JSON AST.
        body: PathBuf,
        /// Argument value (a value-expression JSON file). Repeatable, positional order.
        #[arg(long = "arg")]
        args: Vec<PathBuf>,
        /// Grant an effect the body may perform (e.g. `io.console`, `random`, `fs.read`, `fs.write`,
        /// `net.write@api.example.com` — a net grant may be HOST-scoped with `@host`).
        /// Repeatable. An effectful builtin whose effect is not granted is rejected at eval time;
        /// pure bodies need no grants. The performed effects are printed as a trace.
        #[arg(long = "grant")]
        grants: Vec<String>,
        /// Supply a named secret (`NAME=VALUE`) for `{{secret:NAME}}` placeholders in `http` header
        /// values. Substituted only at the effect boundary: the value never enters the trace (the
        /// trace keeps the placeholder) and replay needs no secrets at all. Repeatable.
        #[arg(long = "secret")]
        secrets: Vec<String>,
        /// Supply an OAuth2 client-credentials identity (`NAME=token_url|client_id|client_secret`)
        /// for `{{oauth:NAME}}` placeholders in `http` header values. The access token is fetched
        /// from the token endpoint inside the live effect boundary (cached per evaluation) — like a
        /// secret it never enters a record or the trace, and replay needs no identity. Repeatable.
        #[arg(long = "oauth")]
        oauth: Vec<String>,
        /// Replay a recorded effect trace (a trace artifact from --trace-out, or a legacy bare
        /// JSON array): effectful builtins return their recorded results instead of performing
        /// real I/O — deterministic re-execution (P5).
        #[arg(long)]
        replay: Option<PathBuf>,
        /// Write the recorded effect trace here as a first-class trace ARTIFACT ({kind: "trace",
        /// version, ops: [{effect, detail, result}, …]}, spec/trace.schema.json) instead of dumping
        /// events to stderr — feed it back to a later `--replay`, publish it to the commons, or
        /// reference it from an `observed` claim by its trc_… content-address.
        #[arg(long = "trace-out")]
        trace_out: Option<PathBuf>,
        /// Directory of records/bodies to resolve `fn_ref`s against, so a COMPOSED body — one that
        /// applies commons functions by content-address — evaluates (and its trace can be captured)
        /// exactly as `run --records` executes it.
        #[arg(long = "records")]
        records: Option<PathBuf>,
    },
    /// Run a function record's worked `examples[]` through its `body`: bind each
    /// example's args, evaluate the body, and check the result equals the claimed
    /// `result`. Turns the examples into executable tests. Exit 1 if any example
    /// fails (or errors).
    ///
    /// Supply the body with `--body <body.json>`, or with `--records <dir>` to
    /// LINK: resolve the record's `body_hash` from the directory, and resolve any
    /// `fn_ref` arguments to their referenced records' bodies so composites run
    /// end-to-end (e.g. map's example applying `double` by address). Passing BOTH
    /// runs the supplied `--body` while still resolving `fn_ref`s against `--records`
    /// — e.g. grading a hand-written higher-order body whose examples apply a helper.
    Run {
        /// Path to the function record (provides examples).
        record: PathBuf,
        /// Path to the body-expression JSON AST to execute. With --records, the supplied body is run
        /// and fn_refs still resolve against the directory; without it, the body comes from --records.
        #[arg(long)]
        body: Option<PathBuf>,
        /// Directory of records/bodies to link `body_hash` and `fn_ref`s against.
        #[arg(long)]
        records: Option<PathBuf>,
        /// Supply a named secret (`NAME=VALUE`) for `{{secret:NAME}}` placeholders in `http`
        /// header values, so an authenticated record's examples can run. (Effect GRANTS need no
        /// flag here: `run` grants exactly the record's declared `signature.effects` — its
        /// examples are its own tests, and an under-declaring record fails them.) Repeatable.
        #[arg(long = "secret")]
        secrets: Vec<String>,
        /// Supply an OAuth2 client-credentials identity (`NAME=token_url|client_id|client_secret`)
        /// for `{{oauth:NAME}}` placeholders — needed only for LIVE example runs; a trace-carrying
        /// example replays with no identity at all. Repeatable.
        #[arg(long = "oauth")]
        oauth: Vec<String>,
    },
    /// Statically infer a function record's effects from its `body` and check them against the
    /// declared `signature.effects` — the verification counterpart to runtime enforcement (no
    /// execution). Prints SOUND / UNVERIFIABLE (an opaque higher-order/`fn_ref` call could do more) /
    /// UNDER-DECLARED (exit 1 — the body performs an effect the record doesn't declare).
    CheckEffects {
        /// Path to the function record (provides signature.effects).
        record: PathBuf,
        /// Path to the body-expression JSON AST.
        #[arg(long)]
        body: PathBuf,
        /// Directory of records to resolve `fn_ref` callees against, folding in their declared
        /// effects (so a composed body verifies as SOUND rather than reading UNVERIFIABLE).
        #[arg(long)]
        records: Option<PathBuf>,
    },
    /// Type-check a function record's body against its declared `signature.type`
    /// (Hindley-Milner inference; spec/type-expression.schema.json). The second
    /// pillar of "verified by default": confirms the body actually has its
    /// declared type. Exit 1 if ILL-TYPED. The body AST is supplied with `--body`.
    Typecheck {
        /// Path to the function record (provides signature.type).
        record: PathBuf,
        /// Path to the body-expression JSON AST to check.
        #[arg(long)]
        body: PathBuf,
    },
    /// Verify a body satisfies the **refinement implied by its declared type** — currently the `nat`
    /// refinement. A `nat` is a non-negative `int`, which the typechecker erases to `int` and does not
    /// check, so a body declared `… -> nat` that can go negative type-checks clean today. This proves
    /// `∀ params. (∧ nat params ≥ 0) ⟹ body ≥ 0` via the SMT/induction backend. Prints SOUND / VIOLATED
    /// (exit 1 — a counterexample input on which the body goes negative) / UNVERIFIABLE / NOT-APPLICABLE
    /// (the result type is not `nat`). The body AST is supplied with `--body`.
    CheckRefinement {
        /// Path to the function record (provides signature.type).
        record: PathBuf,
        /// Path to the body-expression JSON AST.
        #[arg(long)]
        body: PathBuf,
        /// SMT solver binary to invoke.
        #[arg(long, default_value = "z3")]
        solver: String,
    },
    /// Verify a record's declared `signature.terminates: always` by **structural** analysis (no solver):
    /// a non-recursive first-order body, or one whose every `self`-call descends `tail^k` of a fixed
    /// parameter, provably halts. Prints SOUND (declared `always`, verified), VERIFIED (provably always
    /// but declared weaker — could be strengthened), or UNVERIFIABLE (declared `always`, not provable
    /// here — a higher-order/opaque callee or a non-structural recursion). Sound and conservative: never a
    /// false `always`. The body AST is supplied with `--body`.
    CheckTermination {
        /// Path to the function record (provides signature.terminates).
        record: PathBuf,
        /// Path to the body-expression JSON AST.
        #[arg(long)]
        body: PathBuf,
    },
    /// Verify a record's declared `signature.complexity` (an `O(…)` upper bound) by **structural** cost
    /// analysis (no solver): it infers a sound upper-bound class from the body — non-recursive first-order
    /// bodies are `O(1)`/`O(n)`, and a structural recursion is solved as a recurrence `T(n) = a·T(n−k) + w`
    /// (one self-call with `O(1)`/`O(n)` per-step work → `O(n)`/`O(n²)`, two+ constant-descent calls →
    /// exponential, a halving descent → `O(log n)`/`O(n log n)`) — then compares it to the declared class.
    /// Prints SOUND (the body is within its declared bound), VERIFIED (provably tighter — the declared bound
    /// could be strengthened), UNVERIFIABLE (declared, but the sound structural bound is worse or the body is
    /// opaque/higher-order), N/A (no complexity declared — the inferred bound is reported), or UNKNOWN.
    /// Sound and conservative: a bound can be verified but never refuted. The body AST is supplied with
    /// `--body`.
    CheckComplexity {
        /// Path to the function record (provides signature.complexity).
        record: PathBuf,
        /// Path to the body-expression JSON AST.
        #[arg(long)]
        body: PathBuf,
    },
    /// **Certify** a function record end to end: run every "verified by default" check against the body in
    /// one pass — `typecheck` (type), `check-effects` (effects ⊆ declared), `check-refinement` (the type
    /// `nat` + declared `pre`/`post`), `check-termination` (a declared `terminates: always`), and
    /// `check-complexity` (a declared `complexity` / structured `cost`) — and emit a single verification
    /// verdict. Prints a per-check table; `--json` emits a machine-readable certificate. A record is
    /// **CERTIFIED** unless a check actively *fails* its declaration (ILL-TYPED, an UNDER-DECLARED effect, or
    /// a VIOLATED refinement); conservative UNVERIFIABLE verdicts (a bound/termination the structural
    /// analysis can't confirm) are noted but don't revoke certification. Exit 1 if not certified.
    Certify {
        /// Path to the function record.
        record: PathBuf,
        /// Path to the body-expression JSON AST.
        #[arg(long)]
        body: PathBuf,
        /// Directory of records to resolve `fn_ref` callees against (folds in their declared effects).
        #[arg(long)]
        records: Option<PathBuf>,
        /// SMT solver binary for the refinement check.
        #[arg(long, default_value = "z3")]
        solver: String,
        /// Emit the verification certificate as JSON instead of the human table.
        #[arg(long)]
        json: bool,
        /// Sign the certificate with the deterministic Ed25519 key derived from this seed, producing a
        /// content-addressed, signed **certification** record (a first-class commons artifact). Implies
        /// JSON output. Verify it later with `nl-validator verify`.
        #[arg(long)]
        sign: Option<String>,
        /// Optional ISO-8601 timestamp to stamp into a signed certification (omitted → no timestamp, so the
        /// certificate stays byte-reproducible).
        #[arg(long)]
        timestamp: Option<String>,
    },
    /// Produce a signed **eval attestation** (`evl_…`) about a weights record (spec/weights.md rung 3)
    /// — the weights analogue of `certify --sign`: a certifier's statement of MEASURED capability.
    /// Verifies the weights record's `wgt_` hash first (attest only what you can address), then builds
    /// `{subject, eval, results}` from the supplied descriptor + results JSON and signs it with the
    /// deterministic Ed25519 key derived from the seed. The attestation graph ingests the result as an
    /// `attests-eval` edge; `certified --subject wgt_…` answers whether a trusted certifier attested it.
    AttestWeights {
        /// Path to the weights record (`wgt_…`) being attested.
        record: PathBuf,
        /// Path to the eval descriptor JSON: `{harness, settings?, task_set{sha256?, tasks?, …}}` —
        /// what was measured, precisely enough for an independent re-run.
        #[arg(long)]
        eval: PathBuf,
        /// Path to the measured results JSON (the scores the named harness emitted).
        #[arg(long)]
        results: PathBuf,
        /// Seed for the attesting identity's deterministic Ed25519 key.
        #[arg(long)]
        sign: String,
        /// Optional ISO-8601 timestamp (omitted → no timestamp, so the attestation stays
        /// byte-reproducible).
        #[arg(long)]
        timestamp: Option<String>,
    },
    /// Prove a record's `forall` `properties[]` over the UNBOUNDED domain with an SMT solver — the rung
    /// above bounded `check-properties`. Each property + the function body is translated to SMT-LIB 2
    /// (the Int/Bool fragment); the solver checks the negation of the law. Reports PROVED (unsat — holds
    /// for all inputs), REFUTED (sat — with a counterexample, exit 1), UNKNOWN, UNSUPPORTED (out of the
    /// fragment, e.g. lists/higher-order), or NO-SOLVER. The emitted SMT-LIB is a re-checkable proof
    /// certificate; `--smt-out <dir>` writes one per property.
    Prove {
        /// Path to the function record (provides `properties[]`).
        record: PathBuf,
        /// Body-expression AST of the function under test (required if a property references `self`).
        #[arg(long)]
        body: Option<PathBuf>,
        /// Directory to write each property's SMT-LIB certificate into (created if absent).
        #[arg(long = "smt-out")]
        smt_out: Option<PathBuf>,
        /// SMT solver binary to invoke (must accept SMT-LIB 2 on stdin via `-in`).
        #[arg(long, default_value = "z3")]
        solver: String,
    },
    /// Prove two functions **semantically equivalent** — `∀x. f(x) = g(x)` over the unbounded domain
    /// (the rung above hash equality: behaviorally-identical functions with different content-addresses).
    /// Reuses the property prover: inlines a non-recursive side, or — when both sides recurse — inducts
    /// over the leading list parameter (arity ≤ 2, one spectator threaded through, drawing on the
    /// list-algebra lemma catalog when a step needs it). EQUIVALENT / DISTINCT (with a counterexample,
    /// exit 1) / UNKNOWN / UNSUPPORTED. Any arity ≥ 1 with one side non-recursive, or both-recursive arity ≤ 2.
    Equiv {
        /// Body-expression AST of the first function (a `lambda`).
        #[arg(long = "body-f")]
        body_f: PathBuf,
        /// Body-expression AST of the second function (a `lambda`).
        #[arg(long = "body-g")]
        body_g: PathBuf,
        /// SMT solver binary to invoke.
        #[arg(long, default_value = "z3")]
        solver: String,
    },
    /// Derive a sequential pipeline's **composite metadata** from its stages' signatures (the
    /// "composition opacity" problem): type composability stage-to-stage, the *union* of effects and
    /// capabilities, `always` termination only if every stage is, and the composite complexity —
    /// **precise** (sound under expansion) when every stage carries `cost` metadata, else a coarse max
    /// bound (the `cost-basis` line says which). Each stage is a unary function applied to the previous
    /// stage's result. Exit 1 if not composable.
    Compose {
        /// Function-record files, in pipeline order (`f1 f2 …`; each applied to the previous result).
        #[arg(required = true)]
        records: Vec<PathBuf>,
    },
    /// Goal-directed **assembly** from the commons (spec/agent-loop.md — "assemble, don't write"):
    /// given a GOAL of input→output examples, SEARCH the commons for a pipeline of functions whose
    /// composition reproduces every example (breadth-first, so the shortest pipeline wins; pruned by
    /// `compose`'s type-composability), then VERIFY it — it must `compose`, its synthesized composite
    /// body must run every example through the resolved stages, and with `--require-certified` every
    /// stage must certify. Emits the discovered pipeline and a derived composite record whose body
    /// chains the stages by content-address. Exit 1 if no pipeline within `--max-stages` fits.
    Assemble {
        /// Directory of records + bodies (the local commons view). Exactly one of `--records`/`--node`.
        #[arg(long)]
        records: Option<PathBuf>,
        /// A LIVE commons node URL (e.g. https://nl.1105software.com): the candidate functions are
        /// discovered via the node's `POST /v0/query` and fetched by content-address (every record
        /// and body **re-hashed locally** — the store stays untrusted), then the same search runs.
        #[arg(long)]
        node: Option<String>,
        /// With `--node`: scope the candidate set to functions carrying this intent tag (repeatable,
        /// matched as `any`). Omit to search over ALL of the node's functions.
        #[arg(long)]
        intent: Vec<String>,
        /// With `--node`: cap the candidate set fetched from the node.
        #[arg(long, default_value_t = 200)]
        limit: usize,
        /// Goal file: `{"examples":[{"input":<value-AST | [primary,aux…]>,"output":<value-AST>}, …]}`.
        #[arg(long, required = true)]
        goal: PathBuf,
        /// Maximum pipeline length to search.
        #[arg(long, default_value_t = 3)]
        max_stages: usize,
        /// Certify every stage before assembling ("assemble only from verified parts"); abort if any
        /// stage isn't certified.
        #[arg(long)]
        require_certified: bool,
        /// SMT solver for the certify step.
        #[arg(long, default_value = "z3")]
        solver: String,
        /// Write the derived composite record here (its `body_hash` addresses the composite body,
        /// written alongside as `<expr_…>.json` so the pipeline runs via `run --records`).
        #[arg(long)]
        emit: Option<PathBuf>,
        /// With `--node`: publish the assembled composite (its record AND self-contained inlined
        /// body) back to the node through the verify-then-store gate — closing the loop, so the
        /// whole becomes a first-class commons artifact others can discover and assemble from.
        #[arg(long)]
        publish: bool,
    },
    /// **Cluster** a directory of function records into behavioral-equivalence classes (the rung above
    /// hash equality), proving `∀x. f(x) = g(x)` pairwise within each signature-shape bucket. Prints each
    /// class of ≥ 2 members with its canonical representative (smallest content-address). Follows
    /// `equiv`: any arity ≥ 1 with one side non-recursive, plus both-recursive pairs of arity ≤ 2.
    Cluster {
        /// Directory of records (and their bodies) — the commons view to cluster.
        #[arg(long)]
        records: PathBuf,
        /// SMT solver binary to invoke.
        #[arg(long, default_value = "z3")]
        solver: String,
    },
    /// Rewrite a body-expression AST to its **canonical normal form** via meaning-preserving rewrites
    /// (α-renaming of bound variables, AC ordering of commutative operators, constant folding, identity
    /// elimination). Two functions with equal normal form are equivalent — decided with no solver. Prints
    /// the normal form (its `expr_` content-address with `--hash`). A canonical artifact per equivalence
    /// class, the rung above picking a representative.
    Normalize {
        /// Body-expression AST (a `lambda` or a bare expression).
        #[arg(long)]
        body: PathBuf,
        /// Print the normal form's `expr_` content-address instead of the JSON.
        #[arg(long)]
        hash: bool,
    },
    /// Nova Locutio agent loop: consume a signed message and emit a signed reply (spec/agent-loop.md).
    /// Handles `request`/`apply` (run the target on the value-expression args → an `assert` whose
    /// `predicate` claim is `eq(target(args…), result)`, self-verifiable by re-running),
    /// `request`/`validate` (typecheck + run the target → `assert` it `verified`, else `reject`),
    /// `request`/`store` (verify the payload's content-address → `ack`/`reject`), `query` (search the
    /// records → `ack` with the matches), `propose` (test-run, then `commit` or `reject`), a received
    /// `commit` (fulfil it → `assert`), and `delegate`/`retract` (`ack`). `apply`/`propose` are
    /// capability-gated: a target declaring required `signature.capabilities` is fulfilled only if the
    /// request presents them, else `reject` `not_authorized`. Threaded by `in_reply_to`, addressed
    /// back to the sender. Prints the signed reply JSON.
    Respond {
        /// Path to the message to answer (a `request`/`query`/`propose`/`commit`/`delegate`/`retract`).
        request: PathBuf,
        /// Directory of records/bodies to resolve the target body and `fn_ref` args against.
        #[arg(long)]
        records: PathBuf,
        /// Seed string used to derive the responder's Ed25519 signing identity.
        #[arg(long)]
        seed: String,
        /// Optional ISO 8601 timestamp for the assert (default: null, keeping the reply
        /// deterministic for a given seed).
        #[arg(long)]
        timestamp: Option<String>,
        /// Grant an effect this responder will perform on a remote sender's behalf (e.g. `net.read`).
        /// Repeatable. Default NONE — the responder fulfils only pure targets; an effectful target is
        /// refused with a signed `reject` (`effect not granted: …`). Grants are measured against the
        /// target's *verified* effect declaration and enforced by the runtime sandbox at perform time.
        /// Granting `net.read` means this machine fetches URLs chosen by remote input — front with
        /// egress controls if that matters (spec/agent-loop.md §Scope). A net grant may be scoped
        /// (`net.write@api.example.com` — any path on the host; `net.write@api.example.com/v0` —
        /// only under that path; `fs.read@/data` — only under that directory), enforced
        /// segment-aligned at the effect boundary.
        #[arg(long)]
        grant: Vec<String>,
        /// Grant an effect ONLY when the message's target function is certified by a certifier
        /// trusted under `--certifier-policy` (a per-function trust-gated grant —
        /// spec/agent-loop.md §Scope). Same grammar as --grant, including scoping. An uncertified
        /// target sees only the unconditional --grant set. Repeatable.
        #[arg(long = "grant-certified", requires = "certifier_policy")]
        grant_certified: Vec<String>,
        /// Local trust policy (JSON — the `certified` command's format) whose trusted certifiers
        /// back --grant-certified.
        #[arg(long = "certifier-policy")]
        certifier_policy: Option<PathBuf>,
        /// Signed attestation/certification messages backing the --grant-certified trust gate
        /// (repeatable).
        #[arg(long = "attestation")]
        attestations: Vec<PathBuf>,
        /// Supply a named secret (`NAME=VALUE`) for `{{secret:NAME}}` placeholders in `http` header
        /// values — effect-boundary configuration, never a language value, never in the trace.
        #[arg(long = "secret")]
        secret: Vec<String>,
        /// Supply an OAuth2 client-credentials identity (`NAME=token_url|client_id|client_secret`)
        /// for `{{oauth:NAME}}` placeholders — the same effect-boundary doctrine as --secret; the
        /// fetched token never enters a record or the trace. Repeatable.
        #[arg(long = "oauth")]
        oauth: Vec<String>,
        /// Where to write the recorded trace artifact when the reply carries an `observed` claim
        /// (an effectful fulfilment). The claim references the trace by `trc_…` content-address;
        /// the artifact itself must accompany the assert or no receiver can replay-verify it.
        #[arg(long = "trace-out")]
        trace_out: Option<PathBuf>,
    },
    /// Autonomous orchestration (spec/agent-loop.md): drive a full `query → propose → commit →
    /// assert → verify` conversation. The orchestrator discovers a commons function by `--intent`,
    /// proposes applying it to the `--arg`s, the responder commits + fulfils, and the orchestrator
    /// verifies the result. Prints the signed transcript; exit 1 if it isn't CONFIRMED.
    Orchestrate {
        /// Directory of records/bodies (the commons view). Exactly one of `--records`/`--node`.
        #[arg(long, conflicts_with = "node", required_unless_present = "node")]
        records: Option<PathBuf>,
        /// A LIVE commons node URL (e.g. https://nl.1105software.com): discovery goes through the
        /// node's `POST /v0/query`, and every record/body is fetched by content-address and
        /// **hash-verified locally** — the store stays untrusted (principle 7). Same loop,
        /// network-fed.
        #[arg(long)]
        node: Option<String>,
        /// With `--node`: publish the final signed `assert` (the result claim) back to the node
        /// through its verify-then-store gate, closing the loop.
        #[arg(long, requires = "node")]
        publish: bool,
        /// Intent tag for a pipeline stage (repeatable). Each `--intent` discovers a function by that
        /// intent and applies it to the previous stage's result — composing the discovered functions.
        #[arg(long = "intent")]
        intents: Vec<String>,
        /// Argument value (a value-expression JSON file). Repeatable, positional order.
        #[arg(long = "arg")]
        args: Vec<PathBuf>,
        /// Seed for the orchestrator's signing identity (signs query + propose).
        #[arg(long)]
        seed: String,
        /// Seed for the responder's identity (signs the replies).
        #[arg(long, default_value = "novae-linguae-example-responder")]
        responder_seed: String,
        /// Optional ISO 8601 timestamp for the messages (default: null, deterministic per seed).
        #[arg(long)]
        timestamp: Option<String>,
        /// Run the *verified* loop: after discovering the function, prove its declared property and
        /// (with `--policy`) trust-gate it before applying. Single `--intent` only.
        #[arg(long)]
        verify: bool,
        /// Local trust policy (JSON) for the `--verify` trust gate; an untrusted function aborts the run.
        #[arg(long)]
        policy: Option<PathBuf>,
        /// Signed attestation messages backing the trust gate (repeatable).
        #[arg(long = "attestation")]
        attestations: Vec<PathBuf>,
        /// SMT solver for the `--verify` proof step.
        #[arg(long, default_value = "z3")]
        solver: String,
        /// In the `--verify` loop, **certify** the discovered function (every verified-by-default check)
        /// before applying it, and ABORT if it isn't certified — "assemble only from verified parts".
        #[arg(long)]
        require_certified: bool,
        /// With `--verify`: the EXPECTED result value (a value-expression JSON file) — goal-aware
        /// discovery ranking. Candidates whose declared result type cannot produce it are dropped
        /// (the argument-only filter can't split same-argument fits that return different sorts),
        /// and statically pure+terminating candidates are dry-run on the arguments: an exact match
        /// ranks first, a mismatch last. The per-candidate scores land in the `rank` step.
        #[arg(long, requires = "verify")]
        expect: Option<PathBuf>,
        /// Grant an effect the responder half of this loop will perform (e.g. `net.read`). Repeatable.
        /// Default NONE (pure-only). The orchestrator's own verify step re-runs under the same grants.
        /// Granting `net.read` means this machine fetches URLs chosen by the discovered function's
        /// arguments — see spec/agent-loop.md §Scope. A net grant may be scoped
        /// (`net.write@api.example.com` — any path on the host; `net.write@api.example.com/v0/things`
        /// — only under that path; `fs.read@/data` — only under that directory), enforced
        /// segment-aligned at the effect boundary.
        #[arg(long)]
        grant: Vec<String>,
        /// Grant an effect ONLY to a target function certified by a certifier trusted under
        /// `--policy` (a per-function trust-gated grant — spec/agent-loop.md §Scope). Same grammar
        /// as --grant, including host/path scoping. Decided per candidate against the attestation
        /// graph and recorded as a `grants` transcript step; an uncertified candidate sees only the
        /// unconditional --grant set (and draws a signed `reject` if it needs more). Repeatable.
        #[arg(long = "grant-certified", requires = "verify", requires = "policy")]
        grant_certified: Vec<String>,
        /// Supply a named secret (`NAME=VALUE`) for `{{secret:NAME}}` placeholders in `http` header
        /// values — effect-boundary configuration, never a language value, never in the trace.
        #[arg(long = "secret")]
        secret: Vec<String>,
        /// Supply an OAuth2 client-credentials identity (`NAME=token_url|client_id|client_secret`)
        /// for `{{oauth:NAME}}` placeholders — same doctrine as --secret. Repeatable.
        #[arg(long = "oauth")]
        oauth: Vec<String>,
    },
    /// Verify a Nova Locutio `assert` by RE-RUNNING its `predicate` claim against the commons:
    /// resolve the claim's content-addressed function(s) from `--records` and evaluate it. The
    /// receiver half of the agent loop — trust nothing, re-execute (principle 3). Exit 0 if the
    /// claim re-runs true (CONFIRMED), 1 if false (REFUTED) or undecidable.
    VerifyClaim {
        /// The `assert` message whose claim to re-run: a path to a JSON file, or (with `--node`) a
        /// bare `msg_…` content-address fetched from the node — the true third-party receiver, who
        /// knows only an address and a node URL.
        assert: String,
        /// Directory of records/bodies to resolve the claim's functions against.
        #[arg(long, conflicts_with = "node", required_unless_present = "node")]
        records: Option<PathBuf>,
        /// A live commons node URL: the assert (when given as an address) and every function/body
        /// the claim references are fetched by content-address and hash-verified locally.
        #[arg(long)]
        node: Option<String>,
        /// Grant an effect the re-run may perform (repeatable; default NONE). An effectful claim
        /// without matching grants is undecidable — it is the signer's testimony, not something this
        /// verifier can CONFIRM by re-execution (spec/agent-loop.md §Scope).
        #[arg(long)]
        grant: Vec<String>,
        /// Supply a named secret (`NAME=VALUE`) for `{{secret:NAME}}` placeholders in `http` header
        /// values, so a granted re-run can authenticate with the verifier's OWN credentials.
        #[arg(long = "secret")]
        secret: Vec<String>,
        /// Supply an OAuth2 client-credentials identity (`NAME=token_url|client_id|client_secret`)
        /// so a granted LIVE re-run can authenticate as the verifier's OWN identity. An `observed`
        /// claim replays from its trace and needs none. Repeatable.
        #[arg(long = "oauth")]
        oauth: Vec<String>,
    },
    /// Verify a delegation chain (spec/trust-model.md): can `--grantee` wield `--capability` by a chain
    /// of signed `delegate` tokens back to a recognized `--root`? Checks every token's signature,
    /// attenuation (no link widens the grant), expiry (against `--at`), and that the chain reaches a
    /// recognized root. Prints the verified chain + any accumulated conditions. Exit 0 if AUTHORIZED,
    /// 1 if not.
    VerifyDelegation {
        /// The capability the action requires (e.g. `cap:apply/double`).
        #[arg(long)]
        capability: String,
        /// The DID that must end up authorized (the presenter).
        #[arg(long)]
        grantee: String,
        /// A recognized root DID (repeatable): an authority the receiver trusts per local policy.
        #[arg(long = "root")]
        roots: Vec<String>,
        /// Directory of `delegate` message JSON files forming the available token pool. Repeatable.
        #[arg(long = "delegations")]
        delegations: Vec<PathBuf>,
        /// Optional verification instant (RFC 3339 UTC, e.g. `2026-06-08T00:00:00Z`); tokens whose
        /// `expires_at` precedes it are skipped. Omit to ignore expiry.
        #[arg(long)]
        at: Option<String>,
    },
    /// Evaluate trust under a local policy (spec/trust-model.md): is `--subject` trusted, given the
    /// attestation graph built from `--attestations` (signed `assert`/attestation + `retract`
    /// messages)? The reference policy engine spreads trust from the policy's `trusted_roots`, requires
    /// `min_distinct_paths` distinct trusted attesters for the subject (diversity / Sybil mitigation),
    /// honors `distrusts` and retractions, and prunes expired attestations. Exit 0 if TRUSTED.
    EvaluateTrust {
        /// Path to the local policy JSON (`trusted_roots`, `max_depth`, `min_distinct_paths`, …).
        #[arg(long)]
        policy: PathBuf,
        /// Directory/file of attestation + retract messages forming the graph. Repeatable.
        #[arg(long = "attestations")]
        attestations: Vec<PathBuf>,
        /// The agent DID (or artifact address) whose trust to evaluate.
        #[arg(long)]
        subject: String,
        /// Optional domain to scope the query to (matches `trusts-claims-about` attestations).
        #[arg(long)]
        domain: Option<String>,
        /// Optional verification instant (RFC 3339 UTC) for pruning expired attestations.
        #[arg(long)]
        at: Option<String>,
    },
    /// Check whether a **function** (its `fn_…` content-address) is **certified by a certifier the policy
    /// trusts**. Ingests signed certification records (`certify --sign`) — which contribute `certifies`
    /// edges — together with vouch attestations from `--attestations`, builds the trust graph, and reports
    /// whether any of the function's certifiers is itself trusted under `--policy` (a certificate is only as
    /// good as its certifier). The trust-delegation counterpart to running `certify` locally. Exit 0 if
    /// CERTIFIED.
    Certified {
        /// Path to the local policy JSON (`trusted_roots`, …).
        #[arg(long)]
        policy: PathBuf,
        /// Directory/file of certification records + vouch/retract attestations. Repeatable.
        #[arg(long = "attestations")]
        attestations: Vec<PathBuf>,
        /// The function's `fn_…` content-address to check certification for.
        #[arg(long)]
        subject: String,
        /// Optional domain to scope the certifier-trust query to.
        #[arg(long)]
        domain: Option<String>,
        /// Optional verification instant (RFC 3339 UTC).
        #[arg(long)]
        at: Option<String>,
    },
    /// Authorize a capability under a local policy: verify a signed delegation chain back to one of the
    /// policy's `trusted_roots`, then enforce that every condition the chain carries is one the policy
    /// declares it can satisfy (`satisfied_conditions`). Exit 0 if AUTHORIZED. The policy-aware
    /// counterpart to `verify-delegation`.
    Authorize {
        /// Path to the local policy JSON (`trusted_roots`, `satisfied_conditions`, …).
        #[arg(long)]
        policy: PathBuf,
        /// The capability the action requires (e.g. `cap:apply/double`).
        #[arg(long)]
        capability: String,
        /// The DID that must end up authorized (the presenter).
        #[arg(long)]
        grantee: String,
        /// Directory/file of `delegate` tokens forming the available pool. Repeatable.
        #[arg(long = "delegations")]
        delegations: Vec<PathBuf>,
        /// Optional verification instant (RFC 3339 UTC) for expiry checks.
        #[arg(long)]
        at: Option<String>,
    },
    /// Parse a Nova Lingua type-expression surface string into its JSON AST.
    /// Reads the surface string from the `input` argument, or from stdin when
    /// omitted. Writes the AST as pretty JSON to stdout. See
    /// `spec/surface-syntax.md` §1.
    #[cfg(feature = "surface")]
    ParseType {
        /// The surface string (e.g. "forall a. List a -> List a"). If omitted,
        /// the surface string is read from stdin.
        input: Option<String>,
    },
    /// Pretty-print a Nova Lingua type-expression JSON AST back to its canonical
    /// surface string. Reads the AST from a JSON file, or from stdin when the
    /// path is `-`. Writes the surface string to stdout.
    #[cfg(feature = "surface")]
    UnparseType {
        /// Path to the type-expression JSON AST, or `-` for stdin.
        file: PathBuf,
    },
    /// Parse a Nova Lingua value-expression surface string into its JSON AST.
    /// Reads the surface string from the `input` argument, or from stdin when
    /// omitted. Writes the AST as pretty JSON to stdout. See
    /// `spec/surface-syntax.md` §3.
    #[cfg(feature = "surface")]
    ParseValue {
        /// The surface string (e.g. "[1, 2, 3]" or "Some(42)"). If omitted, the
        /// surface string is read from stdin.
        input: Option<String>,
    },
    /// Pretty-print a Nova Lingua value-expression JSON AST back to its canonical
    /// surface string. Reads the AST from a JSON file, or from stdin when the
    /// path is `-`. Writes the surface string to stdout.
    #[cfg(feature = "surface")]
    UnparseValue {
        /// Path to the value-expression JSON AST, or `-` for stdin.
        file: PathBuf,
    },
    /// Parse a Nova Lingua predicate-expression surface string into its JSON
    /// AST. Reads the surface string from the `input` argument, or from stdin
    /// when omitted. Writes the AST as pretty JSON to stdout. See
    /// `spec/surface-syntax.md` §2.
    #[cfg(feature = "surface")]
    ParsePredicate {
        /// The surface string (e.g. "forall xs. length(xs) >= 0"). If omitted,
        /// the surface string is read from stdin.
        input: Option<String>,
    },
    /// Pretty-print a Nova Lingua predicate-expression JSON AST back to its
    /// canonical surface string. Reads the AST from a JSON file, or from stdin
    /// when the path is `-`. Writes the surface string to stdout.
    #[cfg(feature = "surface")]
    UnparsePredicate {
        /// Path to the predicate-expression JSON AST, or `-` for stdin.
        file: PathBuf,
    },
    /// Parse a Nova Lingua body-expression surface string into its JSON AST.
    /// Reads the surface string from the `input` argument, or from stdin when
    /// omitted. Writes the AST as pretty JSON to stdout. See
    /// `spec/surface-syntax.md` §4.
    #[cfg(feature = "surface")]
    ParseBody {
        /// The surface string (e.g. "\\(n: nat) -> n + n"). If omitted, the
        /// surface string is read from stdin.
        input: Option<String>,
    },
    /// Pretty-print a Nova Lingua body-expression JSON AST back to its canonical
    /// surface string. Reads the AST from a JSON file, or from stdin when the
    /// path is `-`. Writes the surface string to stdout.
    #[cfg(feature = "surface")]
    UnparseBody {
        /// Path to the body-expression JSON AST, or `-` for stdin.
        file: PathBuf,
    },
}

/// Parse repeated `--secret NAME=VALUE` flags. The value may itself contain `=`.
fn parse_secrets(raw: &[String]) -> Result<Vec<(String, String)>> {
    raw.iter()
        .map(|s| {
            s.split_once('=')
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .ok_or_else(|| anyhow!("--secret expects NAME=VALUE, got `{s}`"))
        })
        .collect()
}

/// Parse repeated `--oauth NAME=token_url|client_id|client_secret` flags (client-credentials
/// identities for `{{oauth:NAME}}` placeholders). `|` separates the three parts — it cannot
/// appear in a URL authority/path or a sane client credential; the secret may contain `=`.
fn parse_oauth(raw: &[String]) -> Result<Vec<(String, nl_validator::OAuthConfig)>> {
    raw.iter()
        .map(|s| {
            let (name, rest) = s
                .split_once('=')
                .ok_or_else(|| anyhow!("--oauth expects NAME=token_url|client_id|client_secret, got `{s}`"))?;
            let mut parts = rest.splitn(3, '|');
            match (parts.next(), parts.next(), parts.next()) {
                (Some(url), Some(id), Some(secret)) if !url.is_empty() && !id.is_empty() => {
                    Ok((name.to_string(), nl_validator::OAuthConfig {
                        token_url: url.to_string(),
                        client_id: id.to_string(),
                        client_secret: secret.to_string(),
                    }))
                }
                _ => Err(anyhow!("--oauth expects NAME=token_url|client_id|client_secret, got `{s}`")),
            }
        })
        .collect()
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let (result, print_ok) = match cli.command {
        Commands::Validate { schema, record } => (cmd_validate(&schema, &record), true),
        Commands::Canonicalize { record } => (cmd_canonicalize(&record), false),
        Commands::Hash { record, kind } => (cmd_hash(&record, kind.map(Into::into)), false),
        Commands::Verify { record, kind } => (cmd_verify(&record, kind.map(Into::into)), false),
        Commands::Sign {
            record,
            seed,
            in_place,
        } => (cmd_sign(&record, &seed, in_place), false),
        Commands::CheckType { record } => (cmd_check_type(&record), true),
        Commands::CheckPredicate { record } => (cmd_check_predicate(&record), true),
        Commands::CheckValue { record } => (cmd_check_value(&record), true),
        Commands::CheckBody { record } => (cmd_check_body(&record), true),
        Commands::CheckProperties { record, body, generate, cases } => {
            (cmd_check_properties(&record, body.as_ref(), generate.then_some(cases)), true)
        }
        Commands::Eval { body, args, grants, secrets, oauth, replay, trace_out, records } => {
            match parse_secrets(&secrets).and_then(|s| Ok((s, parse_oauth(&oauth)?))) {
                Ok((s, o)) => {
                    nl_validator::set_effect_secrets(s);
                    nl_validator::set_effect_oauth(o);
                    (cmd_eval(&body, &args, &grants, replay.as_ref(), trace_out.as_ref(), records.as_ref()), false)
                }
                Err(e) => (Err(e), false),
            }
        }
        Commands::Run { record, body, records, secrets, oauth } => {
            match parse_secrets(&secrets).and_then(|s| Ok((s, parse_oauth(&oauth)?))) {
                Ok((s, o)) => {
                    nl_validator::set_effect_secrets(s);
                    nl_validator::set_effect_oauth(o);
                    (cmd_run(&record, body.as_ref(), records.as_ref()), false)
                }
                Err(e) => (Err(e), false),
            }
        }
        Commands::CheckEffects { record, body, records } => {
            (cmd_check_effects(&record, &body, records.as_ref()), false)
        }
        Commands::Typecheck { record, body } => (cmd_typecheck(&record, &body), false),
        Commands::CheckRefinement { record, body, solver } => (cmd_check_refinement(&record, &body, &solver), false),
        Commands::CheckTermination { record, body } => (cmd_check_termination(&record, &body), false),
        Commands::CheckComplexity { record, body } => (cmd_check_complexity(&record, &body), false),
        Commands::Certify { record, body, records, solver, json, sign, timestamp } => {
            (cmd_certify(&record, &body, records.as_ref(), &solver, json, sign.as_deref(), timestamp.as_deref()), false)
        }
        Commands::AttestWeights { record, eval, results, sign, timestamp } => {
            (cmd_attest_weights(&record, &eval, &results, &sign, timestamp.as_deref()), false)
        }
        Commands::Respond { request, records, seed, timestamp, grant, grant_certified, certifier_policy, attestations, secret, oauth, trace_out } => {
            match parse_secrets(&secret).and_then(|s| Ok((s, parse_oauth(&oauth)?))) {
                Ok((s, o)) => {
                    nl_validator::set_effect_secrets(s);
                    nl_validator::set_effect_oauth(o);
                    (cmd_respond(&request, &records, &seed, timestamp.as_deref(), trace_out.as_ref(), &grant, &grant_certified, certifier_policy.as_ref(), &attestations), false)
                }
                Err(e) => (Err(e), false),
            }
        }
        Commands::Prove { record, body, smt_out, solver } => {
            (cmd_prove(&record, body.as_ref(), smt_out.as_ref(), &solver), false)
        }
        Commands::Equiv { body_f, body_g, solver } => (cmd_equiv(&body_f, &body_g, &solver), false),
        Commands::Compose { records } => (cmd_compose(&records), false),
        Commands::Assemble { records, node, intent, limit, goal, max_stages, require_certified, solver, emit, publish } => {
            (cmd_assemble(records.as_deref(), node.as_deref(), &intent, limit, &goal, max_stages, require_certified, &solver, emit.as_deref(), publish), false)
        }
        Commands::Cluster { records, solver } => (cmd_cluster(&records, &solver), false),
        Commands::Normalize { body, hash } => (cmd_normalize(&body, hash), false),
        Commands::VerifyClaim { assert, records, node, grant, secret, oauth } => {
            nl_validator::set_effect_grants(grant.iter().cloned());
            match parse_secrets(&secret).and_then(|s| Ok((s, parse_oauth(&oauth)?))) {
                Ok((s, o)) => {
                    nl_validator::set_effect_secrets(s);
                    nl_validator::set_effect_oauth(o);
                    (cmd_verify_claim(&assert, records.as_ref(), node.as_deref()), false)
                }
                Err(e) => (Err(e), false),
            }
        }
        Commands::VerifyDelegation { capability, grantee, roots, delegations, at } => {
            (cmd_verify_delegation(&capability, &grantee, &roots, &delegations, at.as_deref()), false)
        }
        Commands::Certified { policy, attestations, subject, domain, at } => {
            (cmd_certified(&policy, &attestations, &subject, domain.as_deref(), at.as_deref()), false)
        }
        Commands::EvaluateTrust { policy, attestations, subject, domain, at } => {
            (cmd_evaluate_trust(&policy, &attestations, &subject, domain.as_deref(), at.as_deref()), false)
        }
        Commands::Authorize { policy, capability, grantee, delegations, at } => {
            (cmd_authorize(&policy, &capability, &grantee, &delegations, at.as_deref()), false)
        }
        Commands::Orchestrate { records, node, publish, intents, args, seed, responder_seed, timestamp, verify, policy, attestations, solver, require_certified, expect, grant, grant_certified, secret, oauth } => {
            nl_validator::set_effect_grants(grant.iter().cloned());
            match parse_secrets(&secret).and_then(|s| Ok((s, parse_oauth(&oauth)?))) {
                Err(e) => (Err(e), false),
                Ok((s, o)) => {
                    nl_validator::set_effect_secrets(s);
                    nl_validator::set_effect_oauth(o);
                    if verify {
                        (cmd_orchestrate_verified(records.as_ref(), node.as_deref(), publish, &intents, &args, &seed, &responder_seed, timestamp.as_deref(), policy.as_ref(), &attestations, &solver, require_certified, expect.as_ref(), &grant_certified), false)
                    } else {
                        (cmd_orchestrate(records.as_ref(), node.as_deref(), publish, &intents, &args, &seed, &responder_seed, timestamp.as_deref()), false)
                    }
                }
            }
        }
        #[cfg(feature = "surface")]
        Commands::ParseType { input } => (cmd_parse_type(input), false),
        #[cfg(feature = "surface")]
        Commands::UnparseType { file } => (cmd_unparse_type(&file), false),
        #[cfg(feature = "surface")]
        Commands::ParseValue { input } => (cmd_parse_value(input), false),
        #[cfg(feature = "surface")]
        Commands::UnparseValue { file } => (cmd_unparse_value(&file), false),
        #[cfg(feature = "surface")]
        Commands::ParsePredicate { input } => (cmd_parse_predicate(input), false),
        #[cfg(feature = "surface")]
        Commands::UnparsePredicate { file } => (cmd_unparse_predicate(&file), false),
        #[cfg(feature = "surface")]
        Commands::ParseBody { input } => (cmd_parse_body(input), false),
        #[cfg(feature = "surface")]
        Commands::UnparseBody { file } => (cmd_unparse_body(&file), false),
    };

    match result {
        Ok(()) => {
            if print_ok {
                eprintln!("OK");
            }
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("{err:#}");
            ExitCode::FAILURE
        }
    }
}

fn cmd_validate(schema: &PathBuf, record: &PathBuf) -> Result<()> {
    let schema_value = nl_validator::read_json(schema)?;
    let instance = nl_validator::read_json(record)?;
    // Cross-file `$ref`s resolve against sibling schema files in the schema's
    // own directory. Schemas with only same-document refs are unaffected.
    let spec_dir = schema.parent().unwrap_or_else(|| Path::new("."));
    nl_validator::validate_with_refs(&schema_value, &instance, spec_dir)
}

fn cmd_canonicalize(record: &PathBuf) -> Result<()> {
    use std::io::Write;
    let value = nl_validator::read_json(record)?;
    let canonical = nl_validator::canonicalize(&value)?;
    std::io::stdout()
        .write_all(&canonical)
        .map_err(|e| anyhow::anyhow!("writing canonical bytes to stdout: {e}"))?;
    Ok(())
}

fn cmd_hash(record: &PathBuf, kind_override: Option<nl_validator::ArtifactKind>) -> Result<()> {
    let value = nl_validator::read_json(record)?;
    let hash = match kind_override {
        Some(k) => nl_validator::hash_artifact_with_kind(&value, k)?,
        None => nl_validator::hash_artifact(&value)?,
    };
    println!("{hash}");
    Ok(())
}

fn cmd_verify(record: &PathBuf, kind_override: Option<nl_validator::ArtifactKind>) -> Result<()> {
    let value = nl_validator::read_json(record)?;
    let kind = match kind_override {
        Some(k) => k,
        None => nl_validator::ArtifactKind::detect(&value)?,
    };

    // Body expressions don't have a stored hash field — refuse verify cleanly.
    if matches!(kind, nl_validator::ArtifactKind::BodyExpression) {
        return Err(anyhow::anyhow!(
            "body expressions have no stored `hash` field to verify against; use `hash` to compute the body's content-hash, then compare externally to whichever `body_hash` field references it"
        ));
    }
    if matches!(kind, nl_validator::ArtifactKind::Trace) {
        return Err(anyhow::anyhow!(
            "traces are self-addressing and have no stored `hash` field; use `hash` to compute the trc_… address, then compare externally to the `observed` claim's `trace` reference"
        ));
    }

    let v = nl_validator::verify_artifact_hash_with_kind(&value, kind)?;

    // Hash check (both artifact kinds have a `hash` field).
    let (hash_pass, hash_line) = match (&v.stored, v.matches) {
        (Some(stored), true) => (true, format!("hash      PASS  {stored}")),
        (Some(stored), false) => (
            false,
            format!(
                "hash      FAIL  mismatch\n  stored:   {stored}\n  computed: {}",
                v.computed
            ),
        ),
        (None, _) => (
            false,
            format!(
                "hash      FAIL  no `hash` field on artifact\n  computed: {}",
                v.computed
            ),
        ),
    };
    println!("{hash_line}");

    // Signature check (signed kinds only).
    let sig_pass = match kind {
        nl_validator::ArtifactKind::FunctionRecord => {
            println!("signature N/A   function records have no signature");
            true
        }
        nl_validator::ArtifactKind::Weights => {
            println!("signature N/A   weights records have no signature (provenance is attested, not signed)");
            true
        }
        nl_validator::ArtifactKind::Message
        | nl_validator::ArtifactKind::Certification
        | nl_validator::ArtifactKind::EvalAttestation => match nl_validator::verify_signature(&value) {
            Ok(()) => {
                println!("signature PASS");
                true
            }
            Err(e) => {
                println!("signature FAIL  {e:#}");
                false
            }
        },
        nl_validator::ArtifactKind::BodyExpression | nl_validator::ArtifactKind::Trace => {
            unreachable!("body expressions and traces are refused above")
        }
    };

    if hash_pass && sig_pass {
        Ok(())
    } else {
        Err(anyhow::anyhow!("verification failed"))
    }
}

fn cmd_check_type(record: &PathBuf) -> Result<()> {
    let value = nl_validator::read_json(record)?;
    nl_validator::check_type_well_formed(&value)
}

fn cmd_check_predicate(record: &PathBuf) -> Result<()> {
    let value = nl_validator::read_json(record)?;
    nl_validator::check_predicate_well_formed(&value)
}

fn cmd_check_value(record: &PathBuf) -> Result<()> {
    let value = nl_validator::read_json(record)?;
    nl_validator::check_value_well_formed(&value)
}

fn cmd_check_body(record: &PathBuf) -> Result<()> {
    let value = nl_validator::read_json(record)?;
    nl_validator::check_body_well_formed(&value)
}

fn cmd_check_properties(
    record: &PathBuf,
    body: Option<&PathBuf>,
    generate: Option<usize>,
) -> Result<()> {
    let value = nl_validator::read_json(record)?;
    let body = body.map(|p| nl_validator::read_json(p)).transpose()?;
    nl_validator::check_properties(&value, body.as_ref(), generate)
}

fn cmd_check_effects(record: &PathBuf, body: &PathBuf, records: Option<&PathBuf>) -> Result<()> {
    let record = nl_validator::read_json(record)?;
    let body = nl_validator::read_json(body)?;
    let map = match records {
        Some(dir) => nl_validator::build_record_map(dir)?,
        None => std::collections::HashMap::new(),
    };
    nl_validator::check_effects(&record, &body, &map)
}

fn cmd_typecheck(record: &PathBuf, body: &PathBuf) -> Result<()> {
    let record = nl_validator::read_json(record)?;
    let body = nl_validator::read_json(body)?;
    println!("{}", nl_validator::typecheck_record(&record, &body)?);
    Ok(())
}

fn cmd_check_refinement(record: &PathBuf, body: &PathBuf, solver: &str) -> Result<()> {
    use nl_validator::RefinementOutcome;
    let record = nl_validator::read_json(record)?;
    let body = nl_validator::read_json(body)?;
    let sig_type = record
        .pointer("/signature/type")
        .ok_or_else(|| anyhow::anyhow!("record has no `signature.type`"))?;
    let refinements: Vec<serde_json::Value> =
        record.pointer("/signature/refinements").and_then(|r| r.as_array()).cloned().unwrap_or_default();

    let reports = nl_validator::check_refinements(sig_type, &refinements, &body, solver);
    let mut violated = false;
    let mut no_solver = false;
    for r in &reports {
        let line = match &r.outcome {
            RefinementOutcome::Sound => format!("SOUND        {}", r.label),
            RefinementOutcome::Violated(model) => {
                violated = true;
                format!(
                    "VIOLATED     {} — counterexample: {}",
                    r.label,
                    if model.is_empty() { "(model)".into() } else { model.clone() }
                )
            }
            RefinementOutcome::Unverifiable(why) => format!("UNVERIFIABLE {} — {why}", r.label),
            RefinementOutcome::NotApplicable => {
                format!("N/A          {} (no type-implied or declared refinements to check)", r.label)
            }
            RefinementOutcome::NoSolver => {
                no_solver = true;
                format!("NO-SOLVER    `{solver}` not found")
            }
        };
        println!("{line}");
    }
    if no_solver {
        return Err(anyhow::anyhow!("no SMT solver available"));
    }
    if violated {
        return Err(anyhow::anyhow!("a refinement is VIOLATED"));
    }
    Ok(())
}

fn cmd_check_termination(record: &PathBuf, body: &PathBuf) -> Result<()> {
    use nl_validator::TerminationOutcome;
    let record = nl_validator::read_json(record)?;
    let body = nl_validator::read_json(body)?;
    let declared = record.pointer("/signature/terminates").and_then(|v| v.as_str()).unwrap_or("unknown");
    match nl_validator::analyze_termination_typed(&body, &nl_validator::nat_param_positions(&record)) {
        TerminationOutcome::Always => match declared {
            "always" => println!("SOUND        the body provably always terminates (matches declared `always`)"),
            other => {
                println!("VERIFIED     provably always-terminates — declared `{other}` could be strengthened to `always`")
            }
        },
        TerminationOutcome::Unknown(why) => match declared {
            "always" => println!("UNVERIFIABLE declared `always`, but structural analysis can't prove it: {why}"),
            other => println!("UNKNOWN      not provably terminating ({why}); consistent with declared `{other}`"),
        },
    }
    Ok(())
}

fn cmd_check_complexity(record: &PathBuf, body: &PathBuf) -> Result<()> {
    let record = nl_validator::read_json(record)?;
    let body = nl_validator::read_json(body)?;
    let inferred = nl_validator::analyze_complexity(&body);

    // (1) The flat `signature.complexity` bound.
    let declared = record.pointer("/signature/complexity").and_then(|v| v.as_str());
    println!("{}", time_verdict("complexity", declared, &inferred));

    // (2) The structured `signature.cost`: verify `cost.time` (a time class in the given measure) and
    //     `cost.output_size` (how the result size grows) — the fields `compose` threads through a pipeline.
    if let Some(cost) = record.pointer("/signature/cost") {
        if let Some(t) = cost.get("time").and_then(|t| t.as_str()) {
            let measure = cost.get("measure").and_then(|m| m.as_str()).unwrap_or("size");
            println!("{}", time_verdict(&format!("cost.time ({measure})"), Some(t), &inferred));
        }
        if let Some(os) = cost.get("output_size").and_then(|o| o.as_str()) {
            println!("{}", output_size_verdict(&record, &body, os));
        }
    }
    Ok(())
}

/// Format a time-class verdict line: compare an inferred sound upper bound to a declared `O(…)` class.
fn time_verdict(label: &str, declared: Option<&str>, inferred: &nl_validator::ComplexityOutcome) -> String {
    use nl_validator::ComplexityOutcome;
    match inferred {
        ComplexityOutcome::Opaque(why) => match declared {
            Some(d) => format!("UNVERIFIABLE {label}: declared `{d}`, but structural analysis can't establish a bound: {why}"),
            None => format!("UNKNOWN      {label}: none declared; structural analysis can't infer one: {why}"),
        },
        ComplexityOutcome::Bound(inferred) => {
            let inf = inferred.display();
            match declared {
                None => format!("N/A          {label}: none declared; a sound structural bound is {inf}"),
                Some(d) => match nl_validator::parse_class(d) {
                    None => format!("UNVERIFIABLE {label}: declared `{d}` is not a recognized class; inferred bound is {inf}"),
                    Some(dc) if *inferred == dc => format!("SOUND        {label}: the body is within its declared `{d}` (inferred bound {inf})"),
                    Some(dc) if *inferred < dc => format!("VERIFIED     {label}: provably {inf}, tighter than declared `{d}` — could be strengthened"),
                    Some(_) => format!("UNVERIFIABLE {label}: declared `{d}`, but the sound structural bound is {inf} (worse) — not established"),
                },
            }
        }
    }
}

/// Format an output-size verdict line: compare the inferred result-size growth to a declared `output_size`.
fn output_size_verdict(record: &serde_json::Value, body: &serde_json::Value, declared: &str) -> String {
    use nl_validator::OutputSize;
    let label = "cost.output_size";
    let result_ty = record.pointer("/signature/type").and_then(unwrap_result_type);
    let inferred = match result_ty {
        Some(rt) => nl_validator::analyze_output_size(&rt, body),
        None => OutputSize::Unknown,
    };
    let dec = nl_validator::parse_output_size(declared);
    match (inferred.degree(), dec.degree()) {
        (None, _) => format!("UNVERIFIABLE {label}: declared `{declared}`, but the result-size growth can't be inferred ({})", inferred.label()),
        (_, None) => format!("N/A          {label}: declared `{declared}` (nothing to verify); inferred {}", inferred.label()),
        (Some(i), Some(d)) if i == d => format!("SOUND        {label}: the result is {declared} (inferred {})", inferred.label()),
        (Some(i), Some(d)) if i < d => format!("VERIFIED     {label}: provably {}, tighter than declared `{declared}` — could be strengthened", inferred.label()),
        (Some(_), Some(_)) => format!("UNVERIFIABLE {label}: declared `{declared}`, but the result grows faster ({}) — not established", inferred.label()),
    }
}

/// The result type of a record's `signature.type` (unwrapping a `forall`), for output-size analysis.
fn unwrap_result_type(ty: &serde_json::Value) -> Option<serde_json::Value> {
    let t = if ty.get("kind").and_then(|k| k.as_str()) == Some("forall") { ty.get("body")? } else { ty };
    t.get("result").cloned()
}

fn cmd_certify(
    record: &PathBuf,
    body: &PathBuf,
    records: Option<&PathBuf>,
    solver: &str,
    json: bool,
    sign: Option<&str>,
    timestamp: Option<&str>,
) -> Result<()> {
    let record = nl_validator::read_json(record)?;
    let body = nl_validator::read_json(body)?;
    let record_map = match records {
        Some(dir) => nl_validator::build_record_map(dir)?,
        None => std::collections::HashMap::new(),
    };
    let cert = nl_validator::certify_record(&record, &body, &record_map, solver);
    let certified = cert.certified;

    // The certification artifact — the same object whether printed as JSON or signed into a commons record.
    let checks: Vec<serde_json::Value> = cert
        .checks
        .iter()
        .map(|c| serde_json::json!({ "check": c.check, "verdict": c.verdict, "detail": c.detail }))
        .collect();
    let mut cert_json = serde_json::json!({
        "schema_version": "0.2.0",
        "kind": "certification",
        "subject": cert.subject,
        "body_hash": cert.body_hash,
        "checks": checks,
        "certified": certified,
    });
    if let Some(ts) = timestamp {
        cert_json.as_object_mut().unwrap().insert("timestamp".into(), serde_json::Value::String(ts.into()));
    }

    if let Some(seed) = sign {
        // Sign the certification into a content-addressed, verifiable commons artifact.
        use std::io::Write;
        let key = nl_validator::signing_key_from_seed(seed);
        nl_validator::sign_artifact(&mut cert_json, &key, nl_validator::ArtifactKind::Certification)?;
        let pretty = serde_json::to_string_pretty(&cert_json).map_err(|e| anyhow::anyhow!("{e}"))?;
        std::io::stdout().write_all(pretty.as_bytes())?;
        std::io::stdout().write_all(b"\n")?;
    } else if json {
        use std::io::Write;
        let pretty = serde_json::to_string_pretty(&cert_json).map_err(|e| anyhow::anyhow!("{e}"))?;
        std::io::stdout().write_all(pretty.as_bytes())?;
        std::io::stdout().write_all(b"\n")?;
    } else {
        println!("certify {}", cert.subject);
        for c in &cert.checks {
            println!("  {:<22} {:<14} {}", c.check, c.verdict, c.detail);
        }
        println!("  => {}", if certified { "CERTIFIED" } else { "NOT CERTIFIED" });
    }

    if !certified {
        return Err(anyhow::anyhow!("record is NOT certified (a check failed its declaration)"));
    }
    Ok(())
}

fn cmd_attest_weights(
    record: &PathBuf,
    eval: &PathBuf,
    results: &PathBuf,
    seed: &str,
    timestamp: Option<&str>,
) -> Result<()> {
    use std::io::Write;
    // Attest only what you can address: the weights record's wgt_ hash must verify.
    let weights = nl_validator::read_json(record)?;
    let kind = nl_validator::ArtifactKind::detect(&weights)?;
    if kind != nl_validator::ArtifactKind::Weights {
        return Err(anyhow::anyhow!(
            "attest-weights takes a weights record (kind = \"weights\"); got {kind:?}"
        ));
    }
    let v = nl_validator::verify_artifact_hash_with_kind(&weights, kind)?;
    if !v.matches {
        return Err(anyhow::anyhow!(
            "weights record hash does not verify (stored {:?}, computed {}) — refusing to attest",
            v.stored,
            v.computed
        ));
    }
    let subject = v.computed;

    let eval_desc = nl_validator::read_json(eval)?;
    let results_json = nl_validator::read_json(results)?;
    let mut att = serde_json::json!({
        "schema_version": "0.1.0",
        "kind": "eval-attestation",
        "subject": subject,
        "eval": eval_desc,
        "results": results_json,
    });
    if let Some(ts) = timestamp {
        att.as_object_mut().unwrap().insert("timestamp".into(), serde_json::Value::String(ts.into()));
    }
    let key = nl_validator::signing_key_from_seed(seed);
    nl_validator::sign_artifact(&mut att, &key, nl_validator::ArtifactKind::EvalAttestation)?;
    let pretty = serde_json::to_string_pretty(&att).map_err(|e| anyhow::anyhow!("{e}"))?;
    std::io::stdout().write_all(pretty.as_bytes())?;
    std::io::stdout().write_all(b"\n")?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn cmd_respond(
    request: &PathBuf,
    records: &PathBuf,
    seed: &str,
    timestamp: Option<&str>,
    trace_out: Option<&PathBuf>,
    grants: &[String],
    grant_certified: &[String],
    certifier_policy: Option<&PathBuf>,
    attestations: &[PathBuf],
) -> Result<()> {
    use std::io::Write;
    let message = nl_validator::read_json(request)?;
    // PER-FUNCTION TRUST-GATED GRANTS: a `--grant-certified` effect applies only when the
    // message's target function is certified by a certifier the operator's policy trusts —
    // decided here, before the responder's static effect gate measures the target against the
    // (effective) granted set. The verdict goes to stderr so the operator sees which set applied.
    let mut effective: Vec<String> = grants.to_vec();
    if !grant_certified.is_empty() {
        let pol_path = certifier_policy.ok_or_else(|| anyhow::anyhow!("--grant-certified requires --certifier-policy"))?;
        let pol = nl_validator::Policy::from_json(&nl_validator::read_json(pol_path)?)?;
        let msgs = load_json_messages(attestations, None)?;
        let graph = nl_validator::AttestationGraph::from_messages(&msgs, timestamp);
        let (open, reason) = match message.pointer("/body/target").and_then(|t| t.as_str()) {
            Some(target) => {
                let v = pol.certification_verdict(&graph, target, None, timestamp);
                (v.certified, format!("`{target}`: {}", v.reason))
            }
            None => (false, "message carries no target function".to_string()),
        };
        if open {
            effective.extend(grant_certified.iter().cloned());
        }
        eprintln!(
            "trust gate {} — {reason}",
            if open { "OPEN (certified-gated grants apply)" } else { "closed (unconditional grants only)" }
        );
    }
    nl_validator::set_effect_grants(effective);
    let link_map = nl_validator::build_link_map(records)?;
    let record_map = nl_validator::build_record_map(records)?;
    let key = nl_validator::signing_key_from_seed(seed);
    let reply = nl_validator::respond_to_message(&message, link_map, record_map, &key, timestamp)?;
    // An effectful fulfilment produced an `observed` claim + its recorded trace. The claim only
    // references the trace by trc_… address — persist the artifact or nobody can replay-verify it.
    if let Some(trace) = nl_validator::take_trace_artifact() {
        let addr = nl_validator::hash_artifact_with_kind(&trace, nl_validator::ArtifactKind::Trace)?;
        match trace_out {
            Some(path) => {
                let pretty = serde_json::to_string_pretty(&trace)
                    .map_err(|e| anyhow::anyhow!("serializing trace artifact: {e}"))?;
                std::fs::write(path, pretty)
                    .map_err(|e| anyhow::anyhow!("writing {}: {e}", path.display()))?;
                eprintln!("trace artifact {addr} written to {}", path.display());
            }
            None => eprintln!(
                "note: the reply's `observed` claim references trace {addr}, which was NOT saved — pass --trace-out <path> to keep the artifact receivers need for replay-verification"
            ),
        }
    }
    let pretty = serde_json::to_string_pretty(&reply)
        .map_err(|e| anyhow::anyhow!("serializing reply: {e}"))?;
    std::io::stdout().write_all(pretty.as_bytes())?;
    std::io::stdout().write_all(b"\n")?;
    Ok(())
}

fn cmd_prove(
    record: &PathBuf,
    body: Option<&PathBuf>,
    smt_out: Option<&PathBuf>,
    solver: &str,
) -> Result<()> {
    use nl_validator::ProofOutcome;
    let value = nl_validator::read_json(record)?;
    let body_ast = body.map(|p| nl_validator::read_json(p)).transpose()?;
    let props = value.get("properties").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    if props.is_empty() {
        println!("no properties to prove");
        return Ok(());
    }
    // The float-domain guard (GW5): the fragment's arithmetic is Int — proving a float-typed
    // record's law over Int semantics would be unsound (associativity is the classic divergence).
    if let Some(ty) = value.pointer("/signature/type") {
        if nl_validator::type_mentions_float(ty) {
            for prop in &props {
                let name = prop.get("name").and_then(|v| v.as_str()).unwrap_or("<unnamed>");
                println!("{name}: UNSUPPORTED  float domain (the proof fragment is Int/Bool/String; IEEE float laws would be mis-proved over Int)");
            }
            return Ok(());
        }
    }
    if let Some(dir) = smt_out {
        std::fs::create_dir_all(dir).map_err(|e| anyhow::anyhow!("creating {}: {e}", dir.display()))?;
    }
    let mut refuted = Vec::new();
    let mut no_solver = false;
    for (i, prop) in props.iter().enumerate() {
        let name = prop.get("name").and_then(|v| v.as_str()).unwrap_or("<unnamed>").to_string();
        let expr = prop.get("expr").ok_or_else(|| anyhow::anyhow!("property `{name}` missing `expr`"))?;
        let safe: String = name.chars().map(|c| if c.is_alphanumeric() { c } else { '_' }).collect();
        let write_cert = |suffix: &str, smt: &str| -> Result<()> {
            if let Some(dir) = smt_out {
                let path = dir.join(format!("{i:02}-{safe}{suffix}.smt2"));
                std::fs::write(&path, smt).map_err(|e| anyhow::anyhow!("writing {}: {e}", path.display()))?;
            }
            Ok(())
        };

        let (outcome, cert) = nl_validator::prove_property(expr, body_ast.as_ref(), solver);
        if let Some(cert) = &cert {
            write_cert("", &cert.smt)?;
        }
        // First-order SMT can't reach list/recursion laws — fall back to structural induction
        // (with auxiliary-lemma discovery for laws a single unfold + IH can't close).
        let label = if matches!(outcome, ProofOutcome::Unsupported(_)) {
            use nl_validator::InductionOutcome;
            // Catalog lemma discovery, then theory exploration when the catalog can't close it.
            let (iout, icert) = nl_validator::prove_by_induction_with_exploration(
                expr,
                body_ast.as_ref(),
                solver,
                nl_validator::DEFAULT_LEMMA_DEPTH,
            );
            if let Some(c) = &icert {
                write_cert(".base", &c.base)?;
                write_cert(".step", &c.step)?;
                // Each discovered lemma's own discharge, so the proof tree re-checks end to end.
                for lem in &c.lemmas {
                    write_cert(&format!(".lemma-{}.base", lem.name), &lem.base)?;
                    write_cert(&format!(".lemma-{}.step", lem.name), &lem.step)?;
                }
            }
            match iout {
                InductionOutcome::Proved => {
                    let v = icert.as_ref().map(|c| c.var.as_str()).unwrap_or("?");
                    format!("PROVED       by structural induction on `{v}` (base + step both unsat)")
                }
                InductionOutcome::ProvedWithLemmas(lemmas) => {
                    let v = icert.as_ref().map(|c| c.var.as_str()).unwrap_or("?");
                    format!(
                        "PROVED       by structural induction on `{v}` using lemmas: {}",
                        lemmas.join(", ")
                    )
                }
                InductionOutcome::Failed(why) => format!("NOT-PROVED   induction did not close: {why}"),
                InductionOutcome::Unknown => "UNKNOWN      induction step needs a lemma we lack (solver undecided)".to_string(),
                InductionOutcome::NoSolver => {
                    no_solver = true;
                    format!("NO-SOLVER    `{solver}` not found; obligations emitted, re-check elsewhere")
                }
                // Neither engine applies — report the first-order reason (the more general one).
                InductionOutcome::Unsupported(_) => match &outcome {
                    ProofOutcome::Unsupported(why) => format!("UNSUPPORTED  {why}"),
                    _ => "UNSUPPORTED".to_string(),
                },
            }
        } else {
            match &outcome {
                ProofOutcome::Proved => "PROVED       holds for all inputs (unsat negation)".to_string(),
                ProofOutcome::Refuted(model) => {
                    refuted.push(name.clone());
                    format!("REFUTED      counterexample: {}", if model.is_empty() { "(model)" } else { model })
                }
                ProofOutcome::Unknown => "UNKNOWN      solver could not decide".to_string(),
                ProofOutcome::NoSolver => {
                    no_solver = true;
                    format!("NO-SOLVER    `{solver}` not found; certificate emitted, re-check elsewhere")
                }
                ProofOutcome::Unsupported(why) => format!("UNSUPPORTED  {why}"),
            }
        };
        println!("{name}: {label}");
    }
    if !refuted.is_empty() {
        Err(anyhow::anyhow!("properties refuted by SMT counterexample: {}", refuted.join(", ")))
    } else if no_solver {
        Err(anyhow::anyhow!("no SMT solver available — obligations emitted but not discharged"))
    } else {
        Ok(())
    }
}

fn cmd_equiv(body_f: &PathBuf, body_g: &PathBuf, solver: &str) -> Result<()> {
    use nl_validator::EquivVerdict;
    let f = nl_validator::read_json(body_f)?;
    let g = nl_validator::read_json(body_g)?;
    match nl_validator::prove_equivalent(&f, &g, solver) {
        EquivVerdict::Equivalent(lemmas) => {
            if lemmas.is_empty() {
                println!("EQUIVALENT   f ≡ g for all inputs (unsat negation)");
            } else {
                println!("EQUIVALENT   f ≡ g, using lemmas: {}", lemmas.join(", "));
            }
            Ok(())
        }
        EquivVerdict::EquivalentByNormalization => {
            println!("EQUIVALENT   f ≡ g (identical canonical normal form; no solver needed)");
            Ok(())
        }
        EquivVerdict::Distinct(model) => {
            Err(anyhow::anyhow!("DISTINCT     counterexample: {}", if model.is_empty() { "(model)".into() } else { model }))
        }
        EquivVerdict::Unknown => {
            println!("UNKNOWN      could not decide equivalence");
            Ok(())
        }
        EquivVerdict::Unsupported(why) => {
            println!("UNSUPPORTED  {why}");
            Ok(())
        }
        EquivVerdict::NoSolver => Err(anyhow::anyhow!("NO-SOLVER    `{solver}` not found")),
    }
}

fn cmd_normalize(body: &PathBuf, hash: bool) -> Result<()> {
    let b = nl_validator::read_json(body)?;
    let nf = nl_validator::normalize(&b);
    if hash {
        let h = nl_validator::hash_artifact_with_kind(&nf, nl_validator::ArtifactKind::BodyExpression)?;
        println!("{h}");
    } else {
        println!("{}", serde_json::to_string_pretty(&nf)?);
    }
    Ok(())
}

fn cmd_compose(records: &[PathBuf]) -> Result<()> {
    let recs = records.iter().map(|p| nl_validator::read_json(p)).collect::<Result<Vec<_>>>()?;
    let m = nl_validator::compose(&recs);
    let ty = |t: &Option<serde_json::Value>| t.as_ref().map(|v| v.to_string()).unwrap_or_else(|| "?".into());
    if m.composable {
        println!("COMPOSABLE   {}", m.reason);
        // The composite is `(primary, aux…) -> output`; show the auxiliary inputs from multi-arg stages.
        let inputs = if m.extra_input_types.is_empty() {
            ty(&m.input_type)
        } else {
            let mut parts = vec![ty(&m.input_type)];
            parts.extend(m.extra_input_types.iter().map(|t| t.to_string()));
            format!("({})", parts.join(", "))
        };
        println!("  type        {} -> {}", inputs, ty(&m.output_type));
        println!("  effects     {:?}", m.effects);
        println!("  capabilities {:?}", m.capabilities);
        println!("  terminates  {}", m.terminates);
        println!("  complexity  {}", m.complexity);
        println!("  cost-basis  {}", m.complexity_basis);
        Ok(())
    } else {
        Err(anyhow::anyhow!("NOT-COMPOSABLE  {}", m.reason))
    }
}

/// Read a function's ARITY from the `type` field of a commons summary (`?include=summary`), for the
/// arity prune in [`cmd_assemble`]. The node stores `type_str` as the raw v0.1 surface string, or the
/// JSON-serialized v0.2 structured type. We read the structured form reliably (`params.len()`); a
/// non-`fn` structured type is a value (arity 0). Anything we can't confidently parse — including v0.1
/// surface strings — returns `None`, which the caller treats as "keep" (never prune on doubt).
fn type_arity(type_str: &str) -> Option<usize> {
    let mut j: serde_json::Value = serde_json::from_str(type_str).ok()?;
    // A polymorphic type is `forall`-wrapped around the function — unwrap so the arity is still read
    // (otherwise every generic candidate falls through to keep-on-doubt and dodges the prune).
    if j.get("kind").and_then(|k| k.as_str()) == Some("forall") {
        j = match j.get("body") {
            Some(b) => b.clone(),
            None => return None,
        };
    }
    match j.get("kind").and_then(|k| k.as_str()) {
        Some("fn") => j.get("params").and_then(|p| p.as_array()).map(|a| a.len()),
        // A structured type that isn't a function is a plain value — arity 0, never a usable stage.
        Some(_) => Some(0),
        None => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn cmd_assemble(
    records_dir: Option<&Path>,
    node: Option<&str>,
    intents: &[String],
    limit: usize,
    goal: &Path,
    max_stages: usize,
    require_certified: bool,
    solver: &str,
    emit: Option<&Path>,
    publish: bool,
) -> Result<()> {
    if publish && node.is_none() {
        return Err(anyhow::anyhow!("--publish requires --node (the node to publish the composite back to)"));
    }
    // Parse the goal FIRST — its argument shape bounds which candidates can ever be a pipeline stage,
    // which lets the live-node fetch prune before paying for per-record + per-body round-trips.
    let goal_v = nl_validator::read_json(goal)?;
    // Each example's `input` is either a single value-AST (a 1-argument goal) or an array of
    // value-ASTs `[primary, aux…]` (a multi-argument goal — the primary is threaded, the rest are
    // the auxiliary pool multi-arg stages draw from).
    let examples: Vec<(Vec<serde_json::Value>, serde_json::Value)> = goal_v
        .get("examples")
        .and_then(|e| e.as_array())
        .ok_or_else(|| anyhow::anyhow!("goal must have an `examples` array"))?
        .iter()
        .map(|ex| {
            let input = ex.get("input").ok_or_else(|| anyhow::anyhow!("example missing `input`"))?;
            let args: Vec<serde_json::Value> = match input {
                serde_json::Value::Array(a) => a.clone(),
                other => vec![other.clone()],
            };
            let output = ex.get("output").cloned().ok_or_else(|| anyhow::anyhow!("example missing `output`"))?;
            Ok((args, output))
        })
        .collect::<Result<_>>()?;
    // A pipeline stage consumes the running value (1) plus (arity−1) auxiliaries drawn from the goal's
    // aux pool, so any usable stage has 1 ≤ arity ≤ aux_count+1. This ceiling is a SOUND prune: an
    // arity-0 (nothing to thread) or over-arity (not enough auxiliaries) function can never appear,
    // whatever the search does.
    let max_usable_arity = examples.iter().map(|(a, _)| a.len().saturating_sub(1)).max().unwrap_or(0) + 1;

    // The commons view: a local directory, or a live node's candidate set (queried by filter,
    // fetched by content-address, every artifact re-hashed locally — the store stays untrusted).
    let (records, bodies) = match (records_dir, node) {
        (Some(dir), None) => (nl_validator::build_record_map(dir)?, nl_validator::build_link_map(dir)?),
        (None, Some(url)) => {
            // Function records carry no `kind` column, but every one has a `terminates` field
            // (values `always`/`conditional`/`unknown`) that non-function artifacts lack — so this
            // enumerates exactly the commons's functions (v0.1 string-typed and v0.2 structured
            // alike, where a `type_contains` filter would miss the structured ones).
            let mut filter = serde_json::json!({
                "terminates": ["always", "conditional", "unknown"], "limit": limit });
            if !intents.is_empty() {
                filter["intent_tags"] = serde_json::json!({ "any": intents });
            }
            // Summary-first discovery: one `?include=summary` round-trip reads every candidate's
            // signature, then the arity ceiling prunes the unusable ones BEFORE the expensive
            // per-record + per-body fetch. Keep-on-doubt — a candidate whose arity we can't read from
            // its summary stays in, so the prune only ever removes provably-unusable functions.
            let summaries = nl_validator::commons_client::query_summaries(url, &filter)?;
            if summaries.is_empty() {
                return Err(anyhow::anyhow!("the node returned no candidate functions for the filter"));
            }
            let mut viable: Vec<String> = Vec::new();
            let mut pruned = 0usize;
            for s in &summaries {
                let Some(hash) = s.get("hash").and_then(|h| h.as_str()) else { continue };
                let keep = match s.get("type").and_then(|t| t.as_str()).and_then(type_arity) {
                    Some(a) => a >= 1 && a <= max_usable_arity,
                    None => true, // unreadable arity ⇒ keep, never prune on doubt
                };
                if keep { viable.push(hash.to_string()); } else { pruned += 1; }
            }
            if viable.is_empty() {
                return Err(anyhow::anyhow!(
                    "no candidate has a usable arity (1..={max_usable_arity}) for this goal (pruned all {} summaries)",
                    summaries.len()));
            }
            eprintln!(
                "discovered {} candidate(s); arity-pruned {} (unusable at arity 1..={}), fetching {} by content-address…",
                summaries.len(), pruned, max_usable_arity, viable.len());
            // Seed the reference-closure walk with the surviving candidates, fetching each record AND
            // its body, every one hash-verified locally. Lenient: a function whose body the node
            // doesn't serve is skipped, not fatal — it just isn't a candidate.
            nl_validator::commons_client::maps_from_node_lenient(url, &viable, viable.len() * 8 + 64)?
        }
        (Some(_), Some(_)) => return Err(anyhow::anyhow!("supply exactly one of --records / --node")),
        (None, None) => return Err(anyhow::anyhow!("supply one of --records / --node")),
    };

    let assembled = nl_validator::assemble(&records, &bodies, &examples, max_stages, require_certified, solver)?;
    let Some(a) = assembled else {
        return Err(anyhow::anyhow!(
            "NO PIPELINE  no composition of ≤{max_stages} commons functions reproduces the {} example(s)",
            examples.len()
        ));
    };

    let pipeline = if a.stages.is_empty() {
        "(identity — the goal's input already equals its output)".to_string()
    } else {
        a.stages.iter().map(|s| s.name.clone()).collect::<Vec<_>>().join(" → ")
    };
    println!("ASSEMBLED    {}", pipeline);
    for (i, s) in a.stages.iter().enumerate() {
        println!("  stage {}     {}  {}", i + 1, s.name, s.hash);
    }
    let ty = |t: &Option<serde_json::Value>| t.as_ref().map(|v| v.to_string()).unwrap_or_else(|| "?".into());
    let inputs = if a.composite.extra_input_types.is_empty() {
        ty(&a.composite.input_type)
    } else {
        let mut parts = vec![ty(&a.composite.input_type)];
        parts.extend(a.composite.extra_input_types.iter().map(|t| t.to_string()));
        format!("({})", parts.join(", "))
    };
    println!("  type        {} -> {}", inputs, ty(&a.composite.output_type));
    println!("  effects     {:?}", a.composite.effects);
    println!("  terminates  {}", a.composite.terminates);
    println!("  complexity  {}", a.composite.complexity);
    println!("  examples    {}/{} verified through the composite", a.examples_verified, examples.len());
    if require_certified {
        println!("  certified   {} (every stage certifies)", a.certified);
    }
    println!("  composite   {}", a.composite_record["hash"].as_str().unwrap_or("?"));
    // The composite's declared metadata re-proven against its (inlined, self-contained) body.
    let checks = a.composite_checks.iter().map(|(n, v)| format!("{n}={v}")).collect::<Vec<_>>().join("  ");
    println!("  re-proven   {} against the inlined body  [{}]",
             if a.composite_certified { "CERTIFIED" } else { "NOT certified" }, checks);

    if let Some(dir) = emit {
        std::fs::create_dir_all(dir)?;
        let rh = a.composite_record["hash"].as_str().unwrap();
        std::fs::write(dir.join(format!("{rh}.json")), serde_json::to_string_pretty(&a.composite_record)?)?;
        let bh = a.composite_record["body_hash"].as_str().unwrap();
        std::fs::write(dir.join(format!("{bh}.json")), serde_json::to_string_pretty(&a.composite_body)?)?;
        println!("  emitted     {} + {} -> {}", rh, bh, dir.display());
    }

    // Close the loop: publish the assembled composite (its self-contained inlined body first, so the
    // record's body_hash resolves, then the record) back to the node's verify-then-store gate.
    if publish {
        let url = node.expect("--publish requires --node");
        nl_validator::commons_client::publish_artifact(url, &a.composite_body)?;
        nl_validator::commons_client::publish_artifact(url, &a.composite_record)?;
        println!("  published   composite record + body to {url}  (now discoverable + assemble-able)");
    }
    Ok(())
}

fn cmd_cluster(records: &PathBuf, solver: &str) -> Result<()> {
    let classes = nl_validator::cluster_dir(records, solver)?;
    let multi: Vec<&Vec<String>> = classes.iter().filter(|c| c.len() > 1).collect();
    if multi.is_empty() {
        println!("no equivalence classes found ({} function(s), all distinct)", classes.len());
    } else {
        for class in &multi {
            println!("class (canonical {}):", class[0]);
            for m in class.iter().skip(1) {
                println!("  ≡ {m}");
            }
        }
    }
    println!("{} class(es) over {} function(s)", classes.len(), classes.iter().map(|c| c.len()).sum::<usize>());
    Ok(())
}

fn cmd_verify_claim(assert: &str, records: Option<&PathBuf>, node: Option<&str>) -> Result<()> {
    // The assert is a local file, or — the third-party receiver's case — a bare `msg_…` address
    // fetched (and hash-verified) from the node.
    let assert = if assert.starts_with("msg_") {
        let url = node.ok_or_else(|| anyhow::anyhow!("a msg_… address needs --node to fetch from"))?;
        nl_validator::commons_client::fetch_artifact(url, assert)?
    } else {
        nl_validator::read_json(&PathBuf::from(assert))?
    };
    let link_map = match (records, node) {
        (Some(dir), None) => nl_validator::build_link_map(dir)?,
        (None, Some(url)) => {
            // Resolve everything the claim references from the node, hash-verified.
            let (_, link_map) = nl_validator::commons_client::maps_from_node(
                url,
                &nl_validator::commons_client::seed_addresses(&assert),
            )?;
            link_map
        }
        _ => anyhow::bail!("supply exactly one of --records / --node"),
    };
    let observed = assert.pointer("/body/claim/kind").and_then(|k| k.as_str()) == Some("observed");
    if nl_validator::verify_claim(&assert, link_map)? {
        if observed {
            println!("CONFIRMED  the claim replayed true against its recorded observations (the observations themselves are the signer's testimony)");
        } else {
            println!("CONFIRMED  the claim re-ran true against the commons");
        }
        Ok(())
    } else {
        Err(anyhow::anyhow!("REFUTED  the claim re-ran false"))
    }
}

/// Load JSON messages from a list of paths (each a `.json` file or a directory of them). When
/// `kind_filter` is `Some(k)`, only messages whose `kind` is `k` are kept; `None` keeps all. Directory
/// entries are read in sorted order for determinism.
fn load_json_messages(paths: &[PathBuf], kind_filter: Option<&str>) -> Result<Vec<serde_json::Value>> {
    let mut out = Vec::new();
    for path in paths {
        let mut consider = |p: &Path| -> Result<()> {
            if p.extension().and_then(|e| e.to_str()) != Some("json") {
                return Ok(());
            }
            let v = nl_validator::read_json(&p.to_path_buf())?;
            let keep = kind_filter.is_none_or(|k| v.get("kind").and_then(|x| x.as_str()) == Some(k));
            if keep {
                out.push(v);
            }
            Ok(())
        };
        if path.is_dir() {
            let mut entries: Vec<PathBuf> = std::fs::read_dir(path)
                .map_err(|e| anyhow::anyhow!("reading {}: {e}", path.display()))?
                .filter_map(|e| e.ok().map(|e| e.path()))
                .collect();
            entries.sort();
            for p in entries {
                consider(&p)?;
            }
        } else {
            consider(path)?;
        }
    }
    Ok(out)
}

fn cmd_verify_delegation(
    capability: &str,
    grantee: &str,
    roots: &[String],
    delegations: &[PathBuf],
    at: Option<&str>,
) -> Result<()> {
    use std::collections::BTreeSet;
    let tokens = load_json_messages(delegations, Some("delegate"))?;
    let roots: BTreeSet<String> = roots.iter().cloned().collect();
    let verdict = nl_validator::verify_delegation_chain(capability, grantee, &tokens, &roots, at);
    if verdict.authorized {
        println!("AUTHORIZED  {}", verdict.reason);
        for (i, link) in verdict.chain.iter().enumerate() {
            let to = link.grantee.as_deref().unwrap_or("<bearer>");
            println!("  [{i}] {} --({})--> {}", link.granter, link.capability, to);
        }
        if !verdict.conditions.is_empty() {
            println!("  conditions (enforce in policy): {}", verdict.conditions.join("; "));
        }
        Ok(())
    } else {
        Err(anyhow::anyhow!("UNAUTHORIZED  {}", verdict.reason))
    }
}

fn cmd_evaluate_trust(
    policy: &PathBuf,
    attestations: &[PathBuf],
    subject: &str,
    domain: Option<&str>,
    at: Option<&str>,
) -> Result<()> {
    let policy = nl_validator::Policy::from_json(&nl_validator::read_json(policy)?)?;
    let messages = load_json_messages(attestations, None)?; // asserts + retracts
    let graph = nl_validator::AttestationGraph::from_messages(&messages, at);
    let verdict = policy.evaluate_trust(&graph, subject, domain, at);
    let scope = domain.map(|d| format!(" for domain `{d}`")).unwrap_or_default();
    if verdict.trusted {
        println!("TRUSTED      `{subject}`{scope}: {}", verdict.reason);
        Ok(())
    } else {
        Err(anyhow::anyhow!("UNTRUSTED    `{subject}`{scope}: {}", verdict.reason))
    }
}

fn cmd_certified(
    policy: &PathBuf,
    attestations: &[PathBuf],
    subject: &str,
    domain: Option<&str>,
    at: Option<&str>,
) -> Result<()> {
    let policy = nl_validator::Policy::from_json(&nl_validator::read_json(policy)?)?;
    let messages = load_json_messages(attestations, None)?; // certifications + eval attestations + attestations + retracts
    let graph = nl_validator::AttestationGraph::from_messages(&messages, at);
    // A `wgt_…` subject asks the weights question (signed eval attestations, `attests-eval` edges);
    // anything else asks the function question (signed certifications, `certifies` edges).
    let verdict = if subject.starts_with("wgt_") {
        policy.eval_attestation_verdict(&graph, subject, domain, at)
    } else {
        policy.certification_verdict(&graph, subject, domain, at)
    };
    if verdict.certified {
        println!("CERTIFIED    `{subject}`: {}", verdict.reason);
        for c in &verdict.trusted_certifiers {
            println!("  certifier: {c}");
        }
        Ok(())
    } else {
        Err(anyhow::anyhow!("UNCERTIFIED  `{subject}`: {}", verdict.reason))
    }
}

fn cmd_authorize(
    policy: &PathBuf,
    capability: &str,
    grantee: &str,
    delegations: &[PathBuf],
    at: Option<&str>,
) -> Result<()> {
    let policy = nl_validator::Policy::from_json(&nl_validator::read_json(policy)?)?;
    let tokens = load_json_messages(delegations, Some("delegate"))?;
    let verdict = policy.authorize_capability(capability, grantee, &tokens, at);
    if verdict.authorized {
        println!("AUTHORIZED  {}", verdict.reason);
        Ok(())
    } else {
        Err(anyhow::anyhow!("UNAUTHORIZED  {}", verdict.reason))
    }
}

/// Materialize the commons view for the loop: from a local directory, or from a live node by
/// querying each intent for candidate `fn_…` addresses and walking their hash-verified reference
/// closure ([`nl_validator::commons_client::maps_from_node`]).
fn commons_view(
    records: Option<&PathBuf>,
    node: Option<&str>,
    intents: &[String],
) -> Result<(std::collections::HashMap<String, serde_json::Value>, std::collections::HashMap<String, serde_json::Value>)> {
    match (records, node) {
        (Some(dir), None) => Ok((nl_validator::build_link_map(dir)?, nl_validator::build_record_map(dir)?)),
        (None, Some(url)) => {
            let mut seeds: Vec<String> = Vec::new();
            for intent in intents {
                let matched = nl_validator::commons_client::query_intent(url, intent, 25)?;
                eprintln!("node query  intent {intent:?} -> {} match(es)", matched.len());
                seeds.extend(matched);
            }
            let (record_map, link_map) = nl_validator::commons_client::maps_from_node(url, &seeds)?;
            eprintln!("node fetch  {} record(s) + {} linked artifact(s), all hash-verified", record_map.len(),
                      link_map.len().saturating_sub(record_map.len()));
            Ok((link_map, record_map))
        }
        _ => anyhow::bail!("supply exactly one of --records / --node"),
    }
}

/// With `--publish`, send the run's final signed `assert` (the result claim) back to the node —
/// through its verify-then-store gate, like any other artifact. Any recorded trace artifacts go
/// FIRST: an `observed` claim is unverifiable by a third party who cannot fetch its trace.
fn publish_final_assert(node: &str, steps: &[nl_validator::Step]) -> Result<()> {
    let assert = steps
        .iter()
        .rev()
        .find(|s| s.message.get("kind").and_then(|k| k.as_str()) == Some("assert"))
        .ok_or_else(|| anyhow::anyhow!("no assert step to publish"))?;
    for trace in steps.iter().filter(|s| s.message.get("kind").and_then(|k| k.as_str()) == Some("trace")) {
        let t = nl_validator::commons_client::publish_artifact(node, &trace.message)?;
        let addr = nl_validator::hash_artifact_with_kind(&trace.message, nl_validator::ArtifactKind::Trace)?;
        println!("published  {addr} -> node accepted ({})",
                 t.get("status").and_then(|s| s.as_str()).unwrap_or("stored"));
    }
    let resp = nl_validator::commons_client::publish_artifact(node, &assert.message)?;
    let h = assert.message.get("hash").and_then(|h| h.as_str()).unwrap_or("");
    // The FULL address, deliberately: this line is the handoff — a third party re-verifies with
    // exactly `verify-claim <this address> --node <url>`, so a truncated print is unusable.
    println!("published  {h} -> node accepted ({})",
             resp.get("status").and_then(|s| s.as_str()).unwrap_or("stored"));
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn cmd_orchestrate(
    records: Option<&PathBuf>,
    node: Option<&str>,
    publish: bool,
    intents: &[String],
    args: &[PathBuf],
    seed: &str,
    responder_seed: &str,
    timestamp: Option<&str>,
) -> Result<()> {
    let argv = args.iter().map(|p| nl_validator::read_json(p)).collect::<Result<Vec<_>>>()?;
    let orch = nl_validator::signing_key_from_seed(seed);
    let resp = nl_validator::signing_key_from_seed(responder_seed);
    let (link, recs) = commons_view(records, node, intents)?;
    let run = nl_validator::orchestrate_with_maps(link, recs, intents, argv, &orch, &resp, timestamp)?;
    for step in &run.steps {
        let m = &step.message;
        let hash = m.get("hash").and_then(|h| h.as_str()).unwrap_or("");
        let short = &hash[..hash.len().min(18)];
        let kind = step.label.rsplit(':').next().unwrap_or(&step.label);
        let detail = match kind {
            "query" => format!("intent {}", m.pointer("/body/pattern/intent_tags").map(|v| v.to_string()).unwrap_or_default()),
            "ack" => format!("matches {}", m.pointer("/body/result/matches").map(|v| v.to_string()).unwrap_or_default()),
            "propose" => format!("apply {}", m.pointer("/body/target").and_then(|t| t.as_str()).unwrap_or_default()),
            "commit" => format!("commit apply {}", m.pointer("/body/commitment/fn").and_then(|t| t.as_str()).unwrap_or_default()),
            "trace" => format!("{} recorded observation(s)", m.get("ops").and_then(|o| o.as_array()).map(|a| a.len()).unwrap_or(0)),
            "assert" => format!("result {}", m.pointer("/body/claim/expr/args/1/value").map(|v| v.to_string()).unwrap_or_default()),
            other => other.to_string(),
        };
        println!("{:>8}  {short}…  {detail}", step.label);
    }
    if run.confirmed {
        println!("CONFIRMED  discovered the function, applied it, and re-verified the result");
        if publish {
            publish_final_assert(node.expect("--publish requires --node"), &run.steps)?;
        }
        Ok(())
    } else {
        Err(anyhow::anyhow!("orchestration did not confirm (rejected, or the claim failed to re-run)"))
    }
}

#[allow(clippy::too_many_arguments)]
fn cmd_orchestrate_verified(
    records: Option<&PathBuf>,
    node: Option<&str>,
    publish: bool,
    intents: &[String],
    args: &[PathBuf],
    seed: &str,
    responder_seed: &str,
    timestamp: Option<&str>,
    policy: Option<&PathBuf>,
    attestations: &[PathBuf],
    solver: &str,
    require_certified: bool,
    expect: Option<&PathBuf>,
    grant_certified: &[String],
) -> Result<()> {
    if intents.len() != 1 {
        anyhow::bail!("--verify supports exactly one --intent (got {})", intents.len());
    }
    let argv = args.iter().map(|p| nl_validator::read_json(p)).collect::<Result<Vec<_>>>()?;
    let orch = nl_validator::signing_key_from_seed(seed);
    let resp = nl_validator::signing_key_from_seed(responder_seed);
    let pol = policy.map(|p| nl_validator::read_json(p).and_then(|j| nl_validator::Policy::from_json(&j))).transpose()?;
    let atts = attestations.iter().map(|p| nl_validator::read_json(p)).collect::<Result<Vec<_>>>()?;
    let exp = expect.map(|p| nl_validator::read_json(p)).transpose()?;
    let (link, recs) = commons_view(records, node, intents)?;
    let run = nl_validator::orchestrate_verified_with_maps(link, recs, &intents[0], argv, &orch, &resp, solver, pol.as_ref(), &atts, timestamp, require_certified, exp, grant_certified)?;

    for step in &run.steps {
        let m = &step.message;
        let detail = match step.label.as_str() {
            "query" => format!("intent {}", m.pointer("/body/pattern/intent_tags").map(|v| v.to_string()).unwrap_or_default()),
            "ack" => format!("matches {}", m.pointer("/body/result/matches").map(|v| v.to_string()).unwrap_or_default()),
            "trust" => format!("trusted={} — {}", m.get("trusted").map(|v| v.to_string()).unwrap_or_default(), m.get("reason").and_then(|r| r.as_str()).unwrap_or("")),
            "rank" => format!(
                "goal-ordered {} candidate(s): {}",
                m.get("ordered").and_then(|o| o.as_array()).map(|a| a.len()).unwrap_or(0),
                m.get("scores").map(|v| v.to_string()).unwrap_or_default()
            ),
            "certify" => format!("certified={} {}", m.get("certified").map(|v| v.to_string()).unwrap_or_default(), m.get("failed").map(|v| v.to_string()).unwrap_or_default()),
            "grants" => format!(
                "trust gate {} for {} — {}",
                if m.get("trust_gate_open").and_then(|v| v.as_bool()).unwrap_or(false) { "OPEN (certified-gated grants apply)" } else { "closed (unconditional grants only)" },
                m.get("function").and_then(|f| f.as_str()).unwrap_or(""),
                m.get("reason").and_then(|r| r.as_str()).unwrap_or("")
            ),
            "prove" => format!("property `{}` proved={}", m.get("property").and_then(|p| p.as_str()).unwrap_or(""), m.get("proved").map(|v| v.to_string()).unwrap_or_default()),
            "propose" => format!("apply {}", m.pointer("/body/target").and_then(|t| t.as_str()).unwrap_or_default()),
            "trace" => format!("{} recorded observation(s)", m.get("ops").and_then(|o| o.as_array()).map(|a| a.len()).unwrap_or(0)),
            "assert" => format!("result {}", m.pointer("/body/claim/expr/args/1/value").map(|v| v.to_string()).unwrap_or_default()),
            other => other.to_string(),
        };
        println!("{:>8}  {detail}", step.label);
    }

    let property_ok = run.property.as_ref().map(|(_, p)| *p).unwrap_or(true);
    let trust_ok = run.trusted != Some(false);
    let certify_ok = !require_certified || run.certified == Some(true);
    if run.confirmed && property_ok && trust_ok && certify_ok {
        let cert_note = match run.certified { Some(true) => ", certified", Some(false) => ", NOT certified", None => "" };
        println!("CONFIRMED  trusted, its property proved{cert_note}, applied, and re-verified");
        if publish {
            publish_final_assert(node.expect("--publish requires --node"), &run.steps)?;
        }
        Ok(())
    } else if run.trusted == Some(false) {
        Err(anyhow::anyhow!("ABORTED    the discovered function is not trusted under the policy"))
    } else if require_certified && run.certified != Some(true) {
        Err(anyhow::anyhow!("ABORTED    the discovered function is not certified (--require-certified)"))
    } else if !property_ok {
        Err(anyhow::anyhow!("NOT-PROVEN the discovered function's own property did not prove"))
    } else {
        Err(anyhow::anyhow!("orchestration did not confirm (rejected, or the claim failed to re-run)"))
    }
}

fn cmd_eval(
    body: &PathBuf,
    args: &[PathBuf],
    grants: &[String],
    replay: Option<&PathBuf>,
    trace_out: Option<&PathBuf>,
    records: Option<&PathBuf>,
) -> Result<()> {
    let body = nl_validator::read_json(body)?;
    let argv = args.iter().map(|p| nl_validator::read_json(p)).collect::<Result<Vec<_>>>()?;
    if let Some(dir) = records {
        // Link: a composed body applies commons functions by `fn_ref`; resolve them from the
        // directory so composition evaluates (and traces) like `run --records`.
        nl_validator::set_resolver(nl_validator::build_link_map(dir)?);
    }
    // Effect sandbox: the body may only perform effects in the granted set.
    nl_validator::set_effect_grants(grants.iter().cloned());
    if let Some(rp) = replay {
        let entries = nl_validator::read_json(rp)?;
        // Accept both the bare-array form (legacy --trace-out) and the trace ARTIFACT form
        // ({kind: "trace", version, ops: […]} — spec/trace.schema.json, what --trace-out now writes).
        let arr = entries
            .get("ops")
            .and_then(|o| o.as_array())
            .or_else(|| entries.as_array())
            .ok_or_else(|| anyhow::anyhow!("replay file must be a trace artifact ({{kind: \"trace\", ops: […]}}) or a JSON array of trace entries"))?;
        nl_validator::set_effect_replay(arr.clone());
    }
    let result = nl_validator::eval_body(&body, &argv);
    let trace = nl_validator::take_effect_trace();
    nl_validator::clear_effects();
    nl_validator::clear_resolver();
    let result = result?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    if let Some(out) = trace_out {
        // Written as a first-class trace ARTIFACT — self-describing (principle 1), self-addressing
        // as trc_…, publishable to the commons, and referenceable by an `observed` claim.
        let artifact = serde_json::json!({ "kind": "trace", "version": "0.1.0", "ops": trace });
        let addr = nl_validator::hash_artifact_with_kind(&artifact, nl_validator::ArtifactKind::Trace)?;
        let pretty = serde_json::to_string_pretty(&artifact)
            .map_err(|e| anyhow::anyhow!("serializing trace: {e}"))?;
        std::fs::write(out, format!("{pretty}\n")).map_err(|e| anyhow::anyhow!("writing {}: {e}", out.display()))?;
        eprintln!("trace artifact {addr} written to {}", out.display());
    } else if !trace.is_empty() {
        eprintln!("effect trace ({} event{}):", trace.len(), if trace.len() == 1 { "" } else { "s" });
        for ev in &trace {
            eprintln!("  {}", serde_json::to_string(ev)?);
        }
    }
    Ok(())
}

fn cmd_run(record: &PathBuf, body: Option<&PathBuf>, records: Option<&PathBuf>) -> Result<()> {
    let record = nl_validator::read_json(record)?;
    let body = match (body, records) {
        (Some(b), Some(dir)) => {
            // Supplied body, but still resolve `fn_ref` arguments against the directory — so a
            // hand-supplied (or model-written) body whose examples reference commons helpers by
            // address runs end-to-end, exactly as the `--records`-only path does.
            let map = nl_validator::build_link_map(dir)?;
            nl_validator::set_resolver(map);
            nl_validator::read_json(b)?
        }
        (Some(b), None) => nl_validator::read_json(b)?,
        (None, Some(dir)) => {
            // Link: resolve the record's body_hash from the directory, and set the resolver so that
            // fn_ref arguments resolve to their referenced bodies (composition).
            let map = nl_validator::build_link_map(dir)?;
            let bh = record
                .get("body_hash")
                .and_then(|b| b.as_str())
                .ok_or_else(|| anyhow::anyhow!("record has no body_hash to resolve"))?;
            let resolved = map
                .get(bh)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("could not resolve body_hash {bh} from {}", dir.display()))?;
            nl_validator::set_resolver(map);
            resolved
        }
        (None, None) => bail!("provide --body <body.json> or --records <dir> to resolve the body"),
    };
    // Enforce effects: the body may only perform effects the record declares in signature.effects.
    let declared_effects: Vec<String> = record
        .get("signature")
        .and_then(|s| s.get("effects"))
        .and_then(|e| e.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default();
    nl_validator::set_effect_grants(declared_effects);
    let runs = nl_validator::run_examples(&record, &body);
    nl_validator::clear_resolver();
    nl_validator::clear_effects();
    let runs = runs?;
    if runs.is_empty() {
        println!("run: no examples to execute");
        return Ok(());
    }
    let mut failed = 0;
    for r in &runs {
        if r.passed {
            println!("example {:>2}  PASS  {}", r.index, r.got);
        } else {
            failed += 1;
            match &r.error {
                Some(e) => println!("example {:>2}  FAIL  error: {e}", r.index),
                None => println!("example {:>2}  FAIL  got {}  want {}", r.index, r.got, r.expected),
            }
        }
    }
    let passed = runs.len() - failed;
    if failed == 0 {
        println!("run: {passed}/{} examples passed", runs.len());
        Ok(())
    } else {
        Err(anyhow::anyhow!("run: {failed}/{} examples failed", runs.len()))
    }
}

/// Read a raw surface string from the `input` argument, or from stdin when it is
/// `None`. Used by the `parse-*` subcommands, whose input is a surface string,
/// not a JSON file.
#[cfg(feature = "surface")]
fn read_surface_input(input: Option<String>) -> Result<String> {
    match input {
        Some(s) => Ok(s),
        None => {
            use std::io::Read;
            let mut s = String::new();
            std::io::stdin()
                .read_to_string(&mut s)
                .map_err(|e| anyhow::anyhow!("reading surface string from stdin: {e}"))?;
            Ok(s)
        }
    }
}

/// Read a JSON AST from a file path, or from stdin when the path is `-`. Used by
/// the `unparse-*` subcommands.
#[cfg(feature = "surface")]
fn read_ast_input(file: &Path) -> Result<serde_json::Value> {
    if file.as_os_str() == "-" {
        use std::io::Read;
        let mut s = String::new();
        std::io::stdin()
            .read_to_string(&mut s)
            .map_err(|e| anyhow::anyhow!("reading JSON AST from stdin: {e}"))?;
        serde_json::from_str(&s).map_err(|e| anyhow::anyhow!("parsing JSON AST from stdin: {e}"))
    } else {
        nl_validator::read_json(file)
    }
}

#[cfg(feature = "surface")]
fn cmd_parse_type(input: Option<String>) -> Result<()> {
    let src = read_surface_input(input)?;
    let ast = nl_validator::surface::parse_type(&src).map_err(|e| anyhow::anyhow!("{e}"))?;
    let pretty = serde_json::to_string_pretty(&ast)
        .map_err(|e| anyhow::anyhow!("serializing type AST: {e}"))?;
    println!("{pretty}");
    Ok(())
}

#[cfg(feature = "surface")]
fn cmd_unparse_type(file: &Path) -> Result<()> {
    let value = read_ast_input(file)?;
    let s = nl_validator::surface::unparse_type(&value).map_err(|e| anyhow::anyhow!("{e}"))?;
    println!("{s}");
    Ok(())
}

#[cfg(feature = "surface")]
fn cmd_parse_value(input: Option<String>) -> Result<()> {
    let src = read_surface_input(input)?;
    let ast = nl_validator::surface::parse_value(&src).map_err(|e| anyhow::anyhow!("{e}"))?;
    let pretty = serde_json::to_string_pretty(&ast)
        .map_err(|e| anyhow::anyhow!("serializing value AST: {e}"))?;
    println!("{pretty}");
    Ok(())
}

#[cfg(feature = "surface")]
fn cmd_unparse_value(file: &Path) -> Result<()> {
    let value = read_ast_input(file)?;
    let s = nl_validator::surface::unparse_value(&value).map_err(|e| anyhow::anyhow!("{e}"))?;
    println!("{s}");
    Ok(())
}

#[cfg(feature = "surface")]
fn cmd_parse_predicate(input: Option<String>) -> Result<()> {
    let src = read_surface_input(input)?;
    let ast = nl_validator::surface::parse_predicate(&src).map_err(|e| anyhow::anyhow!("{e}"))?;
    let pretty = serde_json::to_string_pretty(&ast)
        .map_err(|e| anyhow::anyhow!("serializing predicate AST: {e}"))?;
    println!("{pretty}");
    Ok(())
}

#[cfg(feature = "surface")]
fn cmd_unparse_predicate(file: &Path) -> Result<()> {
    let value = read_ast_input(file)?;
    let s = nl_validator::surface::unparse_predicate(&value).map_err(|e| anyhow::anyhow!("{e}"))?;
    println!("{s}");
    Ok(())
}

#[cfg(feature = "surface")]
fn cmd_parse_body(input: Option<String>) -> Result<()> {
    let src = read_surface_input(input)?;
    let ast = nl_validator::surface::parse_body(&src).map_err(|e| anyhow::anyhow!("{e}"))?;
    let pretty =
        serde_json::to_string_pretty(&ast).map_err(|e| anyhow::anyhow!("serializing body AST: {e}"))?;
    println!("{pretty}");
    Ok(())
}

#[cfg(feature = "surface")]
fn cmd_unparse_body(file: &Path) -> Result<()> {
    let value = read_ast_input(file)?;
    let s = nl_validator::surface::unparse_body(&value).map_err(|e| anyhow::anyhow!("{e}"))?;
    println!("{s}");
    Ok(())
}

fn cmd_sign(record: &PathBuf, seed: &str, in_place: bool) -> Result<()> {
    use std::io::Write;
    let mut value = nl_validator::read_json(record)?;
    // Refuse to sign anything that isn't a Nova Locutio message — signing a
    // function record or body expression makes no sense in v0.1.
    match nl_validator::ArtifactKind::detect(&value)? {
        nl_validator::ArtifactKind::Message => {}
        nl_validator::ArtifactKind::FunctionRecord => {
            return Err(anyhow::anyhow!(
                "`sign` only applies to Nova Locutio messages; got a function record"
            ));
        }
        nl_validator::ArtifactKind::BodyExpression => {
            return Err(anyhow::anyhow!(
                "`sign` only applies to Nova Locutio messages; got a body expression"
            ));
        }
        nl_validator::ArtifactKind::Certification => {
            return Err(anyhow::anyhow!(
                "a certification is signed by `certify --sign <seed>`, not `sign`"
            ));
        }
        nl_validator::ArtifactKind::Weights => {
            return Err(anyhow::anyhow!(
                "weights records are unsigned — measured capability is attested by `attest-weights --sign <seed>`"
            ));
        }
        nl_validator::ArtifactKind::EvalAttestation => {
            return Err(anyhow::anyhow!(
                "an eval attestation is signed by `attest-weights --sign <seed>`, not `sign`"
            ));
        }
        nl_validator::ArtifactKind::Trace => {
            return Err(anyhow::anyhow!(
                "a trace is unsigned — it is content-addressed evidence referenced by a *signed* `observed` assert"
            ));
        }
    }
    let key = nl_validator::signing_key_from_seed(seed);
    nl_validator::sign_message(&mut value, &key)?;
    let pretty = serde_json::to_string_pretty(&value)
        .map_err(|e| anyhow::anyhow!("serializing signed message: {e}"))?;
    if in_place {
        std::fs::write(record, format!("{pretty}\n"))
            .map_err(|e| anyhow::anyhow!("writing {}: {e}", record.display()))?;
        eprintln!("signed in place: {}", record.display());
    } else {
        std::io::stdout().write_all(pretty.as_bytes())?;
        std::io::stdout().write_all(b"\n")?;
    }
    Ok(())
}

#[cfg(test)]
mod arity_tests {
    use super::type_arity;

    #[test]
    fn reads_arity_from_structured_and_polymorphic_types() {
        // Monomorphic function: 2 params.
        let binary = r#"{"kind":"fn","params":[{"kind":"builtin","name":"nat"},{"kind":"builtin","name":"int"}],"result":{"kind":"builtin","name":"int"}}"#;
        assert_eq!(type_arity(binary), Some(2));
        // Polymorphic (forall-wrapped) function: still 1 — the forall must be unwrapped.
        let poly = r#"{"kind":"forall","vars":["a"],"body":{"kind":"fn","params":[{"kind":"var","name":"a"}],"result":{"kind":"var","name":"a"}}}"#;
        assert_eq!(type_arity(poly), Some(1));
        // A non-function structured type is a value — arity 0 (never a usable stage).
        assert_eq!(type_arity(r#"{"kind":"builtin","name":"int"}"#), Some(0));
        // A v0.1 surface string / unparseable type is unknown — None (caller keeps it, never prunes).
        assert_eq!(type_arity("nat -> int"), None);
    }
}
