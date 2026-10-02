//! Dormant environment-readiness implementation for Tool Ready.
//!
//! Release builds hard-bypass the legacy checks before loading a specification,
//! repairing the environment, or producing a blocking result. The retained
//! implementation remains covered by unit tests for a future redesign.

use serde_json::Value;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::sync::LazyLock;

// Regex to extract the first version-like token from --version output.
// `split_whitespace().last()` is fragile: tools like `curl` print
// "curl 8.1.2 (x86_64-linux-gnu)" where `.last()` gives the arch suffix,
// and `jq-1.6` has no whitespace around the version. A targeted regex
// anchored on digit groups avoids these false matches.
static VERSION_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"(\d+\.\d+\.\d+)").unwrap());

fn tool_ready_hard_bypassed() -> bool {
    // Release builds always bypass Tool Ready. Unit tests retain a test-only
    // escape hatch so the dormant legacy implementation remains covered.
    #[cfg(test)]
    {
        !matches!(
            std::env::var("TOKENLESS_TOOL_READY_TEST_LEGACY").as_deref(),
            Ok("1")
        )
    }
    #[cfg(not(test))]
    {
        true
    }
}

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

#[cfg(unix)]
fn current_uid() -> u32 {
    // SAFETY: libc::getuid() is a pure syscall with no preconditions and never fails.
    unsafe { libc::getuid() }
}

#[cfg(unix)]
/// Check whether a file path is trusted for execution or reading.
///
/// Verifies: system path prefix → symlink target resolution → parent directory
/// owner/world-writable → file owner/world-writable.
///
/// KEEP IN SYNC with the shell equivalent in
/// `adapters/tokenless/common/hooks/tool_ready_hook.sh` (`is_trusted_file`).
/// Changes to trust criteria must be applied to both implementations.
#[allow(clippy::collapsible_if)]
fn is_trusted_path(path: &std::path::Path) -> bool {
    // System paths are always trusted
    if path.starts_with("/usr/share")
        || path.starts_with("/usr/libexec")
        || path.starts_with("/usr/lib/anolisa")
        || path.starts_with("/usr/local/share")
    {
        return true;
    }
    // Resolve symlink target before owner/perm checks
    let symlink_parent = if path.is_symlink() {
        path.parent().map(|p| p.to_path_buf())
    } else {
        None
    };
    let check_path = if path.is_symlink() {
        match fs::canonicalize(path) {
            Ok(resolved) => {
                // System targets are always trusted
                if resolved.starts_with("/usr/share")
                    || resolved.starts_with("/usr/libexec")
                    || resolved.starts_with("/usr/lib/anolisa")
                    || resolved.starts_with("/usr/local/share")
                {
                    return true;
                }
                resolved
            }
            Err(_) => return false,
        }
    } else {
        path.to_path_buf()
    };
    // Helper: check a directory for foreign ownership or world-writable bit.
    fn check_parent_dir(parent: &std::path::Path) -> bool {
        let parent_meta = match fs::symlink_metadata(parent) {
            Ok(m) => m,
            Err(_) => return false,
        };
        let parent_uid = parent_meta.uid();
        if parent_uid != current_uid() && parent_uid != 0 {
            return false;
        }
        if parent_meta.mode() & 0o002 != 0 {
            return false;
        }
        true
    }
    // Check the parent of the resolved target (or the file itself when not
    // a symlink) — a world-writable directory allows TOCTOU file replacement.
    if let Some(parent) = check_path.parent() {
        if !check_parent_dir(parent) {
            return false;
        }
    }
    // When the path is a symlink, also check the symlink's own parent
    // directory. If the symlink sits in a world-writable or foreign-owned
    // directory, an attacker can replace the symlink to point at a
    // malicious file, bypassing the target-level checks.
    if let Some(ref sp) = symlink_parent {
        if sp != check_path.parent().unwrap_or(std::path::Path::new("")) {
            if !check_parent_dir(sp) {
                return false;
            }
        }
    }
    match fs::symlink_metadata(&check_path) {
        Ok(meta) => {
            let file_uid = meta.uid();
            let current_uid = current_uid();
            if file_uid != current_uid && file_uid != 0 {
                return false;
            }
            let mode = meta.mode();
            if mode & 0o002 != 0 {
                return false;
            }
            true
        }
        Err(_) => false,
    }
}

#[cfg(not(unix))]
fn is_trusted_path(_path: &std::path::Path) -> bool {
    true
}

/// A single dependency entry — normalized from either string or object format.
#[derive(Debug, Clone)]
struct DepEntry {
    binary: String,
    version: Option<String>,
    package: String,
    apt_package: Option<String>,
    apk_package: Option<String>,
    manager: String,
    pip_name: Option<String>,
    uv_name: Option<String>,
    npm_name: Option<String>,
    use_npx: bool,
    fallback: Vec<FallbackEntry>,
}

/// A fallback install strategy.
#[derive(Debug, Clone)]
struct FallbackEntry {
    method: String,
    package: Option<String>,
    binary: Option<String>,
    source: Option<String>,
    manifest: Option<String>,
    features: Option<String>,
    url: Option<String>,
    args: Option<String>,
}

/// Per-tool dependency specification.
#[derive(Debug, Clone)]
struct ToolDepSpec {
    aliases: Vec<String>,
    required: Vec<DepEntry>,
    recommended: Vec<DepEntry>,
    config_files: Vec<String>,
    permissions: Vec<String>,
    network: Vec<String>,
}

/// Result of checking a single dependency item.
#[derive(Debug, Clone, PartialEq)]
enum DepStatus {
    Available,
    Missing,
    VersionLow { installed: String, required: String },
}

/// Overall readiness status for a tool.
#[derive(Debug, Clone, PartialEq)]
enum ReadyStatus {
    /// All required and recommended dependencies satisfied.
    Ready,
    /// Recommended deps missing but required deps OK — degraded but usable.
    Partial,
    /// Required deps or permissions missing — tool cannot function.
    NotReady,
    /// Tool not found in config dictionary — normal skip, no action needed.
    Unknown,
}

