//! Linux procfs adapter and Scope-owned discovery workers.

#[cfg(test)]
include!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../fixtures/policy.rs"
));

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use asc_daemon_core::scope_discovery::{
    ProcessFingerprint, ProcessObservation, ScopeDiscoveryState,
};
use asc_pap::PapError;
use asc_policy_types::identifiers::ResourceId;
use asc_policy_types::process_discovery::ProcessIdentity;
#[cfg(test)]
use asc_policy_types::scope::PreparedScope;

const SCAN_INTERVAL: Duration = Duration::from_secs(2);
const MAX_DISCOVERY_JOBS: usize = 32;

/// Bounded owner of discovery jobs created through Scope admission.
pub struct ScopeDiscoveryRegistry {
    // None closes admission before workers are taken out for joining.
    jobs: Mutex<Option<Vec<ScopeDiscoveryJob>>>,
    sink: Arc<dyn asc_pap::ScopeBindingSink>,
}

impl ScopeDiscoveryRegistry {
    /// Creates an empty registry; Scope creation is the only job admission path.
    pub fn new(sink: Arc<dyn asc_pap::ScopeBindingSink>) -> Self {
        Self {
            jobs: Mutex::new(Some(Vec::new())),
            sink,
        }
    }

    /// Closes creation before cancelling and joining all owned workers.
    ///
    /// # Errors
    /// Reports a poisoned registry or worker panic, after attempting every join.
    pub fn shutdown(&self) -> io::Result<()> {
        let jobs = self
            .jobs
            .lock()
            .map_err(|_| io::Error::other("scope discovery registry poisoned"))?
            .take();
        let mut failure = None;
        for mut job in jobs.into_iter().flatten() {
            if let Err(error) = job.shutdown() {
                failure = Some(error);
            }
        }
        failure.map_or(Ok(()), Err)
    }
}

impl asc_pap::ScopeDiscovery for ScopeDiscoveryRegistry {
    fn start(&self, scope: &asc_pap::ScopeDiscoverySeed) -> Result<(), PapError> {
        let mut guard = self.jobs.lock().map_err(|_| PapError::Unavailable)?;
        let jobs = guard.as_mut().ok_or(PapError::Unavailable)?;
        if jobs.len() >= MAX_DISCOVERY_JOBS {
            return Err(PapError::Unavailable);
        }
        if jobs.iter().any(|job| job.scope_id == scope.scope_id) {
            return Err(PapError::Conflict);
        }
        let state =
            ScopeDiscoveryState::from_seed(scope.clone()).map_err(|_| PapError::Conflict)?;
        let job = ScopeDiscoveryJob::spawn(
            scope.scope_id.clone(),
            state,
            PathBuf::from("/proc"),
            SCAN_INTERVAL,
            self.sink.clone(),
        )
        .map_err(|_| PapError::Unavailable)?;
        jobs.push(job);
        Ok(())
    }

    fn stop(&self, id: &ResourceId) -> Result<(), PapError> {
        let mut guard = self.jobs.lock().map_err(|_| PapError::Unavailable)?;
        let jobs = guard.as_mut().ok_or(PapError::Unavailable)?;
        if let Some(index) = jobs.iter().position(|job| &job.scope_id == id) {
            jobs.remove(index)
                .shutdown()
                .map_err(|_| PapError::Persistence)?;
        }
        Ok(())
    }
}

/// An owned discovery job. Dropping it cancels and joins its worker.
pub struct ScopeDiscoveryJob {
    scope_id: ResourceId,
    stop: Arc<AtomicBool>,
    state: Arc<Mutex<ScopeDiscoveryState>>,
    worker: Option<JoinHandle<()>>,
}

