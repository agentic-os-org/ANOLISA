mod runtime_path;

use runtime_path::RuntimeLease;

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

mod sinks;

use asc_action_runtime::Finalizer;
use asc_daemon::{Cli, ParseOutcome, ProcessSignals, run_with_shutdown_timeout, serve};
use asc_daemon_core::{PrincipalPolicy, RootManagedPrincipalPolicy};
use asc_daemon_handler::{DaemonDispatcher, JsonRejectionEncoder};
use asc_daemon_service::ShutdownToken;
use asc_event_sink::ConfiguredSecurityEventSinks;
use asc_pap::PapService;
use asc_pap_repository_memory::ProcessLocalPapRepository;
use asc_policy_engine::PolicyTemplateCompiler;
use asc_policy_runtime::reconciliation::ReconciliationRuntime;
use asc_security_events::config::daemon_security_event_paths;

use crate::sinks::EventSinkAdapter;

const RUNTIME_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(1);

fn main() -> ExitCode {
    install_panic_hook();
    rustix::process::umask(rustix::fs::Mode::from_raw_mode(0o077));
    let outcome = match Cli::parse_from(std::env::args_os()) {
        Ok(outcome) => outcome,
        Err(problem) => {
            eprintln!("agent-sec-daemon: {problem}");
            return ExitCode::from(2);
        }
    };
    let ParseOutcome::Serve(cli) = outcome else {
        let ParseOutcome::Help(help) = outcome else {
            unreachable!("all parse outcomes are covered")
        };
        print!("{help}");
        return ExitCode::SUCCESS;
    };

    let lease = match RuntimeLease::acquire(&cli.bootstrap.socket_path) {
        Ok(lease) => lease,
        Err(problem) => {
            report_error(&problem);
            return ExitCode::FAILURE;
        }
    };
    // Retain the singleton through the outer Tokio blocking-task shutdown window.
    match run_with_shutdown_timeout(run(cli, &lease), RUNTIME_SHUTDOWN_TIMEOUT) {
        Ok((exit_code, event_sinks)) => {
            if let Some(sinks) = event_sinks {
                sinks.close();
            }
            exit_code
        }
        Err(problem) => {
            report_error(&problem);
            ExitCode::FAILURE
        }
    }
}

async fn run(
    cli: Cli,
    lease: &RuntimeLease,
) -> (ExitCode, Option<Arc<ConfiguredSecurityEventSinks>>) {
    if let Err(problem) = lease.prepare_socket().await {
        report_error(&problem);
        return (ExitCode::FAILURE, None);
    }
    let signals = match ProcessSignals::install() {
        Ok(signals) => signals,
        Err(problem) => {
            eprintln!("agent-sec-daemon: {problem}");
            return (ExitCode::FAILURE, None);
        }
    };
    let repository = Arc::new(ProcessLocalPapRepository::default());
    let (finalizer, event_sinks) = match event_finalizer() {
        Ok(sinks) => sinks,
        Err(error) => {
            eprintln!("agent-sec-daemon: security event storage unavailable: {error}");
            return (ExitCode::FAILURE, None);
        }
    };
    let policy_runtime = match asc_daemon::start_policy_reconciliation(repository.clone()) {
        Ok(runtime) => Some(runtime),
        Err(error) => {
            eprintln!("asc-daemon: reconciliation unavailable; Binding mutations disabled");
            report_error(&error);
            None
        }
    };
    let enqueuer: Arc<dyn asc_pap::BindingReconcileEnqueuer> = policy_runtime.as_ref().map_or_else(
        || {
            Arc::new(asc_daemon::UnavailableReconciliation)
                as Arc<dyn asc_pap::BindingReconcileEnqueuer>
        },
        |runtime| runtime.enqueuer(),
    );
    let pap = PapService::new(repository, Arc::new(PolicyTemplateCompiler))
        .with_reconcile_enqueuer(enqueuer);
    let principal_policy = Arc::new(RootManagedPrincipalPolicy::with_admin_uids(
        cli.policy_admin_uids,
    ));
    let policy_for_handler: Arc<dyn PrincipalPolicy> = principal_policy.clone();
    let dispatcher = Arc::new(DaemonDispatcher::new(
        pap,
        policy_for_handler,
        asc_daemon::scan_application(finalizer),
    ));
    eprintln!("agent-sec-daemon: warning: PAP state is process-local and is lost on restart");

    let shutdown = ShutdownToken::new();
    let health_task = policy_runtime
        .as_ref()
        .map(|runtime| watch_policy_health(runtime.enqueuer()));
    let signal_task = tokio::spawn(signals.request_shutdown(shutdown.clone()));
    let result = serve(
        cli.bootstrap,
        dispatcher,
        Arc::new(JsonRejectionEncoder),
        shutdown,
    )
    .await;
    signal_task.abort();
    if let Some(health_task) = health_task {
        health_task.abort();
    }
    // The UDS service has stopped admission and drained requests. Retain the
    // blocking join task even on timeout; only process exit may cut off calls.
    let drain = tokio::task::spawn_blocking(move || {
        policy_runtime.map_or(Ok(()), ReconciliationRuntime::shutdown)
    });
    let exit_code = if matches!(
        tokio::time::timeout(Duration::from_secs(30), drain).await,
        Ok(Ok(Ok(())))
    ) {
        match result {
            Ok(_) => ExitCode::SUCCESS,
            Err(problem) => {
                report_error(&problem);
                ExitCode::FAILURE
            }
        }
    } else {
        eprintln!("asc-daemon: reconciliation drain failed or timed out");
        ExitCode::FAILURE
    };
    (exit_code, Some(event_sinks))
}