/// Combined result for a tool's environment check.
struct ToolReadyResult {
    tool_name: String,
    status: ReadyStatus,
    required_results: Vec<(DepEntry, DepStatus)>,
    recommended_results: Vec<(DepEntry, DepStatus)>,
    config_results: Vec<(String, bool)>,
    permission_results: Vec<(String, bool)>,
    network_results: Vec<(String, bool)>,
}

/// Normalize a JSON value (string or object) into a DepEntry.
/// String "jq" → DepEntry { binary: "jq", package: "jq", manager: "rpm" }
/// Object {binary, version, package, manager, ...} → DepEntry
fn normalize_dep(value: &Value) -> DepEntry {
    match value {
        Value::String(s) => {
            // Handle version constraints: "rtk>=0.35"
            if let Some(idx) = s.find(">=") {
                let binary = s[..idx].to_string();
                let version = Some(s[idx..].to_string());
                DepEntry {
                    binary,
                    version,
                    package: s[..idx].to_string(),
                    apt_package: None,
                    apk_package: None,
                    manager: "rpm".to_string(),
                    pip_name: None,
                    uv_name: None,
                    npm_name: None,
                    use_npx: false,
                    fallback: Vec::new(),
                }
            } else {
                DepEntry {
                    binary: s.clone(),
                    version: None,
                    package: s.clone(),
                    apt_package: None,
                    apk_package: None,
                    manager: "rpm".to_string(),
                    pip_name: None,
                    uv_name: None,
                    npm_name: None,
                    use_npx: false,
                    fallback: Vec::new(),
                }
            }
        }
        Value::Object(obj) => {
            let binary = obj
                .get("binary")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let version = obj
                .get("version")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let package = obj
                .get("package")
                .and_then(|v| v.as_str())
                .unwrap_or(&binary)
                .to_string();
            let apt_package = obj
                .get("apt_package")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let apk_package = obj
                .get("apk_package")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let manager = obj
                .get("manager")
                .and_then(|v| v.as_str())
                .unwrap_or("rpm")
                .to_string();
            let pip_name = obj
                .get("pip_name")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let uv_name = obj
                .get("uv_name")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let npm_name = obj
                .get("npm_name")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let use_npx = obj
                .get("use_npx")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            let fallback = obj
                .get("fallback")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|fb| {
                            if let Value::Object(fb_obj) = fb {
                                Some(FallbackEntry {
                                    method: fb_obj
                                        .get("method")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("")
                                        .to_string(),
                                    package: fb_obj
                                        .get("package")
                                        .and_then(|v| v.as_str())
                                        .map(|s| s.to_string()),
                                    binary: fb_obj
                                        .get("binary")
                                        .and_then(|v| v.as_str())
                                        .map(|s| s.to_string()),
                                    source: fb_obj
                                        .get("source")
                                        .and_then(|v| v.as_str())
                                        .map(|s| s.to_string()),
                                    manifest: fb_obj
                                        .get("manifest")
                                        .and_then(|v| v.as_str())
                                        .map(|s| s.to_string()),
                                    features: fb_obj
                                        .get("features")
                                        .and_then(|v| v.as_str())
                                        .map(|s| s.to_string()),
                                    url: fb_obj
                                        .get("url")
                                        .and_then(|v| v.as_str())
                                        .map(|s| s.to_string()),
                                    args: fb_obj
                                        .get("args")
                                        .and_then(|v| v.as_str())
                                        .map(|s| s.to_string()),
                                })
                            } else {
                                None
                            }
                        })
                        .collect()
                })
                .unwrap_or_default();

            DepEntry {
                binary,
                version,
                package,
                apt_package,
                apk_package,
                manager,
                pip_name,
                uv_name,
                npm_name,
                use_npx,
                fallback,
            }
        }
        _ => DepEntry {
            binary: "".to_string(),
            version: None,
            package: "".to_string(),
            apt_package: None,
            apk_package: None,
            manager: "rpm".to_string(),
            pip_name: None,
            uv_name: None,
            npm_name: None,
            use_npx: false,
            fallback: Vec::new(),
        },
    }
}

/// Normalize an array of dep values (strings or objects) into `Vec<DepEntry>`.
fn normalize_deps(array: &Value) -> Vec<DepEntry> {
    array
        .as_array()
        .map(|arr| arr.iter().map(normalize_dep).collect())
        .unwrap_or_default()
}

