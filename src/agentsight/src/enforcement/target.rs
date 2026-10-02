//! Validates process and file targets before building enforcement requests.

use std::fs;
use std::path::{Path, PathBuf};

/// Errors that prevent a process or file from being used in an enforcement policy.
#[derive(Debug, thiserror::Error)]
pub(crate) enum TargetValidationError {
    #[error("PID {0} is not an eligible Agent process")]
    ProtectedProcess(i32),
    #[error("cannot inspect PID {pid}: {source}")]
    ProcessIo { pid: i32, source: std::io::Error },
    #[error("invalid /proc/{0}/stat start time")]
    InvalidStat(i32),
    #[error("invalid policy file {path}: {message}")]
    InvalidPath { path: PathBuf, message: String },
}

/// Reads the Linux process start time after excluding protected service processes.
///
/// If `pid` is a namespace-local PID (e.g. from a container), it is first
/// resolved to the host PID via [`resolve_to_host_pid`] using `expected_start_time`
/// for disambiguation.  The returned tuple is `(host_pid, start_time)` so callers
/// can use the resolved PID for BPF map seeding.
pub(crate) fn resolve_and_read_target(
    pid: i32,
    expected_start_time: u64,
) -> Result<(i32, u64), TargetValidationError> {
    let host_pid = resolve_to_host_pid(pid, expected_start_time);
    let start_time = read_process_start_time(host_pid)?;
    Ok((host_pid, start_time))
}

/// Reads the Linux process start time after excluding protected service processes.
pub(crate) fn read_process_start_time(pid: i32) -> Result<u64, TargetValidationError> {
    if pid <= 1 || pid == std::process::id() as i32 {
        return Err(TargetValidationError::ProtectedProcess(pid));
    }

    let stat = fs::read_to_string(format!("/proc/{pid}/stat"))
        .map_err(|source| TargetValidationError::ProcessIo { pid, source })?;
    let open = stat
        .find('(')
        .ok_or(TargetValidationError::InvalidStat(pid))?;
    let close = stat
        .rfind(')')
        .filter(|close| *close > open)
        .ok_or(TargetValidationError::InvalidStat(pid))?;
    let process_name = &stat[open + 1..close];
    if matches!(
        process_name,
        "agentsight" | "agentsight-enfo" | "agentsight-enforcer"
    ) {
        return Err(TargetValidationError::ProtectedProcess(pid));
    }

    stat[close + 1..]
        .split_whitespace()
        .nth(19)
        .ok_or(TargetValidationError::InvalidStat(pid))?
        .parse()
        .map_err(|_| TargetValidationError::InvalidStat(pid))
}

/// Resolves an existing regular file that is safe to embed in the policy lexer.
pub(crate) fn canonical_policy_file(path: &Path) -> Result<PathBuf, TargetValidationError> {
    if !path.is_absolute() {
        return Err(invalid_path(path, "path must be absolute"));
    }
    validate_policy_path_text(path)?;
    let canonical = path.canonicalize().map_err(|error| {
        invalid_path(
            path,
            format!("cannot canonicalize path {}: {error}", path.display()),
        )
    })?;
    let metadata = fs::metadata(&canonical).map_err(|error| {
        invalid_path(
            &canonical,
            format!("cannot inspect path {}: {error}", canonical.display()),
        )
    })?;
    if !metadata.is_file() {
        return Err(invalid_path(
            &canonical,
            "path must identify an existing regular file",
        ));
    }
    validate_policy_path_text(&canonical)?;
    Ok(canonical)
}

fn validate_policy_path_text(path: &Path) -> Result<(), TargetValidationError> {
    let value = path
        .to_str()
        .ok_or_else(|| invalid_path(path, "path must be valid UTF-8"))?;
    if value.contains(['\0', '"', '\r', '\n']) {
        return Err(invalid_path(
            path,
            "path contains characters unsupported by the policy lexer",
        ));
    }
    Ok(())
}

fn invalid_path(path: &Path, message: impl Into<String>) -> TargetValidationError {
    TargetValidationError::InvalidPath {
        path: path.into(),
        message: message.into(),
    }
}

