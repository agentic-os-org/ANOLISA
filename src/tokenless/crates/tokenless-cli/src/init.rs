//! Community entry point — detect agent frameworks and install adapters.
//!
//! Scans the adapter directory for framework-specific `detect.sh` scripts,
//! reports each framework's status, and optionally runs `install.sh` to
//! register the tokenless adapter with the selected framework.
//!
//! # Detector contract
//!
//! A detector reports the state of *its own adapter*, never merely the host
//! binary that adapter talks to. Two shapes are understood, in this order:
//!
//! 1. An explicit JSON object on stdout carrying
//!    `"status": "ready" | "installable" | "missing_prereqs"`. Detectors whose
//!    exit code already means something else publish it: `codex` probes the
//!    tokenless binary, `qoder` probes host-CLI capability, and `qwenwork`
//!    returns 0 whether or not its plugin is sideloaded.
//! 2. The shared tri-state exit code — `0` = adapter registered and ready,
//!    `1` = not registered but installable, `2` (or any other code) = missing
//!    prerequisites. `openclaw`, `hermes`, `claude-code`, `opencode`,
//!    `qwencode` and `qwenpaw` follow it.
//!
//! A JSON object carrying `installed` but no `status` predates this contract.
//! `installed: false` still downgrades exit 0 to installable, so an
//! availability-only probe is never read as a registered adapter.
//!
//! The tri-state exit code is only trusted when the adapter tree declares the
//! contract, i.e. its manifest carries `"detectContract": "status-v1"`. A tree
//! without it predates the contract, and such trees are reachable: npm preserves
//! a shared adapter directory owned by another installation, and
//! `ANOLISA_ADAPTER_DIR` accepts any directory. Its detectors may use exit 0 to
//! mean "the host binary works" (`codex`) or "the host CLI is compatible"
//! (`qoder`), so exit 0 is capped at installable there — a stale resource
//! re-registers an adapter rather than claim one is already registered.
//!
//! Every lifecycle script is Bash, so a host without it cannot report any
//! status. A detector that cannot be launched is an error rather than an
//! adapter state: folding it into `missing prereqs` let `--all` exit 0 with
//! "No frameworks are waiting for installation" having installed nothing.

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io::{self, IsTerminal as _, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::Deserialize;

/// Sub-path from a data root to the tokenless adapter directory.
///
/// Mirrors the packaged layout: `packaging/install.sh` installs adapters under
/// `$data_root/$AWPRJ/adapters/tokenless`, with `AWPRJ` defaulting to `anolisa`.
const ADAPTER_SUBPATH: &str = "anolisa/adapters/tokenless";

/// System-wide data roots, in the order the packager writes them.
const SYSTEM_DATA_ROOTS: [&str; 2] = ["/usr/share", "/usr/local/share"];

/// Manifest marker for the detector contract documented in the module header.
///
/// Only an exact match unlocks the tri-state exit code. An unknown value — a
/// contract from a newer adapter tree — is treated as undeclared, so an older
/// CLI stays pessimistic instead of guessing at semantics it does not know.
const DETECT_CONTRACT: &str = "status-v1";

#[derive(Debug, Deserialize)]
struct Manifest {
    /// Detector contract the adapter tree declares, if any.
    #[serde(default, rename = "detectContract")]
    detect_contract: Option<String>,
    targets: BTreeMap<String, ManifestTarget>,
}

impl Manifest {
    /// True when the tree states its detectors follow [`DETECT_CONTRACT`], so
    /// exit 0 may be read as "adapter registered".
    fn declares_detect_contract(&self) -> bool {
        self.detect_contract.as_deref() == Some(DETECT_CONTRACT)
    }
}

#[derive(Debug, Deserialize)]
struct ManifestTarget {
    #[serde(default)]
    actions: Option<ManifestActions>,
    #[serde(default)]
    capabilities: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct ManifestActions {
    detect: Option<String>,
    install: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DetectStatus {
    Ready,
    Installable,
    MissingPrereqs,
    NotChecked,
    /// The detector could not be launched, so nothing is known about the
    /// adapter. Reported as an error, not as a status a caller can act on.
    Unavailable,
}

impl DetectStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Installable => "installable",
            Self::MissingPrereqs => "missing prereqs",
            Self::NotChecked => "n/a",
            Self::Unavailable => "unavailable",
        }
    }

    /// Parse the `status` value a detector publishes as JSON.
    fn from_published(value: &serde_json::Value) -> Option<Self> {
        match value.as_str()? {
            "ready" => Some(Self::Ready),
            "installable" => Some(Self::Installable),
            "missing_prereqs" => Some(Self::MissingPrereqs),
            _ => None,
        }
    }
}

