//! Sanitized, versioned diagnostic bundle export for production troubleshooting.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd};

#[cfg(unix)]
use nix::dir::{Dir, Type as DirectoryEntryType};
#[cfg(unix)]
use nix::fcntl::{openat, OFlag};
#[cfg(unix)]
use nix::sys::stat::Mode;
use serde::Serialize;
use serde_json::{json, Value};

use crate::config::{load_config, CoshConfig};
use crate::diagnostics::doctor::run_doctor_report_from_process_cwd;
use crate::evidence::redact_sensitive_output;

mod health;

use health::health_json;

const BUNDLE_FORMAT: &str = "cosh-diagnostic-bundle";
const BUNDLE_VERSION: u32 = 1;
const DEFAULT_SINCE_HOURS: u64 = 24;
const MAX_SOURCE_BYTES: u64 = 1024 * 1024;
const MAX_FILES_PER_SOURCE: usize = 20;

#[derive(Debug, Serialize)]
struct DiagnosticBundle {
    format: &'static str,
    version: u32,
    created_at_ms: u128,
    manifest: Vec<ManifestEntry>,
    sources: BundleSources,
}

#[derive(Debug, Serialize)]
struct BundleSources {
    environment: Value,
    configuration: Value,
    health: Option<Value>,
    recent_events: Vec<CollectedFile>,
    logs: Vec<CollectedFile>,
    crashes: Vec<CollectedFile>,
}

#[derive(Debug, Serialize)]
struct CollectedFile {
    path: String,
    modified_at_ms: Option<u128>,
    content: String,
    truncated: bool,
}

#[derive(Debug, Serialize)]
struct ManifestEntry {
    source: &'static str,
    status: SourceStatus,
    items: usize,
    detail: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum SourceStatus {
    Included,
    Unavailable,
    Partial,
}

#[derive(Debug)]
struct ExportOptions {
    output: PathBuf,
    since: Duration,
}

pub(crate) fn run_cli(args: &[String]) -> i32 {
    let top_level_help = args
        .first()
        .is_some_and(|flag| flag == "--help" || flag == "-h");
    let export_help = matches!(
        args,
        [command, flag] if command == "export" && (flag == "--help" || flag == "-h")
    );
    if top_level_help || export_help {
        print_help();
        return 0;
    }
    let options = match parse_args(args) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}");
            print_help();
            return 2;
        }
    };

    match export(&options) {
        Ok(()) => {
            println!("{}", options.output.display());
            0
        }
        Err(error) => {
            eprintln!("diagnostic bundle export failed: {error}");
            1
        }
    }
}

fn parse_args(args: &[String]) -> Result<ExportOptions, String> {
    if args.first().map(String::as_str) != Some("export") {
        return Err("expected `diagnostics export`".to_string());
    }
    let mut output = None;
    let mut since_hours = DEFAULT_SINCE_HOURS;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--output" | "-o" => {
                let value = args
                    .get(index + 1)
                    .filter(|value| !value.starts_with('-'))
                    .ok_or_else(|| "--output requires a path".to_string())?;
                output = Some(PathBuf::from(value));
                index += 2;
            }
            "--since-hours" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--since-hours requires a positive integer".to_string())?;
                since_hours = value
                    .parse::<u64>()
                    .ok()
                    .filter(|hours| *hours > 0)
                    .ok_or_else(|| "--since-hours requires a positive integer".to_string())?;
                since_hours
                    .checked_mul(3600)
                    .ok_or_else(|| "--since-hours is too large".to_string())?;
                index += 2;
            }
            other => return Err(format!("unknown diagnostics export option: {other}")),
        }
    }
    Ok(ExportOptions {
        output: output.unwrap_or_else(default_output_path),
        since: Duration::from_secs(since_hours * 3600),
    })
}

fn print_help() {
    println!("Usage: cosh-shell diagnostics export [--output PATH] [--since-hours HOURS]");
}

fn default_output_path() -> PathBuf {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default();
    PathBuf::from(format!("cosh-diagnostic-{seconds}.json"))
}

