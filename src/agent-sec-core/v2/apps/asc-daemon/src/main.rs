mod runtime_path;
mod skill_sec;

use runtime_path::RuntimeLease;

use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

mod sinks;

use asc_action_runtime::Finalizer;
use asc_capability_pii_scan::{CustomRuleStatus, PiiRuleSet};
use asc_daemon::retention::{RetentionPass, RetentionReport, RetentionSchedule, RetentionTask};
use asc_daemon::{Cli, ParseOutcome, ProcessSignals, run_with_shutdown_timeout, serve};
use asc_daemon_core::{PrincipalPolicy, RootManagedPrincipalPolicy};
use asc_daemon_handler::{DaemonDispatcher, JsonRejectionEncoder, QueryHandler};
use asc_daemon_service::ShutdownToken;
use asc_event_sink::ConfiguredSecurityEventSinks;
use asc_pap::{PapRepository, PapService, ScopeDiscovery};
use asc_policy_repository_sqlite::{SqlitePolicyRepository, select_policy_database_path};
use asc_policy_runtime::reconciliation::ReconciliationRuntime;
use asc_policy_types::scope::ScopeStatus;
use asc_security_events::config::daemon_security_event_paths;

use crate::sinks::EventSinkAdapter;

const RUNTIME_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(1);

/// One shared stop deadline for the retention terminal join and the worker
/// drains.
///
/// The unit's `TimeoutStopSec=75` must cover the UDS request drain (2s),
/// this budget, the Tokio runtime shutdown (1s), the security final pass,
/// and the telemetry shutdown (2s), so the retention join and the worker
/// drains wait concurrently under one deadline instead of stacking two full
/// budgets back to back (see `DAEMON_PROCESS_DEPLOYMENT_CONTRACT_zh.md`).
const RUNTIME_DRAIN_BUDGET: Duration = Duration::from_secs(65);

/// The wall-clock budget the security final pass may still claim when the
/// drains are done.
///
/// `2s + 65s + 1s + 5s + 2s = 75s`: the final pass gets whatever the drains
/// left of this window, and is skipped entirely when that remainder cannot
/// fit it (see `SqliteSink::close_with_deadline` and
/// `DAEMON_PROCESS_DEPLOYMENT_CONTRACT_zh.md`).
const FINAL_PASS_BUDGET: Duration = Duration::from_secs(5);

/// The telemetry runtime's own shutdown grace, reserved at the end of the
/// unit stop budget.
const TELEMETRY_SHUTDOWN: Duration = Duration::from_secs(2);
const FALLBACK_POLICY_DATABASE: &str = "/var/lib/agent-sec/policy/policy-state.db";

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
            telemetry.shutdown(TELEMETRY_SHUTDOWN);
            return ExitCode::FAILURE;
        }
    };
    let paths = asc_security_events::config::DaemonDataDirEnv::from_process();
    let data_dir = paths.override_dir.as_ref().unwrap_or(&paths.system_dir);
    let fallback = paths
        .override_dir
        .is_none()
        .then(|| Path::new(FALLBACK_POLICY_DATABASE));
    let repository = select_policy_database_path(&data_dir.join("policy-state.db"), fallback)
        .map_err(std::io::Error::other)
        .and_then(|path| {
            tracing::info!(target: "asc_process_diagnostic", path = %path.display(), "Policy database selected");
            SqlitePolicyRepository::open(&path).map_err(|error| std::io::Error::other(format!(
                "cannot open Policy database '{}': {error}; refusing to switch databases; existing Policy state must be preserved",
                path.display()
            )))
        });
    let repository = match repository {
        Ok(repository) => Arc::new(repository),
        Err(problem) => {
            report_error(&telemetry, &problem);
            telemetry.shutdown(TELEMETRY_SHUTDOWN);
            return ExitCode::FAILURE;
        }
    };
    // Keep database ownership even if the outer Tokio drain times out.
    let _database_lease = repository.lease();
    // Retain the singleton through the outer Tokio blocking-task shutdown window.
    let outcome = match run_with_shutdown_timeout(
        run(*cli, &lease, &telemetry, repository),
        RUNTIME_SHUTDOWN_TIMEOUT,
    ) {
        Ok((exit_code, shutdown_plan)) => {
            if let Some(plan) = shutdown_plan {
                plan.close();
            }
            exit_code
        }
        Err(problem) => {
            report_error(&telemetry, &problem);
            ExitCode::FAILURE
        }
    };
    telemetry.shutdown(TELEMETRY_SHUTDOWN);
    outcome
}

