use std::fs::{self, OpenOptions};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use nix::libc;

use super::super::builder::HealthReportBuilder;
use super::super::collectors::{
    self, collect_configured_services, collect_cpu, collect_host, collect_kernel_signals,
};
use super::super::env_collectors::{run_environment_check, EnvironmentConfigSample};
use super::super::model::{HealthCollector, HealthProgress};
use super::job::{install_worker_timeout_handler, timer_remaining, JobDeadline};
use super::plan::{checks, ScanContext, WorkerRequest};
use super::process::{set_nonblocking, COMMAND_POLL_INTERVAL};
use super::protocol::{self, Decoder, Payload, ProtocolError, FRAME_COUNT_LIMIT, TOTAL_LIMIT};

pub(super) const IPC_FD: i32 = 3;
pub(crate) const PRIVATE_WORKER_FLAG: &str = "--__cosh-health-worker-v1";
pub(super) const MAX_WORKER_BUDGET: Duration = Duration::from_secs(3);

pub(crate) fn run_private_health_worker() -> i32 {
    if run_worker().is_ok() {
        0
    } else {
        2
    }
}

fn run_worker() -> Result<(), ProtocolError> {
    install_worker_timeout_handler().map_err(ProtocolError::Io)?;
    validate_isolation().map_err(ProtocolError::Io)?;
    let budget = timer_remaining().map_err(ProtocolError::Io)?;
    if budget.is_zero() || budget > MAX_WORKER_BUDGET {
        return Err(ProtocolError::Invalid);
    }
    let bootstrap = JobDeadline::after(budget).map_err(ProtocolError::Io)?;
    let mut stream = unsafe { UnixStream::from_raw_fd(IPC_FD) };
    let mut address: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
    let mut length = std::mem::size_of_val(&address) as libc::socklen_t;
    if unsafe {
        libc::getsockname(
            IPC_FD,
            (&mut address as *mut libc::sockaddr_storage).cast(),
            &mut length,
        )
    } < 0
        || address.ss_family as libc::c_int != libc::AF_UNIX
    {
        return Err(ProtocolError::Invalid);
    }
    set_nonblocking(stream.as_raw_fd()).map_err(ProtocolError::Io)?;
    let mut decoder = Decoder::default();
    let frame = loop {
        let eof = decoder.receive(&mut stream, bootstrap)?;
        if let Some(frame) = decoder.next()? {
            break frame;
        }
        if eof {
            return Err(ProtocolError::Invalid);
        }
        thread::sleep(COMMAND_POLL_INTERVAL);
    };
    if !decoder.is_empty() {
        return Err(ProtocolError::Invalid);
    }
    if frame.sequence != 0 {
        return Err(ProtocolError::Invalid);
    }
    let Payload::Start(mut request) = frame.payload else {
        return Err(ProtocolError::Invalid);
    };
    let remaining = request.deadline.remaining().map_err(ProtocolError::Io)?;
    if remaining.is_zero()
        || remaining > MAX_WORKER_BUDGET
        || (request.context == ScanContext::OnDemand) != request.environment.is_some()
        || request.scan_id != super::super::builder::health_scan_id(request.started_at_ms)
    {
        return Err(ProtocolError::Invalid);
    }
    let descriptor_flags = unsafe { libc::fcntl(IPC_FD, libc::F_GETFD) };
    if descriptor_flags < 0
        || unsafe { libc::fcntl(IPC_FD, libc::F_SETFD, descriptor_flags | libc::FD_CLOEXEC) } < 0
    {
        return Err(ProtocolError::Io(io::Error::last_os_error()));
    }
    collectors::enter_worker_job(request.deadline).map_err(|_| ProtocolError::Invalid)?;
    let login_config = if request.config_source == super::plan::ConfigSource::LoginConfigProjection
    {
        if request.context != ScanContext::LoginBanner {
            return Err(ProtocolError::Invalid);
        }
        let config = crate::config::read_login_health_config().map_err(ProtocolError::Io)?;
        request.config = config.health.clone();
        Some(config)
    } else {
        None
    };
    let mut writer = ProgressWriter {
        stream,
        nonce: frame.nonce,
        deadline: request.deadline,
        sequence: 1,
        total: 0,
    };
    if let Some(config) = login_config {
        writer.write_payload(Payload::Configuration(config))?;
    }
    let writer = Arc::new(Mutex::new(writer));
    if let Some(name) = &request.fixture_name {
        let options = super::HealthScanOptions {
            started_at_ms: request.started_at_ms,
            now_epoch_seconds: request.now_epoch_seconds,
            total_budget: request.deadline.remaining().map_err(ProtocolError::Io)?,
            mode: super::HealthScanMode::Fixture(name.clone()),
        };
        let report = super::fixtures::fixture_report_for_context(
            name,
            &request.config,
            &options,
            request.context,
            &request.disabled,
        );
        let mut output = writer.lock().map_err(|_| ProtocolError::Invalid)?;
        for check in report.check_results.iter().filter(|check| {
            check.collector.is_some()
                && check.status != super::super::model::HealthCheckStatus::Disabled
        }) {
            if check.collector == Some(HealthCollector::Host) {
                if let Some(host) = &report.host {
                    output.write(HealthProgress::Host(host.clone()))?;
                }
            }
            for id in &check.fact_ids {
                let fact = report
                    .facts
                    .iter()
                    .find(|fact| fact.id == *id)
                    .ok_or(ProtocolError::Invalid)?;
                let metadata = report.fact_metadata.get(id).ok_or(ProtocolError::Invalid)?;
                output.write(HealthProgress::Fact {
                    check_id: check.id.clone(),
                    fact: fact.clone(),
                    metadata: metadata.clone(),
                })?;
            }
            output.write(HealthProgress::Check(check.clone()))?;
        }
        return Ok(());
    }
    let output = writer.clone();
    let mut builder = HealthReportBuilder::new(&request.scan_id, request.started_at_ms)
        .with_progress(move |progress| {
            let sent = output
                .lock()
                .map(|mut writer| writer.write(progress).is_ok())
                .unwrap_or(false);
            if !sent {
                collectors::stop_worker_job();
            }
            sent
        });
    let sources = super::plan::sources(&request);
    let memory_sample = sources
        .contains(&super::super::plan::SampleSource::Meminfo)
        .then(std::cell::OnceCell::new);
    let disk_sample = sources
        .contains(&super::super::plan::SampleSource::Filesystems)
        .then(std::cell::OnceCell::new);
    let planned = checks(&request);
    let root_enabled = planned
        .iter()
        .any(|check| check.id == "filesystem.root" && check.enabled);
    let mounts_enabled = planned
        .iter()
        .any(|check| check.id == "filesystem.mounts" && check.enabled);
    let environment = request
        .environment
        .as_ref()
        .and_then(|snapshot| snapshot.config(request.config.clone()));
    if request.environment.is_some() && environment.is_none() {
        return Err(ProtocolError::Invalid);
    }
    let environment_sample = std::cell::OnceCell::<EnvironmentConfigSample>::new();
    for spec in planned {
        if !spec.enabled {
            builder.declare_check_id(spec.id, spec.collector, false);
            continue;
        }
        if builder.progress_failed() || collectors::worker_command_stopped() {
            break;
        }
        if request
            .deadline
            .remaining()
            .map_err(ProtocolError::Io)?
            .is_zero()
        {
            return Err(ProtocolError::Timeout);
        }
        collectors::begin_worker_check(Duration::from_millis(spec.budget_ms))
            .map_err(|_| ProtocolError::Invalid)?;
        builder.start_check_id(
            spec.id,
            spec.collector,
            spec.scope,
            spec.coverage,
            now_millis(),
        );
        match spec.collector {
            HealthCollector::Cpu => collect_cpu(&mut builder),
            HealthCollector::Memory => {
                let (sampled_at, sample) = memory_sample
                    .as_ref()
                    .ok_or(ProtocolError::Invalid)?
                    .get_or_init(|| (now_millis(), collectors::read_memory()));
                builder.set_sampled_at(*sampled_at);
                match sample {
                    Ok(facts) => {
                        facts.record(&mut builder, 0);
                        builder.add_check_done(spec.id);
                        if spec.id == "resource.swap"
                            && !super::super::rules::swap_context_complete(
                                builder.report(),
                                &request.config,
                            )
                        {
                            builder.add_unavailable(
                                spec.collector,
                                super::super::model::HealthUnavailableReason::MissingContext,
                                super::super::model::HealthSeverity::Unavailable,
                                0,
                            );
                        }
                    }
                    Err(reason) => {
                        builder.add_unavailable(
                            spec.collector,
                            *reason,
                            super::super::model::HealthSeverity::Degraded,
                            0,
                        );
                    }
                }
            }
            HealthCollector::Host => collect_host(&mut builder),
            HealthCollector::Disk => {
                let (sampled_at, sample) = disk_sample
                    .as_ref()
                    .ok_or(ProtocolError::Invalid)?
                    .get_or_init(|| {
                        (
                            now_millis(),
                            collectors::read_disk_sample(
                                &request.config,
                                request.context.passive(),
                                root_enabled,
                                mounts_enabled,
                            ),
                        )
                    });
                builder.set_sampled_at(*sampled_at);
                match sample {
                    Ok(facts) => {
                        facts.record(&mut builder, 0);
                        builder.add_check_done(spec.id);
                    }
                    Err(reason) => {
                        builder.add_unavailable(
                            spec.collector,
                            *reason,
                            super::super::model::HealthSeverity::Degraded,
                            0,
                        );
                    }
                }
            }
            HealthCollector::KernelSignal => {
                collect_kernel_signals(&mut builder, request.now_epoch_seconds)
            }
            HealthCollector::ConfiguredService => {
                collect_configured_services(&mut builder, &request.config)
            }
            collector => {
                let config = environment.as_ref().ok_or(ProtocolError::Invalid)?;
                let snapshot = request.environment.as_ref().ok_or(ProtocolError::Invalid)?;
                let cwd = if snapshot.cwd.is_absolute() {
                    snapshot.cwd.clone()
                } else {
                    std::env::current_dir()
                        .map(|cwd| cwd.join(&snapshot.cwd))
                        .unwrap_or_else(|_| snapshot.cwd.clone())
                };
                run_environment_check(
                    &mut builder,
                    collector,
                    config,
                    &cwd,
                    0,
                    request.context == ScanContext::OnDemand,
                    &environment_sample,
                );
            }
        }
        builder.complete_check();
    }
    if builder.progress_failed() {
        return Err(ProtocolError::Invalid);
    }
    Ok(())
}

