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
//! An adapter tree that predates the contract is reachable — npm preserves a
//! shared adapter directory owned by another installation, and
//! `ANOLISA_ADAPTER_DIR` accepts any directory — so its manifest may lack the
//! `"detectContract": "status-v1"` marker. Such a tree is *not* treated as
//! uniformly unknown: most of its detectors already reported adapter state
//! through the tri-state exit code, and discarding that would turn a verified
//! `ready` into an offer to reinstall. Only the three whose detectors reported
//! the host instead ([`PRE_CONTRACT_HOST_ONLY_DETECTORS`]) are refused, because
//! nothing about their adapter state can be established and their installers
//! exit successfully without registering anything.
//!
//! A detector that publishes a status document owes the caller valid JSON. When
//! its output was meant to be JSON but does not parse, the answer is
//! [`DetectStatus::Unreadable`]: neither the exit code nor the prose may stand
//! in for it, because an installer whose exit 0 means "I skipped the work"
//! would then be reported as a successful registration. A codex detector
//! without `jq` twice interpolated a path it had not escaped, and each time the
//! exit 0 that followed claimed a registration nobody had verified.
//!
//! That document shares stdout with whatever else writes to it. The lifecycle
//! scripts inherit the caller's environment, so a `BASH_ENV` startup file that
//! prints a line lands ahead of the status, and a detector may log a line of
//! its own around a pretty-printed document. The status is therefore read
//! wherever it appears in the stream — as the whole of it, or as any balanced
//! object span inside it — and a document that was opened and could not be
//! parsed stays [`DetectStatus::Unreadable`] whatever precedes it. For a
//! detector that publishes JSON the exit code is a constant that describes no
//! adapter, so it may not stand in for a status the parser lost.
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
/// Only an exact match declares the contract. An unknown value — a contract
/// from a newer adapter tree — counts as undeclared, so an older CLI falls back
/// to the frozen inventory below instead of guessing at semantics it lacks.
const DETECT_CONTRACT: &str = "status-v1";

/// Targets whose detectors predated [`DETECT_CONTRACT`] and reported the *host*
/// rather than the adapter.
///
/// A frozen inventory: it cannot grow, because every adapter added from here on
/// ships in a tree that declares the contract. It is consulted only for a tree
/// that does not declare it, so a current tree is never second-guessed.
///
/// * `codex` probed the tokenless binary and exited 0 either way, so a working
///   binary read as a registered adapter and its installer exits 0 after
///   printing "skipping plugin registration" when no Codex CLI resolves.
/// * `qoder` probed host-CLI capability: 0 = a compatible `qodercli`, 1 = none.
/// * `qwenwork` returned 0 whether or not its plugin was sideloaded.
///
/// Every other pre-contract detector already used the tri-state exit code, so
/// its semantics are known and stay in force.
const PRE_CONTRACT_HOST_ONLY_DETECTORS: [&str; 3] = ["codex", "qoder", "qwenwork"];

/// What an adapter tree establishes about one target's detector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DetectorContract {
    /// The tree declares [`DETECT_CONTRACT`]: exit 0 means "registered".
    Declared,
    /// The tree predates it, but this detector already reported adapter state
    /// through the tri-state exit code, so its answer is still readable.
    LegacyTriState,
    /// The tree predates it and this detector reported host availability, so its
    /// answer says nothing about the adapter.
    LegacyHostOnly,
}

impl DetectorContract {
    /// The contract one target's detector runs under in one adapter tree.
    fn for_target(tree_declares_contract: bool, name: &str) -> Self {
        if tree_declares_contract {
            return Self::Declared;
        }
        if PRE_CONTRACT_HOST_ONLY_DETECTORS.contains(&name) {
            Self::LegacyHostOnly
        } else {
            Self::LegacyTriState
        }
    }
}

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
    /// The detector ran, but it belongs to an adapter tree that predates the
    /// contract and only ever reported the host, so its answer cannot be
    /// mapped to an adapter state. Never offered for installation.
    Unverifiable,
    /// The detector meant to publish a JSON status and produced something that
    /// does not parse, so it published nothing. Never offered for installation.
    Unreadable,
}

