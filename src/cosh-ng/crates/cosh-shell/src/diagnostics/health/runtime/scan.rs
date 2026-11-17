use std::collections::BTreeSet;
use std::io;
use std::net::Shutdown;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use nix::libc;

use super::super::builder::HealthReportBuilder;
use super::super::model::{
    HealthCheckStatus, HealthCollector, HealthProgress, HealthScanReport, HealthSeverity,
    HealthUnavailableReason,
};
use super::super::plan::FinishPolicy;
use super::super::rules::evaluate_judgement_rules;
use super::cleanup::{confirm_group_gone, GROUP_CONFIRM_BUDGET};
use super::clock::{boottime_ns, HealthDeadline};
use super::job::{configure_cloexec, configure_isolated_worker};
use super::plan::{checks, WorkerRequest};
use super::process::{
    child_has_exited, ensure_waitable_children, CleanupScope, CommandChild, ReapOwner,
    COMMAND_POLL_INTERVAL,
};
use super::protocol::{self, CollectTask, Decoder, Payload, ProtocolError};
use super::worker::{IPC_FD, PRIVATE_WORKER_FLAG};

mod dispatch;
mod sources;
use sources::{
    build_plan, cleanup_reservation, AwaitedTask, SourcePlan, MIN_SOURCE_WORK, SPAWN_ADMIT_COST,
};

pub(crate) struct HealthScanHandle {
    state: ScanState,
    cancelled: bool,
}
enum ScanState {
    Running(Box<RunningScan>),
    Sealed(Option<Box<HealthScanReport>>),
}
struct RunningScan {
    request: WorkerRequest,
    builder: HealthReportBuilder,
    child: Option<CommandChild<'static>>,
    child_pgid: Option<u32>,
    stream: Option<UnixStream>,
    decoder: Decoder,
    nonce: String,
    sequence: u64,
    enabled: BTreeSet<String>,
    configured: bool,
    started: Instant,
    started_ns: u64,
    plan: Vec<SourcePlan>,
    next_source: usize,
    task_id: u64,
    generation: u64,
    awaiting: Option<AwaitedTask>,
    source_done: bool,
    bootstrap_deadline: HealthDeadline,
}

impl HealthScanHandle {
    pub(crate) fn ready(report: HealthScanReport) -> Self {
        Self {
            state: ScanState::Sealed(Some(Box::new(report))),
            cancelled: false,
        }
    }

    pub(super) fn start(request: WorkerRequest) -> Self {
        let mut builder = HealthReportBuilder::new(&request.scan_id, request.started_at_ms);
        builder.set_role(if request.config.enabled {
            request.config.role.clone()
        } else {
            None
        });
        let mut enabled = BTreeSet::new();
        let configured = request.config_source == super::plan::ConfigSource::CallerSnapshot;
        if configured {
            for spec in checks(&request) {
                builder.declare_check_id(spec.id, spec.collector, spec.enabled);
                if spec.enabled {
                    enabled.insert(spec.id.to_string());
                }
            }
        } else {
            builder.declare_check_id("environment.config", HealthCollector::Config, true);
        }
        let plan = if configured {
            build_plan(&enabled, request.context.passive())
        } else {
            Vec::new()
        };
        let mut bootstrap_deadline = request.deadline;
        if cfg!(target_os = "linux") {
            let _ = bootstrap_deadline.shorten_within(Duration::from_millis(
                super::super::banner_policy::BOOTSTRAP_BUDGET_MS,
            ));
        }
        let mut running = RunningScan {
            started: Instant::now(),
            started_ns: request.deadline.started_ns,
            request,
            builder,
            child: None,
            child_pgid: None,
            stream: None,
            decoder: Decoder::default(),
            nonce: uuid::Uuid::new_v4().simple().to_string(),
            sequence: 1,
            enabled,
            configured,
            plan,
            next_source: 0,
            task_id: 0,
            generation: 0,
            awaiting: None,
            source_done: false,
            bootstrap_deadline,
        };
        let launch = running.launch();
        let mut handle = Self {
            state: ScanState::Running(Box::new(running)),
            cancelled: false,
        };
        if let Err(error) = launch {
            handle.seal(error_reason(&error));
        }
        handle
    }

