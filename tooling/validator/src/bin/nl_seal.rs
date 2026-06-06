//! nl-seal: hardened reference CLI for the Nova Locutio encrypted envelope (spec/encryption.md).
//!
//! Built on vetted constant-time crates and byte-compatible with the Python reference
//! (tooling/crypto-python) — conformance is the shared vectors in spec/conformance/encryption.json.
//!
//!   nl-seal seal   <plaintext-file> --to <did> [--to <did> ...] [--aad <str>] [--seed <hex>]
//!   nl-seal open   <envelope.json>  --did <did> --recipient-seed <user-seed>
//!   nl-seal conformance [<encryption.json>]   # replay the conformance vectors

use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[derive(Parser)]
#[command(name = "nl-seal", about = "Hardened encrypted-envelope seal/open for Nova Locutio")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Seal a plaintext file to one or more did:nova recipients; prints the envelope JSON.
    Seal {
        /// File whose bytes are the plaintext payload.
        plaintext: PathBuf,
        /// Recipient did:nova DID (repeatable).
        #[arg(long = "to", required = true)]
        to: Vec<String>,
        /// Optional additional authenticated data (UTF-8 string).
        #[arg(long)]
        aad: Option<String>,
        /// Deterministic RNG seed (hex). Reproducible — for tests/vectors only; omit for OS randomness.
        #[arg(long)]
        seed: Option<String>,
        /// Stealth addressing: hide the recipient set (omit cleartext `to`; recover by trial-decrypt).
        #[arg(long)]
        stealth: bool,
    },
    /// Open an envelope for a recipient, deriving its X25519 secret from a user seed; prints plaintext.
    Open {
        /// Path to the envelope JSON.
        envelope: PathBuf,
        /// Which recipient DID to open as.
        #[arg(long)]
        did: String,
        /// The recipient's user seed (the X25519 secret is derived from it).
        #[arg(long = "recipient-seed")]
        recipient_seed: String,
    },
    /// Replay the encryption conformance vectors (default: spec/conformance/encryption.json).
    Conformance {
        /// Path to the conformance vectors JSON.
        vectors: Option<PathBuf>,
    },
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<()> {
    use nl_validator::seal as s;
    match cli.command {
        Commands::Seal { plaintext, to, aad, seed, stealth } => {
            let pt = std::fs::read(&plaintext).with_context(|| format!("reading {}", plaintext.display()))?;
            let aad_bytes = aad.unwrap_or_default().into_bytes();
            let mut rng = match seed {
                Some(h) => s::Rng::seeded(decode_hex(&h)?),
                None => s::Rng::Os,
            };
            let env = s::seal(&pt, &to, &aad_bytes, &mut rng, stealth)?;
            println!("{}", serde_json::to_string_pretty(&env)?);
        }
        Commands::Open { envelope, did, recipient_seed } => {
            use std::io::Write;
            let env: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&envelope)?).context("parsing envelope")?;
            let xsk = s::x25519_secret_from_user_seed(&recipient_seed);
            let pt = s::open(&env, &did, &xsk)?;
            std::io::stdout().write_all(&pt)?;
        }
        Commands::Conformance { vectors } => {
            let path = vectors.unwrap_or_else(|| {
                Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spec/conformance/encryption.json")
            });
            let v: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&path)?).context("parsing vectors")?;
            s::run_conformance(&v)?;
            eprintln!("conformance: OK");
        }
    }
    Ok(())
}

fn decode_hex(h: &str) -> Result<Vec<u8>> {
    if h.len() % 2 != 0 {
        return Err(anyhow!("odd-length hex seed"));
    }
    (0..h.len() / 2)
        .map(|i| u8::from_str_radix(&h[2 * i..2 * i + 2], 16).map_err(|e| anyhow!("hex: {e}")))
        .collect()
}