// Keep startup admission and shutdown ordering visible together in the composition root.
#[allow(clippy::too_many_lines)]
async fn run(
    cli: Cli,
    lease: &RuntimeLease,
    telemetry: &asc_observability::TelemetryRuntime,
    repository: Arc<SqlitePolicyRepository>,
) -> (ExitCode, Option<sinks::ShutdownPlan>) {
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
    let pii_rules = match load_pii_rules(cli.pii_rules.as_deref(), telemetry) {
        Ok(rules) => Arc::new(rules),
        Err(error) => {
            telemetry.report(&format!("agent-sec-daemon: {error}"));
            return (ExitCode::FAILURE, None);
        }
    };
    let (finalizer, durable_sinks, queries) = match event_finalizer(telemetry) {
        Ok(sinks) => sinks,
        Err(error) => {
            telemetry.report(&format!(
                "agent-sec-daemon: security event storage unavailable: {error}"
            ));
            return (ExitCode::FAILURE, None);
        }
    };
    let skill_worker = Arc::new(asc_daemon::SkillWorker::default());
    let skillfs = match prepare_skillfs(skillfs_config, &skill_worker) {
        Ok(bridge) => bridge,
        Err(error) => {
            report_error(telemetry, &error);
            return (
                ExitCode::FAILURE,
                Some(sinks::ShutdownPlan::full(durable_sinks)),
            );
        }
    };
    let mut executor = asc_capability_skill_sec::executor::SkillSecExecutor::new(skill_sec.clone());
    if let Some(bridge) = &skillfs {
        executor = executor.with_environment(bridge.environment());
    }
    let actions = asc_daemon::skill_application(finalizer, pii_rules, executor.clone());
    if let Err(error) = start_skills(&actions, &skill_worker, executor) {
        report_error(telemetry, &error);
        return (
            ExitCode::FAILURE,
            Some(sinks::ShutdownPlan::full(durable_sinks)),
        );
    }
    // Exclusive DB ownership proves no pre-crash discovery worker can still write.
    if let Err(error) = recover_deleting_scopes(repository.as_ref()) {
        report_error(telemetry, &error);
        let _ = tokio::task::spawn_blocking(move || skill_worker.shutdown()).await;
        return (
            ExitCode::FAILURE,
            Some(sinks::ShutdownPlan::full(durable_sinks)),
        );
    }
    let policy_runtime = match asc_daemon::start_policy_reconciliation(repository.clone()) {
        Ok(runtime) => runtime,
        Err(error) => {
            report_error(telemetry, &error);
            let _ = tokio::task::spawn_blocking(move || skill_worker.shutdown()).await;
            return (
                ExitCode::FAILURE,
                Some(sinks::ShutdownPlan::full(durable_sinks)),
            );
        }
    };
    let pap =
        PapService::new(repository.clone()).with_reconcile_enqueuer(policy_runtime.enqueuer());
    // The sink clone has no registry, avoiding a worker/service ownership cycle.
    let discovery_registry = Arc::new(asc_daemon::ScopeDiscoveryRegistry::new(Arc::new(
        pap.clone(),
    )));
    if let Err(error) = recover_active_scopes(repository.as_ref(), discovery_registry.as_ref()) {
        report_error(telemetry, &error);
        let stop = tokio::time::Instant::now() + RUNTIME_DRAIN_BUDGET;
        drain_runtimes(skill_worker, discovery_registry, policy_runtime, stop).await;
        return (
            ExitCode::FAILURE,
            Some(sinks::ShutdownPlan::full(durable_sinks)),
        );
    }
    let pap = pap.with_scope_discovery(discovery_registry.clone());
    let principal_policy = Arc::new(RootManagedPrincipalPolicy::with_admin_uids(
        cli.policy_admin_uids,
    ));
    let policy_for_handler: Arc<dyn PrincipalPolicy> = principal_policy.clone();
    let dispatcher = DaemonDispatcher::new(pap, policy_for_handler, actions)
        .with_observability(asc_daemon_core::ObservabilityService::new(Arc::new(
            sinks::ObservabilitySinkAdapter(Arc::clone(&durable_sinks.observability)),
        )))
        .with_queries(queries);
    let dispatcher = Arc::new(asc_daemon::skillfs::SkillFsDispatcher::new(
        dispatcher,
        skillfs.clone(),
    ));

    let shutdown = ShutdownToken::new();
    let health_task = watch_policy_health(policy_runtime.enqueuer(), telemetry.reporter());
    // The signal task is spawned before the retention catch-up so a stop
    // signal arriving during the (unbounded) startup pass is observed at
    // once: the catch-up then abandons admission and the daemon goes
    // straight to the shutdown flow instead of making systemd wait out
    // `TimeoutStopSec` and SIGKILL the process mid-catch-up.
    let signal_task = tokio::spawn(signals.request_shutdown(shutdown.clone()));
    // The retention lifecycle: the startup catch-up pass runs to completion
    // before `serve` admits requests (a stop signal abandons admission
    // instead), and the periodic task owns the rest. This is the
    // concurrency boundary the store mutex demands: the first pass may
    // prune a large historical backlog, and request audit writes share the
    // same connection mutex (see `retention` module docs and
    // `DAEMON_JOB_CONTRACT_zh.md` section 11.5).
    let (retention_task, admitted) = start_retention(
        &durable_sinks.security,
        &durable_sinks.observability,
        telemetry,
        shutdown.clone(),
    )
    .await;
    let result = if admitted {
        Some(
            serve(
                cli.bootstrap,
                dispatcher,
                Arc::new(JsonRejectionEncoder),
                shutdown,
            )
            .await,
        )
    } else {
        telemetry.report(
            "agent-sec-daemon: shutdown requested during the startup retention catch-up; skipping admission",
        );
        None
    };
    signal_task.abort();
    health_task.abort();
    // Cancellation plus terminal join, concurrent with the worker drains
    // under one shared stop deadline: serializing two full budgets back to
    // back would exceed the unit's `TimeoutStopSec=75` and systemd would
    // SIGKILL the process mid-drain. The bounded final pass in
    // `ShutdownPlan::close()` must run only after the retention task - and
    // any in-flight `spawn_blocking` pass it owns - has terminated, so the
    // two can never contend for the same `SqliteStore` mutex. A bare
    // `abort()` cannot guarantee that: the blocking closure keeps running
    // past the abort.
    let stop = tokio::time::Instant::now() + RUNTIME_DRAIN_BUDGET;
    let (retention_joined, runtimes_drained) = tokio::join!(
        retention_task.shutdown_by(stop),
        drain_runtimes(skill_worker, discovery_registry, policy_runtime, stop),
    );
    // A retention join timeout degrades both final passes: the retention
    // task's blocking attempt covers both stores, so an orphaned pass may
    // still hold either store's mutex when the join times out.
    let mut exit_code = if runtimes_drained {
        match result {
            // A stop that won the startup race never admitted a request, so
            // both it and a served run exit cleanly and systemd can finish
            // the restart chain; only a served error fails the unit.
            Some(Ok(_)) | None => ExitCode::SUCCESS,
            Some(Err(problem)) => {
                report_error(telemetry, &problem);
                ExitCode::FAILURE
            }
        }
    } else {
        telemetry.report("asc-daemon: background worker drain failed or timed out");
        ExitCode::FAILURE
    };
    if !retention_joined {
        telemetry.report(
            "asc-daemon: retention task did not terminate within the shared drain budget; skipping the final retention passes",
        );
        exit_code = ExitCode::FAILURE;
    }
    // The final retention pass must finish before the telemetry shutdown
    // consumes the tail of the unit stop budget, so it gets an explicit
    // deadline and is skipped when the remainder cannot fit it.
    let final_pass_deadline = stop + RUNTIME_SHUTDOWN_TIMEOUT + FINAL_PASS_BUDGET;
    (
        exit_code,
        Some(sinks::ShutdownPlan::new(
            durable_sinks,
            retention_joined,
            Some(final_pass_deadline.into_std()),
        )),
    )
}

