//! `nl-validator`: reference CLI for validating Novae Linguae artifacts.

use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;
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
    let schema = nl_validator::read_json(schema)?;
    let instance = nl_validator::read_json(record)?;
    nl_validator::validate(&schema, &instance)
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
