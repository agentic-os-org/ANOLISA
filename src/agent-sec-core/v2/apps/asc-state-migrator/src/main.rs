//! Binary entry of `asc-state-migrator`.
//!
//! Exit codes: 0 success, 1 runtime failure, 2 usage (clap).

use std::io::Write;
use std::process::ExitCode;

use asc_state_migrator::cli::Cli;
use clap::Parser;

fn main() -> ExitCode {
    let cli = match Cli::try_parse_from(std::env::args_os()) {
        Ok(cli) => cli,
        Err(error) => {
            let code = if error.use_stderr() { 2 } else { 0 };
            let _ = error.print();
            return ExitCode::from(code);
        }
    };

    match asc_state_migrator::run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = writeln!(std::io::stderr(), "error: {error}");
            ExitCode::from(1)
        }
    }
}
