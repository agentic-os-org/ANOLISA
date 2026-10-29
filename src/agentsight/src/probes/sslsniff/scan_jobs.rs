//! One bounded worker inspects stable file descriptors without moving BPF objects.

use super::find_static_ssl_offsets_paced;
use super::{FileKey, StaticSslOffsets, stat_dev_to_key};
use crate::utils::cpu_budget::CpuBudget;
use anyhow::{Context, Result};
use crossbeam_channel::{Receiver, Sender};
use std::collections::HashSet;
use std::fs::File;
use std::os::fd::AsRawFd;
use std::os::unix::fs::MetadataExt;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};

const MAX_SCANS: usize = 32;

#[derive(Debug)]
pub(super) enum SubmitError {
    QueueFull,
    Failed(anyhow::Error),
}

struct Job {
    key: FileKey,
    file: File,
    revision: (u64, i64, i64),
}

/// Retains the scanned inode until userspace installs its global uprobes.
pub(super) struct Scanned {
    pub(super) key: FileKey,
    file: File,
    pub(super) offsets: Option<StaticSslOffsets>,
    pub(super) unchanged: bool,
}

impl Scanned {
    /// A process exit or pathname replacement cannot invalidate this descriptor.
    pub(super) fn path(&self) -> String {
        format!("/proc/self/fd/{}", self.file.as_raw_fd())
    }
}

