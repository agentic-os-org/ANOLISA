//! Explicit local native assessment; only dynamic execution uses the optional worker.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

use asc_skill_score::Error;
use clap::{Args, Subcommand};

/// Local assessment commands; no daemon or signed Ledger state is involved.
#[derive(Debug, Args)]
pub struct SkillScoreCommand {
    #[command(subcommand)]
    action: ScoreAction,
}

#[derive(Debug, Subcommand)]
enum ScoreAction {
    /// Plan an assessment of one local Skill directory.
    Plan {
        skill_dir: PathBuf,
        /// Save the task JSON to a new file.
        #[arg(long)]
        output_file: Option<PathBuf>,
    },
    /// Execute and score a task JSON file, inline JSON, or - for stdin.
    Run {
        task_input: OsString,
        /// Save the report JSON to a new file.
        #[arg(long)]
        output_file: Option<PathBuf>,
        /// Keep detailed evidence in an absent or empty directory.
        #[arg(long)]
        evidence_dir: Option<PathBuf>,
    },
}

impl SkillScoreCommand {
    /// Execute the explicitly requested native local assessment without a daemon fallback.
    pub fn execute(&self) -> u8 {
        match self.run_native() {
            Ok(code) => code,
            Err(error) => {
                let message = error.to_string();
                // Paths are caller-controlled and can contain the configured model credential.
                let message = asc_model_client::remote::RemoteModel::from_environment()
                    .map_or(message.clone(), |model| model.redact_diagnostic(&message));
                eprintln!("skill-score: {message}");
                if matches!(
                    error,
                    asc_skill_score::Error::Contract(_) | asc_skill_score::Error::Json(_)
                ) {
                    2
                } else {
                    1
                }
            }
        }
    }

    // Debug formatting keeps diagnostic paths quoted and escapes control characters.
    #[allow(clippy::unnecessary_debug_formatting)]
    fn input(
        &self,
        cancelled: &std::sync::atomic::AtomicBool,
    ) -> asc_skill_score::Result<Input<'_>> {
        use std::io::Read as _;
        let (task, skill, output_file, evidence_dir) = match &self.action {
            ScoreAction::Plan {
                skill_dir,
                output_file,
            } => (
                None,
                skill_dir.canonicalize().map_err(|error| {
                    asc_skill_score::Error::Contract(format!(
                        "stage=plan.skill.resolve path={skill_dir:?}: Skill directory is unavailable: {error}"
                    ))
                })?,
                output_file.as_deref(),
                None,
            ),
            ScoreAction::Run {
                task_input,
                output_file,
                evidence_dir,
            } => {
                let input = task_input.to_str().ok_or_else(|| {
                    asc_skill_score::Error::Contract("task input must be UTF-8".into())
                })?;
                let mut bytes = Vec::new();
                let base = if input == "-" {
                    bytes = read_stdin(cancelled)?;
                    std::env::current_dir()
                        .map_err(|e| Error::at_path("task.cwd", Path::new("."), e))?
                } else if input.trim_start().starts_with('{') {
                    bytes.extend_from_slice(input.as_bytes());
                    std::env::current_dir()
                        .map_err(|e| Error::at_path("task.cwd", Path::new("."), e))?
                } else {
                    let path = Path::new(input)
                        .canonicalize()
                        .map_err(|e| Error::at_path("task.resolve", Path::new(input), e))?;
                    std::fs::File::open(&path)
                        .map_err(|e| Error::at_path("task.read", &path, e))?
                        .take(16_777_217)
                        .read_to_end(&mut bytes)
                        .map_err(|e| Error::at_path("task.read", &path, e))?;
                    path.parent()
                        .ok_or_else(|| {
                            asc_skill_score::Error::Contract("task path lacks parent".into())
                        })?
                        .to_path_buf()
                };
                if bytes.len() > 16_777_216 {
                    return Err(asc_skill_score::Error::Contract(
                        "task exceeds 16 MiB".into(),
                    ));
                }
                let task = asc_skill_score::parse_task(serde_json::from_slice(&bytes)?, &base)?;
                let skill = PathBuf::from(task["skill_path"].as_str().ok_or_else(|| {
                    asc_skill_score::Error::Contract("skill_path missing".into())
                })?);
                (
                    Some(task),
                    skill,
                    output_file.as_deref(),
                    evidence_dir.as_deref(),
                )
            }
        };
        Ok(Input {
            task,
            skill,
            output_file,
            evidence_dir,
        })
    }

    // Debug formatting keeps diagnostic paths quoted and escapes control characters.
    #[allow(clippy::unnecessary_debug_formatting)]
    fn run_native(&self) -> asc_skill_score::Result<u8> {
        use std::io::Write as _;
        use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
        let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let term = signal_hook::flag::register(signal_hook::consts::SIGTERM, cancelled.clone())?;
        let interrupt =
            signal_hook::flag::register(signal_hook::consts::SIGINT, cancelled.clone())?;
        let result = (|| {
            let Input {
                task,
                skill,
                output_file,
                evidence_dir,
            } = self.input(&cancelled)?;
            if !skill.join("SKILL.md").is_file() {
                let path = skill.join("SKILL.md");
                return Err(asc_skill_score::Error::Contract(format!(
                    "stage=skill.entry.inspect path={path:?}: Skill directory must contain SKILL.md"
                )));
            }
            check_targets(&skill, output_file, evidence_dir)?;
            let temporary = tempfile::Builder::new()
                .prefix("asc-skill-score-")
                .tempdir()
                .map_err(|e| Error::at_path("evidence.temp.create", &std::env::temp_dir(), e))?;
            let evidence = if let Some(path) = evidence_dir {
                std::fs::create_dir_all(path)
                    .map_err(|e| Error::at_path("evidence.create", path, e))?;
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
                    .map_err(|e| Error::at_path("evidence.permissions", path, e))?;
                path.canonicalize()
                    .map_err(|e| Error::at_path("evidence.resolve", path, e))?
            } else {
                temporary.path().to_path_buf()
            };
            let (mut value, code) = if let Some(task) = task {
                let executable = std::env::current_exe().map_err(|e| {
                    Error::at_path("runtime.executable.resolve", Path::new("/proc/self/exe"), e)
                })?;
                let prefix = executable.parent().and_then(Path::parent).ok_or_else(|| {
                    asc_skill_score::Error::Runtime("cannot locate ASC installation".into())
                })?;
                let outcome = asc_skill_score::run(&task, &evidence, prefix, cancelled.clone())?;
                (outcome.report, outcome.exit_code)
            } else {
                (asc_skill_score::plan(&skill, cancelled.clone())?, 0)
            };
            if evidence_dir.is_some() {
                value["evidence_dir"] = evidence.to_string_lossy().into_owned().into();
            }
            let encoded = serde_json::to_string_pretty(&value)? + "\n";
            if let Some(path) = output_file {
                if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| Error::at_path("report.directory.create", parent, e))?;
                }
                let mut output = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(path)
                    .map_err(|e| Error::at_path("report.write", path, e))?;
                output
                    .write_all(encoded.as_bytes())
                    .map_err(|e| Error::at_path("report.write", path, e))?;
            }
            std::io::stdout().lock().write_all(encoded.as_bytes())?;
            Ok(code)
        })();
        signal_hook::low_level::unregister(term);
        signal_hook::low_level::unregister(interrupt);
        result
    }
}