/// Detect the system's native package manager by checking the underlying
/// package management mechanism (rpm vs dpkg vs apk), then selecting the
/// best frontend within that family. Override via TOKENLESS_PACKAGE_MANAGER
/// env var (useful for testing).
fn detect_system_manager() -> String {
    if let Ok(mgr) = std::env::var("TOKENLESS_PACKAGE_MANAGER") {
        return mgr;
    }
    // Detect by underlying mechanism first, then pick frontend within family
    // rpm-based: prefer dnf (modern), then yum (legacy)
    // dpkg-based: apt-get
    // apk-based: apk
    let rpm_exists = Command::new("sh")
        .args(["-c", "command -v rpm"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    let dpkg_exists = Command::new("sh")
        .args(["-c", "command -v dpkg"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    let apk_exists = Command::new("sh")
        .args(["-c", "command -v apk"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if rpm_exists {
        // Pick best frontend: dnf (modern Fedora/RHEL 8+) > yum (legacy)
        if Command::new("sh")
            .args(["-c", "command -v dnf"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
        {
            return "dnf".to_string();
        }
        return "yum".to_string();
    }
    if dpkg_exists {
        return "apt".to_string();
    }
    if apk_exists {
        return "apk".to_string();
    }
    "rpm".to_string()
}

/// Resolve a semantic manager label to the actual system package manager.
/// "rpm" maps to the detected system manager; other labels pass through unchanged.
fn resolve_manager(manager: &str) -> String {
    if manager == "rpm" {
        detect_system_manager()
    } else {
        manager.to_string()
    }
}

/// Resolve the actual package name for the detected system manager.
/// When manager="rpm" (meaning auto-detect), the detected manager may be apt/apk,
/// and those systems have different package names. apt_package/apk_package override
/// the default package field when present.
fn resolve_package(dep: &DepEntry) -> String {
    let detected = resolve_manager(&dep.manager);
    if dep.manager == "rpm" {
        match detected.as_str() {
            "apt" => dep
                .apt_package
                .as_deref()
                .unwrap_or(&dep.package)
                .to_string(),
            "apk" => dep
                .apk_package
                .as_deref()
                .unwrap_or(&dep.package)
                .to_string(),
            _ => dep.package.clone(),
        }
    } else {
        dep.package.clone()
    }
}

/// Extract the required version from a constraint string like ">=0.35".
fn extract_required_version(version: &str) -> &str {
    version
        .strip_prefix(">=")
        .or_else(|| version.strip_prefix(">"))
        .unwrap_or(version)
}

/// Compare version strings (semver-like: major.minor.patch).
/// Handles prefixed versions like "v22.1.0" and build suffixes like "1.2.3-rc1".
fn version_ge(installed: &str, required: &str) -> bool {
    fn parse_ver(s: &str) -> Vec<u32> {
        let cleaned = s
            .trim()
            .strip_prefix('v')
            .or_else(|| s.trim().strip_prefix('V'))
            .unwrap_or(s.trim());
        cleaned
            .split('.')
            .filter_map(|seg| {
                let num_part = seg
                    .split(|c: char| !c.is_ascii_digit())
                    .next()
                    .unwrap_or("");
                num_part.parse().ok()
            })
            .collect()
    }
    let i_parts = parse_ver(installed);
    let r_parts = parse_ver(required);

    for i in 0..i_parts.len().max(r_parts.len()) {
        let iv = i_parts.get(i).copied().unwrap_or(0);
        let rv = r_parts.get(i).copied().unwrap_or(0);
        if iv > rv {
            return true;
        }
        if iv < rv {
            return false;
        }
    }
    true
}

// KEEP IN SYNC with common/hooks/hook_utils.py, tool_ready_hook.sh, OpenClaw's
// fallback constants, and the Codex standalone scripts. Makefile and the
// Anolisa component manifest define the supported layouts; the canonical
// order is user, /usr/local, /usr, then legacy.
fn binary_fallback_paths(binary: &str, home: &str) -> Vec<PathBuf> {
    let mut components = std::path::Path::new(binary).components();
    if !matches!(
        (components.next(), components.next()),
        (Some(std::path::Component::Normal(_)), None)
    ) {
        return Vec::new();
    }

    let home = std::path::Path::new(home);
    let user_home = (!home.as_os_str().is_empty() && home.is_absolute()).then_some(home);
    let mut paths = Vec::new();
    if let Some(home) = user_home {
        paths.push(home.join(".local/bin").join(binary));
        if binary == "rtk" {
            paths.extend([
                // Anolisa CLI user mode.
                home.join(".local/lib/anolisa/libexec/tokenless")
                    .join(binary),
                // Makefile user mode.
                home.join(".local/libexec/anolisa/tokenless").join(binary),
            ]);
        }
    }
    paths.push(PathBuf::from("/usr/local/bin").join(binary));
    if binary == "rtk" {
        // Anolisa CLI system mode.
        paths.push(PathBuf::from("/usr/local/libexec/anolisa/tokenless").join(binary));
    }
    paths.push(PathBuf::from("/usr/bin").join(binary));
    if binary == "rtk" {
        paths.extend([
            // Makefile system mode and RPM.
            PathBuf::from("/usr/libexec/anolisa/tokenless").join(binary),
            // Debian and pre-layout-migration compatibility.
            PathBuf::from("/usr/lib/anolisa/tokenless").join(binary),
        ]);
        if let Some(home) = user_home {
            paths.extend([
                home.join(".local/share/anolisa/tokenless").join(binary),
                home.join(".local/lib/anolisa/tokenless").join(binary),
            ]);
        }
    }

    paths
}

fn is_executable_file(path: &std::path::Path) -> bool {
    if !path.is_file() || !is_trusted_path(path) {
        return false;
    }
    std::fs::metadata(path)
        .map(|metadata| {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                metadata.permissions().mode() & 0o111 != 0
            }
            #[cfg(not(unix))]
            true
        })
        .unwrap_or(false)
}

pub(crate) fn resolve_binary_path(binary: &str) -> Option<String> {
    let which_result = Command::new("sh")
        .args(["-c", "command -v \"$1\"", "--", binary])
        .output();

    match which_result {
        Ok(output) if output.status.success() => {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            (!path.is_empty()).then_some(path)
        }
        _ => {
            // PATH lookup failed — cover Makefile, Anolisa user/system, and
            // package-manager layouts. Trust validation prevents a spoofed
            // user path from supplying the executable used below.
            let home = crate::get_home_dir();
            binary_fallback_paths(binary, &home)
                .into_iter()
                .find(|path| is_executable_file(path))
                .map(|path| path.to_string_lossy().into_owned())
        }
    }
}

pub(crate) fn resolve_rtk_path() -> Option<String> {
    let path_candidate = if let Ok(output) = Command::new("sh")
        .args(["-c", "command -v \"$1\"", "--", "rtk"])
        .output()
        && output.status.success()
    {
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        (!path.is_empty()).then(|| PathBuf::from(path))
    } else {
        None
    };
    select_rtk_path(path_candidate, &crate::get_home_dir())
}

fn select_rtk_path(path_candidate: Option<PathBuf>, home: &str) -> Option<String> {
    let mut candidates = path_candidate.into_iter().collect::<Vec<_>>();
    candidates.extend(binary_fallback_paths("rtk", home));
    candidates.into_iter().find_map(|path| {
        if !is_executable_file(&path) {
            return None;
        }
        let output = Command::new(&path).arg("--version").output().ok()?;
        if !output.status.success() {
            return None;
        }
        let version_text = format!(
            "{} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let version = VERSION_RE
            .captures(&version_text)
            .and_then(|captures| captures.get(1))?
            .as_str();
        version_ge(version, "0.35.0").then(|| path.to_string_lossy().into_owned())
    })
}

/// Check if a binary is available and meets version constraints.
fn check_dep(dep: &DepEntry) -> DepStatus {
    let binary_path = resolve_binary_path(&dep.binary);
    match binary_path {
        Some(path) => {
            if let Some(ref version) = dep.version {
                let required_version = extract_required_version(version);
                let version_output = Command::new(&path).arg("--version").output();
                let installed_version = match version_output {
                    Ok(out) => {
                        let stdout = String::from_utf8_lossy(&out.stdout);
                        // Use regex to find the first version-like token
                        // (digits.digits.digits), not split_whitespace().last()
                        // which fails for "curl 8.1.2 (x86_64...)" and "jq-1.6".
                        VERSION_RE
                            .find(&stdout)
                            .map(|m| m.as_str().to_string())
                            .unwrap_or_else(|| "0.0.0".to_string())
                    }
                    Err(_) => "0.0.0".to_string(),
                };

                if version_ge(&installed_version, required_version) {
                    DepStatus::Available
                } else {
                    DepStatus::VersionLow {
                        installed: installed_version,
                        required: required_version.to_string(),
                    }
                }
            } else {
                DepStatus::Available
            }
        }
        _ => DepStatus::Missing,
    }
}

/// Expand ~/... in paths to HOME directory.
/// Paths that escape via traversal (`~/../../etc/passwd`) are rejected and
/// the original path is returned unchanged. A component-based check is used
/// instead of canonicalize so the expansion still works for config paths
/// that have not been created yet.
fn expand_path(path: &str) -> String {
    if path == "~" || path.starts_with("~/") {
        if std::path::Path::new(path)
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return path.to_string();
        }
        let home = crate::get_home_dir();
        path.replacen("~", &home, 1)
    } else {
        path.to_string()
    }
}

/// Check if a config file exists.
fn check_config_file(path: &str) -> bool {
    let expanded = expand_path(path);
    fs::metadata(&expanded).is_ok()
}

/// Check a permission type.
fn check_permission(perm: &str) -> bool {
    match perm {
        "file_read" => fs::read_to_string("/proc/self/status").is_ok(),
        "file_write" => {
            let test_path =
                std::env::temp_dir().join(format!(".tokenless-ready-test-{}", std::process::id()));
            let can_write = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&test_path)
                .is_ok();
            if can_write {
                let _ = fs::remove_file(&test_path);
            }
            can_write
        }
        "exec_shell" => resolve_binary_path("bash").is_some(),
        _ => true,
    }
}

/// Check network connectivity.
fn check_network(net: &str) -> bool {
    match net {
        "https_outbound" => Command::new("curl")
            .args(["-s", "--max-time", "2", "https://example.com"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false),
        _ => true,
    }
}

/// Load tool-ready-spec.json with both string and object format support.
fn load_spec(
    spec_path: &PathBuf,
) -> Result<std::collections::HashMap<String, ToolDepSpec>, String> {
    let content =
        fs::read_to_string(spec_path).map_err(|e| format!("Failed to read spec file: {e}"))?;
    let value: Value =
        serde_json::from_str(&content).map_err(|e| format!("Failed to parse spec JSON: {e}"))?;

    let mut specs = std::collections::HashMap::new();
    // Skip _comment key
    if let Value::Object(obj) = value {
        for (tool_name, tool_spec) in obj {
            if tool_name.starts_with('_') {
                continue;
            }
            if let Value::Object(spec_obj) = tool_spec {
                let aliases = spec_obj
                    .get("aliases")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                let required = normalize_deps(
                    spec_obj
                        .get("required")
                        .unwrap_or(&Value::Array(Vec::new())),
                );
                let recommended = normalize_deps(
                    spec_obj
                        .get("recommended")
                        .unwrap_or(&Value::Array(Vec::new())),
                );
                let config_files = spec_obj
                    .get("config_files")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                let permissions = spec_obj
                    .get("permissions")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                let network = spec_obj
                    .get("network")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();

                specs.insert(
                    tool_name,
                    ToolDepSpec {
                        aliases,
                        required,
                        recommended,
                        config_files,
                        permissions,
                        network,
                    },
                );
            }
        }
    }
    Ok(specs)
}

/// Check a specific tool's environment readiness.
fn check_tool(tool_name: &str, spec: &ToolDepSpec) -> ToolReadyResult {
    let required_results: Vec<(DepEntry, DepStatus)> = spec
        .required
        .iter()
        .map(|d| (d.clone(), check_dep(d)))
        .collect();

    let recommended_results: Vec<(DepEntry, DepStatus)> = spec
        .recommended
        .iter()
        .map(|d| (d.clone(), check_dep(d)))
        .collect();

    let config_results: Vec<(String, bool)> = spec
        .config_files
        .iter()
        .map(|f| (f.clone(), check_config_file(f)))
        .collect();

    let permission_results: Vec<(String, bool)> = spec
        .permissions
        .iter()
        .map(|p| (p.clone(), check_permission(p)))
        .collect();

    let network_results: Vec<(String, bool)> = spec
        .network
        .iter()
        .map(|n| (n.clone(), check_network(n)))
        .collect();

    let has_required_missing = required_results
        .iter()
        .any(|(_, s)| s == &DepStatus::Missing || matches!(s, DepStatus::VersionLow { .. }));
    let has_perm_missing = permission_results.iter().any(|(_, ok)| !ok);
    let has_recommended_missing = recommended_results
        .iter()
        .any(|(_, s)| s == &DepStatus::Missing || matches!(s, DepStatus::VersionLow { .. }));
    let has_config_missing = config_results.iter().any(|(_, ok)| !ok);
    let has_net_missing = network_results.iter().any(|(_, ok)| !ok);

    let status = if has_required_missing || has_perm_missing {
        ReadyStatus::NotReady
    } else if has_recommended_missing || has_config_missing || has_net_missing {
        ReadyStatus::Partial
    } else {
        ReadyStatus::Ready
    };

    ToolReadyResult {
        tool_name: tool_name.to_string(),
        status,
        required_results,
        recommended_results,
        config_results,
        permission_results,
        network_results,
    }
}

/// Format a DepStatus as a human-readable string (with icon).
fn format_dep_status(status: &DepStatus) -> String {
    match status {
        DepStatus::Available => "✓".to_string(),
        DepStatus::Missing => "missing".to_string(),
        DepStatus::VersionLow {
            installed,
            required,
        } => {
            format!("version low ({installed} < {required})")
        }
    }
}

/// Format a DepStatus as a text status label (no icon, no emoji).
fn format_dep_status_label(status: &DepStatus) -> String {
    match status {
        DepStatus::Available => "INSTALLED".to_string(),
        DepStatus::Missing => "MISSING".to_string(),
        DepStatus::VersionLow {
            installed,
            required,
        } => {
            format!("OUTDATED ({installed}/{required})")
        }
    }
}

/// Format a ReadyStatus as a human-readable label.
fn format_status(status: &ReadyStatus) -> &'static str {
    match status {
        ReadyStatus::Ready => "READY",
        ReadyStatus::Partial => "PARTIAL",
        ReadyStatus::NotReady => "NOT_READY",
        ReadyStatus::Unknown => "UNKNOWN",
    }
}

/// Generate a full checklist string — two-level layout:
/// Level 1: Agent tool category (Shell/WebFetch/Read/Write)
/// Level 2: Binary list under each category, with text status labels.
fn generate_checklist(results: &[ToolReadyResult]) -> String {
    let mut output = String::new();
    output.push_str("Tool Environment Ready Checklist\n");
    output.push_str("=================================\n\n");

    for result in results {
        let category_status = format_status(&result.status);
        output.push_str(&format!("{} [{}]\n", result.tool_name, category_status));

        for (dep, status) in &result.required_results {
            let label = format_dep_status_label(status);
            output.push_str(&format!("  required:   {:12} {}\n", dep.binary, label));
        }
        for (dep, status) in &result.recommended_results {
            let label = format_dep_status_label(status);
            output.push_str(&format!("  recommended:{:12} {}\n", dep.binary, label));
        }
        for (cfg, ok) in &result.config_results {
            let label = if *ok { "INSTALLED" } else { "MISSING" };
            output.push_str(&format!("  config:     {cfg:12} {label}\n"));
        }
        for (perm, ok) in &result.permission_results {
            let label = if *ok { "GRANTED" } else { "DENIED" };
            output.push_str(&format!("  permission: {perm:12} {label}\n"));
        }
        for (net, ok) in &result.network_results {
            let label = if *ok { "OK" } else { "FAIL" };
            output.push_str(&format!("  network:    {net:12} {label}\n"));
        }
        if !result.required_results.is_empty()
            || !result.recommended_results.is_empty()
            || !result.config_results.is_empty()
            || !result.permission_results.is_empty()
            || !result.network_results.is_empty()
        {
            output.push('\n');
        }
    }

    let ready_count = results
        .iter()
        .filter(|r| r.status == ReadyStatus::Ready)
        .count();
    let partial_count = results
        .iter()
        .filter(|r| r.status == ReadyStatus::Partial)
        .count();
    let not_ready_count = results
        .iter()
        .filter(|r| r.status == ReadyStatus::NotReady)
        .count();
    let unknown_count = results
        .iter()
        .filter(|r| r.status == ReadyStatus::Unknown)
        .count();

    let mut summary = format!(
        "Summary: {ready_count} ready, {partial_count} partial, {not_ready_count} not ready"
    );
    if unknown_count > 0 {
        summary.push_str(&format!(", {unknown_count} unknown"));
    }
    summary.push_str(&format!(" (total: {})\n", results.len()));
    output.push_str(&summary);

    output
}

/// Serialize checklist results as a JSON object for machine consumption.
///
/// **Sync rule**: when `ToolReadyResult` gains a new check category, add a
/// matching array here so JSON consumers see every field the text path shows.
///
/// **Ordering contract**: the `tools` array preserves the caller's order.
/// `run()` sorts tool names before building results, so output is stable
/// across processes and safe for diff/hash/cache/snapshot consumers.
///
/// Schema:
/// ```json
/// {
///   "tools": [
///     {
///       "tool": "<name>",
///       "status": "READY|PARTIAL|NOT_READY|UNKNOWN",
///       "required":    [{"binary":"…","status":"…"}, …],
///       "recommended": [{"binary":"…","status":"…"}, …],
///       "config":      [{"name":"…","ok":true}, …],
///       "permissions": [{"name":"…","ok":true}, …],
///       "network":     [{"name":"…","ok":true}, …]
///     },
///     …
///   ],
///   "summary": {
///     "ready": N, "partial": N, "not_ready": N, "unknown": N, "total": N
///   }
/// }
/// ```
fn generate_checklist_json(results: &[ToolReadyResult]) -> Value {
    let tools: Vec<Value> = results
        .iter()
        .map(|r| {
            let mut obj = serde_json::Map::new();
            obj.insert("tool".to_string(), Value::String(r.tool_name.clone()));
            obj.insert(
                "status".to_string(),
                Value::String(format_status(&r.status).to_string()),
            );

            let required: Vec<Value> = r
                .required_results
                .iter()
                .map(|(dep, status)| {
                    serde_json::json!({
                        "binary": dep.binary,
                        "status": format_dep_status_label(status)
                    })
                })
                .collect();
            obj.insert("required".to_string(), Value::Array(required));

            let recommended: Vec<Value> = r
                .recommended_results
                .iter()
                .map(|(dep, status)| {
                    serde_json::json!({
                        "binary": dep.binary,
                        "status": format_dep_status_label(status)
                    })
                })
                .collect();
            obj.insert("recommended".to_string(), Value::Array(recommended));

            let config: Vec<Value> = r
                .config_results
                .iter()
                .map(|(name, ok)| serde_json::json!({"name": name, "ok": ok}))
                .collect();
            obj.insert("config".to_string(), Value::Array(config));

            let permissions: Vec<Value> = r
                .permission_results
                .iter()
                .map(|(name, ok)| serde_json::json!({"name": name, "ok": ok}))
                .collect();
            obj.insert("permissions".to_string(), Value::Array(permissions));

            let network: Vec<Value> = r
                .network_results
                .iter()
                .map(|(name, ok)| serde_json::json!({"name": name, "ok": ok}))
                .collect();
            obj.insert("network".to_string(), Value::Array(network));

            Value::Object(obj)
        })
        .collect();

    let ready = results
        .iter()
        .filter(|r| r.status == ReadyStatus::Ready)
        .count();
    let partial = results
        .iter()
        .filter(|r| r.status == ReadyStatus::Partial)
        .count();
    let not_ready = results
        .iter()
        .filter(|r| r.status == ReadyStatus::NotReady)
        .count();
    let unknown = results
        .iter()
        .filter(|r| r.status == ReadyStatus::Unknown)
        .count();

    serde_json::json!({
        "tools": tools,
        "summary": {
            "ready": ready,
            "partial": partial,
            "not_ready": not_ready,
            "unknown": unknown,
            "total": results.len()
        }
    })
}

/// Build the ordered candidate list for `auto_fix`.
///
/// Order mirrors `spec_path_candidates`: env override → user dot-dir →
/// user FHS → system FHS → legacy flat-layout mirrors.
fn fix_script_candidates() -> Vec<String> {
    let home = super::get_home_dir();
    vec![
        std::env::var("TOKENLESS_ENV_FIX_SCRIPT").ok(),
        Some(format!("{home}/.tokenless/tokenless-env-fix.sh")),
        Some(format!(
            "{home}/.local/share/anolisa/adapters/tokenless/common/tokenless-env-fix.sh"
        )),
        Some("/usr/local/share/anolisa/adapters/tokenless/common/tokenless-env-fix.sh".into()),
        Some("/usr/share/anolisa/adapters/tokenless/common/tokenless-env-fix.sh".into()),
        Some(format!(
            "{home}/.local/share/anolisa/adapters/tokenless/tokenless-env-fix.sh"
        )),
        Some("/usr/local/share/anolisa/adapters/tokenless/tokenless-env-fix.sh".into()),
        Some("/usr/share/anolisa/adapters/tokenless/tokenless-env-fix.sh".into()),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// Auto-fix missing dependencies via tokenless-env-fix.sh.
fn auto_fix(missing_deps: &[DepEntry]) -> Result<String, String> {
    let home = super::get_home_dir();
    let fix_script = fix_script_candidates()
        .into_iter()
        .find(|p| {
            let path = std::path::Path::new(p);
            path.exists() && is_trusted_path(path)
        })
        .unwrap_or_else(|| format!("{home}/.tokenless/tokenless-env-fix.sh"));

    // Build JSON array of missing deps
    let deps_json: Vec<Value> = missing_deps
        .iter()
        .map(|dep| {
            let mut obj = serde_json::Map::new();
            obj.insert("binary".to_string(), Value::String(dep.binary.clone()));
            if let Some(ref v) = dep.version {
                obj.insert("version".to_string(), Value::String(v.clone()));
            }
            obj.insert("package".to_string(), Value::String(resolve_package(dep)));
            if let Some(ref ap) = dep.apt_package {
                obj.insert("apt_package".to_string(), Value::String(ap.clone()));
            }
            if let Some(ref akp) = dep.apk_package {
                obj.insert("apk_package".to_string(), Value::String(akp.clone()));
            }
            obj.insert("manager".to_string(), Value::String(dep.manager.clone()));
            if let Some(ref pn) = dep.pip_name {
                obj.insert("pip_name".to_string(), Value::String(pn.clone()));
            }
            if let Some(ref un) = dep.uv_name {
                obj.insert("uv_name".to_string(), Value::String(un.clone()));
            }
            if let Some(ref nn) = dep.npm_name {
                obj.insert("npm_name".to_string(), Value::String(nn.clone()));
            }
            if dep.use_npx {
                obj.insert("use_npx".to_string(), Value::Bool(true));
            }
            if !dep.fallback.is_empty() {
                let fb_arr: Vec<Value> = dep
                    .fallback
                    .iter()
                    .map(|fb| {
                        let mut fb_obj = serde_json::Map::new();
                        fb_obj.insert("method".to_string(), Value::String(fb.method.clone()));
                        if let Some(ref p) = fb.package {
                            fb_obj.insert("package".to_string(), Value::String(p.clone()));
                        }
                        if let Some(ref b) = fb.binary {
                            fb_obj.insert("binary".to_string(), Value::String(b.clone()));
                        }
                        if let Some(ref s) = fb.source {
                            fb_obj.insert("source".to_string(), Value::String(s.clone()));
                        }
                        if let Some(ref m) = fb.manifest {
                            fb_obj.insert("manifest".to_string(), Value::String(m.clone()));
                        }
                        if let Some(ref f) = fb.features {
                            fb_obj.insert("features".to_string(), Value::String(f.clone()));
                        }
                        if let Some(ref u) = fb.url {
                            fb_obj.insert("url".to_string(), Value::String(u.clone()));
                        }
                        if let Some(ref a) = fb.args {
                            fb_obj.insert("args".to_string(), Value::String(a.clone()));
                        }
                        Value::Object(fb_obj)
                    })
                    .collect();
                obj.insert("fallback".to_string(), Value::Array(fb_arr));
            }
            Value::Object(obj)
        })
        .collect();

    let json_str =
        serde_json::to_string(&deps_json).map_err(|e| format!("Failed to serialize deps: {e}"))?;

    // Use timeout if available (coreutils procps), otherwise run without timeout
    let has_timeout = Command::new("sh")
        .args(["-c", "command -v timeout"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    let mut child = if has_timeout {
        Command::new("timeout")
            .arg("120")
            .arg("bash")
            .arg(&fix_script)
            .arg("fix-all")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("Failed to run env-fix: {e}"))?
    } else {
        Command::new("bash")
            .arg(&fix_script)
            .arg("fix-all")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("Failed to run env-fix: {e}"))?
    };

    let mut stdin_handle = child
        .stdin
        .take()
        .ok_or_else(|| "Failed to open stdin for env-fix process".to_string())?;
    stdin_handle
        .write_all(json_str.as_bytes())
        .map_err(|e| format!("Failed to write deps to env-fix stdin: {e}"))?;
    drop(stdin_handle);

    let output = child
        .wait_with_output()
        .map_err(|e| format!("Failed to wait for env-fix: {e}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Surface stderr (and stdout) so the caller can show the failure
        // instead of silently treating an error message as a "success" payload.
        return Err(format!(
            "env-fix exited with {}: {}{}",
            output.status,
            stderr.trim(),
            if stdout.is_empty() {
                String::new()
            } else {
                format!(" | stdout: {}", stdout.trim())
            }
        ));
    }
    Ok(stdout)
}

/// Build the ordered candidate list for `find_spec_path`.
///
/// Order: env override → user dot-dir → user FHS → system FHS (usr/local,
/// then usr) → legacy flat-layout mirrors of the same four roots.
fn spec_path_candidates() -> Vec<String> {
    let home = super::get_home_dir();
    vec![
        std::env::var("TOKENLESS_TOOL_READY_SPEC").ok(),
        Some(format!("{home}/.tokenless/tool-ready-spec.json")),
        Some(format!(
            "{home}/.local/share/anolisa/adapters/tokenless/common/tool-ready-spec.json"
        )),
        Some("/usr/local/share/anolisa/adapters/tokenless/common/tool-ready-spec.json".into()),
        Some("/usr/share/anolisa/adapters/tokenless/common/tool-ready-spec.json".into()),
        Some(format!(
            "{home}/.local/share/anolisa/adapters/tokenless/tool-ready-spec.json"
        )),
        Some("/usr/local/share/anolisa/adapters/tokenless/tool-ready-spec.json".into()),
        Some("/usr/share/anolisa/adapters/tokenless/tool-ready-spec.json".into()),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// Find the spec file path.
fn find_spec_path() -> Result<PathBuf, String> {
    let candidates = spec_path_candidates();

    for candidate in candidates.iter().map(PathBuf::from) {
        if candidate.exists() && is_trusted_path(&candidate) {
            return Ok(candidate);
        }
    }

    Err(format!(
        "No spec file found in any candidate path: {}",
        candidates.join(", ")
    ))
}

/// Build a JSON result for a single tool check.
fn build_json_result(
    tool_name: &str,
    status: &ReadyStatus,
    fixed: &[String],
    missing: &[String],
) -> Value {
    let mut obj = serde_json::Map::new();
    obj.insert("tool".to_string(), Value::String(tool_name.to_string()));
    obj.insert(
        "status".to_string(),
        Value::String(format_status(status).to_string()),
    );
    if !fixed.is_empty() {
        obj.insert(
            "fixed".to_string(),
            Value::Array(fixed.iter().map(|s| Value::String(s.clone())).collect()),
        );
    }
    if !missing.is_empty() {
        obj.insert(
            "missing".to_string(),
            Value::Array(missing.iter().map(|s| Value::String(s.clone())).collect()),
        );
    }
    if *status == ReadyStatus::NotReady {
        let diag_parts: Vec<String> = missing
            .iter()
            .map(|m| format!("required dependency missing: {m}"))
            .collect();
        obj.insert(
            "diagnostic".to_string(),
            Value::String(format!(
                "[tokenless:ready] {}: NOT_READY — {}. Skip retry.",
                tool_name,
                diag_parts.join(", ")
            )),
        );
    }
    Value::Object(obj)
}

/// Run the env-check command with optional JSON output.
pub fn run(
    tool: Option<&str>,
    all: bool,
    fix: bool,
    checklist: bool,
    json: bool,
) -> Result<(), (String, i32)> {
    if tool.is_none() && !all && !checklist {
        return Err(("Specify --tool <name> or --all".to_string(), 1));
    }

    // HARD BYPASS: the legacy readiness model can incorrectly block valid
    // agent work. JSON callers receive only {tool,status,enabled}; in
    // particular, checklist mode never exposes its dormant tools/summary
    // schema. Re-enabling checks or expanding this contract requires an
    // intentional source change.
    if tool_ready_hard_bypassed() {
        if json {
            let result = serde_json::json!({
                "tool": tool.unwrap_or(if all { "all" } else { "checklist" }),
                "status": "UNKNOWN",
                "enabled": false,
            });
            println!("{}", serde_json::to_string(&result).unwrap());
        } else {
            println!("Tool Ready is hard-disabled in this build");
        }
        return Ok(());
    }

    let spec_path = find_spec_path().map_err(|e| (e, 1))?;
    let specs = load_spec(&spec_path).map_err(|e| (e, 1))?;

    if checklist {
        // Sort tool names before building results so both the text and JSON
        // checklist output keep a stable order across processes; `specs` is a
        // HashMap, whose iteration order is randomized per process.
        let mut tool_names: Vec<&str> = specs.keys().map(String::as_str).collect();
        tool_names.sort_unstable();
        let results: Vec<ToolReadyResult> = tool_names
            .iter()
            .map(|name| check_tool(name, specs.get(*name).unwrap()))
            .collect();
        if json {
            let json_value = serde_json::to_string(&generate_checklist_json(&results))
                .map_err(|e| (format!("Failed to serialize checklist JSON: {e}"), 1))?;
            println!("{json_value}");
        } else {
            println!("{}", generate_checklist(&results));
        }
        return Ok(());
    }

    let tool_names: Vec<String> = if all {
        specs.keys().cloned().collect()
    } else if let Some(t) = tool {
        // Resolve tool name: exact key → aliases → case-insensitive
        let resolved = if specs.contains_key(t) {
            t.to_string()
        } else {
            // Try alias reverse lookup: find spec key whose aliases contain t
            specs
                .iter()
                .find(|(_, spec)| spec.aliases.iter().any(|a| a == t))
                .map(|(k, _)| k.clone())
                .unwrap_or_else(|| {
                    // Case-insensitive fallback
                    specs
                        .keys()
                        .find(|k| k.eq_ignore_ascii_case(t))
                        .cloned()
                        .unwrap_or_else(|| t.to_string())
                })
        };
        if !specs.contains_key(&resolved) {
            if json {
                let result = build_json_result(&resolved, &ReadyStatus::Unknown, &[], &[]);
                println!("{}", serde_json::to_string(&result).unwrap());
                return Ok(());
            }
            println!("{}: {}", t, format_status(&ReadyStatus::Unknown));
            return Ok(());
        }
        vec![resolved]
    } else {
        unreachable!("tool arguments were validated before the readiness gate")
    };

    for tool_name in &tool_names {
        let spec = specs.get(tool_name).unwrap();
        let result = check_tool(tool_name, spec);

        // Collect missing deps for reporting, but auto-fix only required deps.
        let fixable_deps: Vec<DepEntry> = result
            .required_results
            .iter()
            .filter(|(_, s)| matches!(s, DepStatus::Missing | DepStatus::VersionLow { .. }))
            .map(|(d, _)| d.clone())
            .collect();

        let missing_deps: Vec<DepEntry> = result
            .required_results
            .iter()
            .chain(result.recommended_results.iter())
            .filter(|(_, s)| matches!(s, DepStatus::Missing | DepStatus::VersionLow { .. }))
            .map(|(d, _)| d.clone())
            .collect();

        let fixable_names: Vec<String> = fixable_deps.iter().map(|d| d.binary.clone()).collect();
        let missing_names: Vec<String> = missing_deps.iter().map(|d| d.binary.clone()).collect();

        if fix && !fixable_deps.is_empty() {
            if !json {
                println!(
                    "{}: {} (fixing: {})",
                    tool_name,
                    format_status(&result.status),
                    fixable_names.join(", ")
                );
                println!("  Attempting auto-fix...");
            }
            let fix_output = auto_fix(&fixable_deps).map_err(|e| (e, 1))?;
            if !json {
                for line in fix_output.lines() {
                    println!("  {line}");
                }
            }

            // Re-check after fix
            let post_result = check_tool(tool_name, spec);
            let post_missing: Vec<String> = post_result
                .required_results
                .iter()
                .chain(post_result.recommended_results.iter())
                .filter(|(_, s)| matches!(s, DepStatus::Missing | DepStatus::VersionLow { .. }))
                .map(|(d, _)| d.binary.clone())
                .collect();

            let fixed: Vec<String> = fixable_names
                .iter()
                .filter(|n| !post_missing.contains(n))
                .cloned()
                .collect();

            if json {
                let post_status =
                    if post_missing.is_empty()
                        && post_result.permission_results.iter().all(|(_, ok)| *ok)
                    {
                        ReadyStatus::Ready
                    } else if post_result.required_results.iter().any(|(_, s)| {
                        matches!(s, DepStatus::Missing | DepStatus::VersionLow { .. })
                    }) || post_result.permission_results.iter().any(|(_, ok)| !ok)
                    {
                        ReadyStatus::NotReady
                    } else {
                        ReadyStatus::Partial
                    };
                let result_json = build_json_result(tool_name, &post_status, &fixed, &post_missing);
                println!("{}", serde_json::to_string(&result_json).unwrap());
            } else {
                println!("{}: {}", tool_name, format_status(&post_result.status));
            }
        } else if json {
            let result_json = build_json_result(tool_name, &result.status, &[], &missing_names);
            println!("{}", serde_json::to_string(&result_json).unwrap());
        } else {
            println!("{}: {}", tool_name, format_status(&result.status));

            for (dep, status) in &result.required_results {
                println!(
                    "  required: {} — {} [{}]",
                    dep.binary,
                    format_dep_status(status),
                    resolve_manager(&dep.manager)
                );
            }
            for (dep, status) in &result.recommended_results {
                println!(
                    "  recommended: {} — {} [{}]",
                    dep.binary,
                    format_dep_status(status),
                    resolve_manager(&dep.manager)
                );
            }
            for (cfg, ok) in &result.config_results {
                println!("  config: {} — {}", cfg, if *ok { "✓" } else { "missing" });
            }
            for (perm, ok) in &result.permission_results {
                println!(
                    "  permission: {} — {}",
                    perm,
                    if *ok { "✓" } else { "missing" }
                );
            }
            for (net, ok) in &result.network_results {
                println!("  network: {} — {}", net, if *ok { "✓" } else { "missing" });
            }

            if !fixable_deps.is_empty() {
                println!(
                    "  Hint: run with --fix to auto-install missing required deps: {}",
                    fixable_deps
                        .iter()
                        .map(|d| d.binary.clone())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            } else if result
                .recommended_results
                .iter()
                .any(|(_, s)| matches!(s, DepStatus::Missing | DepStatus::VersionLow { .. }))
            {
                println!(
                    "  Note: missing recommended deps are optional and are not auto-installed."
                );
            }
        }

        if !json {
            println!();
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    include!("tests/env_check_tests.rs");
}
