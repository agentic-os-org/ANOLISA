//! Periodic CPU and resident-memory sampling for discovered Agent processes.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, RwLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use procfs::process::Process;

use crate::storage::sqlite::{GenAISqliteStore, ResourceSample};

const SAMPLE_INTERVAL_SECS: u64 = 1;

#[derive(Debug, Clone, Copy)]
struct CpuBaseline {
    process_start_ticks: u64,
    cpu_ticks: u64,
    sampled_at: Instant,
}

struct ResourceSampler {
    baselines: HashMap<u32, CpuBaseline>,
    ticks_per_second: u64,
}

impl ResourceSampler {
    fn new() -> Self {
        Self {
            baselines: HashMap::new(),
            ticks_per_second: procfs::ticks_per_second(),
        }
    }

    fn sample_once(&mut self, targets: &HashMap<u32, String>) -> Vec<ResourceSample> {
        let active_pids: HashSet<u32> = targets.keys().copied().collect();
        self.baselines.retain(|pid, _| active_pids.contains(pid));

        let mut samples = Vec::with_capacity(targets.len());
        for (&pid, agent_name) in targets {
            match self.sample_process(pid, agent_name) {
                Ok(Some(sample)) => samples.push(sample),
                Ok(None) => {}
                Err(error) => {
                    log::debug!("Resource sample skipped for pid={pid}: {error}");
                }
            }
        }
        samples
    }

    fn sample_process(
        &mut self,
        pid: u32,
        agent_name: &str,
    ) -> Result<Option<ResourceSample>, procfs::ProcError> {
        let process = Process::new(pid as i32)?;
        let stat = process.stat()?;
        let now = Instant::now();
        let current = CpuBaseline {
            process_start_ticks: stat.starttime,
            cpu_ticks: stat.utime.saturating_add(stat.stime),
            sampled_at: now,
        };
        let previous = self.baselines.insert(pid, current);
        let Some(previous) = previous else {
            return Ok(None);
        };

        // A reused PID represents a different process and must start a fresh baseline.
        if previous.process_start_ticks != current.process_start_ticks {
            return Ok(None);
        }

        let cpu_percent = compute_cpu_percent(
            previous.cpu_ticks,
            current.cpu_ticks,
            current
                .sampled_at
                .duration_since(previous.sampled_at)
                .as_secs_f64(),
            self.ticks_per_second,
        );
        let memory_bytes = process
            .status()
            .ok()
            .and_then(|status| status.vmrss)
            .map(|kib| kib.saturating_mul(1024))
            .unwrap_or_else(|| stat.rss.saturating_mul(procfs::page_size()));
        let timestamp_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos().min(i64::MAX as u128) as i64)
            .unwrap_or(0);

        Ok(Some(ResourceSample {
            timestamp_ns,
            pid: pid as i32,
            agent_name: Some(agent_name.to_string()),
            cpu_percent,
            memory_bytes: memory_bytes.min(i64::MAX as u64) as i64,
        }))
    }
}

fn compute_cpu_percent(
    previous_ticks: u64,
    current_ticks: u64,
    elapsed_secs: f64,
    ticks_per_second: u64,
) -> f64 {
    if elapsed_secs <= 0.0 || ticks_per_second == 0 {
        return 0.0;
    }
    let cpu_secs = current_ticks.saturating_sub(previous_ticks) as f64 / ticks_per_second as f64;
    cpu_secs / elapsed_secs * 100.0
}

