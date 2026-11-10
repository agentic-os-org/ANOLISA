//! Local fake generation without daemon traffic or token-bearing output.

use std::io;
use std::path::PathBuf;

use asc_credential_store::{generate_fake, read_token_file, write_fake_file};
use clap::{Args, Subcommand};

/// Local credential helpers; values are accepted only through private files.
#[derive(Debug, Subcommand)]
pub enum KeyCommand {
    /// Generate a fake locally into a new 0600 file; never contact the daemon.
    GenerateFake(GenerateFake),
}

/// Shape-only generation; client acceptance of the fake is not guaranteed.
#[derive(Debug, Args)]
pub struct GenerateFake {
    /// Caller-owned private file containing the real token.
    #[arg(long)]
    real_token_file: PathBuf,
    /// New output file; existing paths are never overwritten.
    #[arg(long)]
    output: PathBuf,
    /// Generation heuristic: auto. Unknown shapes require an explicit pair.
    #[arg(long, default_value = "auto")]
    format: String,
}

impl KeyCommand {
    pub(crate) fn run(&self) -> io::Result<u8> {
        let Self::GenerateFake(args) = self;
        let real_path = std::path::absolute(&args.real_token_file)?;
        let output = std::path::absolute(&args.output)?;
        let real = read_token_file(&real_path, rustix::process::geteuid().as_raw())
            .map_err(io::Error::other)?;
        // Generation and persistence share the store's whole-string pair validation.
        let fake = generate_fake(real.expose(), &args.format).map_err(io::Error::other)?;
        write_fake_file(&output, &fake).map_err(io::Error::other)?;
        Ok(0)
    }
}