fn event_finalizer()
-> Result<(Finalizer, Arc<ConfiguredSecurityEventSinks>), asc_event_sink::SinkError> {
    let (jsonl_path, sqlite_path) = daemon_security_event_paths()?;
    let sinks = Arc::new(ConfiguredSecurityEventSinks::new(jsonl_path, sqlite_path));
    sinks.warm_sqlite()?;
    if let Err(error) = sinks.warm_jsonl() {
        eprintln!("agent-sec-daemon: warning: JSONL security event log unavailable: {error}");
    }
    Ok((
        Finalizer::new(
            Arc::new(EventSinkAdapter::new(Arc::clone(&sinks))),
            Arc::new(sinks::TelemetryAdapter(
                asc_event_sink::telemetry::TelemetryWriter::new(
                    asc_telemetry::config::TelemetryConfig::from_process(),
                ),
            )),
            Arc::new(sinks::LifecycleDiagnostics),
        ),
        sinks,
    ))
}

fn install_panic_hook() {
    // Unwind conversion cannot suppress the default hook's raw payload output.
    std::panic::set_hook(Box::new(|info| {
        use std::io::Write;
        let mut stderr = std::io::stderr().lock();
        if let Some(location) = info.location() {
            let _ = writeln!(stderr, "agent-sec-daemon: internal panic at {location}");
        } else {
            let _ = writeln!(stderr, "agent-sec-daemon: internal panic");
        }
    }));
}

fn report_error(problem: &dyn std::error::Error) {
    eprintln!("agent-sec-daemon: {problem}");
    let mut source = problem.source();
    while let Some(cause) = source {
        eprintln!("  caused by: {cause}");
        source = cause.source();
    }
}

fn watch_policy_health(
    queue: Arc<asc_policy_runtime::reconciliation::WorkQueue>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut healthy = true;
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let current = queue.is_healthy();
            if current != healthy {
                eprintln!(
                    "asc-daemon: reconciliation health {}",
                    if current { "running" } else { "degraded" }
                );
                healthy = current;
            }
            if queue.has_failed() {
                break;
            }
        }
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn panic_hook_reports_location_without_payload() {
        const CHILD_ENV: &str = "ASC_PANIC_HOOK_TEST_CHILD";
        const SECRET: &str = "SECRET_PANIC_PAYLOAD";
        if std::env::var_os(CHILD_ENV).is_some() {
            super::install_panic_hook();
            assert!(std::panic::catch_unwind(|| panic!("{SECRET}")).is_err());
            return;
        }

        // Isolate the process-global hook from the other tests and capture real stderr.
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tests::panic_hook_reports_location_without_payload",
                "--nocapture",
            ])
            .env(CHILD_ENV, "1")
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let stderr = String::from_utf8(output.stderr).unwrap();
        let location = stderr
            .trim()
            .strip_prefix("agent-sec-daemon: internal panic at ")
            .expect("panic source location");
        let mut coordinates = location.rsplitn(3, ':');
        assert!(coordinates.next().unwrap().parse::<u32>().unwrap() > 0);
        assert!(coordinates.next().unwrap().parse::<u32>().unwrap() > 0);
        assert_eq!(coordinates.next().unwrap(), file!());
        assert!(!stderr.contains(SECRET));
        assert!(!String::from_utf8(output.stdout).unwrap().contains(SECRET));
    }
}
