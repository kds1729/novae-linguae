//! `nl-validator`: reference CLI for validating Novae Linguae artifacts.

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::ExitCode;

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
    /// whether the input is a function record or a Nova Locutio message,
    /// strips the appropriate fields per spec/canonical-serialization.md,
    /// JCS-canonicalizes, and BLAKE3-256 hashes. Prints `<prefix>_<hex>`
    /// to stdout followed by a newline.
    Hash {
        /// Path to the artifact (function record or Nova Locutio message)
        record: PathBuf,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let (result, print_ok) = match cli.command {
        Commands::Validate { schema, record } => (cmd_validate(&schema, &record), true),
        Commands::Canonicalize { record } => (cmd_canonicalize(&record), false),
        Commands::Hash { record } => (cmd_hash(&record), false),
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

fn cmd_hash(record: &PathBuf) -> Result<()> {
    let value = nl_validator::read_json(record)?;
    let hash = nl_validator::hash_artifact(&value)?;
    println!("{hash}");
    Ok(())
}
