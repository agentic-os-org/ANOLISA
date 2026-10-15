//! One-request tool lifecycle bridge; Tokenless owns optimization policy and state.

use std::{path::PathBuf, process::ExitCode, time::Duration};

fn main() -> ExitCode {
    match execute() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn execute() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    const USAGE: &str = "usage: aw-provider-tokenless --cli ABSOLUTE_PATH --data-dir ABSOLUTE_DIR";
    if args.len() == 1 && args[0] == "--help" {
        println!("{USAGE}");
        return Ok(());
    }
    if args.len() != 4 || args[0] != "--cli" || args[2] != "--data-dir" {
        return Err(USAGE.into());
    }
    let cli = PathBuf::from(&args[1]);
    let data = PathBuf::from(&args[3]);
    if !cli.is_absolute() || !data.is_absolute() {
        return Err("--cli and --data-dir must be absolute paths".into());
    }
    aw_provider_tokenless::run(
        &mut std::io::stdin().lock(),
        &mut std::io::stdout().lock(),
        &cli,
        &data,
        Duration::from_secs(60),
    )?;
    Ok(())
}
