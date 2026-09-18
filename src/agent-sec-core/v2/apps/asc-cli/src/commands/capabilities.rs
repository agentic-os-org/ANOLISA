//! Local command exposing the environment-variable capability view.
//!
//! Filters and the output format are validated here rather than by clap so the
//! rejection text and exit code match the V1 command exactly: V1 prints
//! `Error: ...` on stderr and exits 1, while clap's own value validation would
//! print `error: ...` and exit 2.

use std::io::{self, Write};

use clap::Args;

use crate::capabilities::{self, Environment};

/// Output formats accepted by `--output`; the value is matched case-sensitively.
const OUTPUT_FORMATS: [&str; 2] = ["table", "json"];

/// Shows agent-sec hook capabilities resolved from this process environment.
#[derive(Debug, Args)]
#[command(
    about = "Show agent-sec hook capabilities from the current CLI environment variables.",
    long_about = "Show the capability view using this CLI process environment.\n\n\
This command reports what the hooks would do with the current CLI environment \
variables. It does not read Agent config files, Agent home directories, or \
prove that hooks are loaded in the target Agent process, and it never contacts \
agent-sec-daemon."
)]
pub struct CapabilitiesCommand {
    /// Filter by agent. Allowed: qoder, qwen, codex, cosh, openclaw, hermes.
    #[arg(long, short = 'a')]
    agent: Option<String>,
    /// Filter by capability. Allowed: code-scan, prompt-scan, pii-check, skill-ledger, observability.
    #[arg(long, short = 'c')]
    capability: Option<String>,
    /// Output format: table or json.
    #[arg(long, short = 'o', default_value = "table")]
    output: String,
}

impl CapabilitiesCommand {
    /// Renders the view for `env`, returning the process exit code.
    ///
    /// # Errors
    /// Returns write failures, including a closed output pipe.
    pub fn render(
        &self,
        env: &Environment,
        stdout: &mut impl Write,
        stderr: &mut impl Write,
    ) -> io::Result<u8> {
        if !OUTPUT_FORMATS.contains(&self.output.as_str()) {
            writeln!(stderr, "Error: --output must be one of: json, table.")?;
            return Ok(1);
        }
        let records =
            match capabilities::query(env, self.agent.as_deref(), self.capability.as_deref()) {
                Ok(records) => records,
                Err(error) => {
                    writeln!(stderr, "Error: {error}")?;
                    return Ok(1);
                }
            };
        let rendered = if self.output == "json" {
            capabilities::render_json(&records).map_err(io::Error::other)?
        } else {
            capabilities::render_table(&records)
        };
        writeln!(stdout, "{rendered}")?;
        Ok(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Cli;

    fn parse(arguments: &[&str]) -> Cli {
        let mut argv = vec!["agent-sec-cli", "capabilities"];
        argv.extend_from_slice(arguments);
        Cli::parse_from(argv).expect("capabilities parses without a socket")
    }

    fn run(arguments: &[&str]) -> (u8, String, String) {
        let cli = parse(arguments);
        let crate::Plan::Local(command) = cli.plan() else {
            panic!("capabilities must not reach a daemon");
        };
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = command
            .render(&Environment::new(), &mut stdout, &mut stderr)
            .expect("rendering to a buffer cannot fail");
        (
            code,
            String::from_utf8(stdout).expect("UTF-8 stdout"),
            String::from_utf8(stderr).expect("UTF-8 stderr"),
        )
    }

    #[test]
    fn short_and_long_filters_select_the_same_record() {
        for arguments in [
            [
                "--agent",
                "qoder",
                "--capability",
                "code-scan",
                "--output",
                "json",
            ],
            ["-a", "qoder", "-c", "code-scan", "-o", "json"],
        ] {
            let (code, stdout, stderr) = run(&arguments);
            assert_eq!(code, 0);
            assert_eq!(stderr, "");
            let payload: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
            assert_eq!(payload.as_array().expect("array").len(), 1);
            assert_eq!(payload[0]["agent"], "qoder");
            assert_eq!(payload[0]["capability"], "code-scan");
        }
    }

    #[test]
    fn unknown_filters_and_formats_fail_with_the_v1_text_and_exit_code() {
        for (arguments, expected) in [
            (
                vec!["--agent", "unsupported"],
                "Error: unknown agent: unsupported. Allowed values: qoder, qwen, codex, cosh, openclaw, hermes\n",
            ),
            (
                vec!["--capability", "scan-code"],
                "Error: unknown capability: scan-code. Allowed values: code-scan, prompt-scan, pii-check, skill-ledger, observability\n",
            ),
            (
                vec!["--output", "yaml"],
                "Error: --output must be one of: json, table.\n",
            ),
            (
                vec!["--output", "JSON"],
                "Error: --output must be one of: json, table.\n",
            ),
        ] {
            let (code, stdout, stderr) = run(&arguments);
            assert_eq!(code, 1, "{arguments:?}");
            assert_eq!(stdout, "", "{arguments:?}");
            assert_eq!(stderr, expected, "{arguments:?}");
        }
    }

    #[test]
    fn the_default_format_is_the_grouped_table() {
        let (code, stdout, stderr) = run(&["--agent", "hermes"]);
        assert_eq!(code, 0);
        assert_eq!(stderr, "");
        assert!(stdout.starts_with("[hermes]\n"), "{stdout}");
        assert!(stdout.ends_with('\n'), "{stdout}");
    }

    #[test]
    fn help_documents_the_filters_and_the_environment_only_scope() {
        let error = Cli::parse_from(vec!["agent-sec-cli", "capabilities", "--help"])
            .expect_err("help exits through clap");
        let help = error.render().to_string();
        assert!(help.contains("--agent"), "{help}");
        assert!(help.contains("--capability"), "{help}");
        assert!(help.contains("--output"), "{help}");
        assert!(help.contains("current CLI environment"), "{help}");
    }
}
