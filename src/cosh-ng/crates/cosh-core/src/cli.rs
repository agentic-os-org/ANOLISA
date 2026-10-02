use clap::{Parser, Subcommand};

use crate::config::ApprovalMode;

/// Runtime execution boundary selected before loading workspace-owned state.
#[derive(clap::ValueEnum, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum ExecutionProfile {
    /// Existing direct Core/Shell behavior and private control protocol v1.
    #[default]
    Legacy,
    /// Gateway owns every hosted side effect through private protocol v3.
    GatewayBrokeredV1,
    /// Gateway owns the checkpoint side effect through private protocol v4.
    GatewayBrokeredCheckpointV1,
    /// Core owns one approval-gated workspace write through private protocol v5.
    GatewayBrokeredWorkspaceWriteV1,
}

impl ExecutionProfile {
    pub(crate) const fn wire_name(self) -> &'static str {
        match self {
            Self::Legacy => "legacy",
            Self::GatewayBrokeredV1 => crate::protocol::GATEWAY_BROKERED_V1_EXECUTION_PROFILE,
            Self::GatewayBrokeredCheckpointV1 => {
                crate::protocol::GATEWAY_BROKERED_CHECKPOINT_V1_EXECUTION_PROFILE
            }
            Self::GatewayBrokeredWorkspaceWriteV1 => {
                crate::protocol::GATEWAY_BROKERED_WORKSPACE_WRITE_V1_EXECUTION_PROFILE
            }
        }
    }

    pub(crate) const fn is_brokered(self) -> bool {
        matches!(
            self,
            Self::GatewayBrokeredV1
                | Self::GatewayBrokeredCheckpointV1
                | Self::GatewayBrokeredWorkspaceWriteV1
        )
    }

    pub(crate) const fn hosts_checkpoint(self) -> bool {
        matches!(self, Self::GatewayBrokeredCheckpointV1)
    }

    pub(crate) const fn writes_workspace(self) -> bool {
        matches!(self, Self::GatewayBrokeredWorkspaceWriteV1)
    }
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Manage configured MCP servers.
    Mcp(McpArgs),
}

#[derive(clap::Args, Debug)]
pub struct McpArgs {
    #[command(subcommand)]
    pub command: McpCommand,
}

#[derive(Subcommand, Debug)]
pub enum McpCommand {
    /// List configured MCP servers without exposing credentials.
    List,
    /// Connect to a configured MCP server and display its discovered tools.
    Connect {
        /// Configured MCP server name.
        server: String,
    },
    /// Display a configured MCP server and its currently discovered tools.
    Inspect {
        /// Configured MCP server name.
        server: String,
    },
    /// Reconnect to an enabled MCP server and rediscover its tools.
    Refresh {
        /// Configured MCP server name.
        server: String,
    },
    /// Disable a configured MCP server until it is connected again.
    Disconnect {
        /// Configured MCP server name.
        server: String,
    },
    /// Authorize an MCP server in a browser.
    Login {
        /// Configured MCP server name.
        server: String,
        /// Print the authorization URL and accept a pasted callback URL.
        #[arg(long)]
        manual: bool,
    },
    /// Remove saved OAuth credentials for an MCP server.
    Logout {
        /// Configured MCP server name.
        server: String,
    },
}

#[derive(Parser, Debug)]
#[command(
    name = "cosh-core",
    version,
    about = "cosh core — agent core + interactive terminal"
)]
pub struct CliArgs {
    #[command(subcommand)]
    pub command: Option<Command>,
    /// Force headless JSONL mode (otherwise auto-detected via TTY)
    #[arg(long)]
    pub headless: bool,

    /// Select the trusted runtime execution boundary.
    ///
    /// Gateway is the only supported caller of the brokered profile. Keeping
    /// this hidden prevents it from being mistaken for a user approval mode.
    #[arg(long, value_enum, default_value_t, hide = true)]
    pub execution_profile: ExecutionProfile,

    /// Override the active model from config.toml
    #[arg(long)]
    pub model: Option<String>,

    /// Override approval mode (recommend|auto|trust)
    #[arg(long, value_name = "MODE")]
    pub approval_mode: Option<ApprovalMode>,

    /// Comma-separated list of auto-approved tools
    #[arg(long, value_name = "TOOLS")]
    pub allowed_tools: Option<String>,

