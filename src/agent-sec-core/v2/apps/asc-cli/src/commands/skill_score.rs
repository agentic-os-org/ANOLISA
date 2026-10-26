//! Local assessment dispatch into the scoring runtime shipped with this installation.

use std::ffi::OsString;
use std::io;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

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
    /// Replaces this process with the private scoring runtime, preserving its process group and I/O.
    ///
    /// Success never returns. Signals and exit status remain owned by the scoring process.
    ///
    /// # Errors
    /// Returns the executable-location or exec error, including a missing scoring payload.
    pub fn exec(&self) -> io::Result<std::convert::Infallible> {
        let executable = std::env::current_exe()?;
        let prefix = executable.parent().and_then(Path::parent).ok_or_else(|| {
            io::Error::other("cannot locate the AgentSecCore installation prefix")
        })?;
        let private_cli = prefix.join("lib/anolisa/sec-core/skill-score/cli/bin/agent-sec-cli");
        let error = Command::new(&private_cli).args(self.arguments()).exec();
        Err(io::Error::new(
            error.kind(),
            format!(
                "cannot execute Skill scoring runtime at {}: {error}; install a sec-core package containing the scoring payload",
                private_cli.display()
            ),
        ))
    }

    fn arguments(&self) -> Vec<OsString> {
        let mut arguments = vec![OsString::from("skill-score")];
        let input = match &self.action {
            ScoreAction::Plan {
                skill_dir,
                output_file,
            } => {
                arguments.push("plan".into());
                append_path(&mut arguments, "--output-file=", output_file.as_deref());
                skill_dir.as_os_str()
            }
            ScoreAction::Run {
                task_input,
                output_file,
                evidence_dir,
            } => {
                arguments.push("run".into());
                append_path(&mut arguments, "--output-file=", output_file.as_deref());
                append_path(&mut arguments, "--evidence-dir=", evidence_dir.as_deref());
                task_input.as_os_str()
            }
        };
        // Keep leading hyphens and inline JSON as positional data in the Python parser.
        arguments.extend([OsString::from("--"), input.to_owned()]);
        arguments
    }
}

fn append_path(arguments: &mut Vec<OsString>, option: &str, path: Option<&Path>) {
    if let Some(path) = path {
        let mut value = OsString::from(option);
        value.push(path);
        arguments.push(value);
    }
}