    pub(crate) fn poll_ready(&mut self) -> Option<HealthScanReport> {
        if self.cancelled {
            return None;
        }
        let reason = match &mut self.state {
            ScanState::Sealed(report) => return report.take().map(|report| *report),
            ScanState::Running(running) => running.pump(),
        };
        if let Some(reason) = reason {
            self.seal(reason);
        }
        match &mut self.state {
            ScanState::Sealed(report) => report.take().map(|report| *report),
            ScanState::Running(_) => None,
        }
    }

    pub(crate) fn finish_within(&mut self, wait: Duration) -> Option<HealthScanReport> {
        if self.cancelled {
            return None;
        }
        if let ScanState::Running(running) = &mut self.state {
            if running.request.deadline.shorten_within(wait).is_err() {
                self.seal(HealthUnavailableReason::ParseError);
            } else if let Some(child) = running.child.as_mut() {
                if let Ok(deadline) = running.request.deadline.local_instant() {
                    child.shorten_deadline(deadline);
                }
            }
        }
        loop {
            if let Some(report) = self.poll_ready() {
                return Some(report);
            }
            let ScanState::Running(running) = &self.state else {
                return None;
            };
            let remaining = running.request.deadline.remaining().unwrap_or_default();
            thread::sleep(COMMAND_POLL_INTERVAL.min(remaining));
        }
    }

    pub(crate) fn cancel(&mut self) {
        self.seal(HealthUnavailableReason::Timeout);
        self.cancelled = true;
    }

    fn seal(&mut self, mut reason: HealthUnavailableReason) {
        let state = std::mem::replace(&mut self.state, ScanState::Sealed(None));
        let ScanState::Running(mut running) = state else {
            self.state = state;
            return;
        };
        let mut cleanup_incomplete = false;
        if let Some(mut child) = running.child.take() {
            let cleanup_deadline = if !running.configured {
                running.bootstrap_deadline
            } else {
                running
                    .awaiting
                    .as_ref()
                    .map(|task| task.deadline)
                    .unwrap_or(running.request.deadline)
            };
            if let Ok(deadline) = cleanup_deadline.local_instant() {
                child.shorten_deadline(deadline);
            }
            cleanup_incomplete = !matches!(child.cleanup(), Ok(Some(_)));
            if !cleanup_incomplete {
                if let Some(pgid) = running.child_pgid {
                    cleanup_incomplete = !super::cleanup::confirm_group_gone(
                        pgid,
                        cleanup_deadline
                            .local_instant()
                            .unwrap_or_else(|_| Instant::now())
                            .min(Instant::now() + super::cleanup::GROUP_CONFIRM_BUDGET),
                    );
                }
            }
            if cleanup_incomplete {
                reason = if running
                    .request
                    .deadline
                    .remaining()
                    .unwrap_or_default()
                    .is_zero()
                {
                    HealthUnavailableReason::Timeout
                } else {
                    HealthUnavailableReason::ParseError
                };
            }
        }
        if let Some(mut stream) = running.stream.take() {
            let received = running.decoder.receive_buffered(&mut stream);
            if let Err(error) = running.apply_frames() {
                reason = error_reason(&error);
            }
            if let Err(error) = received {
                reason = error_reason(&error);
            }
            if !running.decoder.is_empty() {
                reason = HealthUnavailableReason::ParseError;
            }
            let _ = stream.shutdown(Shutdown::Both);
        }
        running.stream.take();
        let elapsed = running
            .request
            .deadline
            .elapsed()
            .unwrap_or_else(|_| running.started.elapsed())
            .as_millis();
        if (cleanup_incomplete || reason != HealthUnavailableReason::Timeout)
            && !running.builder.has_pending()
        {
            let (id, collector) = running
                .enabled
                .iter()
                .find_map(|id| {
                    HealthCollector::from_check_id(id).map(|collector| (id.as_str(), collector))
                })
                .unwrap_or(("environment.config", HealthCollector::Config));
            running.builder.add_check_gap(
                id,
                collector,
                reason,
                HealthSeverity::Unavailable,
                elapsed,
            );
        }
        if running.request.fixture_name.is_none()
            && running.enabled.contains("resource.swap")
            && !running.builder.is_pending("resource.swap")
            && !super::super::rules::swap_context_complete(
                running.builder.report(),
                &running.request.config,
            )
        {
            running.builder.seal_check_gap(
                "resource.swap",
                HealthCollector::Memory,
                HealthUnavailableReason::MissingContext,
                0,
            );
        }
        if running.request.config_source == super::plan::ConfigSource::LoginConfigProjection
            && (cleanup_incomplete || reason != HealthUnavailableReason::Timeout)
            && running.next_source == 0
        {
            running.builder.clear_login_config();
        }
        running.builder.seal_pending(reason, elapsed);
        let mut report = running
            .builder
            .finish(running.request.started_at_ms.saturating_add(elapsed));
        let findings = evaluate_judgement_rules(&report, &running.request.config);
        report.findings.extend(findings);
        report.recompute_overall_severity();
        super::super::recommendation::apply_try_candidates(&mut report, &running.request.config);
        report.elapsed_ms = running
            .request
            .deadline
            .elapsed()
            .unwrap_or_else(|_| running.started.elapsed())
            .as_millis();
        self.state = ScanState::Sealed(Some(Box::new(report)));
    }
}