struct FrameworkInfo {
    name: String,
    status: DetectStatus,
    capabilities: Vec<String>,
    install_script: Option<String>,
}

/// Why one target the caller selected could not be installed.
enum InstallError {
    /// The manifest declares no `actions.install` for the target.
    NoScript,
    /// The install script ran and reported failure.
    Failed(String),
}

/// Find the adapter directory by checking known locations.
///
/// An explicit `ANOLISA_ADAPTER_DIR` environment variable takes precedence,
/// which also supports custom install prefixes outside the default data roots.
fn find_adapter_dir() -> Option<PathBuf> {
    adapter_dir_candidates()
        .into_iter()
        .find(|candidate| has_manifest(candidate))
}

/// Adapter directory candidates, most specific first.
fn adapter_dir_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(dir) = env::var("ANOLISA_ADAPTER_DIR")
        && !dir.trim().is_empty()
    {
        candidates.push(PathBuf::from(dir));
    }
    candidates.extend(
        data_roots()
            .into_iter()
            .map(|root| root.join(ADAPTER_SUBPATH)),
    );
    candidates
}

/// Data roots the packager can install into, most specific first.
///
/// `XDG_DATA_HOME` comes first because `packaging/install.sh` resolves the user
/// data root as `${XDG_DATA_HOME:-$HOME/.local/share}`. Honoring only the
/// fallback left an XDG installation invisible to `tokenless init`, which then
/// told the user to install a package that was already there. A relative
/// `XDG_DATA_HOME` is skipped, matching the installer's own validation.
fn data_roots() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Ok(xdg) = env::var("XDG_DATA_HOME") {
        let xdg = PathBuf::from(xdg);
        if xdg.is_absolute() {
            roots.push(xdg);
        }
    }
    if let Some(home) = dirs::home_dir() {
        roots.push(home.join(".local/share"));
    }
    roots.extend(SYSTEM_DATA_ROOTS.map(PathBuf::from));
    roots.dedup();
    roots
}

fn has_manifest(dir: &Path) -> bool {
    dir.join("manifest.json").exists() || dir.join("manifest.json.in").exists()
}

/// Load and parse the adapter manifest. Falls back to the `.in` template
/// (with `@VERSION@` left as-is) when the stamped copy is absent.
fn load_manifest(adapter_dir: &Path) -> Result<Manifest, String> {
    let manifest_path = adapter_dir.join("manifest.json");
    let content = if manifest_path.exists() {
        fs::read_to_string(&manifest_path)
            .map_err(|e| format!("failed to read manifest.json: {}", e))?
    } else {
        fs::read_to_string(adapter_dir.join("manifest.json.in"))
            .map_err(|e| format!("failed to read manifest.json.in: {}", e))?
    };
    serde_json::from_str(&content).map_err(|e| format!("failed to parse manifest: {}", e))
}

