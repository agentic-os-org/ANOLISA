use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use super::personal_model::HistoryCursor;
use super::personal_sanitize::sanitize_shell_command;

const MAX_TAIL_BYTES: u64 = 64 * 1024;
const MAX_ENTRIES: usize = 500;
const MAX_IMPORTS: usize = 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NativeBashHistoryMarker {
    histfile: PathBuf,
}

impl NativeBashHistoryMarker {
    pub(crate) fn new(histfile: PathBuf) -> Self {
        Self { histfile }
    }

    pub(crate) fn histfile(&self) -> &Path {
        &self.histfile
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HistoryControl {
    Enabled,
    Disabled,
    Clear,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct HistorySyncState {
    pub(crate) cursor: Option<HistoryCursor>,
    pub(crate) baseline_pending: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LiveShellCommand {
    pub(crate) command: String,
    pub(crate) observed_hour_bucket: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImportedHistoryEntry {
    pub(crate) command: String,
    pub(crate) execution_hour_bucket: Option<u64>,
    pub(crate) time_unverified: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HistorySyncResult {
    pub(crate) imported: Vec<ImportedHistoryEntry>,
    pub(crate) cursor: Option<HistoryCursor>,
    pub(crate) baseline_pending: bool,
    pub(crate) delete_history_derived: bool,
    pub(crate) analyzer_trigger: bool,
    pub(crate) bytes_read: usize,
    pub(crate) entries_considered: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HistoryError {
    MissingMarker,
    NotAbsolute,
    Symlink,
    NotRegularFile,
    WrongOwner,
    Io(String),
    Sanitize(String),
}

#[derive(Debug)]
struct ParsedEntry {
    command: String,
    execution_hour_bucket: Option<u64>,
    time_unverified: bool,
    fingerprint: String,
}

pub(crate) fn sync_native_bash_history<F>(
    control: HistoryControl,
    marker: Option<&NativeBashHistoryMarker>,
    expected_owner_uid: u32,
    now_unix_secs: u64,
    state: &HistorySyncState,
    live_commands: &[LiveShellCommand],
    digest: F,
) -> Result<HistorySyncResult, HistoryError>
where
    F: Fn(&[u8]) -> String,
{
    if control == HistoryControl::Disabled {
        return Ok(reset_result());
    }

    let Some(marker) = marker else {
        return if control == HistoryControl::Clear {
            Ok(reset_result())
        } else {
            Err(HistoryError::MissingMarker)
        };
    };
    let result = sync_trusted_history(
        marker,
        expected_owner_uid,
        now_unix_secs,
        state,
        live_commands,
        control == HistoryControl::Clear,
        &digest,
    );
    if control != HistoryControl::Clear {
        return result;
    }
    match result {
        Ok(mut result) => {
            result.delete_history_derived = true;
            Ok(result)
        }
        Err(_) => Ok(reset_result()),
    }
}

fn sync_trusted_history<F>(
    marker: &NativeBashHistoryMarker,
    expected_owner_uid: u32,
    now_unix_secs: u64,
    state: &HistorySyncState,
    live_commands: &[LiveShellCommand],
    force_baseline: bool,
    digest: &F,
) -> Result<HistorySyncResult, HistoryError>
where
    F: Fn(&[u8]) -> String,
{
    let path = marker.histfile();
    if !path.is_absolute() {
        return Err(HistoryError::NotAbsolute);
    }
    let path_metadata = std::fs::symlink_metadata(path).map_err(io_error)?;
    if path_metadata.file_type().is_symlink() {
        return Err(HistoryError::Symlink);
    }
    if !path_metadata.is_file() {
        return Err(HistoryError::NotRegularFile);
    }
    if path_metadata.uid() != expected_owner_uid {
        return Err(HistoryError::WrongOwner);
    }
    let mut file = File::open(path).map_err(io_error)?;
    let metadata = file.metadata().map_err(io_error)?;
    if metadata.dev() != path_metadata.dev() || metadata.ino() != path_metadata.ino() {
        return Err(HistoryError::Io(
            "history file changed while opening".to_string(),
        ));
    }
    if !metadata.is_file() {
        return Err(HistoryError::NotRegularFile);
    }
    if metadata.uid() != expected_owner_uid {
        return Err(HistoryError::WrongOwner);
    }

    let (tail, bytes_read) = read_tail(&mut file, metadata.len())?;
    let mut parsed = parse_entries(&tail, now_unix_secs);
    if parsed.len() > MAX_ENTRIES {
        parsed.drain(..parsed.len() - MAX_ENTRIES);
    }
    let entries_considered = parsed.len();
    let file_identity = digest(format!("{}:{}", metadata.dev(), metadata.ino()).as_bytes());
    for entry in &mut parsed {
        let material = format!(
            "{}\0{}",
            entry.execution_hour_bucket.unwrap_or_default(),
            entry.command
        );
        entry.fingerprint = digest(material.as_bytes());
    }
    let cursor = HistoryCursor {
        file_identity_hmac: file_identity.clone(),
        last_entry_hmac: parsed
            .last()
            .map(|entry| entry.fingerprint.clone())
            .unwrap_or_else(|| digest(&[])),
        size_mtime_hmac: digest(
            format!(
                "{}:{}:{}",
                metadata.len(),
                metadata.mtime(),
                metadata.mtime_nsec()
            )
            .as_bytes(),
        ),
    };

    if state.baseline_pending || force_baseline {
        return Ok(enabled_result(
            Vec::new(),
            cursor,
            bytes_read,
            entries_considered,
        ));
    }

    let start = match state.cursor.as_ref() {
        None => 0,
        Some(previous) if previous.file_identity_hmac != file_identity => parsed.len(),
        Some(previous) => parsed
            .iter()
            .rposition(|entry| entry.fingerprint == previous.last_entry_hmac)
            .map_or(parsed.len(), |index| index + 1),
    };
    let mut imported = parsed
        .into_iter()
        .skip(start)
        .filter_map(|entry| sanitize_entry(entry).transpose())
        .collect::<Result<Vec<_>, _>>()?;
    imported.retain(|entry| !duplicates_live(entry, live_commands));
    if imported.len() > MAX_IMPORTS {
        imported.drain(..imported.len() - MAX_IMPORTS);
    }

    Ok(enabled_result(
        imported,
        cursor,
        bytes_read,
        entries_considered,
    ))
}

fn reset_result() -> HistorySyncResult {
    HistorySyncResult {
        imported: Vec::new(),
        cursor: None,
        baseline_pending: true,
        delete_history_derived: true,
        analyzer_trigger: false,
        bytes_read: 0,
        entries_considered: 0,
    }
}

fn enabled_result(
    imported: Vec<ImportedHistoryEntry>,
    cursor: HistoryCursor,
    bytes_read: usize,
    entries_considered: usize,
) -> HistorySyncResult {
    HistorySyncResult {
        imported,
        cursor: Some(cursor),
        baseline_pending: false,
        delete_history_derived: false,
        analyzer_trigger: false,
        bytes_read,
        entries_considered,
    }
}

fn read_tail(file: &mut File, size: u64) -> Result<(String, usize), HistoryError> {
    let start = size.saturating_sub(MAX_TAIL_BYTES);
    file.seek(SeekFrom::Start(start)).map_err(io_error)?;
    let mut bytes = Vec::with_capacity((size - start) as usize);
    file.take(MAX_TAIL_BYTES)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    let bytes_read = bytes.len();
    if start > 0 {
        if let Some(first_newline) = bytes.iter().position(|byte| *byte == b'\n') {
            bytes.drain(..=first_newline);
        } else {
            bytes.clear();
        }
    }
    Ok((String::from_utf8_lossy(&bytes).into_owned(), bytes_read))
}

fn parse_entries(tail: &str, now_unix_secs: u64) -> Vec<ParsedEntry> {
    let lines = tail.lines().collect::<Vec<_>>();
    let timestamped = lines.iter().any(|line| timestamp_marker(line).is_some());
    if !timestamped {
        return lines
            .into_iter()
            .filter(|line| !line.is_empty())
            .map(|line| ParsedEntry {
                command: line.to_string(),
                execution_hour_bucket: None,
                time_unverified: true,
                fingerprint: String::new(),
            })
            .collect();
    }

    let mut entries = Vec::new();
    let mut timestamp = None;
    let mut command_lines = Vec::new();
    for line in lines {
        if let Some(epoch) = timestamp_marker(line) {
            push_timestamped_entry(&mut entries, &mut command_lines, timestamp, now_unix_secs);
            timestamp = Some(epoch);
        } else if timestamp.is_some() {
            command_lines.push(line);
        }
    }
    push_timestamped_entry(&mut entries, &mut command_lines, timestamp, now_unix_secs);
    entries
}

fn push_timestamped_entry(
    entries: &mut Vec<ParsedEntry>,
    command_lines: &mut Vec<&str>,
    timestamp: Option<u64>,
    now_unix_secs: u64,
) {
    let Some(epoch) = timestamp else {
        command_lines.clear();
        return;
    };
    if command_lines.is_empty() {
        return;
    }
    let trusted = epoch <= now_unix_secs;
    entries.push(ParsedEntry {
        command: command_lines.join("\n"),
        execution_hour_bucket: trusted.then_some(epoch / 3600),
        time_unverified: !trusted,
        fingerprint: String::new(),
    });
    command_lines.clear();
}

fn timestamp_marker(line: &str) -> Option<u64> {
    let digits = line.strip_prefix('#')?;
    (!digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| digits.parse().ok())
        .flatten()
}

fn sanitize_entry(entry: ParsedEntry) -> Result<Option<ImportedHistoryEntry>, HistoryError> {
    let sanitized = sanitize_shell_command(&entry.command).map_err(HistoryError::Sanitize)?;
    let command = sanitized.text.trim().to_string();
    if command.is_empty() {
        return Ok(None);
    }
    Ok(Some(ImportedHistoryEntry {
        command,
        execution_hour_bucket: entry.execution_hour_bucket,
        time_unverified: entry.time_unverified,
    }))
}

fn duplicates_live(entry: &ImportedHistoryEntry, live_commands: &[LiveShellCommand]) -> bool {
    live_commands.iter().any(|live| {
        sanitize_shell_command(&live.command)
            .is_ok_and(|sanitized| sanitized.text.trim() == entry.command)
            && match entry.execution_hour_bucket {
                Some(hour) => hour == live.observed_hour_bucket,
                None => true,
            }
    })
}

fn io_error(error: std::io::Error) -> HistoryError {
    HistoryError::Io(error.to_string())
}

#[cfg(test)]
mod tests {
    use std::fs::{self, OpenOptions};
    use std::io::Write;
    use std::os::unix::fs::{symlink, MetadataExt};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static NEXT_DIR: AtomicU64 = AtomicU64::new(1);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let id = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("cosh-personal-history-{}-{id}", std::process::id()));
            fs::create_dir(&path).expect("create test dir");
            Self(path)
        }

        fn history(&self, contents: &str) -> PathBuf {
            let path = self.0.join("bash_history");
            fs::write(&path, contents).expect("write history");
            path
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn digest(bytes: &[u8]) -> String {
        bytes
            .iter()
            .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
                hash.wrapping_mul(0x100_0000_01b3) ^ u64::from(*byte)
            })
            .to_string()
    }

    fn enabled_state() -> HistorySyncState {
        HistorySyncState {
            cursor: None,
            baseline_pending: false,
        }
    }

    fn sync(
        marker: &NativeBashHistoryMarker,
        state: &HistorySyncState,
        live: &[LiveShellCommand],
    ) -> Result<HistorySyncResult, HistoryError> {
        let owner_uid = fs::metadata(marker.histfile()).expect("metadata").uid();
        sync_native_bash_history(
            HistoryControl::Enabled,
            Some(marker),
            owner_uid,
            100_000,
            state,
            live,
            digest,
        )
    }

    #[test]
    fn rejects_relative_symlink_and_wrong_owner() {
        let relative = NativeBashHistoryMarker::new(PathBuf::from(".bash_history"));
        assert!(matches!(
            sync_native_bash_history(
                HistoryControl::Enabled,
                Some(&relative),
                0,
                100_000,
                &enabled_state(),
                &[],
                digest,
            ),
            Err(HistoryError::NotAbsolute)
        ));

        let dir = TestDir::new();
        let target = dir.history("echo safe\n");
        let link = dir.0.join("history-link");
        symlink(&target, &link).expect("create symlink");
        let marker = NativeBashHistoryMarker::new(link);
        let owner_uid = fs::metadata(&target).expect("metadata").uid();
        assert!(matches!(
            sync_native_bash_history(
                HistoryControl::Enabled,
                Some(&marker),
                owner_uid,
                100_000,
                &enabled_state(),
                &[],
                digest,
            ),
            Err(HistoryError::Symlink)
        ));

        let marker = NativeBashHistoryMarker::new(target);
        assert!(matches!(
            sync_native_bash_history(
                HistoryControl::Enabled,
                Some(&marker),
                owner_uid.saturating_add(1),
                100_000,
                &enabled_state(),
                &[],
                digest,
            ),
            Err(HistoryError::WrongOwner)
        ));
    }

    #[test]
    fn parses_timestamped_multiline_and_plain_history() {
        let dir = TestDir::new();
        let timestamped = dir.history("#3600\nprintf 'a\nb'\n#7200\necho done\n");
        let marker = NativeBashHistoryMarker::new(timestamped);
        let result = sync(&marker, &enabled_state(), &[]).expect("sync timestamped history");

        assert_eq!(result.imported.len(), 2);
        assert_eq!(result.imported[0].command, "printf 'a\nb'");
        assert_eq!(result.imported[0].execution_hour_bucket, Some(1));
        assert!(!result.imported[0].time_unverified);
        assert_eq!(result.imported[1].command, "echo done");

        let plain = dir.0.join("plain_history");
        fs::write(&plain, "echo one\necho two\n").expect("write plain history");
        let marker = NativeBashHistoryMarker::new(plain);
        let result = sync(&marker, &enabled_state(), &[]).expect("sync plain history");
        assert_eq!(result.imported.len(), 2);
        assert!(result.imported.iter().all(|entry| entry.time_unverified));
    }

    #[test]
    fn tail_and_entry_and_import_limits_are_enforced() {
        let dir = TestDir::new();
        let contents = (0..900)
            .map(|index| format!("echo {index} {}", "x".repeat(100)))
            .collect::<Vec<_>>()
            .join("\n");
        let path = dir.history(&contents);
        let marker = NativeBashHistoryMarker::new(path);

        let result = sync(&marker, &enabled_state(), &[]).expect("sync bounded history");

        assert!(result.bytes_read <= 64 * 1024);
        assert!(result.entries_considered <= 500);
        assert_eq!(result.imported.len(), 20);
        assert!(result
            .imported
            .last()
            .expect("latest entry")
            .command
            .contains("899"));
    }

    #[test]
    fn baseline_then_cursor_imports_only_new_entries() {
        let dir = TestDir::new();
        let path = dir.history("echo old\n");
        let marker = NativeBashHistoryMarker::new(path.clone());
        let owner_uid = fs::metadata(&path).expect("metadata").uid();
        let baseline = sync_native_bash_history(
            HistoryControl::Enabled,
            Some(&marker),
            owner_uid,
            100_000,
            &HistorySyncState {
                cursor: None,
                baseline_pending: true,
            },
            &[],
            digest,
        )
        .expect("establish baseline");
        assert!(baseline.imported.is_empty());
        assert!(!baseline.baseline_pending);

        let mut file = OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("open history");
        file.write_all(b"echo new\n").expect("append history");
        let next = sync(
            &marker,
            &HistorySyncState {
                cursor: baseline.cursor,
                baseline_pending: false,
            },
            &[],
        )
        .expect("import delta");

        assert_eq!(next.imported.len(), 1);
        assert_eq!(next.imported[0].command, "echo new");
    }

    #[test]
    fn disable_and_clear_require_baseline_without_reading_a_file() {
        for control in [HistoryControl::Disabled, HistoryControl::Clear] {
            let result =
                sync_native_bash_history(control, None, 0, 100_000, &enabled_state(), &[], digest)
                    .expect("apply control");
            assert!(result.delete_history_derived);
            assert!(result.baseline_pending);
            assert!(result.cursor.is_none());
            assert!(result.imported.is_empty());
        }
    }

    #[test]
    fn clear_records_current_eof_when_a_trusted_marker_is_available() {
        let dir = TestDir::new();
        let path = dir.history("echo old\n");
        let marker = NativeBashHistoryMarker::new(path.clone());
        let owner_uid = fs::metadata(&path).expect("metadata").uid();

        let cleared = sync_native_bash_history(
            HistoryControl::Clear,
            Some(&marker),
            owner_uid,
            100_000,
            &enabled_state(),
            &[],
            digest,
        )
        .expect("clear with marker");

        assert!(cleared.delete_history_derived);
        assert!(!cleared.baseline_pending);
        assert!(cleared.cursor.is_some());
        assert!(cleared.imported.is_empty());
    }

    #[test]
    fn live_dedup_requires_the_same_trusted_hour() {
        let dir = TestDir::new();
        let path = dir.history("#3600\necho repeated\n#90000\necho repeated\n");
        let marker = NativeBashHistoryMarker::new(path);
        let live = [LiveShellCommand {
            command: "echo repeated".to_string(),
            observed_hour_bucket: 1,
        }];

        let result = sync(&marker, &enabled_state(), &live).expect("sync history");

        assert_eq!(result.imported.len(), 1);
        assert_eq!(result.imported[0].execution_hour_bucket, Some(25));
    }

    #[test]
    fn live_dedup_uses_the_same_sanitized_command_as_history() {
        let dir = TestDir::new();
        let home = std::env::var("HOME").expect("HOME");
        let command = format!("cat {home}/workspace/payment-api/config.yaml");
        let path = dir.history(&format!("#3600\n{command}\n"));
        let marker = NativeBashHistoryMarker::new(path);
        let live = [LiveShellCommand {
            command,
            observed_hour_bucket: 1,
        }];

        let result = sync(&marker, &enabled_state(), &live).expect("sync history");

        assert!(result.imported.is_empty());
    }

    #[test]
    fn read_does_not_change_mtime_and_never_triggers_analyzer() {
        let dir = TestDir::new();
        let path = dir.history("echo safe\n");
        let marker = NativeBashHistoryMarker::new(path.clone());
        let before = fs::metadata(&path)
            .expect("before metadata")
            .modified()
            .unwrap();

        let result = sync(&marker, &enabled_state(), &[]).expect("sync history");
        let after = fs::metadata(&path)
            .expect("after metadata")
            .modified()
            .unwrap();

        assert_eq!(before, after);
        assert!(!result.analyzer_trigger);
    }

    #[test]
    fn imported_commands_use_the_shell_command_sanitizer() {
        let dir = TestDir::new();
        let path = dir.history("curl 'https://example.test?token=secret-value'\n");
        let marker = NativeBashHistoryMarker::new(path);

        let result = sync(&marker, &enabled_state(), &[]).expect("sync history");

        assert_eq!(result.imported.len(), 1);
        assert!(!result.imported[0].command.contains("secret-value"));
        assert!(result.imported[0].command.contains("<redacted>"));
    }
}