struct Input<'a> {
    task: Option<serde_json::Value>,
    skill: PathBuf,
    output_file: Option<&'a Path>,
    evidence_dir: Option<&'a Path>,
}

fn resolved_output(path: &Path) -> io::Result<PathBuf> {
    let absolute = std::path::absolute(path)?;
    let mut ancestor = absolute.as_path();
    let mut suffix = Vec::new();
    while !ancestor.exists() {
        suffix.push(
            ancestor
                .file_name()
                .ok_or_else(|| io::Error::other("invalid output path"))?,
        );
        ancestor = ancestor
            .parent()
            .ok_or_else(|| io::Error::other("invalid output ancestor"))?;
    }
    let mut result = ancestor.canonicalize()?;
    for name in suffix.iter().rev() {
        for component in Path::new(name).components() {
            match component {
                std::path::Component::ParentDir => {
                    result.pop();
                }
                std::path::Component::Normal(name) => result.push(name),
                std::path::Component::CurDir => {}
                _ => return Err(io::Error::other("invalid output component")),
            }
        }
    }
    Ok(result)
}

fn check_targets(
    skill: &Path,
    output: Option<&Path>,
    evidence: Option<&Path>,
) -> asc_skill_score::Result<()> {
    for path in [output, evidence].into_iter().flatten() {
        if resolved_output(path)
            .map_err(|e| Error::at_path("output.resolve", path, e))?
            .starts_with(skill)
        {
            return Err(asc_skill_score::Error::Contract(
                "outputs must be outside the evaluated Skill".into(),
            ));
        }
    }
    if output.is_some_and(|p| std::fs::symlink_metadata(p).is_ok()) {
        return Err(asc_skill_score::Error::Contract(
            "output file already exists".into(),
        ));
    }
    if let Some(path) = evidence {
        if let Ok(metadata) = std::fs::symlink_metadata(path)
            && (metadata.file_type().is_symlink()
                || !metadata.is_dir()
                || std::fs::read_dir(path)
                    .map_err(|e| Error::at_path("evidence.inspect", path, e))?
                    .next()
                    .is_some())
        {
            return Err(asc_skill_score::Error::Contract(
                "evidence directory must be absent or empty and not a symlink".into(),
            ));
        }
        if let Some(output) = output {
            let output =
                resolved_output(output).map_err(|e| Error::at_path("output.resolve", output, e))?;
            let evidence =
                resolved_output(path).map_err(|e| Error::at_path("output.resolve", path, e))?;
            if evidence.starts_with(&output) {
                return Err(asc_skill_score::Error::Contract(
                    "output file cannot be the evidence directory or its ancestor".into(),
                ));
            }
            if let Ok(relative) = output.strip_prefix(evidence) {
                let first = relative
                    .components()
                    .next()
                    .map(|c| c.as_os_str().to_string_lossy())
                    .unwrap_or_default();
                if [
                    "probes",
                    "task",
                    "environment",
                    "runtime-signals",
                    "source-redaction",
                    "evidence-catalog",
                    "judge-catalog",
                    "adjudication",
                    "critical",
                    "internal-report",
                    "events",
                ]
                .contains(&first.split('.').next().unwrap_or_default())
                {
                    return Err(asc_skill_score::Error::Contract(
                        "output conflicts with internal evidence paths".into(),
                    ));
                }
            }
        }
    }
    Ok(())
}

fn read_stdin(cancelled: &std::sync::atomic::AtomicBool) -> asc_skill_score::Result<Vec<u8>> {
    use std::io::Read as _;
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = std::io::stdin()
            .take(16_777_217)
            .read_to_end(&mut bytes)
            .map(|_| bytes);
        let _ = sender.send(result);
    });
    loop {
        if cancelled.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(asc_skill_score::Error::Cancelled);
        }
        match receiver.recv_timeout(std::time::Duration::from_millis(100)) {
            Ok(result) => return Ok(result?),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err(asc_skill_score::Error::Runtime(
                    "stdin reader stopped unexpectedly".into(),
                ));
            }
        }
    }
}
