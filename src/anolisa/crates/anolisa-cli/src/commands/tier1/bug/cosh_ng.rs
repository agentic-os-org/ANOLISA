//! cosh-ng diagnostic bridge for `anolisa bug --component cosh-ng`.
//!
//! The bridge shells out to the installed `cosh-shell` binary and asks it to
//! export its sanitized diagnostic bundle to a fresh, user-private path, then
//! projects the stable bundle schema (`format`/`version`/`manifest`/`health`)
//! into the bug report. All safety properties — 0600 permissions, refusal to
//! overwrite, bounded collection, allowlisted redaction — belong to the
//! exporter; this side never opens existing user files for writing, performs
//! no network access, and degrades to an explicit `unavailable` outcome
//! instead of failing the whole `bug` command.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;

use anolisa_platform::fs_layout::FsLayout;
use anolisa_platform::fs_layout::InstallMode;

/// Environment override for the `cosh-shell` binary location, following the
/// `DSH_BIN` / `CLAUDE_BIN` adapter precedent. Empty or unset means the
/// install layout and PATH are consulted instead.
const BIN_ENV: &str = "COSH_SHELL_BIN";
const BIN_NAME: &str = "cosh-shell";
/// Subpath of the `cosh-shell` binary below the install layout's libexec
/// directory, as declared by the cosh-ng component manifest
/// (`{libexecdir}/cosh-ng/cosh-shell`). Raw and RPM installs alike place the
/// binary there; only the `cosh` entry point lands on PATH, and that entry
/// would forward `diagnostics export` to the shell instead of running it.
const LIBEXEC_SUBPATH: &str = "cosh-ng/cosh-shell";
/// RPM packages install cosh-ng below the standard `%{_libexecdir}`
/// (`/usr/libexec`), not the raw contract's `/usr/local/libexec` — see
/// `cosh-ng.spec.in` (`%global _libexecdir_cosh %{_libexecdir}/anolisa/cosh-ng`).
/// The path is rebased under the layout prefix so staged sysroots work.
const RPM_LIBEXEC_SUBPATH: &str = "usr/libexec/anolisa/cosh-ng/cosh-shell";
const BUNDLE_FORMAT: &str = "cosh-diagnostic-bundle";
const BUNDLE_VERSION: u64 = 1;
/// Hard upper bound for the export subprocess. The exporter is bounded by
/// design, so a run that outlives this is treated as hung and killed.
const EXPORT_TIMEOUT: Duration = Duration::from_secs(60);
/// Sub-second wait granularity while polling the export subprocess.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Outcome of attempting to bridge the cosh-shell diagnostic bundle.
///
/// Serialized into the `--json` payload under `cosh_ng_diagnostics`; the
/// externally tagged `status` discriminator lets consumers distinguish the
/// two shapes without probing for fields.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(super) enum CoshNgDiagnostics {
    /// The bundle was exported and validated against the stable schema.
    Available {
        /// Display path of the resolved `cosh-shell` binary, home prefix
        /// folded to `~`; used so the printed reproduction commands are
        /// actually executable on this host.
        binary_path: String,
        /// Display path of the exported bundle, home prefix folded to `~`.
        bundle_path: String,
        /// `health.overall_severity` from the bundle, absent when the health
        /// section itself was unavailable at export time.
        overall_severity: Option<String>,
        /// Stable finding IDs and severities; raw details stay in the bundle.
        findings: Vec<FindingSummary>,
        /// Collectors that reported unavailable, by label.
        unavailable_collectors: Vec<String>,
        /// Per-source export manifest (source, status, item count).
        manifest: Vec<ManifestSummary>,
    },
    /// No bundle could be produced; the report explains why and points at the
    /// manual command so the issue material stays actionable.
    Unavailable {
        /// Human-readable reason (missing binary, failed/timed-out export,
        /// invalid bundle); any caller-home path inside is folded to `~`.
        reason: String,
        /// The exact command a user can run to retry collection by hand.
        manual_command: String,
    },
    /// Collection was skipped because the invocation is a dry run: the
    /// exporter is a subprocess that writes a bundle, and `--dry-run`
    /// forbids running it. Reported explicitly rather than disguised as
    /// unavailable or silently omitted.
    Skipped,
}

/// One health finding projected to its stable identifiers only.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(super) struct FindingSummary {
    /// Stable finding ID shared by `/health`, doctor, and export.
    pub id: String,
    /// Severity label (`ok`/`unavailable`/`degraded`/`warning`/`critical`).
    pub severity: String,
}

/// One bundle manifest entry: which source was collected and how it ended.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(super) struct ManifestSummary {
    /// Bundle source name (e.g. `environment`, `logs`).
    pub source: String,
    /// `included` / `unavailable` / `partial`.
    pub status: String,
    /// Number of items collected from the source.
    pub items: u64,
}

/// Collect cosh-ng diagnostics for the bug report.
///
/// The bundle is written below the *calling user's* state root — never the
/// selected install's: a plain user diagnosing a system-scope installation
/// cannot write to its root-owned state directory, and escalating to sudo
/// would make the exporter read root's HOME, config, and logs instead of the
/// affected user's session data.
pub(super) fn collect(layout: &FsLayout) -> CoshNgDiagnostics {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let dir = if home.as_os_str().is_empty() {
        None
    } else {
        Some(FsLayout::user(home.clone()).state_dir.join("diagnostics"))
    };
    collect_impl(resolve_binary(layout), dir, &home, EXPORT_TIMEOUT)
}