/// Extract hook names from the capabilities field for display.
fn extract_hooks(cap: &Option<serde_json::Value>) -> Vec<String> {
    match cap {
        Some(serde_json::Value::Object(map)) => match map.get("hooks") {
            Some(serde_json::Value::Array(arr)) => arr
                .iter()
                .filter_map(|h| h.as_str().map(String::from))
                .collect(),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

/// Run `detect.sh` for a framework and map the result to an adapter status.
///
/// `Err` carries the launch failure: the interpreter every lifecycle script
/// needs is unusable, which is a property of this command's host requirement
/// and not a state of the adapter. `tri_state` says whether the adapter tree
/// declares [`DETECT_CONTRACT`], i.e. whether exit 0 may be read as ready.
fn run_detect(
    adapter_dir: &Path,
    name: &str,
    detect_script: &str,
    tri_state: bool,
) -> Result<DetectStatus, String> {
    let script = adapter_dir.join(detect_script);
    if !script.exists() {
        return Ok(DetectStatus::NotChecked);
    }
    match Command::new("bash")
        .arg(&script)
        .env("ANOLISA_COMPONENT", "tokenless")
        .env("ANOLISA_TARGET", name)
        .env("ANOLISA_ADAPTER_DIR", adapter_dir)
        .output()
    {
        Ok(out) => Ok(interpret_detect(
            out.status.code(),
            &String::from_utf8_lossy(&out.stdout),
            tri_state,
        )),
        Err(e) => Err(format!("{}: {}", script.display(), e)),
    }
}

/// Map one detector run to the status of *its adapter*.
///
/// Split out from `run_detect` so both detector contracts can be exercised
/// without spawning a script. `exit_code` is `None` when the detector was
/// terminated by a signal, and `tri_state` is true only for an adapter tree
/// that declares [`DETECT_CONTRACT`].
fn interpret_detect(exit_code: Option<i32>, stdout: &str, tri_state: bool) -> DetectStatus {
    let published = json_objects(stdout);
    // A detector that publishes its status is self-describing whatever the tree
    // around it declares, so that answer is taken as-is.
    let explicit = published
        .iter()
        .find_map(|object| object.get("status").and_then(DetectStatus::from_published));
    if let Some(status) = explicit {
        return status;
    }
    match exit_code {
        Some(0) => {
            // Two reasons exit 0 may not mean "registered":
            //   * legacy availability-only JSON, where `installed` describes a
            //     prerequisite such as the tokenless binary;
            //   * a tree that does not declare the contract, whose detectors
            //     used exit 0 for host availability or host-CLI capability.
            let uninstalled = published
                .iter()
                .any(|object| object.get("installed") == Some(&serde_json::Value::Bool(false)));
            if uninstalled || !tri_state {
                DetectStatus::Installable
            } else {
                DetectStatus::Ready
            }
        }
        Some(1) => DetectStatus::Installable,
        _ => DetectStatus::MissingPrereqs,
    }
}

/// JSON objects a detector published on stdout.
///
/// The whole output is tried first because `codex` pretty-prints a single
/// object across several lines, then each line on its own so a detector may log
/// to stdout around a one-line status object.
fn json_objects(stdout: &str) -> Vec<serde_json::Value> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    if let Ok(value @ serde_json::Value::Object(_)) =
        serde_json::from_str::<serde_json::Value>(trimmed)
    {
        return vec![value];
    }
    trimmed
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line.trim()).ok())
        .filter(|value| value.is_object())
        .collect()
}

/// Run `install.sh` for a framework with both streams attached to ours.
///
/// Installers print actionable guidance on stdout — OpenCode's restart note,
/// Codex's `plugin list` hint — and diagnostics on stderr. Capturing the child
/// instead discarded stdout entirely and reduced a failing installer to its
/// exit code, so the reason it failed never reached the user.
fn run_install(adapter_dir: &Path, name: &str, install_script: &str) -> Result<(), String> {
    let script = adapter_dir.join(install_script);
    if !script.exists() {
        return Err(format!("install script not found: {}", script.display()));
    }
    let status = Command::new("bash")
        .arg(&script)
        .env("ANOLISA_COMPONENT", "tokenless")
        .env("ANOLISA_TARGET", name)
        .env("ANOLISA_ADAPTER_DIR", adapter_dir)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|e| format!("failed to run {}: {}", script.display(), e))?;
    if status.success() {
        return Ok(());
    }
    Err(match status.code() {
        Some(code) => format!("install.sh for {} exited with code {}", name, code),
        None => format!("install.sh for {} was terminated by a signal", name),
    })
}

