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

/// Reads a `/proc` text file, decoding non-UTF-8 bytes lossily.
///
/// `/proc/<pid>/stat` embeds a process name that may contain non-UTF-8 bytes.
/// Lossy decoding preserves the ASCII start-time field behind that name.
fn read_proc_text_lossy(path: &str) -> std::io::Result<String> {
    fs::read(path).map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
}

/// Reads the Linux process start time after excluding protected service processes.
pub(crate) fn read_process_start_time(pid: i32) -> Result<u64, TargetValidationError> {
    if pid <= 1 || pid == std::process::id() as i32 {
        return Err(TargetValidationError::ProtectedProcess(pid));
    }

    let stat = read_proc_text_lossy(&format!("/proc/{pid}/stat"))
        .map_err(|source| TargetValidationError::ProcessIo { pid, source })?;
    validate_stat(pid, &stat)
}

/// Validates a decoded `/proc/<pid>/stat` payload: excludes protected service
/// processes by name, then reads the start-time field (22, 0-based 19 after
/// the comm) that follows the parenthesized name.
fn validate_stat(pid: i32, stat: &str) -> Result<u64, TargetValidationError> {
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
    fn reads_start_time_for_live_process() {
        use std::process::Command;
        let mut child = Command::new("sleep")
            .arg("10")
            .spawn()
            .expect("spawn sleep");
        let pid = child.id() as i32;
        let start_time = read_process_start_time(pid).expect("child start time");
        assert!(start_time > 0);
        assert_eq!(
            read_process_start_time(pid).expect("stable identity"),
            start_time
        );
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn validate_stat_reads_start_time() {
        let stat = "42 (test-proc) S 1 42 42 0 -1 4194304 100 0 0 0 10 5 0 0 20 0 1 0 12345 1000 100 18446744073709551615 0 0 0 0 0 0 0 0 0 0 0 0 17 0 0 0 0 0 0";
        assert_eq!(validate_stat(42, stat).ok(), Some(12345));
        assert!(validate_stat(42, "garbage").is_err());
        assert!(validate_stat(42, "").is_err());
    }

    #[test]
    fn read_proc_text_lossy_keeps_ascii_fields_across_invalid_bytes() {
        // /proc/<pid>/stat embeds the comm between the parentheses, and the
        // kernel allows non-NUL non-UTF-8 bytes in a comm. `read_to_string`
        // rejects the whole file; the lossy read must keep every ASCII field
        // the parsers rely on and mark only the offending byte.
        let dir = std::env::temp_dir().join(format!("agentsight_stat_{}", std::process::id()));
        fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("stat");
        let mut stat = b"42 (cla".to_vec();
        stat.push(0xa0); // one invalid UTF-8 byte inside the process name
        stat.extend_from_slice(b"ude) S 1 42 42 0 -1 4194304 100 0 0 0 10 5 0 0 20 0 1 0 99999 1000 100 0 0 0 0 0 0 0 0 0 0 0 0 17 0 0 0 0 0 0\n");
        fs::write(&path, &stat).expect("write non-UTF-8 stat");

        let text = read_proc_text_lossy(path.to_str().unwrap()).expect("lossy read must succeed");
        assert!(text.starts_with("42 (cla\u{fffd}ude) S "));
        assert!(
            text.contains(" 99999 "),
            "the ASCII start-time field survives"
        );

        assert_eq!(validate_stat(42, &text).ok(), Some(99999));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_proc_text_lossy_reports_missing_files() {
        assert!(read_proc_text_lossy("/proc/2000000000/stat").is_err());
    }

    #[test]
    fn validate_stat_parses_a_non_utf8_comm_lossily() {
        // End-to-end over the validation flow: the same non-UTF-8 stat that
        // `read_to_string` rejects must validate through the reader plus the
        // protected-name check and the start-time field.
        let dir = std::env::temp_dir().join(format!("agentsight_stat_e2e_{}", std::process::id()));
        fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("stat");
        let mut stat = b"4242 (qoder".to_vec();
        stat.push(0xff);
        stat.extend_from_slice(
            b") S 1 42 42 0 -1 4194304 100 0 0 0 10 5 0 0 20 0 1 0 55555 1000 100 0 0 0 0 0 0 0 0 0 0 0 0 17 0 0 0 0 0 0\n",
        );
        fs::write(&path, &stat).expect("write non-UTF-8 stat");

        let text = read_proc_text_lossy(path.to_str().unwrap()).expect("lossy read");
        assert_eq!(validate_stat(4242, &text).ok(), Some(55555_u64));

        // Guard: the protected-name check still fires on an ordinary stat.
        let protected = "7 (agentsight) S 1 42 42 0 -1 4194304 100 0 0 0 10 5 0 0 20 0 1 0 1 1000 100 0 0 0 0 0 0 0 0 0 0 0 0 17 0 0 0 0 0 0";
        assert!(matches!(
            validate_stat(7, protected),
            Err(TargetValidationError::ProtectedProcess(_))
        ));

        let _ = fs::remove_dir_all(&dir);
    }
}