/// Resolve a potentially namespace-local PID to the global (init namespace)
/// kernel PID.  The enforcer must run in the init PID namespace.
///
/// 1. Fast path: `/proc/<pid>/stat` exists and `start_time` matches → host PID.
/// 2. Slow path: scan `/proc/*/status` for a process whose innermost NSpid
///    equals `input_pid` and whose start_time matches.
/// 3. Fallback: return input unchanged.
pub(crate) fn resolve_to_host_pid(input_pid: i32, input_start_time: u64) -> i32 {
    // Fast path: pid valid in host /proc and start_time matches.
    if let Ok(pst) = proc_start_time(input_pid) {
        if pst == input_start_time {
            return first_nspid(input_pid).unwrap_or(input_pid);
        }
    }

    // Slow path: scan /proc for NSpid inner match + start_time match.
    let Ok(proc_dir) = fs::read_dir("/proc") else {
        return input_pid;
    };
    for entry in proc_dir.flatten() {
        let name = entry.file_name();
        let Ok(host_pid) = name.to_string_lossy().parse::<i32>() else {
            continue;
        };
        if host_pid <= 0 {
            continue;
        }
        let nspid = read_nspid_chain(host_pid);
        if nspid.len() < 2 {
            continue;
        }
        if *nspid.last().unwrap() != input_pid {
            continue;
        }
        if let Ok(pst) = proc_start_time(host_pid) {
            if pst == input_start_time {
                return nspid[0];
            }
        }
    }
    input_pid
}

fn read_nspid_chain(pid: i32) -> Vec<i32> {
    let Ok(status) = fs::read_to_string(format!("/proc/{pid}/status")) else {
        return vec![];
    };
    parse_nspid_from_status(&status)
}

/// Parse the `NSpid:` line from `/proc/<pid>/status` content.
/// Returns empty vec if the line is absent.
fn parse_nspid_from_status(status: &str) -> Vec<i32> {
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("NSpid:") {
            return rest
                .split_whitespace()
                .filter_map(|s| s.parse::<i32>().ok())
                .collect();
        }
    }
    vec![]
}

fn first_nspid(pid: i32) -> Option<i32> {
    read_nspid_chain(pid).first().copied()
}

fn proc_start_time(pid: i32) -> Result<u64, ()> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).map_err(|_| ())?;
    parse_start_time_from_stat(&stat)
}

/// Parse process start time (field 22) from `/proc/<pid>/stat` content.
fn parse_start_time_from_stat(stat: &str) -> Result<u64, ()> {
    let after = &stat[stat.rfind(')').ok_or(())? + 2..];
    after
        .split_ascii_whitespace()
        .nth(19)
        .and_then(|s| s.parse::<u64>().ok())
        .ok_or(())
}