fn export(options: &ExportOptions) -> io::Result<()> {
    if options.output.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("refusing to overwrite {}", options.output.display()),
        ));
    }

    let config = load_config();
    // Share the doctor engine so the bundle's health section carries the same
    // resource + environment collector coverage and cwd semantics as
    // `cosh-shell doctor`. Environment collectors run even when the resource
    // scan is disabled, so the health section is always present.
    let health = Some(health_json(run_doctor_report_from_process_cwd(&config)));
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let (logs, log_errors) = collect_named_files(
        home.as_deref().map(|path| path.join(".copilot-shell/logs")),
        options.since,
        |_| true,
    );
    let (recent_events, event_errors) = collect_named_files(
        home.as_deref().map(|path| path.join(".copilot-shell")),
        options.since,
        |name| contains_any(name, &["event", "activity", "audit", "journal"]),
    );
    let (crashes, crash_errors) = collect_named_files(
        home.as_deref().map(|path| path.join(".copilot-shell")),
        options.since,
        |name| contains_any(name, &["crash", "panic", "error"]),
    );

    let manifest = vec![
        included("environment", 1),
        included("configuration", 1),
        optional("health", usize::from(health.is_some()), Vec::new()),
        optional("recent_events", recent_events.len(), event_errors),
        optional("logs", logs.len(), log_errors),
        optional("crashes", crashes.len(), crash_errors),
    ];
    let bundle = DiagnosticBundle {
        format: BUNDLE_FORMAT,
        version: BUNDLE_VERSION,
        created_at_ms: now_ms(),
        manifest,
        sources: BundleSources {
            environment: environment_json(),
            configuration: config_json(&config),
            health,
            recent_events,
            logs,
            crashes,
        },
    };
    let serialized = serde_json::to_string_pretty(&bundle).map_err(io::Error::other)?;
    atomic_private_write(&options.output, serialized.as_bytes())
}

fn included(source: &'static str, items: usize) -> ManifestEntry {
    ManifestEntry {
        source,
        status: SourceStatus::Included,
        items,
        detail: None,
    }
}

fn optional(source: &'static str, items: usize, errors: Vec<String>) -> ManifestEntry {
    let status = match (items, errors.is_empty()) {
        (0, _) => SourceStatus::Unavailable,
        (_, true) => SourceStatus::Included,
        _ => SourceStatus::Partial,
    };
    ManifestEntry {
        source,
        status,
        items,
        detail: (!errors.is_empty()).then(|| redact(&errors.join("; "))),
    }
}

fn environment_json() -> Value {
    json!({
        "cosh_shell_version": env!("CARGO_PKG_VERSION"),
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "family": std::env::consts::FAMILY,
        "current_executable": std::env::current_exe().ok().and_then(|path| path.file_name().map(|name| name.to_string_lossy().into_owned())),
    })
}

fn config_json(config: &CoshConfig) -> Value {
    json!({
        "shell_default": redact(&config.shell_default),
        "analysis_mode": redact(&config.analysis_mode),
        "approval_mode": redact(config.approval_mode.label()),
        "adapter_default": redact(&config.adapter_default),
        "language": redact(&config.language),
        "startup_banner": config.startup_banner,
        "startup_hooks": config.startup_hooks,
        "debug": config.debug,
        "log_level": redact(&config.log_level),
        "ai_enabled": config.ai_enabled,
        "health": {
            "enabled": config.health.enabled,
            "role": config.health.role.as_deref().map(redact),
            "memory_sensitive": config.health.memory_sensitive,
            "critical_mounts": config.health.critical_mounts.iter().map(|value| redact(value)).collect::<Vec<_>>(),
            "verbose": config.health.verbose,
            "configured_services": config.health.services.len(),
        },
        "trusted_commands_count": config.trusted_commands.len(),
        "trusted_project_roots_count": config.trusted_project_roots.len(),
    })
}

fn collect_named_files<F>(
    root: Option<PathBuf>,
    since: Duration,
    include: F,
) -> (Vec<CollectedFile>, Vec<String>)
where
    F: Fn(&str) -> bool,
{
    #[cfg(unix)]
    {
        collect_named_files_unix(root, since, include)
    }
    #[cfg(not(unix))]
    {
        let _ = (root, since, include);
        (
            Vec::new(),
            vec!["secure diagnostic collection is unavailable on this platform".to_string()],
        )
    }
}

#[cfg(unix)]
fn collect_named_files_unix<F>(
    root: Option<PathBuf>,
    since: Duration,
    include: F,
) -> (Vec<CollectedFile>, Vec<String>)
where
    F: Fn(&str) -> bool,
{
    let Some(root) = root else {
        return (Vec::new(), vec!["HOME is unavailable".to_string()]);
    };
    let cutoff = SystemTime::now().checked_sub(since).unwrap_or(UNIX_EPOCH);
    let mut errors = Vec::new();
    let mut sources = open_source_files(&root, &include, cutoff, &mut errors);
    sources.sort_by_key(|source| source.metadata.modified().ok());
    sources.reverse();
    if sources.len() > MAX_FILES_PER_SOURCE {
        errors.push(format!(
            "source file limit reached; included newest {MAX_FILES_PER_SOURCE} files"
        ));
    }
    sources.truncate(MAX_FILES_PER_SOURCE);

    let files = sources
        .into_iter()
        .filter_map(|source| match read_opened_source(source) {
            Ok(file) => Some(file),
            Err(error) => {
                errors.push(format!("source: {error}"));
                None
            }
        })
        .collect();
    (files, errors)
}

