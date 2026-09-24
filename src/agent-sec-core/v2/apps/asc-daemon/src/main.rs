mod runtime_path;
mod skill_sec;

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

    let telemetry = match asc_observability::init_runtime("asc-daemon") {
        Ok(runtime) => runtime,
        Err(reason) => {
            asc_observability::report_startup_error(&format!("otel: {reason}"));
            return ExitCode::FAILURE;
        }
    };
    let lease = match RuntimeLease::acquire(&cli.bootstrap.socket_path) {
        Ok(lease) => lease,
        Err(problem) => {
            report_error(&telemetry, &problem);
            telemetry.shutdown(Duration::from_millis(2000));
            return ExitCode::FAILURE;
        }
    };
    // Retain the singleton through the outer Tokio blocking-task shutdown window.
    let outcome =
        match run_with_shutdown_timeout(run(cli, &lease, &telemetry), RUNTIME_SHUTDOWN_TIMEOUT) {
            Ok((exit_code, event_sinks)) => {
                if let Some(sinks) = event_sinks {
                    sinks.close();
                }
                exit_code
            }
            Err(problem) => {
                report_error(&telemetry, &problem);
                ExitCode::FAILURE
            }
        };
    telemetry.shutdown(Duration::from_millis(2000));
    outcome
}

async fn run(
    cli: Cli,
    lease: &RuntimeLease,
    telemetry: &asc_observability::TelemetryRuntime,
) -> (ExitCode, Option<Arc<ConfiguredSecurityEventSinks>>) {
    if let Err(problem) = lease.prepare_socket().await {
        report_error(telemetry, &problem);
        return (ExitCode::FAILURE, None);
    }
    let signals = match ProcessSignals::install() {
        Ok(signals) => signals,
        Err(problem) => {
            telemetry.report(&format!("agent-sec-daemon: {problem}"));
            return (ExitCode::FAILURE, None);
        }
    };
    let (skill_sec, skillfs_config) = match skill_sec::start(cli.skillsec_config.as_deref()) {
        Ok(service) => service,
        Err(error) => {
            report_error(telemetry, &error);
            return (ExitCode::FAILURE, None);
        }
    };
    let repository = Arc::new(ProcessLocalPapRepository::default());
    let (finalizer, event_sinks) = match event_finalizer(telemetry) {
        Ok(sinks) => sinks,
        Err(error) => {
            telemetry.report(&format!(
                "agent-sec-daemon: security event storage unavailable: {error}"
            ));
            return (ExitCode::FAILURE, None);
        }
    };
    let skillfs = match prepare_skillfs(skillfs_config) {
        Ok(bridge) => bridge,
        Err(error) => {
            report_error(telemetry, &error);
            return (ExitCode::FAILURE, Some(event_sinks));
        }
    };
    let mut executor = asc_capability_skill_sec::executor::SkillSecExecutor::new(skill_sec.clone());
    if let Some(bridge) = &skillfs {
        executor = executor.with_environment(bridge.environment());
    }
    let actions = asc_daemon::skill_application(finalizer, executor);
    if let Err(error) =
        recover_and_start_skillfs(&skill_sec, &actions, skillfs.as_deref(), telemetry)
    {
        report_error(telemetry, &error);
        return (ExitCode::FAILURE, Some(event_sinks));
    }
    let policy_runtime = match asc_daemon::start_policy_reconciliation(repository.clone()) {
        Ok(runtime) => Some(runtime),
        Err(error) => {
            telemetry.report("asc-daemon: reconciliation unavailable; Binding mutations disabled");
            report_error(telemetry, &error);
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
    let dispatcher = Arc::new(asc_daemon::skillfs::SkillFsDispatcher::new(
        DaemonDispatcher::new(pap, policy_for_handler, actions),
        skillfs.clone(),
    ));
    telemetry
        .report("agent-sec-daemon: warning: PAP state is process-local and is lost on restart");

    let shutdown = ShutdownToken::new();
    let health_task = policy_runtime
        .as_ref()
        .map(|runtime| watch_policy_health(runtime.enqueuer(), telemetry.reporter()));
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
    // UDS has stopped admission and completed its request drain before workers stop.
    let exit_code = if drain_runtimes(skillfs, policy_runtime).await {
        match result {
            Ok(_) => ExitCode::SUCCESS,
            Err(problem) => {
                report_error(telemetry, &problem);
                ExitCode::FAILURE
            }
        }
    } else {
        telemetry.report("asc-daemon: background worker drain failed or timed out");
        ExitCode::FAILURE
    };
    (exit_code, Some(event_sinks))
}

fn prepare_skillfs(
    config: Option<asc_daemon::skillfs::SkillFsConfig>,
) -> Result<Option<Arc<asc_daemon::skillfs::SkillFsBridge>>, asc_daemon::skillfs::SkillFsError> {
    config
        .map(asc_daemon::skillfs::SkillFsBridge::prepare)
        .transpose()
        .map(|bridge| bridge.map(Arc::new))
}

fn recover_and_start_skillfs(
    service: &Arc<asc_capability_skill_sec::SkillSecService>,
    actions: &Arc<asc_daemon_core::ActionService>,
    skillfs: Option<&asc_daemon::skillfs::SkillFsBridge>,
    telemetry: &asc_observability::TelemetryRuntime,
) -> Result<(), asc_daemon::skillfs::SkillFsError> {
    let recovery = skill_sec::recover(service, actions).and_then(|()| {
        if let Some(bridge) = skillfs {
            bridge
                .schedule_reconcile(service.managed_skills()?)
                .map_err(|error| skill_sec::StartupError::Recovery(error.to_string()))?;
        }
        Ok(())
    });
    if let Err(error) = recovery {
        report_error(telemetry, &error);
        telemetry.report(
            "agent-sec-daemon: SkillSec recovery is degraded; status and administrator retry remain available",
        );
    }
    if let Some(bridge) = skillfs {
        bridge.start_worker(actions.clone())?;
    }
    Ok(())
}

async fn drain_runtimes(
    skillfs: Option<Arc<asc_daemon::skillfs::SkillFsBridge>>,
    policy: Option<ReconciliationRuntime>,
) -> bool {
    // Both joins remain tracked by Tokio after timeout; process exit is the final cutoff.
    let skill =
        tokio::task::spawn_blocking(move || skillfs.map_or(Ok(()), |bridge| bridge.shutdown()));
    let policy =
        tokio::task::spawn_blocking(move || policy.map_or(Ok(()), ReconciliationRuntime::shutdown));
    let (skill, policy) = tokio::join!(
        tokio::time::timeout(Duration::from_secs(65), skill),
        tokio::time::timeout(Duration::from_secs(30), policy),
    );
    matches!(skill, Ok(Ok(Ok(())))) && matches!(policy, Ok(Ok(Ok(()))))
}

fn event_finalizer(
    telemetry: &asc_observability::TelemetryRuntime,
) -> Result<(Finalizer, Arc<ConfiguredSecurityEventSinks>), asc_event_sink::SinkError> {
    let (jsonl_path, sqlite_path) = daemon_security_event_paths()?;
    let sinks = Arc::new(ConfiguredSecurityEventSinks::new(jsonl_path, sqlite_path));
    sinks.warm_sqlite()?;
    if let Err(error) = sinks.warm_jsonl() {
        telemetry.report(&format!(
            "agent-sec-daemon: warning: JSONL security event log unavailable: {error}"
        ));
    }
    Ok((
        Finalizer::new(
            Arc::new(EventSinkAdapter::new(Arc::clone(&sinks))),
            Arc::new(sinks::TelemetryAdapter(
                asc_event_sink::telemetry::TelemetryWriter::new(
                    asc_telemetry::config::TelemetryConfig::from_process(),
                ),
            )),
            Arc::new(sinks::LifecycleDiagnostics(telemetry.reporter())),
        ),
        sinks,
    ))
}

fn install_panic_hook() {
    // Unwind conversion cannot suppress the default hook's raw payload output.
    std::panic::set_hook(Box::new(|info| {
        let message = info.location().map_or_else(
            || "agent-sec-daemon: internal panic".to_owned(),
            |location| format!("agent-sec-daemon: internal panic at {location}"),
        );
        asc_observability::report_startup_error(&message);
    }));
}

fn report_error(telemetry: &asc_observability::TelemetryRuntime, problem: &dyn std::error::Error) {
    telemetry.report(&format!("asc-daemon: {problem}"));
    let mut source = problem.source();
    while let Some(cause) = source {
        telemetry.report(&format!("  caused by: {cause}"));
        source = cause.source();
    }
}

fn watch_policy_health(
    queue: Arc<asc_policy_runtime::reconciliation::WorkQueue>,
    report: impl Fn(&str) + Send + 'static,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut healthy = true;
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let current = queue.is_healthy();
            if current != healthy {
                report(&format!(
                    "asc-daemon: reconciliation health {}",
                    if current { "running" } else { "degraded" }
                ));
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
