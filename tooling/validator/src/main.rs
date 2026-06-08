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
    /// end-to-end (e.g. map's example applying `double` by address).
    Run {
        /// Path to the function record (provides examples).
        record: PathBuf,
        /// Path to the body-expression JSON AST to execute (alternative to --records).
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
    /// Nova Locutio agent loop: consume a signed message and emit a signed reply (spec/agent-loop.md).
    /// Handles `request`/`apply` (run the target on the value-expression args → an `assert` whose
    /// `predicate` claim is `eq(target(args…), result)`, self-verifiable by re-running),
    /// `request`/`validate` (typecheck + run the target → `assert` it `verified`, else `reject`), and
    /// `query` (search the records → `ack` with the matches). Threaded by `in_reply_to`, addressed
    /// back to the sender. Prints the signed reply JSON.
    Respond {
        /// Path to the message to answer (a `request` or `query`).
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
        Commands::Respond { request, records, seed, timestamp } => {
            (cmd_respond(&request, &records, &seed, timestamp.as_deref()), false)
        }
        Commands::VerifyClaim { assert, records } => (cmd_verify_claim(&assert, &records), false),
        Commands::Orchestrate { records, intents, args, seed, responder_seed, timestamp } => {
            (cmd_orchestrate(&records, &intents, &args, &seed, &responder_seed, timestamp.as_deref()), false)
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
        (Some(b), _) => nl_validator::read_json(b)?,
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
