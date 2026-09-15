use std::io;
use std::process::ExitCode;

use asc_cli::{
    Cli, InputError, Plan,
    capabilities::process_environment,
    output::{render_binding_mutation, render_policy, render_scan_code, render_skill_guard},
};

fn main() -> ExitCode {
    let cli = match Cli::parse_from(std::env::args_os()) {
        Ok(cli) => cli,
        Err(error) => {
            let code = if error.use_stderr() { 2 } else { 0 };
            return if error.print().is_ok() {
                ExitCode::from(code)
            } else {
                ExitCode::FAILURE
            };
        }
    };
    match run(&cli) {
        Ok(code) => ExitCode::from(code),
        Err(RunError::Input(InputError::AnalyzeInput { code, message })) => {
            let result = serde_json::json!({"schema_version":"1","engine_version":env!("CARGO_PKG_VERSION"),"status":"error","coverage_complete":false,"scanners":[],"errors":[{"code":code,"message":message}]});
            println!("{result}");
            ExitCode::from(2)
        }
        Err(error @ RunError::Input(InputError::EmptyCode)) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
        Err(error) => {
            eprintln!("agent-sec-cli: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: &Cli) -> Result<u8, RunError> {
    let socket = match cli.plan() {
        // The capability view describes the environment this process inherited,
        // so it must resolve it here rather than through a daemon.
        Plan::Local(command) => {
            return command
                .render(
                    &process_environment(),
                    &mut io::stdout().lock(),
                    &mut io::stderr().lock(),
                )
                .map_err(RunError::Output);
        }
        Plan::Daemon { socket } => socket,
    };
    let request = cli.request().map_err(RunError::Input)?;
    let response =
        asc_daemon_client::call(socket, &request, cli.timeout()).map_err(RunError::Client)?;
    if cli.is_skill_guard() {
        let code = render_skill_guard(
            &response,
            &mut io::stdout().lock(),
            &mut io::stderr().lock(),
        )
        .map_err(RunError::Output)?;
        if code == 0 {
            cli.after_success(&request)?;
        }
        Ok(code)
    } else if cli.is_scan_code() {
        render_scan_code(
            &response,
            &mut io::stdout().lock(),
            &mut io::stderr().lock(),
        )
        .map_err(RunError::Output)
    } else if matches!(
        request.method.as_str(),
        asc_daemon_protocol::method::POLICY_BINDINGS_CREATE
            | asc_daemon_protocol::method::POLICY_BINDINGS_UPDATE
            | asc_daemon_protocol::method::POLICY_BINDINGS_DELETE
    ) {
        render_binding_mutation(
            &response,
            &mut io::stdout().lock(),
            &mut io::stderr().lock(),
        )
        .map_err(RunError::Output)
    } else {
        render_policy(
            &response,
            &mut io::stdout().lock(),
            &mut io::stderr().lock(),
        )
        .map_err(RunError::Output)
    }
}

#[derive(Debug, thiserror::Error)]
enum RunError {
    #[error(transparent)]
    Input(#[from] InputError),
    #[error(transparent)]
    Client(#[from] asc_daemon_client::ClientError),
    #[error(transparent)]
    Output(#[from] io::Error),
}