/// Injectable core of [`collect`]: the resolved binary (if any), the
/// user-writable diagnostics directory (if computable), the caller's home
/// for display folding, and the export timeout.
pub(super) fn collect_impl(
    binary: Option<PathBuf>,
    dir: Option<PathBuf>,
    home: &Path,
    timeout: Duration,
) -> CoshNgDiagnostics {
    // The manual retry command must use the resolved absolute binary path
    // when we have one: on raw/RPM installs PATH only carries the `cosh`
    // entry point, which would forward `diagnostics export` to the shell.
    // Every path argument is shell-quoted: the report promises copyable
    // commands, and an override or prefix with spaces must not break that.
    let invoked = binary
        .as_deref()
        .map(|bin| shell_quote(&fold_home_under(bin, home).display().to_string()))
        .unwrap_or_else(|| BIN_NAME.to_string());
    let manual_command = match &dir {
        // Suggest a fresh, unoccupied output name: the exporter refuses to
        // overwrite, so a fixed name would make the retry command fail from
        // the second report on.
        Some(dir) => format!(
            "{invoked} diagnostics export --output {}",
            shell_quote(
                &fold_home_under(&unique_output_path(dir), home)
                    .display()
                    .to_string()
            )
        ),
        None => format!("{invoked} diagnostics export"),
    };
    let Some(binary) = binary else {
        return CoshNgDiagnostics::Unavailable {
            reason: format!(
                "cosh-shell binary not found (searched {BIN_ENV}, the cosh-ng libexec install, and PATH)"
            ),
            manual_command,
        };
    };
    let Some(dir) = dir else {
        return CoshNgDiagnostics::Unavailable {
            reason: "HOME is not set; cannot choose a user-writable diagnostics directory"
                .to_string(),
            manual_command,
        };
    };
    collect_with(&binary, &dir, home, timeout, manual_command)
}

fn collect_with(
    binary: &Path,
    dir: &Path,
    home: &Path,
    timeout: Duration,
    manual_command: String,
) -> CoshNgDiagnostics {
    let output = match fresh_output_path(dir) {
        Ok(path) => path,
        Err(reason) => {
            return CoshNgDiagnostics::Unavailable {
                reason: fold_reason(&reason, home),
                manual_command,
            };
        }
    };
    if let Err(reason) = run_export(binary, &output, timeout) {
        return CoshNgDiagnostics::Unavailable {
            reason: fold_reason(&reason, home),
            manual_command,
        };
    }
    match parse_bundle(&output) {
        Ok(parsed) => CoshNgDiagnostics::Available {
            binary_path: fold_home_under(binary, home).display().to_string(),
            bundle_path: fold_home_under(&output, home).display().to_string(),
            overall_severity: parsed.overall_severity,
            findings: parsed.findings,
            unavailable_collectors: parsed.unavailable_collectors,
            manifest: parsed.manifest,
        },
        Err(reason) => CoshNgDiagnostics::Unavailable {
            reason: format!(
                "{} (bundle left at {} for inspection)",
                fold_reason(&reason, home),
                fold_home_under(&output, home).display()
            ),
            manual_command,
        },
    }
}

/// Resolve the `cosh-shell` binary: `COSH_SHELL_BIN` override first, then the
/// selected installation's private libexec locations (raw contract and RPM —
/// see [`RPM_LIBEXEC_SUBPATH`]), then PATH as a development fallback.
fn resolve_binary(layout: &FsLayout) -> Option<PathBuf> {
    resolve_binary_with(
        std::env::var_os(BIN_ENV)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from),
        layout,
        std::env::var_os("PATH"),
    )
}

pub(super) fn resolve_binary_with(
    override_path: Option<PathBuf>,
    layout: &FsLayout,
    path_var: Option<OsString>,
) -> Option<PathBuf> {
    if let Some(override_path) = override_path {
        return Some(override_path);
    }
    for candidate in install_candidates(layout) {
        if is_executable(&candidate) {
            return Some(candidate);
        }
    }
    let paths = path_var?;
    std::env::split_paths(&paths)
        .map(|dir| dir.join(BIN_NAME))
        .find(|candidate| is_executable(candidate))
}

/// Candidate binary locations below the selected installation, in preference
/// order: the raw contract's libexec directory first, then the RPM's
/// `%{_libexecdir}` location (system scope only — a user install has no RPM).
fn install_candidates(layout: &FsLayout) -> Vec<PathBuf> {
    let mut candidates = vec![layout.libexec_dir.join(LIBEXEC_SUBPATH)];
    if layout.mode == InstallMode::System {
        // `prefix` is "/" for a real system install; joining a relative path
        // rebases the FHS location under staged sysroots.
        candidates.push(layout.prefix.join(RPM_LIBEXEC_SUBPATH));
    }
    candidates
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// Pick a fresh private output path below `dir` without ever touching an
/// existing file: the exporter additionally refuses to overwrite, so a
/// collision here must be ruled out before spawn. A newly created
/// diagnostics directory is tightened to 0700 — the bundle itself is 0600
/// by exporter contract, and the directory should not enumerate either.
fn fresh_output_path(dir: &Path) -> Result<PathBuf, String> {
    let existed = dir.is_dir();
    fs::create_dir_all(dir).map_err(|err| {
        format!(
            "cannot create diagnostics directory {}: {err}",
            dir.display()
        )
    })?;
    #[cfg(unix)]
    if !existed {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).map_err(|err| {
            format!(
                "cannot restrict diagnostics directory {}: {err}",
                dir.display()
            )
        })?;
    }
    Ok(unique_output_path(dir))
}

