//! Skill Ledger business CLI; all scanning and trust operations execute in the daemon.

use crate::InputError;
use asc_daemon_protocol::{DaemonRequest, method};
use clap::{Args, Subcommand};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::fs;
use std::io::Read as _;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Args)]
pub(crate) struct Selection {
    /// Skill path; omitted only with --all.
    skill_dir: Option<PathBuf>,
    /// Include registered Skills and the current user's default Skill directories.
    #[arg(long)]
    all: bool,
}

#[derive(Debug, Subcommand)]
pub(crate) enum SkillLedgerCommand {
    /// Initialize the daemon system key and scan a baseline.
    Init {
        /// Initialize only the key.
        #[arg(long)]
        no_baseline: bool,
        /// Root-only key rotation; old records must establish trust again.
        #[arg(long)]
        force_keys: bool,
        /// Comma-separated built-in scanners.
        #[arg(long)]
        scanners: Option<String>,
        /// Additional exact Skill roots for the baseline, repeatable.
        #[arg(long = "skill-dir")]
        skill_dirs: Vec<PathBuf>,
    },
    /// Check current content against its signed record.
    Check(Selection),
    /// Analyze content without changing keys or Ledger state.
    Analyze {
        skill_dir: Option<PathBuf>,
        #[arg(long, default_value = "json")]
        format: String,
    },
    /// Scan current content and record signed results.
    Scan {
        #[command(flatten)]
        selection: Selection,
        #[arg(long)]
        force: bool,
        #[arg(long)]
        scanners: Option<String>,
    },
    /// Import external findings into the signed Ledger.
    Certify {
        skill_dir: PathBuf,
        #[arg(long)]
        findings: Option<PathBuf>,
        #[arg(long, default_value = "skill-vetter")]
        scanner: String,
        #[arg(long)]
        scanner_version: Option<String>,
        #[arg(long)]
        delete_findings: bool,
    },
    /// Query system key readiness and registered Skill health.
    Status {
        #[arg(long, short)]
        verbose: bool,
    },
    /// Verify every recorded version and optionally its snapshot.
    Audit {
        skill_dir: PathBuf,
        #[arg(long)]
        verify_snapshots: bool,
    },
    /// List daemon-configured scanners.
    ListScanners,
    /// Apply allow, `always_allow`, block or rollback, or clear a decision.
    Decide {
        skill_dir: PathBuf,
        #[arg(long)]
        action: Option<String>,
        #[arg(long)]
        version: Option<String>,
        #[arg(long)]
        reason: Option<String>,
        #[arg(long)]
        clear: bool,
    },
    /// Inspect selected activation and source consistency.
    Show {
        skill_dir: PathBuf,
        #[arg(long)]
        policy: Option<String>,
    },
    /// Export a verified snapshot to an empty directory owned by this caller.
    Export {
        skill_dir: PathBuf,
        #[arg(long, default_value = "latest")]
        version: String,
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        policy: Option<String>,
    },
    /// Retry publishing the selected activation.
    Activate { skill_dir: PathBuf },
    /// Rotate the system signing key as root; no historical key fallback is retained.
    RotateKeys,
}