#[cfg(unix)]
fn open_source_files<F>(
    root: &Path,
    include: &F,
    cutoff: SystemTime,
    errors: &mut Vec<String>,
) -> Vec<OpenedSource>
where
    F: Fn(&str) -> bool,
{
    let mut directory = match Dir::open(
        root,
        OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW,
        Mode::empty(),
    ) {
        Ok(directory) => directory,
        Err(error) => {
            errors.push(format!("{}: {error}", display_private_path(root)));
            return Vec::new();
        }
    };
    let mut sources = Vec::new();
    let directory_fd = directory.as_raw_fd();
    for entry in directory.iter() {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                errors.push(format!("{}: {error}", display_private_path(root)));
                continue;
            }
        };
        let name = entry.file_name();
        let Ok(name) = name.to_str() else {
            continue;
        };
        if matches!(name, "." | "..") {
            continue;
        }
        if !include(name) {
            continue;
        }
        if matches!(entry.file_type(), Some(DirectoryEntryType::Symlink)) {
            continue;
        }
        let file = match openat(
            Some(directory_fd),
            name,
            OFlag::O_RDONLY | OFlag::O_NOFOLLOW,
            Mode::empty(),
        ) {
            Ok(fd) => {
                // `openat` transfers ownership of a newly opened descriptor to this `File`.
                unsafe { File::from_raw_fd(fd) }
            }
            Err(error) => {
                errors.push(format!("{}: {error}", redact(name)));
                continue;
            }
        };
        let metadata = match file.metadata() {
            Ok(metadata) if metadata.file_type().is_file() => metadata,
            Ok(_) => continue,
            Err(error) => {
                errors.push(format!("{}: {error}", redact(name)));
                continue;
            }
        };
        if metadata.modified().is_ok_and(|modified| modified >= cutoff) {
            sources.push(OpenedSource {
                name: redact(name),
                file,
                metadata,
            });
        }
    }
    sources
}

#[cfg(unix)]
struct OpenedSource {
    name: String,
    file: File,
    metadata: fs::Metadata,
}

#[cfg(unix)]
fn read_opened_source(mut source: OpenedSource) -> io::Result<CollectedFile> {
    let truncated = source.metadata.len() > MAX_SOURCE_BYTES;
    if truncated {
        source
            .file
            .seek(SeekFrom::End(-(MAX_SOURCE_BYTES as i64)))?;
    }
    let mut bytes = Vec::with_capacity(source.metadata.len().min(MAX_SOURCE_BYTES) as usize);
    source.file.take(MAX_SOURCE_BYTES).read_to_end(&mut bytes)?;
    let content = String::from_utf8_lossy(&bytes).into_owned();
    Ok(CollectedFile {
        path: source.name,
        modified_at_ms: source.metadata.modified().ok().map(system_time_ms),
        content: redact(&content),
        truncated,
    })
}

fn contains_any(value: &str, needles: &[&str]) -> bool {
    let value = value.to_ascii_lowercase();
    needles.iter().any(|needle| value.contains(needle))
}

fn display_private_path(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "<source>".to_string())
}

fn redact(input: &str) -> String {
    let (input, _) = redact_sensitive_output(input);
    let mut output = String::with_capacity(input.len());
    for (index, line) in input.lines().enumerate() {
        if index > 0 {
            output.push('\n');
        }
        output.push_str(&redact_line(line));
    }
    if input.ends_with('\n') {
        output.push('\n');
    }
    output
}

fn redact_line(line: &str) -> String {
    let mut line = line.to_string();
    for key in [
        "password",
        "passwd",
        "token",
        "secret",
        "api_key",
        "api-key",
        "apikey",
        "access_key",
        "access-key",
        "authorization",
        "credential",
    ] {
        line = redact_key_value(&line, key);
    }
    redact_url_userinfo(&redact_token_prefixes(&line))
}