impl DetectStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Installable => "installable",
            Self::MissingPrereqs => "missing prereqs",
            Self::NotChecked => "n/a",
            Self::Unavailable => "unavailable",
            Self::Unverifiable => "unverified",
            Self::Unreadable => "unreadable",
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
    /// The detector published something meant to be JSON that did not parse, so
    /// `status` is a conservative fallback rather than the detector's answer.
    status_unreadable: bool,
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
/// and not a state of the adapter.
fn run_detect(
    adapter_dir: &Path,
    name: &str,
    detect_script: &str,
    contract: DetectorContract,
) -> Result<(DetectStatus, bool), String> {
    let script = adapter_dir.join(detect_script);
    if !script.exists() {
        return Ok((DetectStatus::NotChecked, false));
    }
    match Command::new("bash")
        .arg(&script)
        .env("ANOLISA_COMPONENT", "tokenless")
        .env("ANOLISA_TARGET", name)
        .env("ANOLISA_ADAPTER_DIR", adapter_dir)
        .output()
    {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            Ok((
                interpret_detect(out.status.code(), &stdout, contract),
                status_unreadable(&stdout),
            ))
        }
        Err(e) => Err(format!("{}: {}", script.display(), e)),
    }
}

/// Map one detector run to the status of *its adapter*.
///
/// Split out from `run_detect` so every detector contract can be exercised
/// without spawning a script. `exit_code` is `None` when the detector was
/// terminated by a signal.
fn interpret_detect(
    exit_code: Option<i32>,
    stdout: &str,
    contract: DetectorContract,
) -> DetectStatus {
    let published = json_objects(stdout);
    // A detector that publishes its status is self-describing whatever the tree
    // around it declares, so that answer is taken as-is. It also lets a
    // partially refreshed tree work before its manifest is regenerated.
    let explicit = published
        .iter()
        .find_map(|object| object.get("status").and_then(DetectStatus::from_published));
    if let Some(status) = explicit {
        return status;
    }
    // A detector that promised machine-readable output and broke it has said
    // nothing. Its exit code may not stand in for the missing answer: for the
    // detectors that publish JSON the code is a constant, and selecting the
    // target would run an installer allowed to exit 0 after skipping its work.
    if status_unreadable(stdout) {
        return DetectStatus::Unreadable;
    }
    // `installed` describes a prerequisite such as the tokenless binary, never
    // a registered adapter, so `installed: false` cannot read as ready.
    let uninstalled = published
        .iter()
        .any(|object| object.get("installed") == Some(&serde_json::Value::Bool(false)));

    if contract == DetectorContract::LegacyHostOnly {
        // This detector reported the host, so no exit code it returned says
        // anything about the adapter: claiming ready would repeat the original
        // false positive, and offering the installer would report success for a
        // registration that never happened. The one assertion it does make is
        // that a prerequisite is absent, so keep that.
        return if exit_code == Some(0) && !uninstalled {
            DetectStatus::Unverifiable
        } else {
            DetectStatus::MissingPrereqs
        };
    }

    match exit_code {
        Some(0) if uninstalled => DetectStatus::Installable,
        Some(0) => DetectStatus::Ready,
        Some(1) => DetectStatus::Installable,
        _ => DetectStatus::MissingPrereqs,
    }
}

/// True when a detector meant to publish a JSON status and produced something
/// unparseable.
///
/// Two shapes establish that intent, because the document does not have to be
/// the whole of the output: a leading brace, which is what a detector that
/// publishes nothing else emits, and a brace followed by a double quote
/// anywhere in the stream, which is how an object literal begins. Every shipped
/// text detector starts its own lines with `[tokenless]`, so neither shape is a
/// prose line that happens to hold a brace — a partial document is a bug in the
/// detector. Honouring only the leading brace let one line of Bash startup
/// output hide a broken document, and the exit code that followed reported a
/// registration nobody had verified.
///
/// The whole output still has to fail: a document that parses and merely
/// carries no recognised field is the detector's answer, not a broken one.
fn status_unreadable(stdout: &str) -> bool {
    opens_json_document(stdout) && json_objects(stdout).is_empty()
}

/// True when `stdout` holds the opening of a JSON object literal.
fn opens_json_document(stdout: &str) -> bool {
    if stdout.trim_start().starts_with('{') {
        return true;
    }
    let bytes = stdout.as_bytes();
    bytes.iter().enumerate().any(|(index, byte)| {
        *byte == b'{'
            && bytes[index + 1..]
                .iter()
                .find(|byte| !byte.is_ascii_whitespace())
                == Some(&b'"')
    })
}