impl Drop for HealthScanHandle {
    fn drop(&mut self) {
        self.cancel();
    }
}

impl RunningScan {
    fn launch(&mut self) -> Result<(), ProtocolError> {
        if self
            .request
            .deadline
            .remaining()
            .map_err(ProtocolError::Io)?
            .is_zero()
        {
            return Err(ProtocolError::Timeout);
        }
        self.generation += 1;
        self.nonce = uuid::Uuid::new_v4().simple().to_string();
        self.sequence = 1;
        self.decoder = Decoder::default();
        let mut start_request = self.request.clone();
        if self.generation > 1 {
            start_request.config_source = super::plan::ConfigSource::CallerSnapshot;
        }
        let bytes = protocol::encode(&self.nonce, 0, Payload::Start(start_request))?;
        let (mut parent, child_socket) = UnixStream::pair().map_err(ProtocolError::Io)?;
        parent.set_nonblocking(true).map_err(ProtocolError::Io)?;
        let mut command = Command::new(std::env::current_exe().map_err(ProtocolError::Io)?);
        #[cfg(not(test))]
        command.arg(PRIVATE_WORKER_FLAG);
        #[cfg(test)]
        command.args([
            "--ignored",
            "--exact",
            "diagnostics::health::runtime::worker::tests::private_worker_child",
            "--test-threads=1",
        ]);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        configure_isolated_worker(
            &mut command,
            self.awaiting
                .as_ref()
                .map(|task| task.work_deadline)
                .unwrap_or(self.bootstrap_deadline)
                .job_deadline()
                .map_err(ProtocolError::Io)?,
        );
        let descriptor = child_socket.as_raw_fd();
        unsafe {
            command.pre_exec(move || {
                if descriptor != IPC_FD && libc::dup2(descriptor, IPC_FD) < 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::fcntl(IPC_FD, libc::F_SETFD, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        configure_cloexec(&mut command, 4).map_err(ProtocolError::Io)?;
        ensure_waitable_children().map_err(ProtocolError::Io)?;
        let owner = ReapOwner::global().map_err(ProtocolError::Io)?;
        let permit = owner
            .reserve()
            .ok_or_else(|| ProtocolError::Io(io::Error::from_raw_os_error(libc::EAGAIN)))?;
        let guard_deadline = self
            .request
            .deadline
            .local_instant()
            .map_err(ProtocolError::Io)?;
        let child = command.spawn().map_err(ProtocolError::Io)?;
        self.child_pgid = Some(child.id());
        self.child = Some(permit.register(child, CleanupScope::OwnedGroup, guard_deadline));
        drop(child_socket);
        protocol::send(&mut parent, &bytes, self.request.deadline)?;
        self.stream = Some(parent);
        Ok(())
    }

    fn pump(&mut self) -> Option<HealthUnavailableReason> {
        if self
            .request
            .deadline
            .remaining()
            .unwrap_or_default()
            .is_zero()
        {
            return if self.awaiting.is_some() {
                self.expire_source()
            } else {
                Some(HealthUnavailableReason::Timeout)
            };
        }
        if self
            .awaiting
            .as_ref()
            .is_some_and(|task| task.work_deadline.remaining().unwrap_or_default().is_zero())
        {
            return self.expire_source();
        }
        if !self.configured
            && self.bootstrap_deadline.remaining().unwrap_or_default() <= Duration::from_millis(5)
        {
            return Some(HealthUnavailableReason::Timeout);
        }
        if self.configured && self.awaiting.is_none() && self.next_source < self.plan.len() {
            if let Some(reason) = self.dispatch_next() {
                return Some(reason);
            }
        }
        if self.configured && self.awaiting.is_none() && !self.builder.has_pending() {
            return Some(HealthUnavailableReason::Timeout);
        }
        // An exit observed after reading may leave final frames unread.
        let child_exited = self
            .child
            .as_ref()
            .is_some_and(|child| child.has_exited().unwrap_or(true));
        let stream = match self.stream.as_mut() {
            Some(stream) => stream,
            None => return Some(HealthUnavailableReason::ParseError),
        };
        let result = self.decoder.receive(stream, self.request.deadline);
        if let Err(error) = self.apply_frames() {
            if error_reason(&error) == HealthUnavailableReason::ParseError {
                if let Some(source) = self
                    .next_source
                    .checked_sub(1)
                    .and_then(|i| self.plan.get(i))
                {
                    self.builder.discard_check_facts(&source.check_ids);
                }
            }
            return self.fail_source(error_reason(&error));
        }
        if let Err(error) = &result {
            if error_reason(error) == HealthUnavailableReason::ParseError {
                if let Some(source) = self
                    .next_source
                    .checked_sub(1)
                    .and_then(|i| self.plan.get(i))
                {
                    self.builder.discard_check_facts(&source.check_ids);
                }
            }
            return self.fail_source(error_reason(error));
        }
        if self.source_done {
            self.source_done = false;
            return self.complete_source();
        }
        if self.configured && self.awaiting.is_none() && !self.builder.has_pending() {
            return Some(HealthUnavailableReason::Timeout);
        }
        match result {
            Err(error) => return self.fail_source(error_reason(&error)),
            Ok(true) => return self.fail_source(self.incomplete_reason()),
            Ok(false) => {}
        }
        if child_exited {
            return self.fail_source(self.incomplete_reason());
        }
        None
    }

    fn cleanup_failure_reason(&self) -> HealthUnavailableReason {
        if self
            .request
            .deadline
            .remaining()
            .unwrap_or_default()
            .is_zero()
        {
            HealthUnavailableReason::Timeout
        } else {
            HealthUnavailableReason::ParseError
        }
    }

    fn incomplete_reason(&self) -> HealthUnavailableReason {
        if self
            .builder
            .report()
            .unavailable
            .iter()
            .any(|gap| gap.reason == HealthUnavailableReason::Timeout)
        {
            HealthUnavailableReason::Timeout
        } else {
            HealthUnavailableReason::ParseError
        }
    }

    fn apply_frames(&mut self) -> Result<(), ProtocolError> {
        while let Some(frame) = self.decoder.next()? {
            if frame.nonce != self.nonce
                || frame.sequence != self.sequence
                || frame.emitted_ns < self.started_ns
            {
                return Err(ProtocolError::Invalid);
            }
            self.sequence += 1;
            if frame.emitted_ns > self.request.deadline.expires_ns {
                return Err(ProtocolError::Timeout);
            }
            let progress = match frame.payload {
                Payload::Configuration(config) => {
                    if self.configured
                        || self.request.config_source
                            != super::plan::ConfigSource::LoginConfigProjection
                        || frame.sequence != 1
                    {
                        return Err(ProtocolError::Invalid);
                    }
                    if frame.emitted_ns
                        > self.bootstrap_deadline.expires_ns.saturating_sub(5_000_000)
                    {
                        return Err(ProtocolError::Timeout);
                    }
                    self.request.config = config.health.clone();
                    self.builder =
                        HealthReportBuilder::new(&self.request.scan_id, self.request.started_at_ms);
                    self.builder.set_role(if config.health.enabled {
                        config.health.role.clone()
                    } else {
                        None
                    });
                    self.builder.set_login_config(config);
                    self.enabled.clear();
                    for spec in checks(&self.request) {
                        self.builder
                            .declare_check_id(spec.id, spec.collector, spec.enabled);
                        if spec.enabled {
                            self.enabled.insert(spec.id.to_string());
                        }
                    }
                    self.plan = build_plan(&self.enabled, self.request.context.passive());
                    self.next_source = 0;
                    self.configured = true;
                    continue;
                }
                Payload::SourceDone { task_id } => {
                    if !self.configured
                        || self
                            .awaiting
                            .as_ref()
                            .is_none_or(|awaited| awaited.task_id != task_id)
                    {
                        return Err(ProtocolError::Invalid);
                    }
                    if self.source_done {
                        return Err(ProtocolError::Invalid);
                    }
                    if self
                        .awaiting
                        .as_ref()
                        .is_some_and(|task| frame.emitted_ns > task.work_deadline.expires_ns)
                    {
                        return Err(ProtocolError::Timeout);
                    }
                    self.source_done = true;
                    continue;
                }
                Payload::Collect(_) => return Err(ProtocolError::Invalid),
                Payload::Progress { task_id, progress } if self.configured => {
                    let awaited = self.awaiting.as_ref().ok_or(ProtocolError::Invalid)?;
                    if task_id != awaited.task_id || self.source_done {
                        return Err(ProtocolError::Invalid);
                    }
                    if frame.emitted_ns > awaited.work_deadline.expires_ns {
                        return Err(ProtocolError::Timeout);
                    }
                    let ids = &self
                        .plan
                        .get(
                            self.next_source
                                .checked_sub(1)
                                .ok_or(ProtocolError::Invalid)?,
                        )
                        .ok_or(ProtocolError::Invalid)?
                        .check_ids;
                    let allowed = match &progress {
                        HealthProgress::Fact { check_id, .. } => ids.contains(check_id),
                        HealthProgress::Check(check) => ids.contains(&check.id),
                        HealthProgress::Host(_) => ids.iter().any(|id| {
                            HealthCollector::from_check_id(id) == Some(HealthCollector::Host)
                        }),
                        HealthProgress::Finding(finding) => {
                            self.plan[self.next_source - 1].source
                                == super::super::plan::SampleSource::Environment
                                && !finding.evidence_fact_ids.is_empty()
                                && finding.evidence_fact_ids.iter().all(|fact_id| {
                                    self.builder.report().check_results.iter().any(|check| {
                                        ids.contains(&check.id) && check.fact_ids.contains(fact_id)
                                    })
                                })
                        }
                    };
                    if !allowed {
                        return Err(ProtocolError::Invalid);
                    }
                    progress
                }
                _ => return Err(ProtocolError::Invalid),
            };
            match &progress {
                HealthProgress::Fact { check_id, fact, .. }
                    if !self.enabled.contains(check_id)
                        || !self.builder.is_pending(check_id)
                        || !super::super::plan::owns_fact(check_id, &fact.key) =>
                {
                    return Err(ProtocolError::Invalid)
                }
                HealthProgress::Host(_)
                    if !self.enabled.iter().any(|id| {
                        HealthCollector::from_check_id(id) == Some(HealthCollector::Host)
                            && self.builder.is_pending(id)
                    }) =>
                {
                    return Err(ProtocolError::Invalid)
                }
                HealthProgress::Check(check) => {
                    if !self.enabled.contains(&check.id)
                        || !self.builder.is_pending(&check.id)
                        || check.collector != HealthCollector::from_check_id(&check.id)
                    {
                        return Err(ProtocolError::Invalid);
                    }
                    let status = if check.gaps.is_empty() {
                        HealthCheckStatus::Ok
                    } else if check.fact_ids.is_empty() {
                        HealthCheckStatus::Unavailable
                    } else {
                        HealthCheckStatus::Partial
                    };
                    if check.status != status
                        || check
                            .gaps
                            .iter()
                            .any(|gap| Some(gap.collector) != check.collector)
                    {
                        return Err(ProtocolError::Invalid);
                    }
                }
                _ => {}
            }
            self.builder
                .apply_progress(progress)
                .map_err(|_| ProtocolError::Invalid)?;
        }
        Ok(())
    }
}

fn error_reason(error: &ProtocolError) -> HealthUnavailableReason {
    match error {
        ProtocolError::Timeout => HealthUnavailableReason::Timeout,
        ProtocolError::Io(error) if error.kind() == io::ErrorKind::NotFound => {
            HealthUnavailableReason::CommandMissing
        }
        ProtocolError::Io(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            HealthUnavailableReason::PermissionDenied
        }
        _ => HealthUnavailableReason::ParseError,
    }
}

#[cfg(test)]
library_tests! {
    mod tests;
}
