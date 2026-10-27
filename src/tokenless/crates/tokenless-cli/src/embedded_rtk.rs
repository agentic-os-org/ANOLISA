//! Adapt the RTK library to Tokenless command names and process lifecycle.

use std::ffi::OsString;
use std::io::{self, Write};

use clap::Parser;

#[derive(Parser)]
#[command(
    name = "tokenless rtk rewrite",
    about = "Rewrite a shell command using the embedded RTK engine"
)]
struct RewriteArguments {
    /// Raw command, supplied as one string or multiple arguments.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,
}

pub(crate) fn run(args: &[OsString]) -> Result<i32, String> {
    #[cfg(unix)]
    // SAFETY: this dedicated CLI branch owns its process-wide SIGPIPE policy.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    if matches!(args, [arg] if arg == "--version" || arg == "-V") {
        println!("rtk {}", rtk::VERSION);
        return Ok(0);
    }
    if let [command, rest @ ..] = args
        && command == "rewrite"
    {
        let arguments = RewriteArguments::try_parse_from(
            std::iter::once(OsString::from("tokenless rtk rewrite")).chain(rest.iter().cloned()),
        );
        let command = match arguments {
            Ok(arguments) => arguments.args.join(" "),
            Err(error) => return print_arguments(error),
        };
        let (rewritten, code) = match rtk::rewrite(&command) {
            rtk::RewriteOutcome::Allow(value) => (value, 0),
            rtk::RewriteOutcome::Ask(value) => (value, 3),
            rtk::RewriteOutcome::Passthrough => return Ok(1),
            rtk::RewriteOutcome::Deny => return Ok(2),
        };
        let rendered = tokenless_runtime::render_rtk_command(&command, &rewritten);
        let mut stdout = io::stdout().lock();
        stdout
            .write_all(rendered.as_bytes())
            .and_then(|_| stdout.flush())
            .map_err(|error| format!("cannot write rewritten command: {error}"))?;
        return Ok(code);
    }
    match rtk::execute(args) {
        Ok(code) => Ok(code),
        Err(rtk::RtkError::Arguments(error)) => print_arguments(error),
        Err(error) => Err(error.to_string()),
    }
}

fn print_arguments(error: clap::Error) -> Result<i32, String> {
    let code = error.exit_code();
    error
        .print()
        .map_err(|error| format!("cannot write argument diagnostic: {error}"))?;
    Ok(code)
}