impl ScopeDiscoveryJob {
    fn spawn(
        scope_id: ResourceId,
        state: ScopeDiscoveryState,
        root: PathBuf,
        interval: Duration,
        sink: Arc<dyn asc_pap::ScopeBindingSink>,
    ) -> Result<Self, PapError> {
        let state = Arc::new(Mutex::new(state));
        let boot_id = fs::read_to_string(root.join("sys/kernel/random/boot_id"))
            .map_err(|_| PapError::Unavailable)?;
        if boot_id.trim().is_empty() {
            return Err(PapError::Unavailable);
        }
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let worker_state = state.clone();
        let worker_scope_id = scope_id.clone();
        let worker = thread::Builder::new().name("scope-discovery".to_owned()).spawn(move || {
            let mut degraded = false;
            while !worker_stop.load(Ordering::Acquire) {
                let run_span = tracing::info_span!("scope_discovery.scan", scope_id = %worker_scope_id);
                let entered = run_span.enter();
                let scan = scan_proc(&root, boot_id.trim(), &worker_stop);
                if worker_stop.load(Ordering::Acquire) { break; }
                match scan {
                    Ok(scan) => {
                        let Ok(mut state) = worker_state.lock() else {
                            tracing::error!(target: "asc_process_diagnostic", scope_id = %worker_scope_id, "scope discovery state poisoned; worker stopped");
                            break;
                        };
                        let result = state.reconcile(scan.observations, &scan.present, scan.complete);
                        if result.instances_selected > 0 {
                            tracing::info!(target: "asc_process_diagnostic", scope_id = %worker_scope_id,
                                instances_selected = result.instances_selected,
                                "process discovery selected new instances");
                        }
                        let instances: Vec<_> = state.instances().cloned().collect();
                        let submission_failed = match sink.sync_instances(&worker_scope_id, &instances) {
                            Ok(()) => false,
                            Err(PapError::OperationInProgress | PapError::NotFound) => break,
                            Err(error) => {
                                tracing::warn!(target: "asc_process_diagnostic", scope_id = %worker_scope_id, %error,
                                    "scope instance admission failed; retrying on next scan");
                                true
                            }
                        };
                        let current = !scan.complete || scan.unreadable > 0 || submission_failed;
                        if current != degraded {
                            tracing::warn!(target: "asc_process_diagnostic", scope_id = %worker_scope_id,
                                degraded = current, unreadable = scan.unreadable,
                                "scope discovery health changed");
                        }
                        degraded = current;
                    }
                    Err(error) => {
                        if !degraded {
                            tracing::warn!(target: "asc_process_diagnostic", scope_id = %worker_scope_id, %error,
                                "scope discovery scan failed; preserving cached processes and retrying");
                        }
                        degraded = true;
                    }
                }
                drop(entered);
                drop(run_span);
                thread::park_timeout(interval);
            }
        }).map_err(|_| PapError::Unavailable)?;
        Ok(Self {
            scope_id,
            stop,
            state,
            worker: Some(worker),
        })
    }

    /// Copies cached selections; query PAP for admitted Bindings and their status.
    ///
    /// # Errors
    /// Returns an error if a worker panicked while holding its state lock.
    pub fn instances(&self) -> io::Result<Vec<ProcessIdentity>> {
        let state = self
            .state
            .lock()
            .map_err(|_| io::Error::other("scope discovery state poisoned"))?;
        Ok(state.instances().cloned().collect())
    }

    /// Cancels and joins the worker, reporting any worker panic.
    ///
    /// # Errors
    /// Returns an error if the worker panicked.
    pub fn shutdown(&mut self) -> io::Result<()> {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.thread().unpark();
            worker
                .join()
                .map_err(|_| io::Error::other("scope discovery worker panicked"))?;
        }
        Ok(())
    }
}

impl Drop for ScopeDiscoveryJob {
    fn drop(&mut self) {
        if let Err(error) = self.shutdown() {
            tracing::error!(target: "asc_process_diagnostic", %error, "scope discovery shutdown failed");
        }
    }
}

struct ProcScan {
    observations: Vec<ProcessObservation>,
    present: BTreeSet<u32>,
    complete: bool,
    unreadable: usize,
}

fn scan_proc(root: &Path, boot_id: &str, stop: &AtomicBool) -> io::Result<ProcScan> {
    let mut scan = ProcScan {
        observations: Vec::new(),
        present: BTreeSet::new(),
        complete: true,
        unreadable: 0,
    };
    for entry in fs::read_dir(root)? {
        if stop.load(Ordering::Acquire) {
            scan.complete = false;
            break;
        }
        let Ok(entry) = entry else {
            scan.complete = false;
            continue;
        };
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
            .filter(|pid| *pid > 0)
        else {
            continue;
        };
        scan.present.insert(pid);
        match observe(root, boot_id, pid) {
            Ok(observation) => scan.observations.push(observation),
            // Zombies, kernel threads and processes exiting during enumeration
            // legitimately have no exe link. Keep their last state until absent.
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(_) => scan.unreadable += 1,
        }
    }
    Ok(scan)
}

