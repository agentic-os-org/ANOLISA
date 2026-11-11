use std::collections::BTreeMap;
use std::path::Path;
#[cfg(not(test))]
use std::process::Command;
use std::time::Duration;
#[cfg(not(test))]
use std::time::Instant;

use serde::{Deserialize, Serialize};

use super::{HealthSuppressionStore, SuppressionDecision};
use crate::config::private_state;
use crate::diagnostics::health::{HealthFinding, HealthScanReport};
#[cfg(not(test))]
use crate::process::BoundedWorker;
use crate::process::{read_worker_request, write_worker_reply};

pub(crate) const WORKER_FLAG: &str = "--__claim-recommendation-v1";
const LIMIT: usize = 4096;
const BUDGET: Duration = Duration::from_millis(100);

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Candidate {
    pub(crate) id: String,
    pub(crate) finding: HealthFinding,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GuideToken {
    generation: String,
    runtime: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    guide: Option<GuideToken>,
    host: Option<String>,
    metrics: BTreeMap<String, f64>,
    candidates: Vec<Candidate>,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Claim {
    pub(crate) selected: String,
    pub(crate) eligible: Vec<String>,
}

pub(crate) fn claim(
    report: &HealthScanReport,
    candidates: Vec<Candidate>,
    generation: &str,
    runtime: bool,
) -> Option<Claim> {
    if candidates.is_empty() {
        return None;
    }
    let request = Request {
        guide: Some(GuideToken {
            generation: generation.into(),
            runtime,
        }),
        host: report.host.clone(),
        metrics: super::metrics_for_report(report),
        candidates,
    };
    #[cfg(test)]
    {
        super::health_suppression_store_path()
            .and_then(|path| claim_at(&path, &request, super::current_time_ms()))
    }
    #[cfg(not(test))]
    {
        let bytes = serde_json::to_vec(&request).ok()?;
        if bytes.len() > LIMIT {
            return None;
        }
        let mut command = Command::new(std::env::current_exe().ok()?);
        command.args(["health", WORKER_FLAG]);
        let deadline = Instant::now() + BUDGET;
        let mut worker = BoundedWorker::spawn(command, &bytes, deadline, LIMIT).ok()?;
        loop {
            if let Some(result) = worker.poll_ready() {
                return serde_json::from_slice::<Option<Claim>>(&result.ok()?)
                    .ok()?
                    .filter(|claim| {
                        claim.eligible.contains(&claim.selected)
                            && claim.eligible.len() <= request.candidates.len()
                            && claim.eligible.iter().all(|id| {
                                request
                                    .candidates
                                    .iter()
                                    .any(|candidate| &candidate.id == id)
                            })
                    });
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

pub(crate) fn run_worker() -> i32 {
    let Ok(bytes) = read_worker_request(LIMIT, BUDGET) else {
        return 1;
    };
    let Ok(request) = serde_json::from_slice::<Request>(&bytes) else {
        return 1;
    };
    if request.guide.is_none() {
        return 1;
    }
    let result = super::health_suppression_store_path()
        .and_then(|path| claim_at(&path, &request, super::current_time_ms()));
    let Ok(bytes) = serde_json::to_vec(&result) else {
        return 1;
    };
    i32::from(write_worker_reply(&bytes, LIMIT).is_err())
}

fn guide_current(token: &GuideToken) -> bool {
    let Ok((store, runtime)) = crate::config::guide::open_optional_user_state() else {
        return false;
    };
    if runtime.is_some() != token.runtime
        || runtime
            .as_ref()
            .is_some_and(|runtime| !matches!(runtime.read(),Ok(Some(state)) if state.enabled))
    {
        return false;
    }
    store.snapshot(runtime.as_ref()).is_ok_and(|snapshot| {
        snapshot.preferences.guide_enabled && snapshot.generation == token.generation
    })
}

fn claim_at(path: &Path, request: &Request, now_ms: u64) -> Option<Claim> {
    if request
        .guide
        .as_ref()
        .is_some_and(|token| !guide_current(token))
    {
        return None;
    }
    if request.candidates.len() > 16 || request.metrics.len() > 128 {
        return None;
    }
    let parent = path.parent()?;
    if private_state::ensure_directory(parent).is_err() {
        let expected = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)?
            .join(".copilot-shell/cosh");
        if parent != expected || std::env::var_os("COSH_SHELL_HEALTH_SUPPRESSION_STORE").is_some() {
            return None;
        }
        private_state::tighten_legacy_directory(parent).ok()?;
    }
    let mut lock_path = path.as_os_str().to_os_string();
    lock_path.push(".lock");
    let _lock = private_state::LockGuard::try_acquire(Path::new(&lock_path)).ok()?;
    let mut store = HealthSuppressionStore::load_from_path(path);
    let mut report = HealthScanReport::new("claim", 0);
    report.host = request.host.clone();
    let host = super::host_id_for_report(&report, super::health_machine_id().as_deref());
    let eligible = request
        .candidates
        .iter()
        .filter(|candidate| {
            super::valid_field(&candidate.id)
                && super::valid_field(&super::finding_fingerprint(&candidate.finding))
                && store.should_show(&host, &candidate.finding, &request.metrics, now_ms)
                    == SuppressionDecision::Show
        })
        .collect::<Vec<_>>();
    let first = *eligible.first()?;
    if request
        .guide
        .as_ref()
        .is_some_and(|token| !guide_current(token))
    {
        return None;
    }
    store
        .record_and_persist(path, &host, &first.finding, request.metrics.clone(), now_ms)
        .ok()?;
    Some(Claim {
        selected: first.id.clone(),
        eligible: eligible
            .iter()
            .map(|candidate| candidate.id.clone())
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::health::{HealthFindingCategory, HealthMessageId, HealthSeverity};
    use std::os::unix::fs::{symlink, PermissionsExt};

    fn request() -> Request {
        Request {
            guide: None,
            host: Some("host".into()),
            metrics: BTreeMap::new(),
            candidates: vec![Candidate {
                id: "memory".into(),
                finding: HealthFinding {
                    id: "J06".into(),
                    category: HealthFindingCategory::Anomaly,
                    severity: HealthSeverity::Warning,
                    title_id: HealthMessageId::HealthFindingMemoryAvailableLow,
                    detail_id: None,
                    detail_args: BTreeMap::new(),
                    evidence_fact_ids: vec![],
                    suggested_try_ids: vec![],
                },
            }],
        }
    }

    #[test]
    fn health_claim_is_persisted_before_a_second_session_can_display_it() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = dir.path().canonicalize().unwrap().join("history");
        assert_eq!(
            claim_at(&path, &request(), 1000).map(|claim| claim.selected),
            Some("memory".into())
        );
        assert_eq!(claim_at(&path, &request(), 1001), None);
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn health_claim_rejects_busy_corrupt_and_special_history_without_resetting_it() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = dir.path().canonicalize().unwrap().join("history");
        let lock =
            private_state::LockGuard::try_acquire(&path.with_file_name("history.lock")).unwrap();
        assert_eq!(claim_at(&path, &request(), 1000), None);
        assert!(!path.exists());
        drop(lock);
        std::fs::write(&path, "broken").unwrap();
        assert_eq!(claim_at(&path, &request(), 1000), None);
        assert_eq!(std::fs::read(&path).unwrap(), b"broken");
        std::fs::remove_file(&path).unwrap();
        nix::unistd::mkfifo(&path, nix::sys::stat::Mode::from_bits_truncate(0o600)).unwrap();
        assert_eq!(claim_at(&path, &request(), 1000), None);
        std::fs::remove_file(&path).unwrap();
        let target = path.with_extension("target");
        std::fs::write(&target, "").unwrap();
        symlink(&target, &path).unwrap();
        assert_eq!(claim_at(&path, &request(), 1000), None);
    }

    #[test]
    fn health_claim_preserves_legacy_readable_history_and_tightens_it_on_next_claim() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = dir.path().canonicalize().unwrap().join("history");
        assert!(claim_at(&path, &request(), 1000).is_some());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(claim_at(&path, &request(), 1001), None);
        assert!(claim_at(&path, &request(), 1000 + 4 * 60 * 60 * 1000).is_some());
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn simultaneous_health_sessions_claim_at_most_once() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = dir.path().canonicalize().unwrap().join("history");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let handles = (0..2)
            .map(|_| {
                let path = path.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    claim_at(&path, &request(), 1000)
                })
            })
            .collect::<Vec<_>>();
        assert_eq!(
            handles
                .into_iter()
                .filter_map(|h| h.join().unwrap())
                .count(),
            1
        );
    }
    #[test]
    fn legacy_default_history_keeps_cooldown_when_its_private_directory_is_migrated() {
        let _guard = crate::diagnostics::test_env::env_guard();
        struct Restore(Vec<(&'static str, Option<std::ffi::OsString>)>);
        impl Drop for Restore {
            fn drop(&mut self) {
                for (name, value) in &self.0 {
                    match value {
                        Some(value) => std::env::set_var(name, value),
                        None => std::env::remove_var(name),
                    }
                }
            }
        }
        let _restore = Restore(
            ["HOME", "COSH_SHELL_HEALTH_SUPPRESSION_STORE"]
                .into_iter()
                .map(|name| (name, std::env::var_os(name)))
                .collect(),
        );
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().canonicalize().unwrap();
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::env::set_var("HOME", &home);
        std::env::remove_var("COSH_SHELL_HEALTH_SUPPRESSION_STORE");
        let original = home.join("original");
        assert!(claim_at(&original, &request(), 1000).is_some());
        let parent = home.join(".copilot-shell/cosh");
        std::fs::create_dir_all(&parent).unwrap();
        std::fs::set_permissions(
            parent.parent().unwrap(),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = parent.join("health-suppression");
        std::fs::copy(&original, &path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert!(claim_at(&path, &request(), 1001).is_none());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(
            std::fs::metadata(&parent).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert!(claim_at(&path, &request(), 1000 + 4 * 60 * 60 * 1000).is_some());
    }
    #[test]
    fn health_claim_custom_dot_lock_path_uses_a_distinct_lock_inode() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = dir.path().canonicalize().unwrap().join("history.lock");
        let lock = path.with_file_name("history.lock.lock");
        let _held = private_state::LockGuard::try_acquire(&lock).unwrap();
        assert!(claim_at(&path, &request(), 1000).is_none());
        assert!(!path.exists());
    }
    #[test]
    fn health_claim_rejects_guide_off_and_a_stale_preparation_generation() {
        let _guard = crate::diagnostics::test_env::env_guard();
        let home = tempfile::tempdir().unwrap();
        let root = home.path().canonicalize().unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let old = std::env::var_os("HOME");
        std::env::set_var("HOME", &root);
        let store = crate::config::guide::open_user_store().unwrap();
        let snapshot = store.snapshot(None).unwrap();
        let mut request = request();
        request.guide = Some(GuideToken {
            generation: snapshot.generation,
            runtime: false,
        });
        let path = root.join("history");
        store.set_guide(None, false).unwrap();
        let closed = claim_at(&path, &request, 1000);
        store.set_guide(None, true).unwrap();
        let stale = claim_at(&path, &request, 1001);
        match old {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        assert!(closed.is_none());
        assert!(stale.is_none());
        assert!(!path.exists());
    }
}
