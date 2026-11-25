#[macro_use]
mod output;

mod color;
mod commands;
mod context;
mod packaged;
mod progress;
mod repo_config;
mod resolution;
mod response;
#[cfg(test)]
mod test_support;

use std::io;
use std::process::ExitCode;

use clap::FromArgMatches as _;
use clap::error::ErrorKind;

use crate::commands::Cli;
use crate::context::CliContext;

fn main() -> ExitCode {
    let matches = match commands::build_cli().try_get_matches() {
        Ok(matches) => matches,
        Err(error) => return handle_clap_error(error),
    };
    let cli = match Cli::from_arg_matches(&matches) {
        Ok(cli) => cli,
        Err(error) => return handle_clap_error(error),
    };
    let ctx = CliContext::from_cli(&cli);
    let result = commands::dispatch(cli, &ctx);
    let exit_code = match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => response::render_error(&ctx, &err),
    };
    finish(exit_code)
}

fn handle_clap_error(error: clap::Error) -> ExitCode {
    let exit_code = match u8::try_from(error.exit_code()) {
        Ok(code) => ExitCode::from(code),
        Err(_) => ExitCode::FAILURE,
    };
    match error.kind() {
        ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => {
            output::write_stdout(format_args!("{error}"), false);
            finish(ExitCode::SUCCESS)
        }
        _ => {
            let _ = error.print();
            exit_code
        }
    }
}

fn finish(exit_code: ExitCode) -> ExitCode {
    finish_with_error(exit_code, output::finish_stdout())
}

fn finish_with_error(exit_code: ExitCode, error: Option<io::Error>) -> ExitCode {
    match error {
        Some(error) => {
            eprintln!("error: failed writing to stdout: {error}");
            ExitCode::from(1)
        }
        None => exit_code,
    }
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::process::ExitCode;

    #[test]
    fn stdout_success_preserves_business_exit_code() {
        // Given a business command with a non-zero result and healthy stdout.
        let business_exit = ExitCode::from(2);

        // When the process resolves stdout precedence.
        let resolved = super::finish_with_error(business_exit, None);

        // Then the business result remains authoritative.
        assert_eq!(ExitCode::from(2), resolved);
    }

    #[test]
    fn stdout_failure_overrides_business_exit_code() {
        // Given a business command result and an actionable stdout failure.
        let business_exit = ExitCode::from(2);
        let error = io::Error::new(io::ErrorKind::PermissionDenied, "injected stdout failure");

        // When the process resolves stdout precedence.
        let resolved = super::finish_with_error(business_exit, Some(error));

        // Then the output failure maps to the execution-failed exit code.
        assert_eq!(ExitCode::from(1), resolved);
    }
}