/// Builds the production retention pass and starts its lifecycle, with the
/// startup catch-up racing the shutdown signal.
///
/// Returns the task handle plus whether `serve` may admit requests; see
/// [`asc_daemon::retention::start_with_shutdown`].
async fn start_retention(
    security: &Arc<ConfiguredSecurityEventSinks>,
    observability: &Arc<asc_event_sink::ConfiguredObservabilitySinks>,
    telemetry: &asc_observability::TelemetryRuntime,
    shutdown: ShutdownToken,
) -> (RetentionTask, bool) {
    let pass: RetentionPass = {
        let security = Arc::clone(security);
        let observability = Arc::clone(observability);
        Arc::new(move |now| {
            // Both gated passes run under one task: each store keeps its own
            // marker and lock, and the combined outcome schedules the failure
            // retry when either store needs it.
            let security_outcome = security.run_sqlite_retention(now);
            let observability_outcome = observability.run_sqlite_retention(now);
            asc_daemon::retention::combine_outcomes(security_outcome, observability_outcome)
        })
    };
    let report: RetentionReport = {
        let reporter = telemetry.reporter();
        Arc::new(move |message| reporter(message))
    };
    asc_daemon::retention::start_with_shutdown(pass, RetentionSchedule::default(), report, shutdown)
        .await
}