/// Check whether the current process is in the init PID namespace.
#[allow(dead_code)] // Used by agentsight-enforcer crate (actplane.rs has its own copy).
pub(crate) fn is_init_pid_namespace() -> bool {
    let Ok(self_ns) = fs::read_link("/proc/self/ns/pid") else {
        return false;
    };
    let Ok(init_ns) = fs::read_link("/proc/1/ns/pid") else {
        return false;
    };
    let result = self_ns == init_ns;
    if !result {
        eprintln!(
            "agentsight: not in init PID namespace (self={}, pid1={}); \
             file_delete_guard disabled",
            self_ns.display(),
            init_ns.display(),
        );
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TemporaryRegularFile {
        path: PathBuf,
    }

    impl TemporaryRegularFile {
        fn create() -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);

            let pid = std::process::id();
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();

            for _ in 0..100 {
                let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
                let path = std::env::temp_dir().join(format!(
                    "agentsight-target-{pid}-{timestamp}-{counter}.policy"
                ));
                match fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&path)
                {
                    Ok(_) => return Self { path },
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(error) => panic!("failed to create temporary policy file: {error}"),
                }
            }

            panic!("could not create a unique temporary policy file")
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TemporaryRegularFile {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.path);
        }
    }

    #[test]
    fn rejects_init_and_agentsight_processes() {
        assert!(matches!(
            read_process_start_time(1),
            Err(TargetValidationError::ProtectedProcess(_))
        ));
        assert!(matches!(
            read_process_start_time(std::process::id() as i32),
            Err(TargetValidationError::ProtectedProcess(_))
        ));
    }

    #[test]
    fn canonicalizes_an_existing_regular_file() {
        let file = TemporaryRegularFile::create();
        assert_eq!(
            canonical_policy_file(file.path()).unwrap(),
            file.path().canonicalize().unwrap()
        );
    }

    #[test]
    fn resolve_to_host_pid_fast_path_returns_self_for_current_process() {
        // In the root namespace the current process PID is its own host PID.
        let pid = std::process::id() as i32;
        let pst = proc_start_time(pid).expect("own start time");
        assert_eq!(resolve_to_host_pid(pid, pst), pid);
    }

    #[test]
    fn resolve_to_host_pid_returns_input_for_nonexistent_pid() {
        // A PID that definitely doesn't exist should fall through to fallback.
        assert_eq!(resolve_to_host_pid(2_000_000_000, 0), 2_000_000_000);
    }

    #[test]
    fn resolve_to_host_pid_rejects_pid_with_wrong_start_time() {
        // Own PID exists but wrong start_time → should not match fast path.
        let pid = std::process::id() as i32;
        let result = resolve_to_host_pid(pid, 999_999_999);
        // Falls through to slow path; no NSpid inner match → returns input.
        assert_eq!(result, pid);
    }

    #[test]
    fn read_nspid_chain_returns_at_least_self() {
        let pid = std::process::id() as i32;
        let chain = read_nspid_chain(pid);
        assert!(!chain.is_empty(), "NSpid chain should not be empty");
        assert_eq!(chain[0], pid, "first NSpid should be host PID");
    }

    #[test]
    fn read_nspid_chain_returns_empty_for_invalid_pid() {
        assert!(read_nspid_chain(2_000_000_000).is_empty());
    }

    #[test]
    fn proc_start_time_succeeds_for_self() {
        let pst = proc_start_time(std::process::id() as i32);
        assert!(pst.is_ok());
        assert!(pst.unwrap() > 0);
    }

    #[test]
    fn proc_start_time_fails_for_nonexistent_pid() {
        assert!(proc_start_time(2_000_000_000).is_err());
    }

    #[test]
    fn is_init_pid_namespace_returns_true_in_test_runner() {
        // Test runner runs on the host (init ns), so this should be true.
        assert!(is_init_pid_namespace());
    }

    #[test]
    fn resolve_and_read_target_succeeds_for_live_process() {
        // Spawn a short-lived child and resolve it.
        use std::process::Command;
        let mut child = Command::new("sleep")
            .arg("10")
            .spawn()
            .expect("spawn sleep");
        let pid = child.id() as i32;
        std::thread::sleep(std::time::Duration::from_millis(100));
        let pst = proc_start_time(pid).expect("child start time");
        let (host_pid, start_time) = resolve_and_read_target(pid, pst).expect("resolve");
        assert_eq!(host_pid, pid);
        assert_eq!(start_time, pst);
        // cleanup
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn parse_nspid_single_level() {
        let status = "Name:\ttest\nPid:\t42\nNSpid:\t42\nTgid:\t42\n";
        assert_eq!(parse_nspid_from_status(status), vec![42]);
    }

    #[test]
    fn parse_nspid_nested_namespace() {
        let status = "Name:\tgateway\nPid:\t39560\nNSpid:\t39560\t1\nTgid:\t39560\n";
        assert_eq!(parse_nspid_from_status(status), vec![39560, 1]);
    }

    #[test]
    fn parse_nspid_missing_line() {
        // Status content without NSpid line (edge case)
        let status = "Name:\ttest\nPid:\t42\nTgid:\t42\n";
        assert_eq!(parse_nspid_from_status(status), Vec::<i32>::new());
    }

    #[test]
    fn parse_start_time_from_valid_stat() {
        // Synthetic /proc/pid/stat content: comm in parens, then 19 fields before starttime
        let stat = "42 (test-proc) S 1 42 42 0 -1 4194304 100 0 0 0 10 5 0 0 20 0 1 0 12345 1000 100 18446744073709551615 0 0 0 0 0 0 0 0 0 0 0 0 17 0 0 0 0 0 0";
        assert_eq!(parse_start_time_from_stat(stat), Ok(12345));
    }

    #[test]
    fn parse_start_time_from_malformed_stat() {
        assert!(parse_start_time_from_stat("garbage").is_err());
        assert!(parse_start_time_from_stat("").is_err());
    }
}