    /// Comma-separated tools exposed to the model (default|empty|names)
    #[arg(long, value_name = "TOOLS")]
    pub tools: Option<String>,

    /// Disable project config, hooks, skills, and extensions
    #[arg(long)]
    pub bare: bool,

    /// Resume an existing session
    #[arg(long, value_name = "SESSION_ID")]
    pub resume: Option<String>,

    /// Compact the resumed session's model context and exit
    #[arg(long)]
    pub compact: bool,

    /// Marks a `--compact` run as automatically triggered (idle-boundary
    /// recommendation) rather than a manual `/session compact`. Affects the
    /// reported trigger and enables revision preflight validation.
    ///
    /// Only valid with `--compact`, and must always carry both
    /// `--expect-generation` and `--expect-revision` (clap enforces this as a
    /// first fail-closed layer, before any provider work).
    #[arg(
        long,
        hide = true,
        requires = "compact",
        requires = "expect_generation",
        requires = "expect_revision"
    )]
    pub auto_compact: bool,

    /// Expected session generation for an automatic compaction. When set with
    /// `--expect-revision`, the compactor fails closed (no provider call) if
    /// the session moved since the recommendation was emitted.
    ///
    /// Only valid on an automatic compaction; requires `--auto-compact` (which
    /// in turn requires `--compact`), so it can never appear on a manual run.
    #[arg(long, value_name = "N", hide = true, requires = "auto_compact")]
    pub expect_generation: Option<u64>,

    /// Expected projection revision for an automatic compaction; paired with
    /// `--expect-generation` to bind the attempt to one exact context.
    ///
    /// Only valid on an automatic compaction; requires `--auto-compact` (which
    /// in turn requires `--compact`), so it can never appear on a manual run.
    #[arg(long, value_name = "N", hide = true, requires = "auto_compact")]
    pub expect_revision: Option<u64>,

    /// Override the workspace scope used for session persistence
    #[arg(long, value_name = "PATH", hide = true)]
    pub workspace: Option<String>,

    /// Run one provider-free session management request from stdin
    #[arg(long, hide = true)]
    pub session_control: bool,

    /// Accept cosh-shell's structured raw prompt field for hook input.
    ///
    /// This is intentionally hidden and is only added by the cosh-shell
    /// adapter when it launches the trusted shell-to-core transport. Generic
    /// headless clients must not be able to select a hook prompt separately
    /// from the provider-facing content.
    #[arg(long, hide = true)]
    pub cosh_shell_transport: bool,

    /// Increase stderr log verbosity
    #[arg(long)]
    pub verbose: bool,

    /// Registry-only mode: respond to one registry_request then exit
    #[arg(long)]
    pub registry: bool,

    /// Enable cosh-shell backed terminal output evidence tool
    #[arg(long)]
    pub enable_shell_evidence_tool: bool,

    // Compatibility flags — accepted but ignored
    #[arg(long, value_name = "FMT", hide = true)]
    pub output_format: Option<String>,

    #[arg(long, value_name = "FMT", hide = true)]
    pub input_format: Option<String>,

    #[arg(long, hide = true)]
    pub include_partial_messages: bool,

    /// Single-shot prompt (headless mode: send one user message then exit)
    pub prompt: Option<String>,
}

impl CliArgs {
    pub fn is_headless(&self) -> bool {
        self.headless || !atty::is(atty::Stream::Stdin)
    }

    pub fn is_registry(&self) -> bool {
        self.registry
    }

    pub fn is_session_control(&self) -> bool {
        self.session_control
    }

    pub fn is_compact(&self) -> bool {
        self.compact
    }

    /// Resolves the workspace root for this process.
    ///
    /// The explicit `--workspace` argument is preferred. Empty strings are
    /// treated as missing so that shells passing `--workspace ""` do not
    /// create an empty workspace scope. Relative paths are absolutized against
    /// the process current directory. The absolute path is then canonicalized
    /// so that `..` components follow filesystem semantics around symlinks.
    /// If the workspace does not exist, the absolute path is kept as-is
    /// instead of lexically collapsing `..`, which would mis-handle symlinks.
    /// Falls back to the process cwd when no workspace is supplied.
    pub fn workspace_root(&self) -> std::path::PathBuf {
        let absolute = self.workspace_path();
        std::fs::canonicalize(&absolute).unwrap_or(absolute)
    }