#[tracing::instrument(
    skip_all,
    target = "asc_daemon",
    name = "policy.recovery.deleting_scopes"
)]
fn recover_deleting_scopes(repository: &dyn PapRepository) -> Result<(), asc_pap::PapError> {
    let mut cursor = None;
    loop {
        let scopes = repository.scan_scopes(cursor.as_ref(), 128)?;
        if scopes.is_empty() {
            return Ok(());
        }
        for scope in scopes {
            cursor = Some(scope.scope_id.clone());
            if scope.status == ScopeStatus::Deleting {
                repository.finish_scope_discovery(&scope.scope_id)?;
                tracing::debug!(
                    target: "asc_observability::diagnostic", component = "policy_recovery",
                    scope_id = %scope.scope_id, "deleting scope recovery barrier committed"
                );
            }
        }
    }
}

#[tracing::instrument(
    skip_all,
    target = "asc_daemon",
    name = "policy.recovery.active_scopes"
)]
fn recover_active_scopes(
    repository: &dyn PapRepository,
    discovery: &dyn ScopeDiscovery,
) -> Result<(), asc_pap::PapError> {
    let mut cursor = None;
    loop {
        let scopes = repository.scan_scopes(cursor.as_ref(), 128)?;
        if scopes.is_empty() {
            return Ok(());
        }
        for scope in scopes {
            cursor = Some(scope.scope_id.clone());
            if scope.status == ScopeStatus::Active {
                discovery.start(&repository.scope_discovery_seed(&scope.scope_id)?)?;
                tracing::debug!(
                    target: "asc_observability::diagnostic", component = "policy_recovery",
                    scope_id = %scope.scope_id, "active scope discovery restored"
                );
            }
        }
    }
}

fn load_pii_rules(
    path: Option<&Path>,
    telemetry: &asc_observability::TelemetryRuntime,
) -> Result<PiiRuleSet, asc_capability_pii_scan::ScanError> {
    let rules = PiiRuleSet::load(path)?;
    if rules.custom_rules().status == CustomRuleStatus::Invalid {
        telemetry.report(&format!(
            "agent-sec-daemon: custom PII rules disabled ({})",
            rules
                .custom_rules()
                .error_code
                .as_deref()
                .unwrap_or("invalid_configuration")
        ));
    }
    Ok(rules)
}

fn prepare_skillfs(
    config: Option<asc_daemon::skillfs::SkillFsConfig>,
    worker: &asc_daemon::SkillWorker,
) -> Result<Option<Arc<asc_daemon::skillfs::SkillFsBridge>>, asc_daemon::skillfs::SkillFsError> {
    config
        .map(|config| asc_daemon::skillfs::SkillFsBridge::prepare(config, worker))
        .transpose()
        .map(|bridge| bridge.map(Arc::new))
}

fn start_skills(
    actions: &Arc<asc_daemon_core::ActionService>,
    worker: &asc_daemon::SkillWorker,
    executor: asc_capability_skill_sec::executor::SkillSecExecutor,
) -> Result<(), asc_daemon::skillfs::SkillFsError> {
    worker.start(actions.clone(), move || {
        executor.discover(std::time::Instant::now() + Duration::from_secs(30))
    })
}

