//! One bounded worker inspects stable file descriptors without moving BPF objects.

use super::{ExecutableScan, FileKey, scan_static_ssl_executable_paced, stat_dev_to_key};
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

/// Which scanner a queued executable needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScanMode {
    /// A known static SSL binary from maps: scan its prologues directly.
    KnownStatic,
    /// An unknown exe-probe candidate: require the BoringSSL marker prefilter.
    BoringSslCandidate,
}

#[derive(Debug)]
pub(super) enum SubmitError {
    QueueFull,
    /// The path no longer resolves to the inode discovery recorded.
    Replaced,
    Failed(anyhow::Error),
}

struct Job {
    key: FileKey,
    file: File,
    revision: (u64, i64, i64),
    mode: ScanMode,
}

/// Retains the scanned inode until userspace installs its global uprobes.
pub(super) struct Scanned {
    pub(super) key: FileKey,
    file: File,
    revision: (u64, i64, i64),
    pub(super) outcome: ExecutableScan,
}

impl Scanned {
    /// A process exit or pathname replacement cannot invalidate this descriptor.
    pub(super) fn path(&self) -> String {
        format!("/proc/self/fd/{}", self.file.as_raw_fd())
    }

    /// Re-check the pinned descriptor immediately before installing uprobes.
    ///
    /// The worker sampled this revision before the result was queued; an
    /// in-place rewrite in that interval must discard the result instead of
    /// attaching offsets computed from the old contents.
    pub(super) fn unchanged_at_attach(&self) -> bool {
        self.file.metadata().is_ok_and(|metadata| {
            (stat_dev_to_key(metadata.dev()), metadata.ino()) == self.key
                && (metadata.len(), metadata.mtime(), metadata.mtime_nsec()) == self.revision
        })
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
                    let outcome = match scan_static_ssl_executable_paced(
                        &path,
                        budget.as_deref(),
                        &worker_stop,
                        job.mode,
                    ) {
                        Some(outcome) => outcome,
                        None if worker_stop.load(Ordering::Relaxed) => break,
                        None => ExecutableScan::Failed,
                    };
                    if worker_stop.load(Ordering::Relaxed) {
                        break;
                    }
                    if finished
                        .send(Scanned {
                            key: job.key,
                            file: job.file,
                            revision: job.revision,
                            outcome,
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

    /// Whether the bounded in-flight set has room for another job.
    pub(super) fn has_capacity(&self) -> bool {
        self.pending.len() < MAX_SCANS
    }

    /// Whether `key` is queued or awaiting result consumption.
    pub(super) fn is_pending(&self, key: FileKey) -> bool {
        self.pending.contains(&key)
    }

    /// Enqueue one executable unless `expected` is already in flight.
    ///
    /// Discovery records the inode before submission. Re-derive the key from
    /// the opened descriptor and reject a path that was replaced in between,
    /// so `pending` and the caller's bookkeeping always describe one inode.
    pub(super) fn submit(
        &mut self,
        path: &str,
        expected: FileKey,
        mode: ScanMode,
    ) -> std::result::Result<(), SubmitError> {
        if self.pending.contains(&expected) {
            return Ok(());
        }
        // Check capacity before opening: the runtime retry scheduler polls this
        // path from the event thread and must not touch the filesystem while
        // the worker queue is saturated.
        if self.pending.len() >= MAX_SCANS {
            return Err(SubmitError::QueueFull);
        }
        let file = File::open(path)
            .with_context(|| format!("open SSL scan input {path}"))
            .map_err(SubmitError::Failed)?;
        let metadata = file
            .metadata()
            .with_context(|| format!("stat SSL scan input {path}"))
            .map_err(SubmitError::Failed)?;
        let key = (stat_dev_to_key(metadata.dev()), metadata.ino());
        if key != expected {
            return Err(SubmitError::Replaced);
        }
        let job = Job {
            key,
            file,
            revision: (metadata.len(), metadata.mtime(), metadata.mtime_nsec()),
            mode,
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
    use super::super::ExecutableScan;
    use super::*;
    use crate::utils::cpu_budget::WorkClass;
    use std::path::Path;
    use std::time::{Duration, Instant};

    fn file_key(path: &Path) -> FileKey {
        let metadata = std::fs::metadata(path).expect("stat fixture");
        (stat_dev_to_key(metadata.dev()), metadata.ino())
    }

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
            let path = file.path().to_str().unwrap();
            let key = file_key(file.path());
            jobs.submit(path, key, ScanMode::KnownStatic).unwrap();
            jobs.submit(path, key, ScanMode::KnownStatic).unwrap();
        }
        assert_eq!(jobs.pending.len(), MAX_SCANS);
        let overflow = file_key(files[MAX_SCANS].path());
        assert!(matches!(
            jobs.submit(
                files[MAX_SCANS].path().to_str().unwrap(),
                overflow,
                ScanMode::KnownStatic,
            ),
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
        let path = file.path().to_str().unwrap().to_owned();
        let key = file_key(file.path());
        let mut jobs = ScanJobs::new().unwrap();
        jobs.set_budget(Some(Arc::clone(&budget)));
        jobs.submit(&path, key, ScanMode::BoringSslCandidate)
            .unwrap();
        std::fs::remove_file(file.path()).unwrap();
        drop(permit);
        let result = receive(&mut jobs);
        assert!(result.unchanged_at_attach());
        assert!(matches!(result.outcome, ExecutableScan::NoMarkers));
        assert_eq!(std::fs::read(result.path()).unwrap(), b"non-SSL executable");
        assert!(jobs.pending.is_empty());
    }

    #[test]
    fn modifying_a_queued_executable_invalidates_the_result() {
        let budget = Arc::new(CpuBudget::new(25).unwrap());
        let permit = budget.acquire(WorkClass::Capture).unwrap();
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), b"old").unwrap();
        let key = file_key(file.path());
        let mut jobs = ScanJobs::new().unwrap();
        jobs.set_budget(Some(Arc::clone(&budget)));
        jobs.submit(file.path().to_str().unwrap(), key, ScanMode::KnownStatic)
            .unwrap();
        std::fs::write(file.path(), b"replacement bytes").unwrap();
        drop(permit);
        assert!(!receive(&mut jobs).unchanged_at_attach());
    }

    #[test]
    fn submit_rejects_a_path_replaced_after_discovery() {
        let first = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(first.path(), b"first").unwrap();
        let expected = file_key(first.path());
        let replacement = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(replacement.path(), b"second").unwrap();
        std::fs::rename(replacement.path(), first.path()).unwrap();

        let mut jobs = ScanJobs::new().unwrap();
        assert!(matches!(
            jobs.submit(
                first.path().to_str().unwrap(),
                expected,
                ScanMode::KnownStatic,
            ),
            Err(SubmitError::Replaced)
        ));
        assert!(jobs.pending.is_empty());
    }

    #[test]
    fn a_consumed_result_frees_capacity_for_the_next_submit() {
        let files: Vec<_> = (0..=MAX_SCANS)
            .map(|_| tempfile::NamedTempFile::new().unwrap())
            .collect();
        let mut jobs = ScanJobs::new().unwrap();
        for file in &files[..MAX_SCANS] {
            jobs.submit(
                file.path().to_str().unwrap(),
                file_key(file.path()),
                ScanMode::KnownStatic,
            )
            .unwrap();
        }
        assert!(!jobs.has_capacity());
        let _ = receive(&mut jobs);
        assert!(jobs.has_capacity());
        jobs.submit(
            files[MAX_SCANS].path().to_str().unwrap(),
            file_key(files[MAX_SCANS].path()),
            ScanMode::KnownStatic,
        )
        .unwrap();
    }

    #[test]
    fn submit_checks_capacity_before_opening_the_path() {
        let mut jobs = ScanJobs::new().unwrap();
        let files: Vec<_> = (0..MAX_SCANS)
            .map(|_| tempfile::NamedTempFile::new().unwrap())
            .collect();
        for file in &files {
            let path = file.path().to_str().unwrap();
            jobs.submit(path, file_key(file.path()), ScanMode::KnownStatic)
                .unwrap();
        }
        assert!(matches!(
            jobs.submit("/nonexistent/ssl-scan-input", (0, 0), ScanMode::KnownStatic),
            Err(SubmitError::QueueFull)
        ));
    }

    #[test]
    fn pending_duplicate_returns_before_opening_the_path() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let key = file_key(file.path());
        let mut jobs = ScanJobs::new().unwrap();
        jobs.submit(file.path().to_str().unwrap(), key, ScanMode::KnownStatic)
            .unwrap();
        assert!(
            jobs.submit("/nonexistent/ssl-scan-input", key, ScanMode::KnownStatic)
                .is_ok()
        );
    }
}