impl SkillLedgerCommand {
    pub(crate) fn request(&self) -> Result<DaemonRequest, InputError> {
        let params = match self {
            Self::Init {
                no_baseline,
                force_keys,
                scanners,
                skill_dirs,
            } => init_params(*no_baseline, *force_keys, scanners.as_deref(), skill_dirs)?,
            Self::Check(selection) => selection.params("check")?,
            Self::Scan {
                selection,
                force,
                scanners,
            } => {
                let mut params = selection.params("scan")?;
                params["force"] = json!(force);
                params["scanners"] = json!(names(scanners.as_deref()));
                params
            }
            Self::Analyze { skill_dir, format } => {
                if !format.eq_ignore_ascii_case("json") {
                    return Err(InputError::AnalyzeInput {
                        code: "unsupported-format",
                        message: "Output format must be 'json'.",
                    });
                }
                let path = skill_dir.as_ref().ok_or(InputError::AnalyzeInput {
                    code: "skill-dir-required",
                    message: "Skill directory is required.",
                })?;
                json!({"command":"analyze","skillDir":absolute(path)?})
            }
            Self::Certify {
                skill_dir,
                findings,
                scanner,
                scanner_version,
                ..
            } => {
                let path = findings.as_ref().ok_or_else(|| {
                    InputError::SkillSec(
                        "--findings is required for certify; use scan for built-in scanners".into(),
                    )
                })?;
                json!({"command":"certify","skillDir":absolute(skill_dir)?,"scanner":scanner,"scannerVersion":scanner_version,"findings":read_findings(path)?})
            }
            Self::Status { verbose } => json!({"command":"status","verbose":verbose}),
            Self::Audit {
                skill_dir,
                verify_snapshots,
            } => {
                json!({"command":"audit","skillDir":absolute(skill_dir)?,"verifySnapshots":verify_snapshots})
            }
            Self::ListScanners => json!({"command":"list-scanners"}),
            Self::Decide {
                skill_dir,
                action,
                version,
                reason,
                clear,
            } => {
                if *clear == action.is_some() {
                    return Err(InputError::SkillSec(
                        "select --clear or --action, exclusively".into(),
                    ));
                }
                json!({"command":"decide","skillDir":absolute(skill_dir)?,"action":action,"version":version,"reason":reason,"clear":clear})
            }
            Self::Show { skill_dir, policy } => {
                check_policy(policy.as_deref())?;
                json!({"command":"show","skillDir":absolute(skill_dir)?})
            }
            Self::Export {
                skill_dir,
                version,
                output,
                policy,
            } => {
                check_policy(policy.as_deref())?;
                let output = absolute(output)?;
                // The caller, not the root daemon, creates output parents under its own permissions.
                if !output.exists() {
                    fs::DirBuilder::new()
                        .recursive(true)
                        .mode(0o700)
                        .create(&output)?;
                }
                json!({"command":"export","skillDir":absolute(skill_dir)?,"version":version,"output":output})
            }
            Self::Activate { skill_dir } => {
                json!({"command":"activate","skillDir":absolute(skill_dir)?})
            }
            Self::RotateKeys => json!({"command":"rotate-keys"}),
        };
        serde_json::from_value::<asc_action_types::SkillSecCommand>(params.clone())
            .map_err(|error| InputError::SkillSec(error.to_string()))?;
        Ok(DaemonRequest {
            method: method::ACTION_SKILL_SEC.into(),
            params,
            trace_context: None,
            compatibility: None,
        })
    }

    pub(crate) fn after_success(&self, request: &DaemonRequest) -> Result<(), InputError> {
        if let Self::Certify {
            findings: Some(path),
            delete_findings: true,
            ..
        } = self
        {
            let path = absolute(path)?;
            let metadata = fs::symlink_metadata(&path)?;
            if !metadata.is_file() || read_findings(&path)? != request.params["findings"] {
                return Err(InputError::SkillSec(
                    "certification succeeded; findings file changed and was not deleted".into(),
                ));
            }
            fs::remove_file(path)?;
        }
        Ok(())
    }
}

impl Selection {
    fn params(&self, command: &str) -> Result<Value, InputError> {
        if self.all == self.skill_dir.is_some() {
            return Err(InputError::SkillSec(
                "select a Skill path or --all, exclusively".into(),
            ));
        }
        Ok(
            json!({"command":command,"skillDir":self.skill_dir.as_ref().map(|p| absolute(p)).transpose()?,"all":self.all,"skillDirs":if self.all { discover()? } else { Vec::new() }}),
        )
    }
}

fn init_params(
    no_baseline: bool,
    force_keys: bool,
    scanners: Option<&str>,
    skill_dirs: &[PathBuf],
) -> Result<Value, InputError> {
    let mut roots = if no_baseline { Vec::new() } else { discover()? };
    if !no_baseline {
        roots.extend(
            skill_dirs
                .iter()
                .map(|p| absolute(p))
                .collect::<Result<Vec<_>, _>>()?,
        );
    }
    Ok(
        json!({"command":"init","baseline":!no_baseline,"forceKeys":force_keys,"skillDirs":roots,"scanners":names(scanners)}),
    )
}

fn names(value: Option<&str>) -> Option<Vec<String>> {
    value.map(|s| {
        s.split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect()
    })
}

