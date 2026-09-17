use std::io;
use std::process::ExitCode;

use asc_cli::{
    Cli, InputError, Plan,
    capabilities::process_environment,
    output::{
        render_binding_mutation, render_policy, render_prompt_warmup, render_scan_code,
        render_scan_prompt, warn_multi_turn_incomplete,
    },
};

fn main() -> ExitCode {
    let cli = match Cli::parse_from(std::env::args_os()) {
        Ok(cli) => cli,
        Err(error) => {
            let code = if error.get(clap::error::ContextKind::Custom).is_some() {
                1
            } else if error.use_stderr() {
                2
            } else {
                0
            };
            return if error.print().is_ok() {
                ExitCode::from(code)
            } else {
                ExitCode::FAILURE
            };
        }
    };
    let runtime = match asc_observability::init_runtime("agent-sec-cli") {
        Ok(runtime) => runtime,
        Err(reason) => {
            asc_observability::report_startup_error(&format!("otel: {reason}"));
            return ExitCode::FAILURE;
        }
    };
    let mut labels = cli
        .context()
        .get::<asc_observability::CompatibilityCorrelation>()
        .cloned()
        .unwrap_or_default();
    labels.invocation_label = std::env::var("AGENT_SEC_INVOCATION_ID")
        .ok()
        .as_deref()
        .and_then(asc_observability::normalize);
    let parent = cli.context().with_value(labels);
    let result = {
        let span = asc_observability::parent_span(
            tracing::info_span!(parent: None, "cli.command"),
            parent,
        );
        span.in_scope(|| {
            let _context = asc_observability::request_context().attach();
            asc_observability::report_propagation_issues();
            let result = run(&cli);
            let success = matches!(result, Ok(0));
            if success {
                asc_observability::mark_success();
            } else {
                asc_observability::mark_error("command_failed");
            }
            asc_observability::diagnostic(if success {
                "command_completed"
            } else {
                "command_failed"
            });
            result
        })
    };
    // Diagnostic draining is best effort and cannot delay exit indefinitely.
    runtime.shutdown(std::time::Duration::from_millis(50));
    match result {
        Ok(code) => ExitCode::from(code),
        // Scan commands own their usage hints, so those errors render
        // verbatim instead of behind the generic prefix.
        Err(RunError::Input(input)) if input.is_usage_hint() => {
            eprintln!("{input}");
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
                    &mut io::stderr(),
                )
                .map_err(RunError::Output);
        }
        Plan::Daemon { socket } => socket,
    };
    // Scan-prompt resolves its own request batch (one per input line or
    // conversation payload) and prints diagnostics around them, so it owns
    // its transport loop instead of sharing the single-request path.
    if cli.is_scan_prompt() {
        let run = cli.prompt_scan_run().map_err(RunError::Input)?;
        for warning in &run.warnings {
            eprintln!("{warning}");
        }
        // A warmup is one readiness probe whose fixed-text message contract
        // replaces the per-result rendering and multi-turn diagnostics of
        // scans, so it dispatches before the scan loop.
        if run.is_warmup
            && let Some(request) = run.requests.first()
        {
            let response = asc_daemon_client::call(socket, request, cli.timeout())
                .map_err(RunError::Client)?;
            return render_prompt_warmup(
                &response,
                &run.mode,
                &mut io::stdout().lock(),
                &mut io::stderr().lock(),
            )
            .map_err(RunError::Output);
        }
        // An empty batch means the caller passed a blank --text: nothing to
        // scan, success, matching the V1 silent exit.
        let mut exit_code = 0;
        for request in &run.requests {
            let response = asc_daemon_client::call(socket, request, cli.timeout())
                .map_err(RunError::Client)?;
            let code = render_scan_prompt(
                &response,
                run.format,
                &mut io::stdout().lock(),
                &mut io::stderr().lock(),
            )
            .map_err(RunError::Output)?;
            if run.is_multi_turn {
                warn_multi_turn_incomplete(&response, &mut io::stderr().lock())
                    .map_err(RunError::Output)?;
            }
            exit_code = exit_code.max(code);
        }
        return Ok(exit_code);
    }
    let request = cli.request().map_err(RunError::Input)?;
    let response =
        asc_daemon_client::call(socket, &request, cli.timeout()).map_err(RunError::Client)?;
    if cli.is_scan_code() {
        render_scan_code(&response, &mut io::stdout().lock(), &mut io::stderr())
            .map_err(RunError::Output)
    } else if matches!(
        request.method.as_str(),
        asc_daemon_protocol::method::POLICY_BINDINGS_CREATE
            | asc_daemon_protocol::method::POLICY_BINDINGS_UPDATE
            | asc_daemon_protocol::method::POLICY_BINDINGS_DELETE
    ) {
        render_binding_mutation(&response, &mut io::stdout().lock(), &mut io::stderr())
            .map_err(RunError::Output)
    } else {
        render_policy(&response, &mut io::stdout().lock(), &mut io::stderr())
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
