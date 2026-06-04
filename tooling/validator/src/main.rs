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
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Commands::Validate { schema, record } => cmd_validate(&schema, &record),
    };

    match result {
        Ok(()) => {
            eprintln!("OK");
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
