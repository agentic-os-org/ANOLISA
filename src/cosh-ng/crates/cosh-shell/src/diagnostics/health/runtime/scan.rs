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
use super::super::rules::evaluate_judgement_rules;
use super::job::{configure_cloexec, configure_isolated_worker, monotonic_ns, JobDeadline};
use super::plan::{checks, WorkerRequest};
use super::process::{
    child_has_exited, ensure_waitable_children, CleanupScope, CommandChild, ReapOwner,
    COMMAND_POLL_INTERVAL,
};
use super::protocol::{self, Decoder, Payload, ProtocolError};
use super::worker::{IPC_FD, PRIVATE_WORKER_FLAG};

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
    stream: Option<UnixStream>,
    decoder: Decoder,
    nonce: String,
    sequence: u64,
    enabled: BTreeSet<String>,
    configured: bool,
    started: Instant,
    started_ns: u64,
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
        let mut running = RunningScan {
            started: Instant::now(),
            started_ns: request.deadline.started_ns,
            request,
            builder,
            child: None,
            stream: None,
            decoder: Decoder::default(),
            nonce: uuid::Uuid::new_v4().simple().to_string(),
            sequence: 1,
            enabled,
            configured,
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
                if let Ok(deadline) = running.request.deadline.instant() {
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
            if let Ok(deadline) = running.request.deadline.instant() {
                child.shorten_deadline(deadline);
            }
            cleanup_incomplete = !matches!(child.cleanup(), Ok(Some(_)));
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
        if running.request.config_source == super::plan::ConfigSource::LoginConfigProjection
            && (cleanup_incomplete || reason != HealthUnavailableReason::Timeout)
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
        let bytes = protocol::encode(&self.nonce, 0, Payload::Start(self.request.clone()))?;
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
        configure_isolated_worker(&mut command, self.request.deadline);
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
        let guard_deadline = self.request.deadline.instant().map_err(ProtocolError::Io)?;
        let child = command.spawn().map_err(ProtocolError::Io)?;
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
            return Some(error_reason(&error));
        }
        if let Err(error) = &result {
            return Some(error_reason(error));
        }
        if self.configured && !self.builder.has_pending() {
            return Some(HealthUnavailableReason::Timeout);
        }
        match result {
            Err(error) => return Some(error_reason(&error)),
            Ok(true) => return Some(self.incomplete_reason()),
            Ok(false) => {}
        }
        if child_exited {
            return Some(self.incomplete_reason());
        }
        None
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
                    self.configured = true;
                    continue;
                }
                Payload::Progress(progress) if self.configured => progress,
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
mod tests;