pub(crate) fn start_resource_sampler(
    store: Arc<GenAISqliteStore>,
    targets: Arc<RwLock<HashMap<u32, String>>>,
    stop: Arc<AtomicBool>,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("agent-resource-sampler".to_string())
        .spawn(move || {
            let mut sampler = ResourceSampler::new();
            // Establish CPU baselines immediately so the first persisted point
            // arrives after one complete sampling interval.
            let initial = targets
                .read()
                .map(|guard| guard.clone())
                .unwrap_or_default();
            let _ = sampler.sample_once(&initial);
            while crate::utils::thread::sleep_or_stop(&stop, SAMPLE_INTERVAL_SECS) {
                let current = targets
                    .read()
                    .map(|guard| guard.clone())
                    .unwrap_or_default();
                let samples = sampler.sample_once(&current);
                if samples.is_empty() {
                    continue;
                }
                if let Err(error) = store.insert_resource_samples(&samples) {
                    log::warn!("Failed to persist Agent resource samples: {error}");
                }
            }
        })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, RwLock};
    use std::time::Duration;

    use super::{CpuBaseline, ResourceSampler, compute_cpu_percent, start_resource_sampler};
    use crate::storage::sqlite::GenAISqliteStore;

    #[test]
    fn cpu_percent_uses_process_time_delta() {
        let percent = compute_cpu_percent(100, 150, 1.0, 100);
        assert!((percent - 50.0).abs() < f64::EPSILON);
    }

    #[test]
    fn cpu_percent_can_exceed_one_core() {
        let percent = compute_cpu_percent(100, 350, 1.0, 100);
        assert!((percent - 250.0).abs() < f64::EPSILON);
    }

    #[test]
    fn cpu_percent_handles_invalid_intervals() {
        assert_eq!(compute_cpu_percent(100, 200, 0.0, 100), 0.0);
        assert_eq!(compute_cpu_percent(100, 200, 1.0, 0), 0.0);
        assert_eq!(compute_cpu_percent(200, 100, 1.0, 100), 0.0);
    }

    #[test]
    fn sampler_establishes_and_uses_a_process_baseline() {
        let pid = std::process::id();
        let mut sampler = ResourceSampler::new();
        let mut targets = HashMap::new();
        targets.insert(pid, "test-agent".to_string());

        assert!(sampler.sample_once(&targets).is_empty());
        std::thread::sleep(Duration::from_millis(5));
        let samples = sampler.sample_once(&targets);

        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].pid, pid as i32);
        assert_eq!(samples[0].agent_name.as_deref(), Some("test-agent"));
        assert!(samples[0].memory_bytes > 0);
        assert!(samples[0].cpu_percent >= 0.0);
    }

    #[test]
    fn sampler_skips_dead_processes_and_discards_stale_baselines() {
        let mut sampler = ResourceSampler::new();
        let dead_pid = u32::MAX;
        assert!(sampler.sample_process(dead_pid, "dead").is_err());

        sampler.baselines.insert(
            42,
            CpuBaseline {
                process_start_ticks: 1,
                cpu_ticks: 1,
                sampled_at: std::time::Instant::now(),
            },
        );
        let targets = HashMap::new();
        assert!(sampler.sample_once(&targets).is_empty());
        assert!(sampler.baselines.is_empty());
    }

    #[test]
    fn sampler_rejects_a_reused_pid_baseline() {
        let pid = std::process::id();
        let process = procfs::process::Process::new(pid as i32).expect("current process");
        let stat = process.stat().expect("current process stat");
        let mut sampler = ResourceSampler::new();
        sampler.baselines.insert(
            pid,
            CpuBaseline {
                process_start_ticks: stat.starttime.saturating_add(1),
                cpu_ticks: stat.utime.saturating_add(stat.stime),
                sampled_at: std::time::Instant::now(),
            },
        );

        assert!(
            sampler
                .sample_process(pid, "test-agent")
                .expect("sample")
                .is_none()
        );
    }

    #[test]
    fn sampler_thread_honors_stop_flag() {
        let path = std::env::temp_dir().join(format!(
            "agentsight-resource-sampler-{}.db",
            std::process::id()
        ));
        let store = Arc::new(
            GenAISqliteStore::new_with_path(&path, crate::config::InsertStoragePolicy::default())
                .expect("resource store"),
        );
        let targets = Arc::new(RwLock::new(HashMap::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let handle = start_resource_sampler(store, targets, stop).expect("sampler thread");
        handle.join().expect("sampler thread join");

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(format!("{}-wal", path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", path.display()));
    }
}