/// Install one target the caller selected.
///
/// A target with no `actions.install` is an error rather than a skip: the
/// caller asked for that target, so exiting successfully would report work that
/// never happened.
fn install_selected(adapter_dir: &Path, fw: &FrameworkInfo) -> Result<(), InstallError> {
    let Some(script) = fw.install_script.as_deref() else {
        return Err(InstallError::NoScript);
    };
    println!("\nInstalling {} adapter...", fw.name);
    if let Err(e) = run_install(adapter_dir, &fw.name, script) {
        return Err(InstallError::Failed(e));
    }
    println!("{} adapter installed successfully.", fw.name);
    Ok(())
}

/// Detect every target the manifest declares, sorted by name.
///
/// The second element is the first detector that could not be launched, which
/// the caller reports instead of acting on statuses it could not establish.
fn collect_frameworks(
    adapter_dir: &Path,
    manifest: &Manifest,
) -> (Vec<FrameworkInfo>, Option<String>) {
    let tri_state = manifest.declares_detect_contract();
    let mut launch_failure: Option<String> = None;
    let mut frameworks: Vec<FrameworkInfo> = manifest
        .targets
        .iter()
        .map(|(name, target)| {
            let capabilities = extract_hooks(&target.capabilities);
            let Some(actions) = &target.actions else {
                return FrameworkInfo {
                    name: name.clone(),
                    status: DetectStatus::NotChecked,
                    capabilities,
                    install_script: None,
                };
            };
            let status = match &actions.detect {
                Some(detect) => match run_detect(adapter_dir, name, detect, tri_state) {
                    Ok(status) => status,
                    Err(reason) => {
                        if launch_failure.is_none() {
                            launch_failure = Some(reason);
                        }
                        DetectStatus::Unavailable
                    }
                },
                None => DetectStatus::NotChecked,
            };
            FrameworkInfo {
                name: name.clone(),
                status,
                capabilities,
                install_script: actions.install.clone(),
            }
        })
        .collect();
    frameworks.sort_by(|a, b| a.name.cmp(&b.name));
    (frameworks, launch_failure)
}

/// Explain the Bash prerequisite when a detector could not be launched.
///
/// Every shipped lifecycle script is Bash, so a host without it can determine
/// nothing and install nothing. `windows` selects the pointer to the native
/// PowerShell lifecycle, which is the documented Windows path that needs no
/// Git Bash.
fn interpreter_missing_error(reason: &str, windows: bool) -> String {
    let mut message = format!(
        "cannot run the adapter lifecycle scripts ({reason}).\n\
         `tokenless init` runs every target's detect.sh and install.sh with Bash, which was not found, \
         so no framework status could be determined and nothing was installed.\n\
         Install Bash and run this command again."
    );
    if windows {
        message.push_str(
            "\nOn Windows the QwenWork adapter has a native lifecycle that needs no Git Bash:\n  \
             powershell -File <adapter-dir>/qwenwork/scripts/lifecycle.ps1 -Action detect|install|uninstall",
        );
    }
    message
}

/// Print the status table, then flag what the caller cannot act on.
fn print_status(frameworks: &[FrameworkInfo], adapter_dir: &Path, contract: bool) {
    println!("\nTokenless Adapter Status\n");
    println!("{:<14} {:<16} Hooks", "Framework", "Status");
    println!("{:-<14} {:-<16} {:-<40}", "", "", "");
    for fw in frameworks {
        let caps = if fw.capabilities.is_empty() {
            "—".to_string()
        } else {
            fw.capabilities.join(", ")
        };
        println!("{:<14} {:<16} {}", fw.name, fw.status.label(), caps);
    }
    println!("\nAdapter directory: {}", adapter_dir.display());

    // Statuses from a tree that does not declare the contract are capped at
    // installable, which reads as "this adapter is broken" unless the reason is
    // stated next to the table.
    if !contract {
        println!(
            "\nwarning: the adapter resources at {} predate the detector status contract, \
             so no framework is reported ready. Reinstall tokenless to refresh them, or point \
             ANOLISA_ADAPTER_DIR at a current adapter directory.",
            adapter_dir.display()
        );
    }

    // A detector can report installable for a target whose manifest declares no
    // install action. Selecting such a target fails, so say so up front.
    let unactionable: Vec<&str> = frameworks
        .iter()
        .filter(|fw| fw.status == DetectStatus::Installable && fw.install_script.is_none())
        .map(|fw| fw.name.as_str())
        .collect();
    if !unactionable.is_empty() {
        println!();
        for name in unactionable {
            println!(
                "warning: {} reports installable but declares no install action; selecting it fails.",
                name
            );
        }
    }
}