struct ProgressWriter {
    stream: UnixStream,
    nonce: String,
    deadline: JobDeadline,
    sequence: u64,
    total: usize,
}

impl ProgressWriter {
    fn write(&mut self, progress: HealthProgress) -> Result<(), ProtocolError> {
        self.write_payload(Payload::Progress(progress))
    }
    fn write_payload(&mut self, payload: Payload) -> Result<(), ProtocolError> {
        if self.sequence > FRAME_COUNT_LIMIT {
            return Err(ProtocolError::TooLarge);
        }
        let bytes = protocol::encode(&self.nonce, self.sequence, payload)?;
        if self.total.saturating_add(bytes.len()) > TOTAL_LIMIT {
            return Err(ProtocolError::TooLarge);
        }
        protocol::send(&mut self.stream, &bytes, self.deadline)?;
        self.sequence += 1;
        self.total += bytes.len();
        Ok(())
    }
}

fn validate_isolation() -> io::Result<()> {
    let null = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/null")?;
    let mut expected: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(null.as_raw_fd(), &mut expected) } < 0 {
        return Err(io::Error::last_os_error());
    }
    for descriptor in 0..3 {
        let mut metadata: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstat(descriptor, &mut metadata) } < 0
            || metadata.st_rdev != expected.st_rdev
            || metadata.st_mode & libc::S_IFMT != libc::S_IFCHR
        {
            return Err(io::Error::from_raw_os_error(libc::EPERM));
        }
    }
    let mut control: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(IPC_FD, &mut control) } < 0
        || control.st_mode & libc::S_IFMT != libc::S_IFSOCK
    {
        return Err(io::Error::from_raw_os_error(libc::EBADF));
    }
    drop(null);
    match OpenOptions::new().read(true).write(true).open("/dev/tty") {
        Err(error) if matches!(error.raw_os_error(), Some(libc::ENXIO | libc::ENOTTY)) => {}
        _ => return Err(io::Error::from_raw_os_error(libc::EPERM)),
    }
    #[cfg(target_os = "linux")]
    let descriptors = "/proc/self/fd";
    #[cfg(not(target_os = "linux"))]
    let descriptors = "/dev/fd";
    let open: Vec<i32> = fs::read_dir(descriptors)?
        .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse().ok())
        .collect();
    if open.into_iter().any(|descriptor| {
        descriptor > IPC_FD && unsafe { libc::fcntl(descriptor, libc::F_GETFD) } >= 0
    }) {
        return Err(io::Error::from_raw_os_error(libc::EPERM));
    }
    Ok(())
}

fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests;