/// Bounds queued jobs, completed results and retained descriptors together.
pub(super) struct ScanJobs {
    requests: Option<Sender<Job>>,
    results: Receiver<Scanned>,
    pending: HashSet<FileKey>,
    budget: Arc<Mutex<Option<Arc<CpuBudget>>>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl ScanJobs {
    /// Build one worker; it never owns or borrows a libbpf object.
    pub(super) fn new() -> Result<Self> {
        let (requests, jobs) = crossbeam_channel::bounded::<Job>(MAX_SCANS);
        let (finished, results) = crossbeam_channel::bounded(MAX_SCANS);
        let budget = Arc::new(Mutex::new(None::<Arc<CpuBudget>>));
        let worker_budget = Arc::clone(&budget);
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let worker = thread::Builder::new()
            .name("ssl-scan".to_owned())
            .spawn(move || {
                while let Ok(job) = jobs.recv() {
                    if worker_stop.load(Ordering::Relaxed) {
                        break;
                    }
                    let budget = worker_budget
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .clone();
                    let path = format!("/proc/self/fd/{}", job.file.as_raw_fd());
                    let offsets =
                        find_static_ssl_offsets_paced(&path, budget.as_deref(), &worker_stop);
                    if worker_stop.load(Ordering::Relaxed) {
                        break;
                    }
                    let unchanged = job.file.metadata().is_ok_and(|m| {
                        (stat_dev_to_key(m.dev()), m.ino()) == job.key
                            && (m.len(), m.mtime(), m.mtime_nsec()) == job.revision
                    });
                    if finished
                        .send(Scanned {
                            key: job.key,
                            file: job.file,
                            offsets,
                            unchanged,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .context("start bounded SSL scan worker")?;
        Ok(Self {
            requests: Some(requests),
            results,
            pending: HashSet::new(),
            budget,
            stop,
            worker: Some(worker),
        })
    }

    /// Share the runtime's process CPU accounting with each executable window.
    pub(super) fn set_budget(&self, budget: Option<Arc<CpuBudget>>) {
        *self
            .budget
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = budget;
    }

    /// Repeated discovery of one inode consumes one queue entry.
    pub(super) fn submit(&mut self, path: &str) -> std::result::Result<(), SubmitError> {
        let file = File::open(path)
            .with_context(|| format!("open SSL scan input {path}"))
            .map_err(SubmitError::Failed)?;
        let metadata = file
            .metadata()
            .with_context(|| format!("stat SSL scan input {path}"))
            .map_err(SubmitError::Failed)?;
        let key = (stat_dev_to_key(metadata.dev()), metadata.ino());
        if self.pending.contains(&key) {
            return Ok(());
        }
        if self.pending.len() >= MAX_SCANS {
            return Err(SubmitError::QueueFull);
        }
        let job = Job {
            key,
            file,
            revision: (metadata.len(), metadata.mtime(), metadata.mtime_nsec()),
        };
        self.requests
            .as_ref()
            .context("SSL scan worker stopped")
            .map_err(SubmitError::Failed)?
            .try_send(job)
            .map_err(|error| match error {
                crossbeam_channel::TrySendError::Full(_) => SubmitError::QueueFull,
                crossbeam_channel::TrySendError::Disconnected(_) => SubmitError::Failed(
                    anyhow::anyhow!("enqueue SSL scan for {path}: worker disconnected"),
                ),
            })?;
        self.pending.insert(key);
        metrics::gauge!("agentsight_ssl_scan_jobs").set(self.pending.len() as f64);
        Ok(())
    }

    /// Polling never waits for file I/O or worker CPU admission.
    pub(super) fn try_recv(&mut self) -> Option<Scanned> {
        let result = self.results.try_recv().ok()?;
        self.pending.remove(&result.key);
        metrics::gauge!("agentsight_ssl_scan_jobs").set(self.pending.len() as f64);
        Some(result)
    }
}

impl Drop for ScanJobs {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.requests.take();
        if let Some(worker) = self.worker.take() {
            if worker.join().is_err() {
                log::error!("SSL scan worker panicked");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::cpu_budget::WorkClass;
    use std::time::{Duration, Instant};

    fn receive(jobs: &mut ScanJobs) -> Scanned {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(result) = jobs.try_recv() {
                return result;
            }
            assert!(Instant::now() < deadline, "scan worker did not finish");
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn duplicate_inodes_and_unread_results_share_one_bound() {
        let budget = Arc::new(CpuBudget::new(25).unwrap());
        let _permit = budget.acquire(WorkClass::Capture).unwrap();
        let mut jobs = ScanJobs::new().unwrap();
        jobs.set_budget(Some(Arc::clone(&budget)));
        let files: Vec<_> = (0..=MAX_SCANS)
            .map(|_| tempfile::NamedTempFile::new().unwrap())
            .collect();
        for file in &files[..MAX_SCANS] {
            jobs.submit(file.path().to_str().unwrap()).unwrap();
            jobs.submit(file.path().to_str().unwrap()).unwrap();
        }
        assert_eq!(jobs.pending.len(), MAX_SCANS);
        assert!(matches!(
            jobs.submit(files[MAX_SCANS].path().to_str().unwrap()),
            Err(SubmitError::QueueFull)
        ));
        // Dropping cancels admission while another thread still owns the permit.
        drop(jobs);
    }

    #[test]
    fn an_unlinked_input_stays_available_until_result_is_applied() {
        let budget = Arc::new(CpuBudget::new(25).unwrap());
        let permit = budget.acquire(WorkClass::Capture).unwrap();
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), b"non-SSL executable").unwrap();
        let mut jobs = ScanJobs::new().unwrap();
        jobs.set_budget(Some(Arc::clone(&budget)));
        jobs.submit(file.path().to_str().unwrap()).unwrap();
        std::fs::remove_file(file.path()).unwrap();
        drop(permit);
        let result = receive(&mut jobs);
        assert!(result.unchanged);
        assert!(result.offsets.is_none());
        assert_eq!(std::fs::read(result.path()).unwrap(), b"non-SSL executable");
        assert!(jobs.pending.is_empty());
    }

    #[test]
    fn modifying_a_queued_executable_invalidates_the_result() {
        let budget = Arc::new(CpuBudget::new(25).unwrap());
        let permit = budget.acquire(WorkClass::Capture).unwrap();
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), b"old").unwrap();
        let mut jobs = ScanJobs::new().unwrap();
        jobs.set_budget(Some(Arc::clone(&budget)));
        jobs.submit(file.path().to_str().unwrap()).unwrap();
        std::fs::write(file.path(), b"replacement bytes").unwrap();
        drop(permit);
        assert!(!receive(&mut jobs).unchanged);
    }
}