    /// Returns the absolute workspace pathname without reopening it for identity.
    ///
    /// Agent startup passes this path directly to the single root-opening
    /// operation and derives the canonical display identity from the pinned
    /// descriptor.
    pub(crate) fn workspace_path(&self) -> std::path::PathBuf {
        self.workspace
            .as_deref()
            .filter(|workspace| !workspace.is_empty())
            .map(std::path::PathBuf::from)
            .map(|path| {
                if path.is_absolute() {
                    path
                } else {
                    std::env::current_dir()
                        .unwrap_or_else(|_| std::path::PathBuf::from("."))
                        .join(path)
                }
            })
            .unwrap_or_else(|| {
                std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
            })
    }
}

#[cfg(test)]
mod compaction_tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<CliArgs, clap::Error> {
        let mut full = vec!["cosh-core"];
        full.extend_from_slice(args);
        CliArgs::try_parse_from(full)
    }

    #[test]
    fn hidden_compaction_flags_require_compact() {
        // The three hidden compaction flags are meaningless without --compact
        // and must be rejected at the clap layer, before any runtime work.
        assert!(parse(&[
            "--auto-compact",
            "--expect-generation",
            "1",
            "--expect-revision",
            "0"
        ])
        .is_err());
        assert!(parse(&["--expect-generation", "1"]).is_err());
        assert!(parse(&["--expect-revision", "0"]).is_err());
    }

    #[test]
    fn auto_compact_requires_both_expected_bounds() {
        assert!(parse(&["--compact", "--auto-compact"]).is_err());
        assert!(parse(&["--compact", "--auto-compact", "--expect-generation", "1"]).is_err());
        assert!(parse(&["--compact", "--auto-compact", "--expect-revision", "0"]).is_err());
    }

    #[test]
    fn expected_bounds_are_rejected_on_manual_compact() {
        // generation/revision may only accompany an automatic compaction.
        assert!(parse(&[
            "--compact",
            "--expect-generation",
            "1",
            "--expect-revision",
            "0"
        ])
        .is_err());
        assert!(parse(&["--compact", "--expect-generation", "1"]).is_err());
    }

    #[test]
    fn valid_manual_and_auto_combinations_parse() {
        assert!(parse(&["--compact"]).is_ok());
        assert!(parse(&[
            "--compact",
            "--auto-compact",
            "--expect-generation",
            "1",
            "--expect-revision",
            "0",
        ])
        .is_ok());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tools_and_bare_are_generic_headless_flags() {
        let args = CliArgs::try_parse_from(["cosh-core", "--headless", "--bare", "--tools", ""])
            .expect("parse analyzer isolation flags");

        assert!(args.headless);
        assert!(args.bare);
        assert_eq!(args.tools.as_deref(), Some(""));
        assert!(args.allowed_tools.is_none());
    }

    #[test]
    fn tools_default_is_distinct_from_empty() {
        let default_args = CliArgs::try_parse_from(["cosh-core", "--tools", "default"])
            .expect("parse default tools");
        let empty_args =
            CliArgs::try_parse_from(["cosh-core", "--tools", ""]).expect("parse empty tools");

        assert_eq!(default_args.tools.as_deref(), Some("default"));
        assert_eq!(empty_args.tools.as_deref(), Some(""));
    }

    #[test]
    fn approval_mode_cli_uses_canonical_type() {
        for (value, expected) in [
            ("recommend", ApprovalMode::Recommend),
            ("balanced", ApprovalMode::Recommend),
            ("strict", ApprovalMode::Recommend),
            ("suggest", ApprovalMode::Recommend),
            ("auto", ApprovalMode::Auto),
            ("trust", ApprovalMode::Trust),
        ] {
            let args = CliArgs::try_parse_from(["cosh-core", "--approval-mode", value])
                .expect("parse approval mode");
            assert_eq!(args.approval_mode, Some(expected));
        }
        assert!(CliArgs::try_parse_from(["cosh-core", "--approval-mode", "invalid"]).is_err());
    }

    #[test]
    fn parses_mcp_login_command() {
        let args =
            CliArgs::try_parse_from(["cosh-core", "mcp", "login", "remote", "--manual"]).unwrap();
        let Some(Command::Mcp(McpArgs {
            command: McpCommand::Login { server, manual },
        })) = args.command
        else {
            panic!("expected mcp login command");
        };
        assert_eq!(server, "remote");
        assert!(manual);
    }

    #[test]
    fn parses_mcp_lifecycle_command() {
        let args = CliArgs::try_parse_from(["cosh-core", "mcp", "refresh", "remote"]).unwrap();
        let Some(Command::Mcp(McpArgs {
            command: McpCommand::Refresh { server },
        })) = args.command
        else {
            panic!("expected MCP refresh command");
        };
        assert_eq!(server, "remote");
    }

    #[test]
    fn preserves_single_shot_prompt() {
        let args = CliArgs::try_parse_from(["cosh-core", "summarize this"]).unwrap();
        assert_eq!(args.prompt.as_deref(), Some("summarize this"));
        assert!(args.command.is_none());
    }

    #[test]
    fn workspace_root_falls_back_to_cwd_when_missing() {
        let args = CliArgs::try_parse_from(["cosh-core"]).unwrap();
        let cwd = std::env::current_dir().expect("cwd is available");
        assert_eq!(args.workspace_root(), cwd);
    }

    #[test]
    fn workspace_root_treats_empty_string_as_missing() {
        let args = CliArgs::try_parse_from(["cosh-core", "--workspace", ""]).unwrap();
        let cwd = std::env::current_dir().expect("cwd is available");
        assert_eq!(args.workspace_root(), cwd);
    }

    #[test]
    fn workspace_root_absolutizes_relative_path() {
        let args = CliArgs::try_parse_from(["cosh-core", "--workspace", "./relative"]).unwrap();
        assert!(args.workspace_root().is_absolute());
    }

    #[test]
    fn workspace_root_canonicalizes_existing_absolute_path() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("workspace");
        std::fs::create_dir(&dir).unwrap();
        let args =
            CliArgs::try_parse_from(["cosh-core", "--workspace", dir.to_str().unwrap()]).unwrap();
        assert_eq!(args.workspace_root(), std::fs::canonicalize(&dir).unwrap());
    }

    #[test]
    fn workspace_root_keeps_nonexistent_absolute_path_as_is() {
        let args = CliArgs::try_parse_from([
            "cosh-core",
            "--workspace",
            "/tmp/absolute-workspace-does-not-exist",
        ])
        .unwrap();
        assert_eq!(
            args.workspace_root(),
            std::path::PathBuf::from("/tmp/absolute-workspace-does-not-exist")
        );
    }

    #[test]
    fn workspace_root_canonicalizes_dot_to_cwd() {
        let args = CliArgs::try_parse_from(["cosh-core", "--workspace", "."]).unwrap();
        let cwd = std::env::current_dir().expect("cwd is available");
        assert_eq!(args.workspace_root(), std::fs::canonicalize(&cwd).unwrap());
    }

    #[test]
    fn workspace_root_canonicalizes_dotdot_to_parent() {
        let args = CliArgs::try_parse_from(["cosh-core", "--workspace", ".."]).unwrap();
        let cwd = std::env::current_dir().expect("cwd is available");
        let parent = cwd.parent().unwrap_or(&cwd);
        assert_eq!(
            args.workspace_root(),
            std::fs::canonicalize(parent).unwrap()
        );
    }

    #[cfg(unix)]
    #[test]
    fn workspace_root_resolves_symlink() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("target");
        let link = temp.path().join("link");
        std::fs::create_dir(&target).unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let args =
            CliArgs::try_parse_from(["cosh-core", "--workspace", link.to_str().unwrap()]).unwrap();
        assert_eq!(
            args.workspace_root(),
            std::fs::canonicalize(&target).unwrap()
        );
        assert_eq!(args.workspace_path(), link);
    }

    #[cfg(unix)]
    #[test]
    fn workspace_root_resolves_symlink_with_parent_dir() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("target");
        let actual = temp.path().join("actual");
        let link = temp.path().join("link");
        std::fs::create_dir(&target).unwrap();
        std::fs::create_dir(&actual).unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let workspace = link.join("..").join("actual");
        let args =
            CliArgs::try_parse_from(["cosh-core", "--workspace", workspace.to_str().unwrap()])
                .unwrap();
        assert_eq!(
            args.workspace_root(),
            std::fs::canonicalize(&actual).unwrap()
        );
    }
}