/// Run one named `--framework` operation and end with its result.
fn run_named(
    adapter_dir: &Path,
    frameworks: &[FrameworkInfo],
    name: &str,
) -> Result<(), (String, i32)> {
    let Some(fw) = frameworks.iter().find(|fw| fw.name == name) else {
        return Err((format!("unknown framework: {}", name), 1));
    };
    match fw.status {
        DetectStatus::Ready => {
            println!("\n{} is already installed and ready.", fw.name);
            Ok(())
        }
        DetectStatus::Installable => match install_selected(adapter_dir, fw) {
            Ok(()) => Ok(()),
            Err(InstallError::NoScript) => {
                Err((format!("no install script configured for {}", fw.name), 1))
            }
            Err(InstallError::Failed(e)) => {
                Err((format!("{} adapter installation failed: {}", fw.name, e), 1))
            }
        },
        other => Err((
            format!("{} is not installable (status: {})", fw.name, other.label()),
            1,
        )),
    }
}

pub fn run(framework: Option<String>, all: bool, list_only: bool) -> Result<(), (String, i32)> {
    // `--framework` already names the single target to act on, so the other two
    // selection modes have nothing left to select. clap rejects both
    // combinations first; these guards keep `run` correct for direct callers.
    if framework.is_some() && list_only {
        return Err((
            "--framework and --list cannot be used together".to_string(),
            1,
        ));
    }
    if framework.is_some() && all {
        return Err((
            "--framework and --all cannot be used together; --framework already names one target"
                .to_string(),
            1,
        ));
    }

    let adapter_dir = find_adapter_dir().ok_or_else(|| {
        (
            "Adapter directory not found.\n\
             Install tokenless via `npm install -g anolisa-tokenless` or `anolisa install tokenless`.\n\
             For a custom location, set ANOLISA_ADAPTER_DIR to the directory containing manifest.json."
                .to_string(),
            1,
        )
    })?;

    let manifest = load_manifest(&adapter_dir).map_err(|e| (e, 1))?;

    let (frameworks, launch_failure) = collect_frameworks(&adapter_dir, &manifest);
    print_status(
        &frameworks,
        &adapter_dir,
        manifest.declares_detect_contract(),
    );

    // The table above is printed first so the user sees which targets could not
    // be probed, then the run stops: acting on statuses that were never
    // established is how `--all` came to exit 0 having installed nothing.
    if let Some(reason) = launch_failure {
        return Err((interpreter_missing_error(&reason, cfg!(windows)), 1));
    }

    // A named operation ends with its own result. Falling through to the
    // generic selection below would reuse the statuses collected *before* the
    // install and offer the same target a second time.
    if let Some(name) = &framework {
        return run_named(&adapter_dir, &frameworks, name);
    }

    if list_only {
        return Ok(());
    }

    let installable: Vec<&FrameworkInfo> = frameworks
        .iter()
        .filter(|f| f.status == DetectStatus::Installable)
        .collect();

    if installable.is_empty() {
        println!("\nNo frameworks are waiting for installation.");
        return Ok(());
    }

    let to_install: Vec<&FrameworkInfo> = if all {
        installable
    } else if io::stdin().is_terminal() {
        println!("\nInstallable frameworks:");
        for (i, f) in installable.iter().enumerate() {
            println!("  [{}] {}", i + 1, f.name);
        }
        print!("\nSelect a framework to install (number, 'a' for all, Enter to skip): ");
        io::stdout().flush().ok();
        let mut input = String::new();
        io::stdin().read_line(&mut input).ok();
        let input = input.trim();
        if input.is_empty() {
            println!("Skipping installation.");
            return Ok(());
        }
        if input == "a" {
            installable
        } else if let Ok(n) = input.parse::<usize>() {
            if n >= 1 && n <= installable.len() {
                vec![installable[n - 1]]
            } else {
                return Err((format!("invalid selection: {}", input), 1));
            }
        } else {
            return Err((format!("invalid selection: {}", input), 1));
        }
    } else {
        print_install_hints();
        return Ok(());
    };

    let mut failed: Vec<String> = Vec::new();
    for fw in &to_install {
        match install_selected(&adapter_dir, fw) {
            Ok(()) => {}
            Err(InstallError::NoScript) => {
                eprintln!("tokenless: no install script for {}", fw.name);
                failed.push(fw.name.clone());
            }
            Err(InstallError::Failed(e)) => {
                eprintln!("{} adapter installation failed: {}", fw.name, e);
                failed.push(fw.name.clone());
            }
        }
    }

    if !failed.is_empty() {
        return Err((format!("installation failed for: {}", failed.join(", ")), 1));
    }

    Ok(())
}