fn redact_key_value(input: &str, key: &str) -> String {
    let lower = input.to_ascii_lowercase();
    let mut output = String::with_capacity(input.len());
    let mut cursor = 0;
    while let Some(relative) = lower[cursor..].find(key) {
        let start = cursor + relative;
        let key_end = start + key.len();
        let boundary_before = start == 0 || !lower.as_bytes()[start - 1].is_ascii_alphanumeric();
        let boundary_after =
            key_end == lower.len() || !lower.as_bytes()[key_end].is_ascii_alphanumeric();
        if !boundary_before || !boundary_after {
            output.push_str(&input[cursor..key_end]);
            cursor = key_end;
            continue;
        }
        let rest = &input[key_end..];
        let separator = rest
            .chars()
            .next()
            .filter(|ch| ch.is_whitespace())
            .map(|_| (0, 0))
            .or_else(|| rest.find(['=', ':']).map(|offset| (offset, 1)));
        let Some((separator, separator_width)) = separator else {
            output.push_str(&input[cursor..key_end]);
            cursor = key_end;
            continue;
        };
        if separator > 3 {
            output.push_str(&input[cursor..key_end]);
            cursor = key_end;
            continue;
        }
        let value_start = key_end + separator + separator_width;
        let prefix = &input[value_start..];
        let whitespace = prefix.len() - prefix.trim_start().len();
        let value_start = value_start + whitespace;
        let quote = input
            .as_bytes()
            .get(value_start)
            .copied()
            .filter(|byte| *byte == b'"' || *byte == b'\'');
        let content_start = value_start + usize::from(quote.is_some());
        let value_end = input[content_start..]
            .find(|ch: char| {
                quote.map_or(ch.is_whitespace() || ch == ',' || ch == '}', |q| {
                    ch as u8 == q
                })
            })
            .map(|end| content_start + end)
            .unwrap_or(input.len());
        output.push_str(&input[cursor..content_start]);
        output.push_str("<redacted>");
        cursor = value_end;
    }
    output.push_str(&input[cursor..]);
    output
}

fn redact_token_prefixes(input: &str) -> String {
    const PREFIXES: &[(&str, usize)] = &[
        ("github_pat_", 16),
        ("ghp_", 12),
        ("glpat-", 16),
        ("xoxb-", 16),
        ("npm_", 16),
        ("pypi-", 16),
        ("AIza", 20),
        ("sk-", 16),
    ];
    let mut output = String::with_capacity(input.len());
    let mut cursor = 0;
    while cursor < input.len() {
        let Some((start, prefix, minimum_len)) = PREFIXES
            .iter()
            .filter_map(|(prefix, minimum_len)| {
                input[cursor..]
                    .find(prefix)
                    .map(|offset| (cursor + offset, *prefix, *minimum_len))
            })
            .min_by_key(|(start, _, _)| *start)
        else {
            break;
        };
        let end = input[start..]
            .find(|ch: char| !(ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-')))
            .map(|offset| start + offset)
            .unwrap_or(input.len());
        if end - start < minimum_len {
            output.push_str(&input[cursor..start + prefix.len()]);
            cursor = start + prefix.len();
            continue;
        }
        output.push_str(&input[cursor..start]);
        output.push_str("<redacted>");
        cursor = end;
    }
    output.push_str(&input[cursor..]);
    output
}

fn redact_url_userinfo(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut cursor = 0;
    while let Some(relative_scheme) = input[cursor..].find("://") {
        let authority_start = cursor + relative_scheme + 3;
        let authority_end = input[authority_start..]
            .find(|ch: char| ch.is_whitespace() || matches!(ch, '/' | '?' | '#'))
            .map(|offset| authority_start + offset)
            .unwrap_or(input.len());
        let Some(relative_at) = input[authority_start..authority_end].rfind('@') else {
            output.push_str(&input[cursor..authority_start]);
            cursor = authority_start;
            continue;
        };
        let at = authority_start + relative_at;
        output.push_str(&input[cursor..authority_start]);
        output.push_str("<redacted>@");
        cursor = at + 1;
    }
    output.push_str(&input[cursor..]);
    output
}

fn atomic_private_write(path: &Path, content: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("bundle");
    let (temporary, mut file) = create_private_temp(parent, file_name)?;
    let result = (|| {
        file.write_all(content)?;
        file.sync_all()?;
        fs::hard_link(&temporary, path)
    })();
    drop(file);
    let _ = fs::remove_file(&temporary);
    result
}

fn create_private_temp(parent: &Path, file_name: &str) -> io::Result<(PathBuf, File)> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    create_private_temp_with_nonce(parent, file_name, nonce)
}

fn create_private_temp_with_nonce(
    parent: &Path,
    file_name: &str,
    nonce: u128,
) -> io::Result<(PathBuf, File)> {
    for attempt in 0..16_u128 {
        let path = parent.join(format!(
            ".{file_name}.{}.{}.tmp",
            std::process::id(),
            nonce.saturating_add(attempt)
        ));
        match private_new_file(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a unique diagnostic bundle temporary file",
    ))
}

#[cfg(unix)]
fn private_new_file(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn private_new_file(path: &Path) -> io::Result<File> {
    let _ = path;
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "secure diagnostic bundle permissions are unavailable on this platform",
    ))
}

fn now_ms() -> u128 {
    system_time_ms(SystemTime::now())
}

fn system_time_ms(time: SystemTime) -> u128 {
    time.duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

#[cfg(test)]
#[path = "bundle/tests.rs"]
mod tests;