/// JSON objects a detector published on stdout.
///
/// The whole output is tried first because a detector that publishes nothing
/// else parses in one step. Failing that, every balanced `{...}` span is tried
/// on its own, so a document survives whatever shares the stream with it:
/// `codex` pretty-prints one object across several lines, a detector may log
/// around it, and the lifecycle scripts inherit the caller's environment — so
/// a `BASH_ENV` startup file that writes to stdout lands ahead of the status.
/// Reading line by line lost that document, and the exit code that followed
/// reported a registration nobody had verified.
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
    json_object_spans(trimmed)
        .into_iter()
        .filter_map(|span| serde_json::from_str::<serde_json::Value>(span).ok())
        .filter(|value| value.is_object())
        .collect()
}

/// Every balanced `{...}` span in `text`, in the order the spans open.
///
/// A brace inside a JSON string does not close a document, so a path holding
/// one still yields a single span. Quotes are tracked only inside a candidate
/// object: prose around it may quote anything without hiding the document that
/// follows. A span that never closes is dropped, which leaves the output with
/// no published object for [`status_unreadable`] to report.
fn json_object_spans(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut spans: Vec<&str> = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (index, &byte) in bytes.iter().enumerate() {
        if in_string {
            match byte {
                b'\\' => escaped = !escaped,
                b'"' => {
                    // A quoted quote stays inside the string.
                    in_string = escaped;
                    escaped = false;
                }
                _ => {}
            }
            continue;
        }
        match byte {
            b'"' if depth > 0 => in_string = true,
            b'{' => {
                if depth == 0 {
                    start = index;
                }
                depth += 1;
            }
            b'}' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    spans.push(&text[start..=index]);
                }
            }
            _ => {}
        }
    }
    spans
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
    let declared = manifest.declares_detect_contract();
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
                    status_unreadable: false,
                };
            };
            let (status, status_unreadable) = match &actions.detect {
                Some(detect) => {
                    let contract = DetectorContract::for_target(declared, name);
                    match run_detect(adapter_dir, name, detect, contract) {
                        Ok(reading) => reading,
                        Err(reason) => {
                            if launch_failure.is_none() {
                                launch_failure = Some(reason);
                            }
                            (DetectStatus::Unavailable, false)
                        }
                    }
                }
                None => (DetectStatus::NotChecked, false),
            };
            FrameworkInfo {
                name: name.clone(),
                status,
                capabilities,
                install_script: actions.install.clone(),
                status_unreadable,
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
fn print_status(frameworks: &[FrameworkInfo], adapter_dir: &Path) {
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

    // A pre-contract tree can hold targets whose detectors reported the host
    // rather than the adapter. Their state cannot be established and their
    // installers exit successfully without registering anything, so say why
    // they are not offered instead of leaving an unexplained row behind.
    let unverified: Vec<&str> = frameworks
        .iter()
        .filter(|fw| fw.status == DetectStatus::Unverifiable)
        .map(|fw| fw.name.as_str())
        .collect();
    if !unverified.is_empty() {
        println!();
        for name in &unverified {
            println!(
                "warning: {name} cannot be verified from these adapter resources, \
                 so it is not offered for installation."
            );
        }
        println!(
            "         {}",
            incompatible_resources_error(&unverified.join(", "), adapter_dir)
        );
    }

    // A detector that published unparseable JSON gave no answer, so its target
    // is not offered for installation: its installer could exit successfully
    // without registering anything and the run would report that as success.
    let unreadable: Vec<&str> = frameworks
        .iter()
        .filter(|fw| fw.status_unreadable)
        .map(|fw| fw.name.as_str())
        .collect();
    if !unreadable.is_empty() {
        println!();
        for name in unreadable {
            println!("warning: {}", unreadable_status_error(name, adapter_dir));
        }
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

/// Explain why targets from an adapter tree that predates the detector contract
/// are neither reported ready nor offered for installation.
///
/// `names` may be one target or the whole affected list: the table footnote
/// explains every unavailable target at once, while a named operation reports
/// just the target the caller asked for.
fn incompatible_resources_error(names: &str, adapter_dir: &Path) -> String {
    format!(
        "{names} cannot be installed from the adapter resources at {dir}: they predate the \
         detector status contract, so those detectors report the host rather than the adapter \
         and their installers exit successfully without registering anything. Reinstall \
         tokenless to refresh them, or point ANOLISA_ADAPTER_DIR at a current adapter \
         directory.",
        dir = adapter_dir.display()
    )
}

/// Explain why a target whose detector published unparseable output is not
/// offered for installation.
fn unreadable_status_error(name: &str, adapter_dir: &Path) -> String {
    format!(
        "{name}'s detector printed output that is not parseable JSON, so nothing is known about \
         the adapter and it was not installed. Its installer may exit successfully without \
         registering anything, so an unreadable answer must not select it. Reinstall tokenless \
         to refresh the adapter resources at {dir}, or point ANOLISA_ADAPTER_DIR at a current \
         adapter directory.",
        dir = adapter_dir.display()
    )
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
        DetectStatus::Unverifiable => Err((incompatible_resources_error(&fw.name, adapter_dir), 1)),
        DetectStatus::Unreadable => Err((unreadable_status_error(&fw.name, adapter_dir), 1)),
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
    print_status(&frameworks, &adapter_dir);

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
        interpret_detect(exit_code, stdout, DetectorContract::Declared)
    }

    /// `interpret_detect` for a pre-contract target whose detector already
    /// reported adapter state through the tri-state exit code.
    fn legacy_tri_state(exit_code: Option<i32>, stdout: &str) -> DetectStatus {
        interpret_detect(exit_code, stdout, DetectorContract::LegacyTriState)
    }

    /// `interpret_detect` for a pre-contract target whose detector reported the
    /// host rather than the adapter.
    fn legacy_host_only(exit_code: Option<i32>, stdout: &str) -> DetectStatus {
        interpret_detect(exit_code, stdout, DetectorContract::LegacyHostOnly)
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
    fn pre_contract_tri_state_detector_keeps_its_verified_readiness() {
        // Discarding the tri-state contract for a whole tree would turn an
        // already registered OpenCode plugin into an offer to reinstall it.
        assert_eq!(
            legacy_tri_state(Some(0), "[tokenless] opencode: ready (1.2.3)\n"),
            DetectStatus::Ready
        );
        assert_eq!(legacy_tri_state(Some(1), ""), DetectStatus::Installable);
        assert_eq!(legacy_tri_state(Some(2), ""), DetectStatus::MissingPrereqs);
        assert_eq!(legacy_tri_state(None, ""), DetectStatus::MissingPrereqs);
    }

    #[test]
    fn pre_contract_host_only_detector_establishes_nothing() {
        // The shapes the previous adapter tree shipped: codex probed the
        // tokenless binary, qoder probed host-CLI capability, qwenwork returned
        // 0 either way — and all three exited 0.
        assert_eq!(
            legacy_host_only(Some(0), r#"{"installed": true, "version": "9.9.9"}"#),
            DetectStatus::Unverifiable
        );
        assert_eq!(
            legacy_host_only(Some(0), "[tokenless] qoder: detected compatible qodercli\n"),
            DetectStatus::Unverifiable
        );
        assert_eq!(legacy_host_only(Some(0), ""), DetectStatus::Unverifiable);
    }

    #[test]
    fn pre_contract_host_only_detector_keeps_its_negative_answers() {
        // A failed probe or an absent prerequisite is still information.
        assert_eq!(
            legacy_host_only(Some(0), r#"{"installed": false}"#),
            DetectStatus::MissingPrereqs
        );
        assert_eq!(legacy_host_only(Some(1), ""), DetectStatus::MissingPrereqs);
        assert_eq!(legacy_host_only(Some(2), ""), DetectStatus::MissingPrereqs);
        assert_eq!(legacy_host_only(None, ""), DetectStatus::MissingPrereqs);
    }

    #[test]
    fn published_status_survives_a_pre_contract_tree() {
        assert_eq!(
            legacy_host_only(Some(0), r#"{"status":"ready"}"#),
            DetectStatus::Ready
        );
        assert_eq!(
            legacy_tri_state(Some(0), r#"{"status":"missing_prereqs"}"#),
            DetectStatus::MissingPrereqs
        );
    }

    #[test]
    fn the_host_only_inventory_is_frozen_and_only_for_pre_contract_trees() {
        for name in ["codex", "qoder", "qwenwork"] {
            assert_eq!(
                DetectorContract::for_target(false, name),
                DetectorContract::LegacyHostOnly,
                "{name}"
            );
            // A declaring tree is never second-guessed.
            assert_eq!(
                DetectorContract::for_target(true, name),
                DetectorContract::Declared,
                "{name}"
            );
        }
        for name in [
            "opencode",
            "openclaw",
            "hermes",
            "claude-code",
            "qwencode",
            "qwenpaw",
        ] {
            assert_eq!(
                DetectorContract::for_target(false, name),
                DetectorContract::LegacyTriState,
                "{name}"
            );
        }
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

    /// The jq-free codex detector output a path holding a quote used to
    /// produce: a document that starts like JSON and never closes a string.
    const BROKEN_CODEX_STATUS: &str = "{\n  \"installed\": true,\n  \"status\": \"installable\",\n  \"path\": \"/opt/we\"ird/tokenless\"\n}\n";

    #[test]
    fn an_unparseable_status_document_selects_nothing() {
        assert!(status_unreadable(BROKEN_CODEX_STATUS));
        // Neither selectable state may stand in for an answer the detector
        // failed to publish: ready would skip the registration, and installable
        // would run an installer allowed to exit 0 after skipping its work.
        assert_eq!(
            declared(Some(0), BROKEN_CODEX_STATUS),
            DetectStatus::Unreadable
        );
        assert_ne!(declared(Some(0), BROKEN_CODEX_STATUS), DetectStatus::Ready);
        assert_ne!(
            declared(Some(0), BROKEN_CODEX_STATUS),
            DetectStatus::Installable
        );
    }

    #[test]
    fn an_unparseable_document_overrides_every_contract_and_exit_code() {
        // The document is the answer these detectors owe, so a broken one is
        // unreadable whatever else the run reported.
        for contract in [
            DetectorContract::Declared,
            DetectorContract::LegacyTriState,
            DetectorContract::LegacyHostOnly,
        ] {
            for exit_code in [Some(0), Some(1), Some(2), None] {
                assert_eq!(
                    interpret_detect(exit_code, BROKEN_CODEX_STATUS, contract),
                    DetectStatus::Unreadable,
                    "{contract:?} exit {exit_code:?}"
                );
            }
        }
    }

    #[test]
    fn a_parseable_document_without_a_status_field_is_still_an_answer() {
        // Only a document that fails to parse is unreadable. One that parses
        // and simply carries no recognised field falls through to the exit
        // code, so an unrelated JSON log cannot disable a working detector.
        assert!(!status_unreadable("{\"installed\": true}"));
        assert_eq!(
            declared(Some(0), "{\"installed\": true}"),
            DetectStatus::Ready
        );
        assert_eq!(
            declared(Some(1), "{\"version\": \"1.2.3\"}"),
            DetectStatus::Installable
        );
        assert_eq!(
            declared(Some(2), "{\"version\": \"1.2.3\"}"),
            DetectStatus::MissingPrereqs
        );
    }

    #[test]
    fn only_an_intended_json_document_can_be_unreadable() {
        assert!(!status_unreadable(""));
        assert!(!status_unreadable("[tokenless] opencode: ready (1.2.3)\n"));
        assert!(!status_unreadable("[tokenless] detect\n{not json}\n"));
        assert!(!status_unreadable("{\"status\": \"ready\"}"));
        assert!(!status_unreadable("{\n  \"installed\": false\n}\n"));
        assert!(status_unreadable("{\"status\": }"));
        assert!(status_unreadable("  { broken"));
        // A document no longer has to open the output to be recognised: one
        // line of inherited Bash startup output may precede it.
        assert!(status_unreadable(
            "shell-initialized\n{\n  \"status\": \"ready\"\n"
        ));
        assert!(status_unreadable("shell-initialized\n{\"status\": }"));
        assert!(!status_unreadable(
            "shell-initialized\n{\"status\": \"ready\"}\n"
        ));
        // Prose that holds a brace but never opens an object literal is not a
        // document, so it cannot disable a detector that reports by exit code.
        assert!(!status_unreadable("[tokenless] run: jq '{status: 1}'\n"));
    }

    /// One line of Bash startup output: the lifecycle scripts inherit the
    /// caller's environment, so a `BASH_ENV` file that writes to stdout lands
    /// ahead of whatever the detector itself publishes.
    const STARTUP_LINE: &str = "shell-initialized\n";

    /// What the shipped Codex detector emits without `jq`: one object spread
    /// over several lines, so it is neither the whole output nor one line of it
    /// once something else shares the stream.
    const PRETTY_CODEX_STATUS: &str = "{\n  \"installed\": true,\n  \"status\": \"missing_prereqs\",\n  \"version\": \"9.9.9\",\n  \"path\": \"/opt/bin/tokenless\"\n}\n";

    #[test]
    fn published_status_survives_a_bash_startup_line() {
        let stdout = format!("{STARTUP_LINE}{PRETTY_CODEX_STATUS}");
        assert_eq!(declared(Some(0), &stdout), DetectStatus::MissingPrereqs);
        // The exit code of a detector that publishes its status is a constant,
        // so losing the document may not read as a verified registration.
        assert_ne!(declared(Some(0), &stdout), DetectStatus::Ready);
        assert_ne!(declared(Some(0), &stdout), DetectStatus::Installable);
        assert_eq!(
            legacy_host_only(Some(0), &stdout),
            DetectStatus::MissingPrereqs
        );
        assert_eq!(
            legacy_tri_state(Some(0), &stdout),
            DetectStatus::MissingPrereqs
        );
    }

    #[test]
    fn published_status_survives_output_on_both_sides_of_it() {
        // A startup file may print after the document as well, and a detector
        // may log a line of its own around a compact status.
        let after = format!("{STARTUP_LINE}{PRETTY_CODEX_STATUS}{STARTUP_LINE}");
        assert_eq!(declared(Some(0), &after), DetectStatus::MissingPrereqs);
        let around = format!(
            "[tokenless] qoder: detected compatible qodercli\n{{\"status\":\"installable\"}}\n{STARTUP_LINE}"
        );
        assert_eq!(declared(Some(0), &around), DetectStatus::Installable);
    }

    #[test]
    fn json_objects_reads_a_multiline_document_beside_prose() {
        let stdout = format!("{STARTUP_LINE}{PRETTY_CODEX_STATUS}");
        let objects = json_objects(&stdout);
        assert_eq!(objects.len(), 1, "{objects:?}");
        assert_eq!(
            objects[0].get("status").and_then(|value| value.as_str()),
            Some("missing_prereqs")
        );
        // Two documents on one line are two answers, neither of them lost.
        assert_eq!(json_objects("{\"a\": 1} {\"status\": \"ready\"}").len(), 2);
    }

    #[test]
    fn json_object_spans_track_strings_and_nesting() {
        // A quoted startup line, a brace inside a path and an escaped quote
        // must not hide or split the document.
        let stdout = "echo \"shell initialized\"\n{\"path\": \"/opt/we{ird}/to\\\"kenless\", \"status\": \"ready\"}\n";
        assert_eq!(json_objects(stdout).len(), 1, "{stdout}");
        assert_eq!(declared(Some(1), stdout), DetectStatus::Ready);
        // A nested object is one document, not two.
        assert_eq!(json_object_spans("{\"a\": {\"b\": 1}}").len(), 1);
        // A document that never closes publishes nothing, which is what makes
        // it unreadable rather than an answer.
        assert!(json_object_spans("{\"a\": \"x").is_empty());
        assert!(json_object_spans("no braces here").is_empty());
    }

    #[test]
    fn an_unparseable_document_behind_a_startup_line_is_still_unreadable() {
        // The escaping bug of the previous round, one startup line later: the
        // document was opened, so nothing it failed to say may be replaced by
        // the exit code that followed it.
        let stdout = format!("{STARTUP_LINE}{BROKEN_CODEX_STATUS}");
        assert!(status_unreadable(&stdout));
        for contract in [
            DetectorContract::Declared,
            DetectorContract::LegacyTriState,
            DetectorContract::LegacyHostOnly,
        ] {
            for exit_code in [Some(0), Some(1), Some(2), None] {
                assert_eq!(
                    interpret_detect(exit_code, &stdout, contract),
                    DetectStatus::Unreadable,
                    "{contract:?} exit {exit_code:?}"
                );
            }
        }
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
