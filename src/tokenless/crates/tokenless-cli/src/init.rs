//! Community entry point — detect agent frameworks and install adapters.
//!
//! Scans the adapter directory for framework-specific `detect.sh` scripts,
//! reports each framework's status, and optionally runs `install.sh` to
//! register the tokenless adapter with the selected framework.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, IsTerminal as _, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

/// Sub-path from a share root to the tokenless adapter directory.
const ADAPTER_SUBPATH: &str = "anolisa/adapters/tokenless";

#[derive(Debug, Deserialize)]
struct Manifest {
    targets: BTreeMap<String, ManifestTarget>,
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
}

impl DetectStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Installable => "installable",
            Self::MissingPrereqs => "missing prereqs",
            Self::NotChecked => "n/a",
        }
    }
}

struct FrameworkInfo {
    name: String,
    status: DetectStatus,
    capabilities: Vec<String>,
    install_script: Option<String>,
}

/// Find the adapter directory by checking known locations.
fn find_adapter_dir() -> Option<PathBuf> {
    if let Some(home) = dirs::home_dir() {
        let p = home.join(".local/share").join(ADAPTER_SUBPATH);
        if has_manifest(&p) {
            return Some(p);
        }
    }
    for prefix in ["/usr/share", "/usr/local/share"] {
        let p = PathBuf::from(prefix).join(ADAPTER_SUBPATH);
        if has_manifest(&p) {
            return Some(p);
        }
    }
    None
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

/// Run `detect.sh` for a framework and interpret the tri-state exit code.
fn run_detect(adapter_dir: &Path, name: &str, detect_script: &str) -> DetectStatus {
    let script = adapter_dir.join(detect_script);
    if !script.exists() {
        return DetectStatus::NotChecked;
    }
    match Command::new("bash")
        .arg(&script)
        .env("ANOLISA_COMPONENT", "tokenless")
        .env("ANOLISA_TARGET", name)
        .env("ANOLISA_ADAPTER_DIR", adapter_dir)
        .output()
    {
        Ok(out) => match out.status.code() {
            Some(0) => {
                // Some detectors (e.g. Codex) always exit 0 but emit
                // {"installed": false} when the tool is absent.
                let stdout = String::from_utf8_lossy(&out.stdout);
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(stdout.trim()) {
                    if v.get("installed") == Some(&serde_json::Value::Bool(false)) {
                        return DetectStatus::Installable;
                    }
                }
                DetectStatus::Ready
            }
            Some(1) => DetectStatus::Installable,
            _ => DetectStatus::MissingPrereqs,
        },
        Err(_) => DetectStatus::MissingPrereqs,
    }
}

/// Run `install.sh` for a framework, streaming output to the terminal.
fn run_install(adapter_dir: &Path, name: &str, install_script: &str) -> Result<(), String> {
    let script = adapter_dir.join(install_script);
    if !script.exists() {
        return Err(format!("install script not found: {}", script.display()));
    }
    let out = Command::new("bash")
        .arg(&script)
        .env("ANOLISA_COMPONENT", "tokenless")
        .env("ANOLISA_TARGET", name)
        .env("ANOLISA_ADAPTER_DIR", adapter_dir)
        .output()
        .map_err(|e| format!("failed to run install.sh: {}", e))?;
    if out.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stderr_snippet = stderr.trim();
        if stderr_snippet.is_empty() {
            Err(format!(
                "install.sh for {} exited with code {}",
                name,
                out.status.code().unwrap_or(-1)
            ))
        } else {
            Err(format!(
                "install.sh for {} exited with code {}: {}",
                name,
                out.status.code().unwrap_or(-1),
                stderr_snippet
            ))
        }
    }
}

pub fn run(framework: Option<String>, all: bool, list_only: bool) -> Result<(), (String, i32)> {
    let adapter_dir = find_adapter_dir().ok_or_else(|| {
        (
            "Adapter directory not found.\nInstall tokenless via `npm install -g anolisa-tokenless` or `anolisa install tokenless`."
                .to_string(),
            1,
        )
    })?;

    let manifest = load_manifest(&adapter_dir).map_err(|e| (e, 1))?;

    let mut frameworks: Vec<FrameworkInfo> = Vec::new();
    for (name, target) in &manifest.targets {
        let caps = extract_hooks(&target.capabilities);
        let actions = match &target.actions {
            Some(a) => a,
            None => {
                frameworks.push(FrameworkInfo {
                    name: name.clone(),
                    status: DetectStatus::NotChecked,
                    capabilities: caps,
                    install_script: None,
                });
                continue;
            }
        };
        let status = match &actions.detect {
            Some(d) => run_detect(&adapter_dir, name, d),
            None => DetectStatus::NotChecked,
        };
        frameworks.push(FrameworkInfo {
            name: name.clone(),
            status,
            capabilities: caps,
            install_script: actions.install.clone(),
        });
    }

    frameworks.sort_by(|a, b| a.name.cmp(&b.name));

    println!("\nTokenless Adapter Status\n");
    println!("{:<14} {:<16} Hooks", "Framework", "Status");
    println!("{:-<14} {:-<16} {:-<40}", "", "", "");
    for fw in &frameworks {
        let caps = if fw.capabilities.is_empty() {
            "—".to_string()
        } else {
            fw.capabilities.join(", ")
        };
        println!("{:<14} {:<16} {}", fw.name, fw.status.label(), caps);
    }
    println!("\nAdapter directory: {}", adapter_dir.display());

    // --framework: check/install one specific framework
    if let Some(name) = &framework {
        let fw = frameworks.iter().find(|f| &f.name == name);
        match fw {
            Some(f) if f.status == DetectStatus::Installable => {
                if let Some(script) = &f.install_script {
                    println!("\nInstalling {} adapter...", f.name);
                    match run_install(&adapter_dir, &f.name, script) {
                        Ok(()) => println!("{} adapter installed successfully.", f.name),
                        Err(e) => {
                            return Err((
                                format!("{} adapter installation failed: {}", f.name, e),
                                1,
                            ));
                        }
                    }
                } else {
                    eprintln!("tokenless: no install script for {}", f.name);
                }
                return Ok(());
            }
            Some(f) if f.status == DetectStatus::Ready => {
                println!("\n{} is already installed and ready.", f.name);
                return Ok(());
            }
            Some(f) => {
                return Err((
                    format!(
                        "{} is not installable (status: {})",
                        f.name,
                        f.status.label()
                    ),
                    1,
                ));
            }
            None => {
                return Err((format!("unknown framework: {}", name), 1));
            }
        }
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
        let script = match &fw.install_script {
            Some(s) => s,
            None => {
                eprintln!("tokenless: no install script for {}", fw.name);
                continue;
            }
        };
        println!("\nInstalling {} adapter...", fw.name);
        match run_install(&adapter_dir, &fw.name, script) {
            Ok(()) => println!("{} adapter installed successfully.", fw.name),
            Err(e) => {
                eprintln!("{} adapter installation failed: {}", fw.name, e);
                failed.push(fw.name.clone());
            }
        }
    }

    if !failed.is_empty() {
        return Err((
            format!("installation failed for: {}", failed.join(", ")),
            1,
        ));
    }

    Ok(())
}

fn print_install_hints() {
    println!("\nRun `tokenless init --framework <name>` to install a specific adapter.");
    println!("Run `tokenless init --all` to install all installable adapters.");
}