async fn drain_runtimes(
    worker: Arc<asc_daemon::SkillWorker>,
    discovery_registry: Arc<asc_daemon::ScopeDiscoveryRegistry>,
    policy: ReconciliationRuntime,
    stop: tokio::time::Instant,
) -> bool {
    // Both joins remain tracked by Tokio after timeout; process exit is the final cutoff.
    let skill = tokio::task::spawn_blocking(move || worker.shutdown());
    let policy =
        tokio::task::spawn_blocking(move || stop_background_jobs(&discovery_registry, policy));
    // The reconciliation join keeps its own shorter cap, still bounded by the
    // shared stop deadline.
    let policy_stop = (tokio::time::Instant::now() + Duration::from_secs(30)).min(stop);
    let (skill, policy) = tokio::join!(
        tokio::time::timeout_at(stop, skill),
        tokio::time::timeout_at(policy_stop, policy),
    );
    matches!(skill, Ok(Ok(Ok(())))) && matches!(policy, Ok(Ok(true)))
}

fn stop_background_jobs(
    discovery_registry: &asc_daemon::ScopeDiscoveryRegistry,
    policy_runtime: ReconciliationRuntime,
) -> bool {
    let discovery_ok = match discovery_registry.shutdown() {
        Ok(()) => true,
        Err(error) => {
            tracing::error!(target: "asc_process_diagnostic", %error, "scope discovery shutdown failed");
            false
        }
    };
    let policies_ok = policy_runtime.shutdown().is_ok();
    discovery_ok && policies_ok
}

fn event_finalizer(
    telemetry: &asc_observability::TelemetryRuntime,
) -> Result<(Finalizer, sinks::DurableSinks, QueryHandler), asc_event_sink::SinkError> {
    let (jsonl_path, sqlite_path) = daemon_security_event_paths()?;
    let observability_path = sqlite_path.with_file_name("observability.db");
    let observability_sinks = Arc::new(asc_event_sink::ConfiguredObservabilitySinks::new(
        jsonl_path.with_file_name("observability.jsonl"),
        observability_path.clone(),
    ));
    let mut queries = QueryHandler::default();
    if observability_sinks.warm_sqlite().is_ok() {
        queries = queries.with_observability_queries(
            asc_daemon_core::query::ObservabilityQueryService::new(
                asc_persistence_sqlite::query::SqliteObservabilityQueries::new(observability_path),
                asc_persistence_sqlite::query::SqliteSecurityQueries::new(sqlite_path.clone()),
            ),
        );
    } else {
        telemetry.report(
            "agent-sec-daemon: observability query storage unavailable; initialization failed",
        );
    }
    let sinks = Arc::new(ConfiguredSecurityEventSinks::new(
        jsonl_path,
        sqlite_path.clone(),
    ));
    sinks.warm_sqlite()?;
    if let Err(error) = sinks.prepare_query_indexes() {
        telemetry.report(&format!(
            "agent-sec-daemon: warning: security query indexes unavailable; queries may be slower: {error}"
        ));
    }
    if let Err(error) = sinks.warm_jsonl() {
        telemetry.report(&format!(
            "agent-sec-daemon: warning: JSONL security event log unavailable: {error}"
        ));
    }
    // The observability store joins the retention lifecycle, so it must be
    // open before the startup catch-up runs; a failed warm is only a warning
    // - the observability stream is best-effort and warms again on the next
    // record.
    if let Err(error) = observability_sinks.warm_sqlite() {
        telemetry.report(&format!(
            "agent-sec-daemon: warning: observability sqlite unavailable: {error}"
        ));
    }
    // The `sec.*` query family reads the same database the writers use. If
    // its read-only handle cannot open even though the writer warmed, the
    // daemon keeps serving scans and rejects queries with `unavailable`
    // rather than guessing at another database.
    match asc_persistence_sqlite::security_events::SqliteEventQuerySource::new(&sqlite_path) {
        Ok(source) => queries = queries.with_security_queries(source),
        Err(error) => telemetry.report(&format!(
            "agent-sec-daemon: warning: security event queries unavailable: {error}"
        )),
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
        sinks::DurableSinks {
            security: sinks,
            observability: observability_sinks,
        },
        queries,
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