fn check_policy(policy: Option<&str>) -> Result<(), InputError> {
    if policy.is_some_and(|p| p != "pass_warn_only") {
        return Err(InputError::SkillSec(
            "only pass_warn_only activation policy is supported".into(),
        ));
    }
    Ok(())
}

fn read_findings(path: &Path) -> Result<Value, InputError> {
    let file = fs::File::from(
        rustix::fs::open(
            absolute(path)?,
            rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::NONBLOCK | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map_err(std::io::Error::from)?,
    );
    if !file.metadata()?.is_file() {
        return Err(InputError::SkillSec(
            "findings must be a regular JSON file".into(),
        ));
    }
    let mut bytes = Vec::new();
    file.take(2 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 2 * 1024 * 1024 {
        return Err(InputError::SkillSec(
            "findings exceed the 2 MiB import limit".into(),
        ));
    }
    Ok(serde_json::from_slice(&bytes)?)
}

fn absolute(path: &Path) -> Result<PathBuf, InputError> {
    let text = path
        .to_str()
        .ok_or_else(|| InputError::SkillSec("Skill paths must be UTF-8".into()))?;
    let path = if text == "~" || text.starts_with("~/") {
        let home = std::env::var_os("HOME")
            .ok_or_else(|| InputError::SkillSec("HOME is unavailable for path expansion".into()))?;
        PathBuf::from(home).join(text.strip_prefix("~/").unwrap_or(""))
    } else {
        path.to_path_buf()
    };
    let full = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut normalized = PathBuf::new();
    for component in full.components() {
        match component {
            Component::ParentDir => {
                normalized.pop();
            }
            Component::CurDir => {}
            other => normalized.push(other.as_os_str()),
        }
    }
    if normalized == Path::new("/") || !normalized.is_absolute() {
        return Err(InputError::SkillSec(
            "Skill path must name a directory below root".into(),
        ));
    }
    Ok(normalized)
}

fn discover() -> Result<Vec<PathBuf>, InputError> {
    let mut parents = vec![
        (PathBuf::from("/usr/share/anolisa/skills"), false),
        (PathBuf::from("/usr/local/share/anolisa/skills"), false),
    ];
    // Match the installer's UID home fallback when HOME is unset or empty.
    let home = dirs::home_dir();
    let data_home = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from);
    if let Some(parent) = anolisa_skill_dir(home.as_deref(), data_home.as_deref()) {
        parents.push((parent, false));
    }
    if let Some(home) = home {
        for path in [
            ".openclaw/skills",
            ".copilot-shell/skills",
            ".hermes/skills",
            ".qoder/skills",
        ] {
            parents.push((home.join(path), path == ".hermes/skills"));
        }
    }
    let mut roots = BTreeSet::new();
    let mut count = 0;
    for (parent, recursive) in parents {
        discover_in(&parent, recursive, 0, &mut count, &mut roots)?;
    }
    Ok(roots.into_iter().collect())
}

fn anolisa_skill_dir(home: Option<&Path>, data_home: Option<&Path>) -> Option<PathBuf> {
    // Match ANOLISA's installer before path normalization can discard dot segments.
    match data_home.filter(|path| {
        path.is_absolute()
            && !path
                .to_string_lossy()
                .split('/')
                .any(|segment| matches!(segment, "." | ".."))
    }) {
        Some(root) => Some(root.join("anolisa/skills")),
        None => home.map(|root| root.join(".local/share/anolisa/skills")),
    }
}

fn discover_in(
    parent: &Path,
    recursive: bool,
    depth: usize,
    count: &mut usize,
    roots: &mut BTreeSet<PathBuf>,
) -> Result<(), InputError> {
    let entries = match fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
            ) =>
        {
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        *count += 1;
        if *count > 10_000 || roots.len() > 1024 || depth > 32 {
            return Err(InputError::SkillSec(
                "Skill discovery exceeds directory limits".into(),
            ));
        }
        let entry = entry?;
        // Ledger snapshots also contain SKILL.md; never discover hidden/internal subtrees.
        if entry.file_name().as_encoded_bytes().starts_with(b".") || !entry.file_type()?.is_dir() {
            continue;
        }
        let path = entry.path();
        if fs::symlink_metadata(path.join("SKILL.md")).is_ok_and(|m| m.is_file()) {
            roots.insert(absolute(&path)?);
        }
        if recursive {
            discover_in(&path, true, depth + 1, count, roots)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{anolisa_skill_dir, discover_in};
    use crate::Cli;
    use serde_json::json;
    use std::collections::BTreeSet;
    use std::fs;
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::Path;

    #[test]
    fn anolisa_root_matches_installer_fallback_rules() {
        let home = Path::new("/home/test");
        for value in [
            None,
            Some(""),
            Some("relative"),
            Some("~/data"),
            Some("/data/./share"),
            Some("/data/../share"),
            Some("/data/."),
        ] {
            assert_eq!(
                anolisa_skill_dir(Some(home), value.map(Path::new)),
                Some(home.join(".local/share/anolisa/skills")),
                "XDG_DATA_HOME={value:?}"
            );
            assert_eq!(anolisa_skill_dir(None, value.map(Path::new)), None);
        }
        for value in [
            "/data",
            "/data with spaces/用户",
            "//data//share/",
            "/",
            "/missing/data",
        ] {
            let root = Path::new(value);
            for home in [Some(home), None] {
                assert_eq!(
                    anolisa_skill_dir(home, Some(root)),
                    Some(root.join("anolisa/skills"))
                );
            }
        }
    }

    #[test]
    fn export_creates_private_directories_and_preserves_existing_permissions() {
        let directory = std::env::temp_dir().join(format!("asc-export-{}", uuid::Uuid::new_v4()));
        let output = directory.join("nested/export");
        let prepare = || {
            Cli::parse_from([
                "agent-sec-cli",
                "skill-ledger",
                "export",
                "/skill",
                "--output",
                output.to_str().unwrap(),
            ])
            .unwrap()
            .request()
            .unwrap()
        };
        prepare();
        for path in [&directory, &directory.join("nested"), &output] {
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        for mode in [0o755, 0o775] {
            fs::set_permissions(&output, fs::Permissions::from_mode(mode)).unwrap();
            prepare();
            // Existing destinations remain subject to the daemon's permission checks.
            assert_eq!(
                fs::metadata(&output).unwrap().permissions().mode() & 0o777,
                mode
            );
        }
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn discovery_excludes_internal_snapshots_but_keeps_nested_skills() {
        let home = std::env::temp_dir().join(format!("asc-discovery-{}", uuid::Uuid::new_v4()));
        let parent = home.join(".hermes/skills");
        for relative in [
            "demo",
            "demo/nested",
            "demo/.skill-meta/versions/v000001.snapshot",
            "demo/.git/hidden-skill",
            ".archive/old-skill",
            ".hidden",
        ] {
            let root = parent.join(relative);
            fs::create_dir_all(&root).unwrap();
            fs::write(root.join("SKILL.md"), "Safe skill").unwrap();
        }
        for recursive in [false, true] {
            let mut roots = BTreeSet::new();
            discover_in(&parent, recursive, 0, &mut 0, &mut roots).unwrap();
            let mut expected = BTreeSet::from([parent.join("demo")]);
            if recursive {
                expected.insert(parent.join("demo/nested"));
            }
            assert_eq!(roots, expected);
        }
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn cli_prepares_current_commands_and_rejects_retired_or_conflicting_inputs() {
        let cli =
            Cli::parse_from(["agent-sec-cli", "skill-ledger", "init", "--no-baseline"]).unwrap();
        let request = cli.request().unwrap();
        assert_eq!(request.params["baseline"], false);
        assert_eq!(request.params["skillDirs"], json!([]));
        assert_eq!(request.params["timeoutMs"], 60_000);
        for arguments in [
            vec!["init-keys"],
            vec!["init", "--passphrase"],
            vec!["check", "/skill", "--all"],
            vec!["decide", "/skill", "--action", "allow", "--clear"],
        ] {
            let argv: Vec<_> = [vec!["agent-sec-cli", "skill-ledger"], arguments].concat();
            assert!(Cli::parse_from(argv).map_or(true, |cli| cli.request().is_err()));
        }
        let cli = Cli::parse_from([
            "agent-sec-cli",
            "skill-ledger",
            "scan",
            "/skill",
            "--scanners",
            " code-scanner, static-scanner ",
        ])
        .unwrap();
        assert_eq!(
            cli.request().unwrap().params["scanners"],
            json!(["code-scanner", "static-scanner"])
        );
    }
}
