//! Standalone AgentSight enforcement daemon.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(feature = "actplane")]
use agentsight_enforcer::ActPlaneBackend;
use agentsight_enforcer::EnforcerService;
#[cfg(all(feature = "mock-backend", not(feature = "actplane")))]
use agentsight_enforcer::MockBackend;

fn main() -> anyhow::Result<()> {
    init_logging();
    let socket_path = std::env::var("AGENTSIGHT_ENFORCER_SOCKET")
        .unwrap_or_else(|_| "/run/agentsight/enforcer.sock".into());
    run(socket_path)
}

/// Installs a stderr logger so the daemon's lifecycle is visible in the
/// systemd journal. journald already timestamps each line, so the format
/// carries only level, target, and message. Defaults to `info`;
/// `RUST_LOG` overrides.
fn init_logging() {
    let mut builder = env_filter::Builder::new();
    match std::env::var("RUST_LOG") {
        Ok(spec) => {
            if let Err(error) = builder.try_parse(&spec) {
                eprintln!("agentsight-enforcer: invalid RUST_LOG={spec:?}: {error}");
                builder.filter_level(log::LevelFilter::Info);
            }
        }
        Err(_) => {
            builder.filter_level(log::LevelFilter::Info);
        }
    }
    let filter = builder.build();
    log::set_max_level(filter.filter());
    if let Err(error) = log::set_boxed_logger(Box::new(StderrLogger { filter })) {
        eprintln!("agentsight-enforcer: failed to install logger: {error}");
    }
}

struct StderrLogger {
    filter: env_filter::Filter,
}

impl log::Log for StderrLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        self.filter.enabled(metadata)
    }

    fn log(&self, record: &log::Record) {
        // `matches` (not just `enabled`) so a `RUST_LOG=level/regex` message
        // filter is honored.
        if self.filter.matches(record) {
            eprintln!(
                "[{:5} {}] {}",
                record.level(),
                record.target(),
                record.args()
            );
        }
    }

    fn flush(&self) {}
}

fn termination_flag() -> anyhow::Result<Arc<AtomicBool>> {
    let stop = Arc::new(AtomicBool::new(false));
    let signal_stop = Arc::clone(&stop);
    ctrlc::set_handler(move || signal_stop.store(true, Ordering::Release))?;
    Ok(stop)
}

#[cfg(feature = "actplane")]
fn run(socket_path: String) -> anyhow::Result<()> {
    let stop = termination_flag()?;
    let service = EnforcerService::bind(&socket_path, Arc::new(ActPlaneBackend::open()?), None)?;
    log::info!("agentsight-enforcer listening on {socket_path}");
    service.serve_until(stop.as_ref())?;
    log::info!("agentsight-enforcer stopped");
    Ok(())
}

#[cfg(all(feature = "mock-backend", not(feature = "actplane")))]
fn run(socket_path: String) -> anyhow::Result<()> {
    log::warn!("agentsight-enforcer is using the mock backend; kernel operations are not enforced");
    let stop = termination_flag()?;
    let service = EnforcerService::bind(&socket_path, Arc::new(MockBackend::new()), None)?;
    log::info!("agentsight-enforcer listening on {socket_path}");
    service.serve_until(stop.as_ref())?;
    log::info!("agentsight-enforcer stopped");
    Ok(())
}

#[cfg(not(any(feature = "mock-backend", feature = "actplane")))]
compile_error!("agentsight-enforcer requires the mock-backend or actplane feature");

#[cfg(test)]
mod tests {
    use super::*;
    use log::Log as _;

    #[test]
    fn stderr_logger_delegates_enabled_to_filter() {
        let filter = env_filter::Builder::new()
            .filter_level(log::LevelFilter::Info)
            .build();
        let logger = StderrLogger { filter };
        // The log! fast path checks max_level and calls log() directly, so
        // enabled()/flush() are only reachable via explicit calls; pin the
        // delegation contract here.
        let info = log::Metadata::builder()
            .level(log::Level::Info)
            .target("enforcer")
            .build();
        assert!(logger.enabled(&info));
        let debug = log::Metadata::builder()
            .level(log::Level::Debug)
            .target("enforcer")
            .build();
        assert!(!logger.enabled(&debug));
        logger.flush();
    }
}