fn print_install_hints() {
    println!("\nRun `tokenless init --framework <name>` to install a specific adapter.");
    println!("Run `tokenless init --all` to install all installable adapters.");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `interpret_detect` for a tree that declares the detector contract.
    fn declared(exit_code: Option<i32>, stdout: &str) -> DetectStatus {
        interpret_detect(exit_code, stdout, true)
    }

    /// `interpret_detect` for a tree that predates it, e.g. an adapter
    /// directory another installation left behind.
    fn undeclared(exit_code: Option<i32>, stdout: &str) -> DetectStatus {
        interpret_detect(exit_code, stdout, false)
    }

    #[test]
    fn tri_state_exit_codes_map_to_adapter_status() {
        assert_eq!(declared(Some(0), ""), DetectStatus::Ready);
        assert_eq!(declared(Some(1), ""), DetectStatus::Installable);
        assert_eq!(declared(Some(2), ""), DetectStatus::MissingPrereqs);
        assert_eq!(declared(None, ""), DetectStatus::MissingPrereqs);
    }

    #[test]
    fn human_readable_detector_output_uses_the_exit_code() {
        let stdout = "[tokenless] opencode: ready (1.2.3)\n";
        assert_eq!(declared(Some(0), stdout), DetectStatus::Ready);
        let stdout = "[tokenless] opencode: detected (1.2.3), plugin not installed\n";
        assert_eq!(declared(Some(1), stdout), DetectStatus::Installable);
    }

    #[test]
    fn published_status_wins_over_the_exit_code() {
        let compact = r#"{"status":"installable"}"#;
        assert_eq!(declared(Some(0), compact), DetectStatus::Installable);
        let pretty = "{\n  \"status\": \"missing_prereqs\"\n}\n";
        assert_eq!(declared(Some(0), pretty), DetectStatus::MissingPrereqs);
        assert_eq!(
            declared(Some(1), r#"{"status":"ready"}"#),
            DetectStatus::Ready
        );
    }

    #[test]
    fn published_status_is_found_beside_log_lines() {
        let stdout = "[tokenless] qoder: detected compatible qodercli at /usr/bin/qodercli\n\
                      {\"status\":\"installable\"}\n";
        assert_eq!(declared(Some(0), stdout), DetectStatus::Installable);
    }

    #[test]
    fn unknown_published_status_falls_back_to_the_exit_code() {
        assert_eq!(
            declared(Some(0), r#"{"status":"banana"}"#),
            DetectStatus::Ready
        );
        assert_eq!(
            declared(Some(1), r#"{"status":42}"#),
            DetectStatus::Installable
        );
    }

    #[test]
    fn legacy_installed_false_is_not_ready() {
        assert_eq!(
            declared(Some(0), r#"{"installed": false}"#),
            DetectStatus::Installable
        );
        let pretty = "{\n  \"installed\": false\n}\n";
        assert_eq!(declared(Some(0), pretty), DetectStatus::Installable);
    }

    #[test]
    fn legacy_installed_does_not_override_a_failing_exit_code() {
        assert_eq!(
            declared(Some(1), r#"{"installed": true}"#),
            DetectStatus::Installable
        );
        assert_eq!(
            declared(Some(2), r#"{"installed": true}"#),
            DetectStatus::MissingPrereqs
        );
    }

    #[test]
    fn json_objects_ignores_non_object_output() {
        assert!(json_objects("").is_empty());
        assert!(json_objects("[1, 2]").is_empty());
        assert_eq!(json_objects("not json\n{\"status\":\"ready\"}\n").len(), 1);
    }

    #[test]
    fn detect_status_parses_published_values() {
        let value = |text: &str| serde_json::from_str::<serde_json::Value>(text).unwrap();
        assert_eq!(
            DetectStatus::from_published(&value("\"ready\"")),
            Some(DetectStatus::Ready)
        );
        assert_eq!(
            DetectStatus::from_published(&value("\"installable\"")),
            Some(DetectStatus::Installable)
        );
        assert_eq!(
            DetectStatus::from_published(&value("\"missing_prereqs\"")),
            Some(DetectStatus::MissingPrereqs)
        );
        assert_eq!(DetectStatus::from_published(&value("\"Ready\"")), None);
        assert_eq!(DetectStatus::from_published(&value("true")), None);
    }

    #[test]
    fn undeclared_tree_never_reads_exit_zero_as_ready() {
        // The shapes the previous adapter tree shipped: codex probed the
        // tokenless binary, qoder probed host-CLI capability, and both exited 0.
        assert_eq!(
            undeclared(Some(0), r#"{"installed": true, "version": "9.9.9"}"#),
            DetectStatus::Installable
        );
        assert_eq!(
            undeclared(Some(0), "[tokenless] qoder: detected compatible qodercli\n"),
            DetectStatus::Installable
        );
        assert_eq!(undeclared(Some(0), ""), DetectStatus::Installable);
        // The pessimism stops at readiness: the other states stay readable.
        assert_eq!(undeclared(Some(1), ""), DetectStatus::Installable);
        assert_eq!(undeclared(Some(2), ""), DetectStatus::MissingPrereqs);
        assert_eq!(undeclared(None, ""), DetectStatus::MissingPrereqs);
    }

    #[test]
    fn published_status_survives_an_undeclared_tree() {
        assert_eq!(
            undeclared(Some(0), r#"{"status":"ready"}"#),
            DetectStatus::Ready
        );
        assert_eq!(
            undeclared(Some(0), r#"{"status":"missing_prereqs"}"#),
            DetectStatus::MissingPrereqs
        );
    }

    #[test]
    fn manifest_contract_marker_is_exact() {
        let parse = |text: &str| serde_json::from_str::<Manifest>(text).unwrap();
        let targets = r#","targets":{}"#;
        assert!(
            parse(&format!(r#"{{"detectContract":"status-v1"{targets}}}"#))
                .declares_detect_contract()
        );
        assert!(
            !parse(&format!(r#"{{"detectContract":"status-v2"{targets}}}"#))
                .declares_detect_contract()
        );
        assert!(
            !parse(&format!(r#"{{"component":"tokenless"{targets}}}"#)).declares_detect_contract()
        );
    }

    #[test]
    fn interpreter_error_is_actionable() {
        let reason = "bash: program not found";
        let unix = interpreter_missing_error(reason, false);
        assert!(unix.contains(reason), "{unix}");
        assert!(unix.contains("Bash"), "{unix}");
        assert!(!unix.contains("PowerShell"), "{unix}");
        assert!(!unix.contains("lifecycle.ps1"), "{unix}");

        let windows = interpreter_missing_error(reason, true);
        assert!(windows.contains(reason), "{windows}");
        assert!(
            windows.contains("qwenwork/scripts/lifecycle.ps1"),
            "{windows}"
        );
    }
}
