//! One-request AW Provider process; sec-core is reached only through its CLI.

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
    const USAGE: &str = "usage: aw-provider-sec-core --cli ABSOLUTE_PATH --socket ABSOLUTE_PATH";
    if args.len() == 1 && args[0] == "--help" {
        println!("{USAGE}");
        return Ok(());
    }
    if args.len() != 4 || args[0] != "--cli" || args[2] != "--socket" {
        return Err(USAGE.into());
    }
    let cli = PathBuf::from(&args[1]);
    let socket = PathBuf::from(&args[3]);
    if !cli.is_absolute() || !socket.is_absolute() {
        return Err("--cli and --socket must be absolute paths".into());
    }
    aw_provider_sec_core::run(
        &mut std::io::stdin().lock(),
        &mut std::io::stdout().lock(),
        &cli,
        &socket,
        // Cap the execution budget, not blocking stdio. The caller must bound
        // the entire process lifetime, including input before EOF and output.
        Duration::from_secs(60),
    )?;
    Ok(())
}
