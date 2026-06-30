//! `nl-validator`: reference CLI for validating Novae Linguae artifacts.

use anyhow::{bail, Result};
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
}

impl From<CliKind> for nl_validator::ArtifactKind {
    fn from(k: CliKind) -> Self {
        match k {
            CliKind::FunctionRecord => Self::FunctionRecord,
            CliKind::Message => Self::Message,
            CliKind::Body => Self::BodyExpression,
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
        /// Grant an effect the body may perform (e.g. `io.console`, `random`, `fs.read`, `fs.write`).
        /// Repeatable. An effectful builtin whose effect is not granted is rejected at eval time;
        /// pure bodies need no grants. The performed effects are printed as a trace.
        #[arg(long = "grant")]
        grants: Vec<String>,
        /// Replay a recorded effect trace (a JSON array from --trace-out): effectful builtins return
        /// their recorded results instead of performing real I/O — deterministic re-execution (P5).
        #[arg(long)]
        replay: Option<PathBuf>,
        /// Write the effect trace (a JSON array of {effect, detail, result}) here instead of to
        /// stderr — feed it back to a later `--replay`.
        #[arg(long = "trace-out")]
        trace_out: Option<PathBuf>,
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
    },
    /// Autonomous orchestration (spec/agent-loop.md): drive a full `query → propose → commit →
    /// assert → verify` conversation. The orchestrator discovers a commons function by `--intent`,
    /// proposes applying it to the `--arg`s, the responder commits + fulfils, and the orchestrator
    /// verifies the result. Prints the signed transcript; exit 1 if it isn't CONFIRMED.
    Orchestrate {
        /// Directory of records/bodies (the commons view).
        #[arg(long)]
        records: PathBuf,
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
    },
    /// Verify a Nova Locutio `assert` by RE-RUNNING its `predicate` claim against the commons:
    /// resolve the claim's content-addressed function(s) from `--records` and evaluate it. The
    /// receiver half of the agent loop — trust nothing, re-execute (principle 3). Exit 0 if the
    /// claim re-runs true (CONFIRMED), 1 if false (REFUTED) or undecidable.
    VerifyClaim {
        /// Path to the `assert` message whose claim to re-run.
        assert: PathBuf,
        /// Directory of records/bodies to resolve the claim's functions against.
        #[arg(long)]
        records: PathBuf,
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
        Commands::Eval { body, args, grants, replay, trace_out } => {
            (cmd_eval(&body, &args, &grants, replay.as_ref(), trace_out.as_ref()), false)
        }
        Commands::Run { record, body, records } => (cmd_run(&record, body.as_ref(), records.as_ref()), false),
        Commands::CheckEffects { record, body, records } => {
            (cmd_check_effects(&record, &body, records.as_ref()), false)
        }
        Commands::Typecheck { record, body } => (cmd_typecheck(&record, &body), false),
        Commands::CheckRefinement { record, body, solver } => (cmd_check_refinement(&record, &body, &solver), false),
        Commands::CheckTermination { record, body } => (cmd_check_termination(&record, &body), false),
        Commands::Respond { request, records, seed, timestamp } => {
            (cmd_respond(&request, &records, &seed, timestamp.as_deref()), false)
        }
        Commands::Prove { record, body, smt_out, solver } => {
            (cmd_prove(&record, body.as_ref(), smt_out.as_ref(), &solver), false)
        }
        Commands::Equiv { body_f, body_g, solver } => (cmd_equiv(&body_f, &body_g, &solver), false),
        Commands::Compose { records } => (cmd_compose(&records), false),
        Commands::Cluster { records, solver } => (cmd_cluster(&records, &solver), false),
        Commands::Normalize { body, hash } => (cmd_normalize(&body, hash), false),
        Commands::VerifyClaim { assert, records } => (cmd_verify_claim(&assert, &records), false),
        Commands::VerifyDelegation { capability, grantee, roots, delegations, at } => {
            (cmd_verify_delegation(&capability, &grantee, &roots, &delegations, at.as_deref()), false)
        }
        Commands::EvaluateTrust { policy, attestations, subject, domain, at } => {
            (cmd_evaluate_trust(&policy, &attestations, &subject, domain.as_deref(), at.as_deref()), false)
        }
        Commands::Authorize { policy, capability, grantee, delegations, at } => {
            (cmd_authorize(&policy, &capability, &grantee, &delegations, at.as_deref()), false)
        }
        Commands::Orchestrate { records, intents, args, seed, responder_seed, timestamp, verify, policy, attestations, solver } => {
            if verify {
                (cmd_orchestrate_verified(&records, &intents, &args, &seed, &responder_seed, timestamp.as_deref(), policy.as_ref(), &attestations, &solver), false)
            } else {
                (cmd_orchestrate(&records, &intents, &args, &seed, &responder_seed, timestamp.as_deref()), false)
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

    // Signature check (messages only).
    let sig_pass = match kind {
        nl_validator::ArtifactKind::FunctionRecord => {
            println!("signature N/A   function records have no signature");
            true
        }
        nl_validator::ArtifactKind::Message => match nl_validator::verify_signature(&value) {
            Ok(()) => {
                println!("signature PASS");
                true
            }
            Err(e) => {
                println!("signature FAIL  {e:#}");
                false
            }
        },
        nl_validator::ArtifactKind::BodyExpression => unreachable!(
            "body expressions are refused above"
        ),
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
    match nl_validator::analyze_termination(&body) {
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

fn cmd_respond(
    request: &PathBuf,
    records: &PathBuf,
    seed: &str,
    timestamp: Option<&str>,
) -> Result<()> {
    use std::io::Write;
    let message = nl_validator::read_json(request)?;
    let link_map = nl_validator::build_link_map(records)?;
    let record_map = nl_validator::build_record_map(records)?;
    let key = nl_validator::signing_key_from_seed(seed);
    let reply = nl_validator::respond_to_message(&message, link_map, record_map, &key, timestamp)?;
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
        println!("  type        {} -> {}", ty(&m.input_type), ty(&m.output_type));
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

fn cmd_verify_claim(assert: &PathBuf, records: &PathBuf) -> Result<()> {
    let assert = nl_validator::read_json(assert)?;
    let link_map = nl_validator::build_link_map(records)?;
    if nl_validator::verify_claim(&assert, link_map)? {
        println!("CONFIRMED  the claim re-ran true against the commons");
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

fn cmd_orchestrate(
    records: &PathBuf,
    intents: &[String],
    args: &[PathBuf],
    seed: &str,
    responder_seed: &str,
    timestamp: Option<&str>,
) -> Result<()> {
    let argv = args.iter().map(|p| nl_validator::read_json(p)).collect::<Result<Vec<_>>>()?;
    let orch = nl_validator::signing_key_from_seed(seed);
    let resp = nl_validator::signing_key_from_seed(responder_seed);
    let run = nl_validator::orchestrate(records, intents, argv, &orch, &resp, timestamp)?;
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
            "assert" => format!("result {}", m.pointer("/body/claim/expr/args/1/value").map(|v| v.to_string()).unwrap_or_default()),
            other => other.to_string(),
        };
        println!("{:>8}  {short}…  {detail}", step.label);
    }
    if run.confirmed {
        println!("CONFIRMED  discovered the function, applied it, and re-verified the result");
        Ok(())
    } else {
        Err(anyhow::anyhow!("orchestration did not confirm (rejected, or the claim failed to re-run)"))
    }
}

#[allow(clippy::too_many_arguments)]
fn cmd_orchestrate_verified(
    records: &PathBuf,
    intents: &[String],
    args: &[PathBuf],
    seed: &str,
    responder_seed: &str,
    timestamp: Option<&str>,
    policy: Option<&PathBuf>,
    attestations: &[PathBuf],
    solver: &str,
) -> Result<()> {
    if intents.len() != 1 {
        anyhow::bail!("--verify supports exactly one --intent (got {})", intents.len());
    }
    let argv = args.iter().map(|p| nl_validator::read_json(p)).collect::<Result<Vec<_>>>()?;
    let orch = nl_validator::signing_key_from_seed(seed);
    let resp = nl_validator::signing_key_from_seed(responder_seed);
    let pol = policy.map(|p| nl_validator::read_json(p).and_then(|j| nl_validator::Policy::from_json(&j))).transpose()?;
    let atts = attestations.iter().map(|p| nl_validator::read_json(p)).collect::<Result<Vec<_>>>()?;
    let run = nl_validator::orchestrate_verified(records, &intents[0], argv, &orch, &resp, solver, pol.as_ref(), &atts, timestamp)?;

    for step in &run.steps {
        let m = &step.message;
        let detail = match step.label.as_str() {
            "query" => format!("intent {}", m.pointer("/body/pattern/intent_tags").map(|v| v.to_string()).unwrap_or_default()),
            "ack" => format!("matches {}", m.pointer("/body/result/matches").map(|v| v.to_string()).unwrap_or_default()),
            "trust" => format!("trusted={} — {}", m.get("trusted").map(|v| v.to_string()).unwrap_or_default(), m.get("reason").and_then(|r| r.as_str()).unwrap_or("")),
            "prove" => format!("property `{}` proved={}", m.get("property").and_then(|p| p.as_str()).unwrap_or(""), m.get("proved").map(|v| v.to_string()).unwrap_or_default()),
            "propose" => format!("apply {}", m.pointer("/body/target").and_then(|t| t.as_str()).unwrap_or_default()),
            "assert" => format!("result {}", m.pointer("/body/claim/expr/args/1/value").map(|v| v.to_string()).unwrap_or_default()),
            other => other.to_string(),
        };
        println!("{:>8}  {detail}", step.label);
    }

    let property_ok = run.property.as_ref().map(|(_, p)| *p).unwrap_or(true);
    let trust_ok = run.trusted != Some(false);
    if run.confirmed && property_ok && trust_ok {
        println!("CONFIRMED  trusted, its property proved, applied, and re-verified");
        Ok(())
    } else if run.trusted == Some(false) {
        Err(anyhow::anyhow!("ABORTED    the discovered function is not trusted under the policy"))
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
) -> Result<()> {
    let body = nl_validator::read_json(body)?;
    let argv = args.iter().map(|p| nl_validator::read_json(p)).collect::<Result<Vec<_>>>()?;
    // Effect sandbox: the body may only perform effects in the granted set.
    nl_validator::set_effect_grants(grants.iter().cloned());
    if let Some(rp) = replay {
        let entries = nl_validator::read_json(rp)?;
        let arr = entries
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("replay file must be a JSON array of trace entries"))?;
        nl_validator::set_effect_replay(arr.clone());
    }
    let result = nl_validator::eval_body(&body, &argv);
    let trace = nl_validator::take_effect_trace();
    nl_validator::clear_effects();
    let result = result?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    if let Some(out) = trace_out {
        let pretty = serde_json::to_string_pretty(&serde_json::Value::Array(trace))
            .map_err(|e| anyhow::anyhow!("serializing trace: {e}"))?;
        std::fs::write(out, format!("{pretty}\n")).map_err(|e| anyhow::anyhow!("writing {}: {e}", out.display()))?;
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