fn unique_output_path(dir: &Path) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let pid = std::process::id();
    for attempt in 0..100u32 {
        let name = if attempt == 0 {
            format!("cosh-diagnostic-{nanos}-{pid}.json")
        } else {
            format!("cosh-diagnostic-{nanos}-{pid}-{attempt}.json")
        };
        let candidate = dir.join(name);
        if !candidate.exists() {
            return candidate;
        }
    }
    dir.join(format!("cosh-diagnostic-{nanos}-{pid}-fallback.json"))
}

/// Run `cosh-shell diagnostics export` with a hard timeout. stdin/stdout/
/// stderr are detached: the command is non-interactive and its chatter must
/// not leak into the bug report.
fn run_export(binary: &Path, output: &Path, timeout: Duration) -> Result<(), String> {
    let mut child = spawn_export(binary, output)?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => {
                return Err(format!("diagnostics export exited with {status}"));
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!(
                        "diagnostics export timed out after {}s",
                        timeout.as_secs()
                    ));
                }
                std::thread::sleep(POLL_INTERVAL);
            }
            Err(err) => return Err(format!("failed to wait on diagnostics export: {err}")),
        }
    }
}

/// Spawn the exporter, retrying a transient ETXTBSY: a package manager
/// atomically replacing the binary (or a just-written test double) keeps the
/// image busy for a few milliseconds, and a read-only diagnostic launch may
/// safely wait that out.
fn spawn_export(binary: &Path, output: &Path) -> Result<std::process::Child, String> {
    const ETXTBSY: i32 = 26;
    let mut attempt = 0;
    loop {
        let result = Command::new(binary)
            .arg("diagnostics")
            .arg("export")
            .arg("--output")
            .arg(output)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        match result {
            Ok(child) => return Ok(child),
            Err(err) if err.raw_os_error() == Some(ETXTBSY) && attempt < 3 => {
                attempt += 1;
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(err) => {
                return Err(format!("failed to launch {}: {err}", binary.display()));
            }
        }
    }
}

struct ParsedBundle {
    overall_severity: Option<String>,
    findings: Vec<FindingSummary>,
    unavailable_collectors: Vec<String>,
    manifest: Vec<ManifestSummary>,
}

/// Validate the bundle against the stable schema markers before trusting any
/// field. Unknown-but-valid bundles degrade rather than guess.
fn parse_bundle(path: &Path) -> Result<ParsedBundle, String> {
    let text = fs::read_to_string(path)
        .map_err(|err| format!("cannot read exported bundle {}: {err}", path.display()))?;
    let value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|err| format!("exported bundle is not valid JSON: {err}"))?;
    let format = value.get("format").and_then(|v| v.as_str());
    if format != Some(BUNDLE_FORMAT) {
        return Err(format!(
            "exported bundle has unexpected format marker {format:?}"
        ));
    }
    let version = value.get("version").and_then(|v| v.as_u64()).unwrap_or(0);
    if version != BUNDLE_VERSION {
        return Err(format!(
            "unsupported bundle version {version} (expected {BUNDLE_VERSION})"
        ));
    }

    let manifest = value
        .get("manifest")
        .and_then(|v| v.as_array())
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| {
                    Some(ManifestSummary {
                        source: entry.get("source")?.as_str()?.to_string(),
                        status: entry
                            .get("status")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown")
                            .to_string(),
                        items: entry.get("items").and_then(|v| v.as_u64()).unwrap_or(0),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let health = value.get("sources").and_then(|s| s.get("health"));
    let overall_severity = health
        .and_then(|h| h.get("overall_severity"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let findings = health
        .and_then(|h| h.get("findings"))
        .and_then(|v| v.as_array())
        .map(|findings| {
            findings
                .iter()
                .filter_map(|finding| {
                    Some(FindingSummary {
                        id: finding.get("id")?.as_str()?.to_string(),
                        severity: finding
                            .get("severity")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown")
                            .to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let unavailable_collectors = health
        .and_then(|h| h.get("unavailable"))
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let collector = item.get("collector")?.as_str()?;
                    let reason = item
                        .get("reason")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");
                    Some(format!("{collector} ({reason})"))
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(ParsedBundle {
        overall_severity,
        findings,
        unavailable_collectors,
        manifest,
    })
}

fn fold_home_under(path: &Path, home: &Path) -> PathBuf {
    if home.as_os_str().is_empty() {
        return path.to_path_buf();
    }
    match path.strip_prefix(home) {
        Ok(rest) if rest.as_os_str().is_empty() => PathBuf::from("~"),
        Ok(rest) => Path::new("~").join(rest),
        Err(_) => path.to_path_buf(),
    }
}

/// Fold occurrences of the caller's home directory inside an outward-facing
/// reason string to `~`. Reasons embed raw paths from the filesystem (the
/// override binary, the diagnostics directory, the bundle), and the reason
/// lands in Markdown/JSON that may be pasted into a public issue.
///
/// Folding happens only at a full path boundary — the match must start at a
/// word boundary and be followed by `/` or the end of the string — so a
/// sibling like `/home/alice2` is never mangled into `~2`.
fn fold_reason(reason: &str, home: &Path) -> String {
    let home_str = home.as_os_str().to_string_lossy();
    // A trailing separator in HOME (`/home/alice/`) must not defeat
    // boundary matching — the character after the match would be the first
    // path component, not `/`. Normalize it away first; `/` itself trims
    // to empty and keeps the early return below.
    let home_str = home_str.trim_end_matches('/');
    if home_str.is_empty() {
        return reason.to_string();
    }
    let mut out = String::with_capacity(reason.len());
    let mut pos = 0;
    while let Some(relative) = reason[pos..].find(home_str) {
        let start = pos + relative;
        let end = start + home_str.len();
        // `home_str` starts with '/', so a match abutting another
        // path-component character (letters, digits, `. _ - ~`) is a suffix
        // of a longer path, not the home directory.
        let preceded = start == 0
            || !matches!(
                reason.as_bytes()[start - 1],
                b'0'..=b'9' | b'a'..=b'z' | b'A'..=b'Z' | b'.' | b'_' | b'-' | b'~'
            );
        let followed = end == reason.len() || reason.as_bytes()[end] == b'/';
        if preceded && followed {
            out.push_str(&reason[pos..start]);
            out.push('~');
        } else {
            out.push_str(&reason[pos..end]);
        }
        pos = end;
    }
    out.push_str(&reason[pos..]);
    out
}

/// POSIX-shell-quote one argument of a copyable report command.
///
/// Display paths are home-folded for privacy, so a leading `~` or `~/`
/// stays unquoted to preserve tilde expansion; the rest is left bare when
/// it uses only the portable safe set and single-quoted otherwise (`'`
/// itself becomes `'\''`). Without this, a `COSH_SHELL_BIN` override or an
/// install prefix containing spaces or metacharacters would render a
/// "copyable" command the shell splits into garbage.
pub(super) fn shell_quote(arg: &str) -> String {
    if arg == "~" {
        return arg.to_string();
    }
    let (prefix, rest) = match arg.strip_prefix("~/") {
        Some(rest) => ("~/", rest),
        None => ("", arg),
    };
    if !rest.is_empty() && rest.bytes().all(is_shell_safe) {
        return arg.to_string();
    }
    let mut out = String::with_capacity(rest.len() + 2);
    out.push_str(prefix);
    out.push('\'');
    for ch in rest.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

/// Characters POSIX leaves unquoted without ambiguity (tilde is only
/// special at the start of a word, which the `~/` handling above covers).
const fn is_shell_safe(byte: u8) -> bool {
    matches!(
        byte,
        b'a'..=b'z'
            | b'A'..=b'Z'
            | b'0'..=b'9'
            | b'_'
            | b'@'
            | b'%'
            | b'+'
            | b'='
            | b':'
            | b','
            | b'.'
            | b'/'
            | b'-'
            | b'~'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic user layout below a temporary root (no XDG env reads).
    fn test_layout(root: &Path) -> FsLayout {
        FsLayout::user_with_overrides(root.join("home"), None, None, None, None, None)
    }

    #[cfg(unix)]
    fn write_executable(path: &Path, body: &str) {
        use std::io::Write as _;
        use std::os::unix::fs::PermissionsExt;
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        let mut file = fs::File::create(path).expect("create executable");
        file.write_all(body.as_bytes()).expect("write executable");
        file.sync_all().expect("sync executable");
        drop(file);
        let mut perms = fs::metadata(path).expect("metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(path, perms).expect("chmod");
    }

    #[cfg(unix)]
    fn write_fake_cosh_shell(dir: &Path, body: &str) -> PathBuf {
        let path = dir.join("fake-cosh-shell");
        write_executable(&path, body);
        path
    }

    const VALID_BUNDLE: &str = r#"{
        "format": "cosh-diagnostic-bundle",
        "version": 1,
        "created_at_ms": 1,
        "manifest": [
            {"source": "environment", "status": "included", "items": 1, "detail": null},
            {"source": "logs", "status": "partial", "items": 3, "detail": "1 error"}
        ],
        "sources": {
            "environment": {},
            "configuration": {},
            "health": {
                "overall_severity": "warning",
                "findings": [
                    {"id": "hooks.user_command_not_found", "severity": "warning"},
                    {"id": "provider.unreachable", "severity": "critical"}
                ],
                "unavailable": [
                    {"collector": "pty", "reason": "unsupported", "severity": "unavailable"}
                ]
            },
            "recent_events": [],
            "logs": [],
            "crashes": []
        }
    }"#;

    fn export_script(json: &str) -> String {
        // Parse --output from the export argv and write the canned bundle.
        format!(
            "#!/bin/sh\n\
             while [ $# -gt 0 ]; do\n\
             \x20 if [ \"$1\" = \"--output\" ]; then shift; out=\"$1\"; fi\n\
             \x20 shift\n\
             done\n\
             \x20 cat > \"$out\" <<'JSON'\n{json}\nJSON\n"
        )
    }

    fn collect_ok(binary: &Path, dir: &Path, home: &Path, timeout: Duration) -> CoshNgDiagnostics {
        let manual = format!(
            "{BIN_NAME} diagnostics export --output {}",
            fold_home_under(&dir.join("cosh-diagnostic-manual.json"), home).display()
        );
        collect_with(binary, dir, home, timeout, manual)
    }

    #[cfg(unix)]
    #[test]
    fn collect_available_summarizes_bundle() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let dir = home.join("diagnostics");
        let bin = write_fake_cosh_shell(tmp.path(), &export_script(VALID_BUNDLE));

        let result = collect_ok(&bin, &dir, &home, Duration::from_secs(10));

        let CoshNgDiagnostics::Available {
            binary_path,
            bundle_path,
            overall_severity,
            findings,
            unavailable_collectors,
            manifest,
        } = result
        else {
            panic!("expected available diagnostics: {result:?}");
        };
        assert!(binary_path.ends_with("fake-cosh-shell"), "{binary_path}");
        assert!(bundle_path.starts_with("~/diagnostics/"), "{bundle_path}");
        assert!(bundle_path.ends_with(".json"), "{bundle_path}");
        assert!(bundle_path.contains("cosh-diagnostic-"), "{bundle_path}");
        assert_eq!(overall_severity.as_deref(), Some("warning"));
        assert_eq!(
            findings,
            vec![
                FindingSummary {
                    id: "hooks.user_command_not_found".to_string(),
                    severity: "warning".to_string(),
                },
                FindingSummary {
                    id: "provider.unreachable".to_string(),
                    severity: "critical".to_string(),
                },
            ]
        );
        assert_eq!(
            unavailable_collectors,
            vec!["pty (unsupported)".to_string()]
        );
        assert_eq!(
            manifest,
            vec![
                ManifestSummary {
                    source: "environment".to_string(),
                    status: "included".to_string(),
                    items: 1,
                },
                ManifestSummary {
                    source: "logs".to_string(),
                    status: "partial".to_string(),
                    items: 3,
                },
            ]
        );
    }

    #[test]
    fn collect_missing_binary_is_unavailable() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let dir = home.join("diagnostics");

        let result = collect_impl(None, Some(dir), &home, Duration::from_secs(10));

        let CoshNgDiagnostics::Unavailable {
            reason,
            manual_command,
        } = result
        else {
            panic!("expected unavailable diagnostics: {result:?}");
        };
        assert!(reason.contains("cosh-shell binary not found"), "{reason}");
        assert!(reason.contains(BIN_ENV), "{reason}");
        assert!(reason.contains("libexec"), "{reason}");
        assert!(
            manual_command.starts_with("cosh-shell diagnostics export --output ~/"),
            "{manual_command}"
        );
        assert!(
            !tmp.path().join("home/diagnostics").exists(),
            "a missing binary must not create the diagnostics directory"
        );
    }

    #[test]
    fn collect_without_home_is_unavailable() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let bin = tmp.path().join("cosh-shell");

        let result = collect_impl(
            Some(bin.clone()),
            None,
            Path::new(""),
            Duration::from_secs(10),
        );

        let CoshNgDiagnostics::Unavailable {
            reason,
            manual_command,
        } = result
        else {
            panic!("expected unavailable diagnostics: {result:?}");
        };
        assert!(reason.contains("HOME is not set"), "{reason}");
        // The manual command still names the resolved binary; with an empty
        // HOME nothing is folded.
        assert_eq!(
            manual_command,
            format!("{} diagnostics export", bin.display())
        );
    }

    #[cfg(unix)]
    #[test]
    fn collect_export_failure_is_unavailable() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let dir = home.join("diagnostics");
        let bin = write_fake_cosh_shell(tmp.path(), "#!/bin/sh\nexit 1\n");

        let result = collect_ok(&bin, &dir, &home, Duration::from_secs(10));

        let CoshNgDiagnostics::Unavailable { reason, .. } = result else {
            panic!("expected unavailable diagnostics: {result:?}");
        };
        assert!(reason.contains("exited with"), "{reason}");
    }

    #[cfg(unix)]
    #[test]
    fn collect_export_timeout_is_unavailable() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let dir = home.join("diagnostics");
        let bin = write_fake_cosh_shell(tmp.path(), "#!/bin/sh\nsleep 30\n");

        let started = Instant::now();
        let result = collect_ok(&bin, &dir, &home, Duration::from_millis(300));

        let CoshNgDiagnostics::Unavailable { reason, .. } = result else {
            panic!("expected unavailable diagnostics: {result:?}");
        };
        assert!(reason.contains("timed out"), "{reason}");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the hung child must be killed, not awaited: {:?}",
            started.elapsed()
        );
    }

    #[cfg(unix)]
    #[test]
    fn collect_invalid_bundle_is_unavailable() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let dir = home.join("diagnostics");
        let bin = write_fake_cosh_shell(tmp.path(), &export_script("not json"));

        let result = collect_ok(&bin, &dir, &home, Duration::from_secs(10));

        let CoshNgDiagnostics::Unavailable { reason, .. } = result else {
            panic!("expected unavailable diagnostics: {result:?}");
        };
        assert!(reason.contains("not valid JSON"), "{reason}");
    }

    #[cfg(unix)]
    #[test]
    fn collect_format_mismatch_is_unavailable() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let dir = home.join("diagnostics");
        let bin = write_fake_cosh_shell(
            tmp.path(),
            &export_script(r#"{"format":"other","version":1}"#),
        );

        let result = collect_ok(&bin, &dir, &home, Duration::from_secs(10));

        let CoshNgDiagnostics::Unavailable { reason, .. } = result else {
            panic!("expected unavailable diagnostics: {result:?}");
        };
        assert!(reason.contains("unexpected format marker"), "{reason}");
    }

    #[cfg(unix)]
    #[test]
    fn collect_never_overwrites_an_existing_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let dir = home.join("diagnostics");
        fs::create_dir_all(&dir).expect("diagnostics dir");
        let sentinel = unique_output_path(&dir);
        fs::write(&sentinel, "keep me").expect("sentinel");

        let bin = write_fake_cosh_shell(tmp.path(), &export_script(VALID_BUNDLE));
        let result = collect_ok(&bin, &dir, &home, Duration::from_secs(10));

        assert!(matches!(result, CoshNgDiagnostics::Available { .. }));
        assert_eq!(
            fs::read_to_string(&sentinel).expect("sentinel"),
            "keep me",
            "an existing file must survive the export"
        );
    }

    /// Production chain for a staged RPM-style system install: the binary
    /// resolves from the private libexec layout (PATH stays empty), and the
    /// bundle lands in the *calling user's* writable state directory — the
    /// root-owned system state root is never touched.
    #[cfg(unix)]
    #[test]
    fn system_scope_install_resolves_libexec_and_writes_to_user_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let system_layout = FsLayout::system(Some(tmp.path().join("sysroot")));
        let libexec_bin = system_layout.libexec_dir.join(LIBEXEC_SUBPATH);
        write_executable(&libexec_bin, &export_script(VALID_BUNDLE));

        let resolved = resolve_binary_with(None, &system_layout, None);
        assert_eq!(resolved.as_deref(), Some(libexec_bin.as_path()));

        let user_layout = test_layout(tmp.path());
        let user_dir = user_layout.state_dir.join("diagnostics");
        let home = tmp.path().join("home");
        let result = collect_impl(
            resolved,
            Some(user_dir.clone()),
            &home,
            Duration::from_secs(10),
        );

        let CoshNgDiagnostics::Available {
            binary_path,
            bundle_path,
            ..
        } = result
        else {
            panic!("expected available diagnostics: {result:?}");
        };
        assert_eq!(binary_path, libexec_bin.display().to_string());
        assert!(bundle_path.starts_with("~/"), "{bundle_path}");
        let written: Vec<_> = fs::read_dir(&user_dir)
            .expect("user diagnostics dir")
            .collect();
        assert_eq!(written.len(), 1, "exactly one bundle in the user dir");
        assert!(
            !system_layout.state_dir.join("diagnostics").exists(),
            "the system state root must stay untouched"
        );
    }

    /// Raw (user-mode) installs place the binary below
    /// `~/.local/lib/anolisa/libexec`; resolution must find it there too.
    #[cfg(unix)]
    #[test]
    fn user_scope_install_resolves_libexec() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let user_layout = test_layout(tmp.path());
        let libexec_bin = user_layout.libexec_dir.join(LIBEXEC_SUBPATH);
        write_executable(&libexec_bin, "#!/bin/sh\n");

        let resolved = resolve_binary_with(None, &user_layout, None);

        assert_eq!(resolved.as_deref(), Some(libexec_bin.as_path()));
    }

    #[cfg(unix)]
    #[test]
    fn env_override_wins_over_libexec() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let user_layout = test_layout(tmp.path());
        let libexec_bin = user_layout.libexec_dir.join(LIBEXEC_SUBPATH);
        write_executable(&libexec_bin, "#!/bin/sh\n");
        let override_bin = tmp.path().join("elsewhere/cosh-shell");

        let resolved = resolve_binary_with(Some(override_bin.clone()), &user_layout, None);

        assert_eq!(resolved, Some(override_bin));
    }

    /// Staged RPM layout: the spec installs to `%{_libexecdir}/anolisa/cosh-ng`
    /// (`/usr/libexec/...`), which the raw-contract libexec probe misses.
    /// Resolution must find the RPM binary, execute it for the export, and
    /// render copyable commands with that absolute path.
    #[cfg(unix)]
    #[test]
    fn rpm_layout_resolves_and_flows_into_commands() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let system_layout = FsLayout::system(Some(tmp.path().join("sysroot")));
        let rpm_bin = system_layout.prefix.join(RPM_LIBEXEC_SUBPATH);
        assert!(
            rpm_bin.starts_with(tmp.path().join("sysroot/usr/libexec")),
            "RPM candidate must be the /usr/libexec location: {}",
            rpm_bin.display()
        );
        write_executable(&rpm_bin, "#!/bin/sh\nexit 1\n");

        let resolved = resolve_binary_with(None, &system_layout, None);
        assert_eq!(resolved.as_deref(), Some(rpm_bin.as_path()));

        // A failing export must still hand the user a runnable command: the
        // resolved absolute RPM path, not a bare `cosh-shell` that PATH lacks.
        let home = tmp.path().join("home");
        let user_dir = test_layout(tmp.path()).state_dir.join("diagnostics");
        let result = collect_impl(resolved, Some(user_dir), &home, Duration::from_secs(10));

        let CoshNgDiagnostics::Unavailable {
            reason,
            manual_command,
        } = result
        else {
            panic!("expected unavailable diagnostics: {result:?}");
        };
        assert!(reason.contains("exited with"), "{reason}");
        assert!(
            manual_command.starts_with(&format!("{} diagnostics export", rpm_bin.display())),
            "manual command must carry the resolved RPM path: {manual_command}"
        );
    }

    /// When both the raw contract and the RPM location exist, the raw
    /// contract wins (preference order is unchanged from the previous round).
    #[cfg(unix)]
    #[test]
    fn raw_libexec_wins_over_rpm_location() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let system_layout = FsLayout::system(Some(tmp.path().join("sysroot")));
        let raw_bin = system_layout.libexec_dir.join(LIBEXEC_SUBPATH);
        let rpm_bin = system_layout.prefix.join(RPM_LIBEXEC_SUBPATH);
        write_executable(&raw_bin, "#!/bin/sh\n");
        write_executable(&rpm_bin, "#!/bin/sh\n");

        let resolved = resolve_binary_with(None, &system_layout, None);

        assert_eq!(resolved.as_deref(), Some(raw_bin.as_path()));
    }

    #[cfg(unix)]
    #[test]
    fn resolve_falls_back_to_path() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let user_layout = test_layout(tmp.path());
        let path_dir = tmp.path().join("path-bin");
        write_executable(&path_dir.join(BIN_NAME), "#!/bin/sh\n");

        let resolved = resolve_binary_with(
            None,
            &user_layout,
            Some(OsString::from(path_dir.as_os_str())),
        );

        assert_eq!(resolved, Some(path_dir.join(BIN_NAME)));
    }

    #[test]
    fn resolve_returns_none_when_nowhere_found() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let user_layout = test_layout(tmp.path());

        let resolved = resolve_binary_with(
            None,
            &user_layout,
            Some(OsString::from(tmp.path().join("empty-path").as_os_str())),
        );

        assert_eq!(resolved, None);
    }

    /// A launch failure must not leak the raw override path (which may be the
    /// user's home) into Markdown/JSON bound for a public issue.
    #[test]
    fn launch_failure_reason_folds_home_paths() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home").join("alice");
        let dir = home.join("diagnostics");
        let missing = home.join("bin/cosh-shell");

        let result = collect_ok(&missing, &dir, &home, Duration::from_secs(10));

        let CoshNgDiagnostics::Unavailable { reason, .. } = result else {
            panic!("expected unavailable diagnostics: {result:?}");
        };
        assert!(reason.contains("~/bin/cosh-shell"), "{reason}");
        assert!(
            !reason.contains(home.to_string_lossy().as_ref()),
            "raw home path leaked: {reason}"
        );
    }

    /// A diagnostics-directory creation failure must not leak the raw
    /// directory path either.
    #[cfg(unix)]
    #[test]
    fn dir_creation_failure_reason_folds_home_paths() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home").join("alice");
        let blocker = home.join("blocker");
        fs::create_dir_all(&home).expect("home");
        fs::write(&blocker, "not a directory").expect("blocker");
        let dir = blocker.join("diagnostics");
        let bin = tmp.path().join("cosh-shell");

        let result = collect_ok(&bin, &dir, &home, Duration::from_secs(10));

        let CoshNgDiagnostics::Unavailable { reason, .. } = result else {
            panic!("expected unavailable diagnostics: {result:?}");
        };
        assert!(
            reason.contains("cannot create diagnostics directory"),
            "{reason}"
        );
        assert!(reason.contains("~/blocker/diagnostics"), "{reason}");
        assert!(
            !reason.contains(home.to_string_lossy().as_ref()),
            "raw home path leaked: {reason}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn new_diagnostics_directory_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("diagnostics");

        fresh_output_path(&dir).expect("fresh path");

        let mode = fs::metadata(&dir).expect("metadata").permissions().mode();
        assert_eq!(mode & 0o777, 0o700, "diagnostics dir must be 0700");
    }

    #[test]
    fn unique_output_path_skips_existing_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let first = unique_output_path(tmp.path());
        fs::write(&first, "occupied").expect("occupy");

        let second = unique_output_path(tmp.path());

        assert_ne!(first, second);
        assert!(!second.exists());
    }

    #[test]
    fn fold_home_under_replaces_prefix() {
        let home = Path::new("/home/user");
        assert_eq!(
            fold_home_under(Path::new("/home/user/.x/bundle.json"), home),
            PathBuf::from("~/.x/bundle.json")
        );
        assert_eq!(
            fold_home_under(Path::new("/home/user"), home),
            PathBuf::from("~")
        );
        assert_eq!(
            fold_home_under(Path::new("/var/tmp/bundle.json"), home),
            PathBuf::from("/var/tmp/bundle.json")
        );
        assert_eq!(
            fold_home_under(Path::new("/home/user2/bundle.json"), home),
            PathBuf::from("/home/user2/bundle.json")
        );
    }

    #[test]
    fn fold_home_under_ignores_empty_home() {
        assert_eq!(
            fold_home_under(Path::new("/anywhere.json"), Path::new("")),
            PathBuf::from("/anywhere.json")
        );
    }

    #[test]
    fn fold_reason_replaces_home_occurrences() {
        let home = Path::new("/home/alice");
        let reason = "failed to launch /home/alice/bin/cosh-shell: os error 2";
        assert_eq!(
            fold_reason(reason, home),
            "failed to launch ~/bin/cosh-shell: os error 2"
        );
        assert_eq!(fold_reason("no paths here", home), "no paths here");
        assert_eq!(
            fold_reason("failed to launch /opt/x: err", Path::new("")),
            "failed to launch /opt/x: err"
        );
    }

    /// HOME folding applies only at full path boundaries: `/home/alice2`
    /// must never become `~2`, while an occurrence that is exactly `$HOME`
    /// or `$HOME/...` still folds.
    #[test]
    fn fold_reason_respects_path_boundaries() {
        let home = Path::new("/home/alice");

        // Sibling prefix: not a boundary, must stay untouched.
        assert_eq!(
            fold_reason(
                "failed to launch /home/alice2/bin/cosh-shell: os error 2",
                home
            ),
            "failed to launch /home/alice2/bin/cosh-shell: os error 2"
        );
        // Exact home at end of string folds.
        assert_eq!(
            fold_reason("cannot create diagnostics directory /home/alice", home),
            "cannot create diagnostics directory ~"
        );
        // Mid-string occurrence followed by a separator folds.
        assert_eq!(
            fold_reason("read /home/alice/x.json failed", home),
            "read ~/x.json failed"
        );
        // A home-shaped suffix of a longer path is not the home directory.
        assert_eq!(
            fold_reason("read /var/chroot/home/alice/x failed", home),
            "read /var/chroot/home/alice/x failed"
        );
        // Dashed sibling is likewise untouched.
        assert_eq!(
            fold_reason("read /home/alice-old/x failed", home),
            "read /home/alice-old/x failed"
        );
    }

    /// A trailing separator in HOME must not defeat folding:
    /// `/home/alice/` still folds `/home/alice/bin` to `~/bin`, while the
    /// sibling-prefix guard and the `/` root early return keep working.
    #[test]
    fn fold_reason_normalizes_trailing_separator_in_home() {
        let home = Path::new("/home/alice/");
        assert_eq!(
            fold_reason(
                "failed to launch /home/alice/bin/cosh-shell: os error 2",
                home
            ),
            "failed to launch ~/bin/cosh-shell: os error 2"
        );
        assert_eq!(
            fold_reason("read /home/alice2/x failed", home),
            "read /home/alice2/x failed"
        );
        assert_eq!(
            fold_reason("read /x failed", Path::new("/")),
            "read /x failed"
        );
        assert_eq!(
            fold_reason("read /x failed", Path::new("")),
            "read /x failed"
        );
    }

    #[test]
    fn shell_quote_leaves_safe_arguments_untouched() {
        assert_eq!(shell_quote(BIN_NAME), BIN_NAME);
        assert_eq!(
            shell_quote("/usr/libexec/anolisa/cosh-ng/cosh-shell"),
            "/usr/libexec/anolisa/cosh-ng/cosh-shell"
        );
        // A bare `~` must keep expanding to the caller's home.
        assert_eq!(shell_quote("~"), "~");
        assert_eq!(
            shell_quote("~/.local/state/anolisa/diagnostics/cosh-diagnostic-manual.json"),
            "~/.local/state/anolisa/diagnostics/cosh-diagnostic-manual.json"
        );
    }

    #[test]
    fn shell_quote_quotes_spaces_and_metacharacters() {
        assert_eq!(
            shell_quote("/opt/my dir/cosh-shell"),
            "'/opt/my dir/cosh-shell'"
        );
        assert_eq!(shell_quote("/opt/a;b/cosh-shell"), "'/opt/a;b/cosh-shell'");
        // `$` must not expand inside the rendered command.
        assert_eq!(
            shell_quote("/opt/$HOME/cosh-shell"),
            "'/opt/$HOME/cosh-shell'"
        );
        // The folded `~/` prefix stays bare so tilde expansion still works.
        assert_eq!(shell_quote("~/my dir/cosh-shell"), "~/'my dir/cosh-shell'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
        assert_eq!(shell_quote(""), "''");
    }

    /// The manual retry command is a promise: with a binary and an output
    /// directory whose paths contain spaces and shell metacharacters, the
    /// rendered command must still run verbatim. The test executes it with
    /// `sh -c` and checks the bundle lands at the quoted `--output` path.
    #[cfg(unix)]
    #[test]
    fn manual_command_with_metachar_paths_executes_verbatim() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let dir = tmp.path().join("state dir$x").join("diagnostics");
        let bin = write_fake_cosh_shell(
            &tmp.path().join("bin dir;$(rm -rf)"),
            &export_script("not json"),
        );

        let result = collect_impl(
            Some(bin.clone()),
            Some(dir.clone()),
            &home,
            Duration::from_secs(10),
        );

        let CoshNgDiagnostics::Unavailable { manual_command, .. } = result else {
            panic!("an invalid bundle must degrade to unavailable: {result:?}");
        };
        assert!(
            manual_command.contains('\''),
            "metachar paths must be quoted: {manual_command}"
        );

        let before: std::collections::BTreeSet<_> = fs::read_dir(&dir)
            .expect("diagnostics dir")
            .map(|entry| entry.expect("entry").file_name())
            .collect();
        let status = Command::new("sh")
            .arg("-c")
            .arg(&manual_command)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("run the rendered manual command");
        assert!(
            status.success(),
            "rendered manual command must run: {manual_command}"
        );
        let new_files: Vec<_> = fs::read_dir(&dir)
            .expect("diagnostics dir")
            .map(|entry| entry.expect("entry").file_name())
            .filter(|name| !before.contains(name))
            .collect();
        assert_eq!(
            new_files.len(),
            1,
            "the quoted --output path must receive exactly one fresh bundle"
        );
    }

    /// The retry command stays repeatable: a bundle left by an earlier
    /// export (including one at the legacy fixed name) must not trip the
    /// exporter's no-overwrite guard when the rendered command is run.
    #[cfg(unix)]
    #[test]
    fn manual_command_stays_repeatable_with_existing_bundles() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        // Keep the diagnostics dir outside `home` so the rendered command
        // carries an absolute path (`~` would expand against the test
        // runner's real HOME when executed below).
        let dir = tmp.path().join("state").join("diagnostics");
        fs::create_dir_all(&dir).expect("diagnostics dir");
        let legacy = dir.join("cosh-diagnostic-manual.json");
        fs::write(&legacy, "old bundle").expect("legacy bundle");
        let bin = write_fake_cosh_shell(tmp.path(), &export_script("not json"));

        let result = collect_impl(Some(bin), Some(dir.clone()), &home, Duration::from_secs(10));

        let CoshNgDiagnostics::Unavailable { manual_command, .. } = result else {
            panic!("an invalid bundle must degrade to unavailable: {result:?}");
        };
        assert!(
            !manual_command.contains("cosh-diagnostic-manual.json"),
            "the retry command must not reuse the legacy fixed name: {manual_command}"
        );

        let before: std::collections::BTreeSet<_> = fs::read_dir(&dir)
            .expect("diagnostics dir")
            .map(|entry| entry.expect("entry").file_name())
            .collect();
        let status = Command::new("sh")
            .arg("-c")
            .arg(&manual_command)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("run the rendered retry command");
        assert!(status.success(), "retry command must run: {manual_command}");
        let new_files: Vec<_> = fs::read_dir(&dir)
            .expect("diagnostics dir")
            .map(|entry| entry.expect("entry").file_name())
            .filter(|name| !before.contains(name))
            .collect();
        assert_eq!(new_files.len(), 1, "the retry must write a fresh bundle");
        assert_eq!(
            fs::read_to_string(&legacy).expect("legacy bundle"),
            "old bundle",
            "the pre-existing bundle must survive"
        );
    }
}