fn observe(root: &Path, boot_id: &str, pid: u32) -> io::Result<ProcessObservation> {
    let process = root.join(pid.to_string());
    let identity = read_identity(&process, boot_id, pid)?;
    let fingerprint = read_fingerprint(&process)?;
    if identity != read_identity(&process, boot_id, pid)?
        || fingerprint != read_fingerprint(&process)?
    {
        return Err(io::Error::other(
            "process changed during observation; retry next scan",
        ));
    }
    Ok(ProcessObservation {
        identity,
        fingerprint,
    })
}

fn read_identity(process: &Path, boot_id: &str, pid: u32) -> io::Result<ProcessIdentity> {
    let stat = fs::read_to_string(process.join("stat"))?;
    let end = stat
        .rfind(')')
        .ok_or_else(|| io::Error::other("invalid proc stat"))?;
    let start_time = stat[end + 1..]
        .split_whitespace()
        .nth(19)
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|time| *time > 0)
        .ok_or_else(|| io::Error::other("invalid proc start time"))?;
    Ok(ProcessIdentity {
        boot_id: boot_id.to_owned(),
        pid,
        pid_namespace: fs::read_link(process.join("ns/pid"))?
            .to_string_lossy()
            .into_owned(),
        start_time,
    })
}

fn read_fingerprint(process: &Path) -> io::Result<ProcessFingerprint> {
    let exe = process.join("exe");
    let metadata = fs::metadata(&exe)?;
    Ok(ProcessFingerprint {
        executable: fs::read_link(exe)?,
        process_name: fs::read_to_string(process.join("comm"))?
            .trim_end_matches('\n')
            .to_owned(),
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;
    use std::process::{Child, Command};
    use std::time::Instant;

    use asc_pap::PapService;
    use asc_pap_repository_memory::ProcessLocalPapRepository;

    use super::*;
    use asc_policy_types::policy::PreparedPolicy;

    fn policies() -> (Arc<ProcessLocalPapRepository>, PreparedPolicy) {
        let repository = Arc::new(ProcessLocalPapRepository::default());
        let pap = PapService::new(repository.clone());
        let policy = pap
            .create_policy("test", &file_policy(vec!["/protected".to_owned()]))
            .unwrap();
        (repository, policy)
    }

    struct Process(Child);
    impl Drop for Process {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn wait_for_instances(job: &ScopeDiscoveryJob, expected: usize) -> Vec<ProcessIdentity> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let instances = job.instances().unwrap();
            if instances.len() == expected {
                return instances;
            }
            assert!(
                Instant::now() < deadline,
                "expected {expected} instances, got {}",
                instances.len()
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    struct TestSink;
    impl asc_pap::ScopeBindingSink for TestSink {
        fn sync_instances(&self, _: &ResourceId, _: &[ProcessIdentity]) -> Result<(), PapError> {
            Ok(())
        }
    }

    #[test]
    fn discovery_selects_instances_and_pap_expands_policies_for_each_scope() {
        use asc_policy_types::scope::{PolicyReference, ProcessMatcher, ScopeSelector};
        let (repository, policy) = policies();
        let pap = PapService::new(repository);
        let registry = Arc::new(ScopeDiscoveryRegistry::new(Arc::new(pap.clone())));
        let pap = pap.with_scope_discovery(registry.clone());
        let second_policy = pap
            .create_policy("second", &file_policy(vec!["/other".to_owned()]))
            .unwrap();
        let refs = [policy.clone(), second_policy.clone()]
            .iter()
            .map(|policy| PolicyReference {
                policy_id: policy.policy_id.clone(),
                policy_revision: policy.revision,
            })
            .collect::<Vec<_>>();
        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("scope-agent");
        fs::copy("/bin/sleep", &executable).unwrap();
        let first = Process(Command::new(&executable).arg("30").spawn().unwrap());
        let selector = ScopeSelector::Process {
            matcher: ProcessMatcher::Executable {
                executable: executable.to_str().unwrap().to_owned(),
            },
        };
        assert!(pap.create_scope_assignment(&selector, &[]).is_err());
        let scope = pap.create_scope_assignment(&selector, &refs).unwrap();
        let other = pap.create_scope_assignment(&selector, &refs[..1]).unwrap();
        let second = Process(Command::new(&executable).arg("30").spawn().unwrap());
        {
            let guard = registry.jobs.lock().unwrap();
            let jobs = guard.as_ref().unwrap();
            let instances = wait_for_instances(&jobs[0], 2);
            assert_eq!(
                instances
                    .iter()
                    .map(|instance| instance.pid)
                    .collect::<BTreeSet<_>>(),
                BTreeSet::from([first.0.id(), second.0.id()])
            );
            assert_eq!(wait_for_instances(&jobs[1], 2), instances);
            let bindings = pap.list_bindings(100, 0).unwrap().items;
            assert_eq!(bindings.len(), 6);
            for (assignment, policies) in [
                (&scope, vec![policy.clone(), second_policy.clone()]),
                (&other, vec![policy.clone()]),
            ] {
                for instance in &instances {
                    let mut found = bindings
                        .iter()
                        .filter(|binding| {
                            binding.spec.scope.scope_id == assignment.scope_id
                                && &binding.spec.scope.process == instance
                        })
                        .map(|binding| binding.spec.policy.clone())
                        .collect::<Vec<_>>();
                    let mut expected = policies.clone();
                    found.sort_by(|a, b| a.policy_id.cmp(&b.policy_id));
                    expected.sort_by(|a, b| a.policy_id.cmp(&b.policy_id));
                    assert_eq!(found, expected);
                }
            }
            thread::sleep(Duration::from_millis(100));
            assert_eq!(jobs[0].instances().unwrap(), instances);
            assert_eq!(pap.list_bindings(100, 0).unwrap().items, bindings);
        }
        pap.delete_policy_revision(&policy.policy_id, policy.revision)
            .unwrap();
        assert_eq!(
            pap.get_scope(&scope.scope_id).unwrap().policy_snapshots,
            vec![policy.clone(), second_policy.clone()]
        );
        pap.delete_scope(&scope.scope_id).unwrap();
        {
            let guard = registry.jobs.lock().unwrap();
            assert_eq!(guard.as_ref().unwrap().len(), 1);
            assert_eq!(&guard.as_ref().unwrap()[0].scope_id, &other.scope_id);
        }
        pap.delete_scope(&other.scope_id).unwrap();
        registry.shutdown().unwrap();
        assert!(matches!(
            pap.create_scope_assignment(&selector, &refs[1..]),
            Err(PapError::Unavailable)
        ));
        assert_eq!(pap.list_scopes(100, 0).unwrap().total, 2);
    }

    #[test]
    fn djob_scope_capacity_releases_on_delete_and_shutdown_closes_admission() {
        let (repository, policy) = policies();
        let pap = PapService::new(repository);
        let registry = Arc::new(ScopeDiscoveryRegistry::new(Arc::new(pap.clone())));
        let pap = pap.with_scope_discovery(registry.clone());
        let scope: PreparedScope = serde_json::from_value(serde_json::json!({
            "scopeId":"capacity", "status":"ACTIVE",
            "selector":{"kind":"process","match":{"executable":"/no-such-scope-agent"}},
            "policySnapshots":[policy]
        }))
        .unwrap();
        let mut scopes = Vec::new();
        for _ in 0..MAX_DISCOVERY_JOBS {
            scopes.push(
                pap.create_scope_assignment(
                    &scope.selector,
                    &[asc_policy_types::scope::PolicyReference {
                        policy_id: policy.policy_id.clone(),
                        policy_revision: policy.revision,
                    }],
                )
                .unwrap(),
            );
        }
        assert!(matches!(
            pap.create_scope_assignment(
                &scope.selector,
                &[asc_policy_types::scope::PolicyReference {
                    policy_id: policy.policy_id.clone(),
                    policy_revision: policy.revision
                }]
            ),
            Err(PapError::Unavailable)
        ));
        assert_eq!(
            pap.list_scopes(100, 0).unwrap().total,
            MAX_DISCOVERY_JOBS as u64
        );
        let removed = scopes.pop().unwrap();
        pap.delete_scope(&removed.scope_id).unwrap();
        let replacement = pap
            .create_scope_assignment(
                &scope.selector,
                &[asc_policy_types::scope::PolicyReference {
                    policy_id: policy.policy_id.clone(),
                    policy_revision: policy.revision,
                }],
            )
            .unwrap();
        assert_ne!(replacement.scope_id, removed.scope_id);
        registry.shutdown().unwrap();
        assert!(matches!(
            pap.create_scope_assignment(
                &scope.selector,
                &[asc_policy_types::scope::PolicyReference {
                    policy_id: policy.policy_id.clone(),
                    policy_revision: policy.revision
                }]
            ),
            Err(PapError::Unavailable)
        ));
    }

    #[test]
    fn djob_scope_discovers_existing_and_future_processes_and_joins_on_stop() {
        let directory = tempfile::tempdir().unwrap();
        let name = format!(
            "scope-test-{}",
            directory.path().file_name().unwrap().to_string_lossy()
        );
        let executable = directory.path().join(&name);
        fs::copy("/bin/sleep", &executable).unwrap();
        let first = Process(Command::new(&executable).arg("30").spawn().unwrap());
        let policy = policies().1;
        let scope: PreparedScope = serde_json::from_value(serde_json::json!({
            "scopeId":"lifecycle", "status":"ACTIVE",
            "selector":{"kind":"process","match":{"executable":executable}},
            "policySnapshots":[policy]
        }))
        .unwrap();
        let state = ScopeDiscoveryState::for_scope(scope.clone()).unwrap();
        let mut job = ScopeDiscoveryJob::spawn(
            scope.scope_id,
            state,
            "/proc".into(),
            Duration::from_millis(20),
            Arc::new(TestSink),
        )
        .unwrap();
        let instances = wait_for_instances(&job, 1);
        assert_eq!(instances[0].pid, first.0.id());
        assert!(instances[0].start_time > 0);
        assert!(instances[0].pid_namespace.starts_with("pid:["));
        let second = Process(Command::new(&executable).arg("30").spawn().unwrap());
        let instances = wait_for_instances(&job, 2);
        assert!(
            instances
                .iter()
                .any(|instance| instance.pid == second.0.id())
        );
        thread::sleep(Duration::from_millis(100));
        assert_eq!(job.instances().unwrap(), instances);
        drop(first);
        wait_for_instances(&job, 1);
        job.shutdown().unwrap();
        drop(second);
        // No scans may run after shutdown returns.
        assert_eq!(job.instances().unwrap().len(), 1);
    }

    #[test]
    fn djob_scope_proc_adapter_preserves_failed_reads_and_rejects_malformed_stat() {
        let directory = tempfile::tempdir().unwrap();
        let process = directory.path().join("42");
        fs::create_dir_all(process.join("ns")).unwrap();
        symlink("pid:[123]", process.join("ns/pid")).unwrap();
        symlink("/bin/sleep", process.join("exe")).unwrap();
        fs::write(
            process.join("stat"),
            "42 (agent (worker)) S 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 987 20",
        )
        .unwrap();
        fs::write(process.join("comm"), "agent\n").unwrap();
        let stop = AtomicBool::new(false);
        let scan = scan_proc(directory.path(), "boot", &stop).unwrap();
        assert_eq!(scan.observations[0].identity.start_time, 987);
        fs::write(process.join("stat"), "malformed").unwrap();
        let scan = scan_proc(directory.path(), "boot", &stop).unwrap();
        assert!(scan.complete);
        assert_eq!(scan.present, BTreeSet::from([42]));
        assert!(scan.observations.is_empty());
        assert_eq!(scan.unreadable, 1);
        assert!(scan_proc(&directory.path().join("missing"), "boot", &stop).is_err());
        stop.store(true, Ordering::Release);
        assert!(!scan_proc(directory.path(), "boot", &stop).unwrap().complete);
    }
}
