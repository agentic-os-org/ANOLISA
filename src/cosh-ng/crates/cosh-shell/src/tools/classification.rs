use crate::types::ImplicitPagerPolicy;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderToolClass {
    Shell,
    ReadOnlyBuiltin,
    WriteBuiltin,
    OtherKnown,
    Unknown,
}

/// A tool identity known to the shell's provider boundary.
///
/// Core-backed identities expose the exact name registered by cosh-core;
/// provider-only identities remain explicit so they cannot accidentally gain
/// the policy of a similarly named core tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KnownProviderTool {
    Shell,
    ReadFile,
    WriteFile,
    Edit,
    NotebookEdit,
    Grep,
    Glob,
    ListDirectory,
    ReadManyFiles,
    SaveMemory,
    Todo,
    TodoWrite,
    WebFetch,
    Skill,
    ShellEvidence,
    AskUserQuestion,
    Lsp,
    WebSearch,
    Agent,
    Workflow,
    SendMessage,
    Task,
    Subagent,
    Delegate,
    TaskCreate,
    TaskUpdate,
    TaskList,
    TaskGet,
    TaskStop,
    CronCreate,
    CronDelete,
    CronList,
    ScheduleWakeup,
}

impl KnownProviderTool {
    /// Returns the core-side canonical name, when this identity has one.
    pub fn core_name(self) -> Option<&'static str> {
        match self {
            Self::Shell => Some("shell"),
            Self::ReadFile => Some("read_file"),
            Self::WriteFile => Some("write_file"),
            Self::Edit | Self::NotebookEdit => Some("edit"),
            Self::Grep => Some("grep"),
            Self::Glob => Some("glob"),
            Self::ListDirectory => Some("list_directory"),
            Self::ReadManyFiles => Some("read_many_files"),
            Self::SaveMemory => Some("save_memory"),
            Self::Todo | Self::TodoWrite => Some("todo"),
            Self::WebFetch => Some("web_fetch"),
            Self::Skill => Some("skill"),
            Self::ShellEvidence => Some("cosh_shell_evidence"),
            Self::AskUserQuestion => Some("ask_user_question"),
            Self::Lsp
            | Self::WebSearch
            | Self::Agent
            | Self::Workflow
            | Self::SendMessage
            | Self::Task
            | Self::Subagent
            | Self::Delegate
            | Self::TaskCreate
            | Self::TaskUpdate
            | Self::TaskList
            | Self::TaskGet
            | Self::TaskStop
            | Self::CronCreate
            | Self::CronDelete
            | Self::CronList
            | Self::ScheduleWakeup => None,
        }
    }

    /// Returns the shell policy class derived from this identity.
    pub fn class(self) -> ProviderToolClass {
        match self {
            Self::Shell => ProviderToolClass::Shell,
            Self::ReadFile
            | Self::Grep
            | Self::Glob
            | Self::ListDirectory
            | Self::ReadManyFiles => ProviderToolClass::ReadOnlyBuiltin,
            Self::WriteFile | Self::Edit | Self::NotebookEdit | Self::SaveMemory => {
                ProviderToolClass::WriteBuiltin
            }
            _ => ProviderToolClass::OtherKnown,
        }
    }

    /// Returns whether the control protocol owns streamed staging for it.
    pub fn is_control_backed(self) -> bool {
        matches!(self, Self::Shell | Self::ShellEvidence)
    }
}

/// Resolves provider spellings through the shell's single alias catalog.
pub fn known_provider_tool(name: &str) -> Option<KnownProviderTool> {
    let name = name.strip_prefix("tool ").unwrap_or(name);
    match name {
        "Bash" | "bash" | "shell" | "run_shell_command" => Some(KnownProviderTool::Shell),
        "Read" | "read_file" => Some(KnownProviderTool::ReadFile),
        "Write" | "write_file" => Some(KnownProviderTool::WriteFile),
        "Edit" | "edit" | "replace" => Some(KnownProviderTool::Edit),
        "NotebookEdit" => Some(KnownProviderTool::NotebookEdit),
        "Grep" | "grep" | "grep_search" | "search_file_content" | "FileSearch" | "file_search" => {
            Some(KnownProviderTool::Grep)
        }
        "Glob" | "glob" | "FindFiles" => Some(KnownProviderTool::Glob),
        "LS" | "list_directory" | "ReadFolder" => Some(KnownProviderTool::ListDirectory),
        "read_many_files" => Some(KnownProviderTool::ReadManyFiles),
        "save_memory" => Some(KnownProviderTool::SaveMemory),
        "todo" => Some(KnownProviderTool::Todo),
        "todo_write" | "TodoWrite" => Some(KnownProviderTool::TodoWrite),
        "WebFetch" | "web_fetch" => Some(KnownProviderTool::WebFetch),
        "Skill" | "skill" | "read_skill" | "ReadSkill" => Some(KnownProviderTool::Skill),
        "cosh_shell_evidence" => Some(KnownProviderTool::ShellEvidence),
        "AskUserQuestion" | "ask_user_question" | "ask_user" | "AskUser" => {
            Some(KnownProviderTool::AskUserQuestion)
        }
        "LSP" => Some(KnownProviderTool::Lsp),
        "WebSearch" | "google_web_search" => Some(KnownProviderTool::WebSearch),
        "Agent" => Some(KnownProviderTool::Agent),
        "Workflow" => Some(KnownProviderTool::Workflow),
        "SendMessage" => Some(KnownProviderTool::SendMessage),
        "Task" => Some(KnownProviderTool::Task),
        "Subagent" => Some(KnownProviderTool::Subagent),
        "Delegate" => Some(KnownProviderTool::Delegate),
        "TaskCreate" => Some(KnownProviderTool::TaskCreate),
        "TaskUpdate" => Some(KnownProviderTool::TaskUpdate),
        "TaskList" => Some(KnownProviderTool::TaskList),
        "TaskGet" => Some(KnownProviderTool::TaskGet),
        "TaskStop" => Some(KnownProviderTool::TaskStop),
        "CronCreate" => Some(KnownProviderTool::CronCreate),
        "CronDelete" => Some(KnownProviderTool::CronDelete),
        "CronList" => Some(KnownProviderTool::CronList),
        "ScheduleWakeup" => Some(KnownProviderTool::ScheduleWakeup),
        _ => None,
    }
}

/// Resolves a provider spelling to its core-side canonical name.
pub fn canonical_tool_name(name: &str) -> Option<&'static str> {
    known_provider_tool(name).and_then(KnownProviderTool::core_name)
}

pub fn provider_tool_class(name: &str) -> ProviderToolClass {
    known_provider_tool(name)
        .map(KnownProviderTool::class)
        .unwrap_or(ProviderToolClass::Unknown)
}

pub fn is_shell_tool_name(name: &str) -> bool {
    provider_tool_class(name) == ProviderToolClass::Shell
}

pub fn is_readonly_builtin_tool_name(name: &str) -> bool {
    provider_tool_class(name) == ProviderToolClass::ReadOnlyBuiltin
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandInteractionProfile {
    pub pty_requirement: PtyRequirement,
    pub output_stability: OutputStability,
    pub approval_risk: ApprovalRisk,
    pub reason: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PtyRequirement {
    NotRequired,
    Required,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputStability {
    StableSnapshot,
    UnstableInteractive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalRisk {
    Medium,
    High,
}

impl ApprovalRisk {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

pub fn classify_command_interaction(command: &str) -> CommandInteractionProfile {
    let assessment = super::command_risk::assess_shell_command(
        command,
        super::command_risk::AssessmentPolicy::ask(
            super::command_risk::AssessmentSource::ProviderShellTool,
        ),
    );
    CommandInteractionProfile {
        pty_requirement: match assessment.interaction {
            super::command_risk::InteractionRequirement::None => PtyRequirement::NotRequired,
            super::command_risk::InteractionRequirement::TtyRequired
            | super::command_risk::InteractionRequirement::CredentialPromptLikely => {
                PtyRequirement::Required
            }
        },
        output_stability: match assessment.output_stability {
            super::command_risk::OutputStability::StableSnapshot
            | super::command_risk::OutputStability::PotentiallyLarge => {
                OutputStability::StableSnapshot
            }
            super::command_risk::OutputStability::Streaming
            | super::command_risk::OutputStability::UnstableInteractive => {
                OutputStability::UnstableInteractive
            }
        },
        approval_risk: match assessment.impact {
            super::command_risk::RiskImpact::High => ApprovalRisk::High,
            super::command_risk::RiskImpact::Low | super::command_risk::RiskImpact::Medium => {
                ApprovalRisk::Medium
            }
        },
        reason: assessment.primary_reason(),
    }
}

/// Picks the implicit-pager policy for an agent-originated shell handoff.
///
/// Commands the agent runs to *read* something (`git log`, `systemctl status`)
/// must not stop at a pager waiting for `q`, so their implicit pagers are
/// disabled. Commands that only make sense with a terminal (`less`, `man`,
/// `top`, `ssh`) keep the user's configuration. Unknown ordinary commands get
/// `Disable`: the pager environment variables are inert for programs that do
/// not consult them.
pub(crate) fn agent_implicit_pager_policy(command: &str) -> ImplicitPagerPolicy {
    match classify_command_interaction(command).pty_requirement {
        PtyRequirement::Required => ImplicitPagerPolicy::Inherit,
        PtyRequirement::NotRequired => ImplicitPagerPolicy::Disable,
    }
}

pub fn obvious_tty_command_reason(command: &str) -> Option<&'static str> {
    let profile = classify_command_interaction(command);
    (profile.pty_requirement != PtyRequirement::NotRequired).then_some(profile.reason)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_shell_provider_aliases() {
        for name in [
            "Bash",
            "bash",
            "shell",
            "run_shell_command",
            "tool Bash",
            "tool shell",
            "tool run_shell_command",
        ] {
            assert_eq!(
                provider_tool_class(name),
                ProviderToolClass::Shell,
                "{name}"
            );
            assert!(is_shell_tool_name(name), "{name}");
        }
        assert!(!is_shell_tool_name("Read"));
    }

    #[test]
    fn classifies_readonly_provider_aliases() {
        for name in [
            "Read",
            "Grep",
            "Glob",
            "LS",
            "read_file",
            "grep_search",
            "glob",
            "list_directory",
            "read_many_files",
            "tool Read",
            "tool Grep",
            "tool Glob",
            "tool LS",
            "tool read_file",
            "tool grep_search",
            "tool glob",
            "tool list_directory",
            "tool read_many_files",
        ] {
            assert_eq!(
                provider_tool_class(name),
                ProviderToolClass::ReadOnlyBuiltin,
                "{name}"
            );
            assert!(is_readonly_builtin_tool_name(name), "{name}");
        }
        assert!(!is_readonly_builtin_tool_name("Bash"));
    }

    #[test]
    fn classifies_write_and_unknown_tools_without_shell_execution() {
        for name in [
            "Write",
            "Edit",
            "edit",
            "write_file",
            "save_memory",
            "tool Write",
            "tool Edit",
        ] {
            assert_eq!(
                provider_tool_class(name),
                ProviderToolClass::WriteBuiltin,
                "{name}"
            );
            assert!(!is_shell_tool_name(name), "{name}");
        }
        assert_eq!(
            provider_tool_class("CustomTool"),
            ProviderToolClass::Unknown
        );
        assert!(!is_shell_tool_name("CustomTool"));

        for (name, canonical, class, control_backed) in [
            ("Grep", "grep", ProviderToolClass::ReadOnlyBuiltin, false),
            (
                "grep_search",
                "grep",
                ProviderToolClass::ReadOnlyBuiltin,
                false,
            ),
            ("edit", "edit", ProviderToolClass::WriteBuiltin, false),
            ("Edit", "edit", ProviderToolClass::WriteBuiltin, false),
            (
                "web_fetch",
                "web_fetch",
                ProviderToolClass::OtherKnown,
                false,
            ),
            (
                "WebFetch",
                "web_fetch",
                ProviderToolClass::OtherKnown,
                false,
            ),
            (
                "save_memory",
                "save_memory",
                ProviderToolClass::WriteBuiltin,
                false,
            ),
            ("todo", "todo", ProviderToolClass::OtherKnown, false),
            ("TodoWrite", "todo", ProviderToolClass::OtherKnown, false),
            ("skill", "skill", ProviderToolClass::OtherKnown, false),
            ("Skill", "skill", ProviderToolClass::OtherKnown, false),
            (
                "cosh_shell_evidence",
                "cosh_shell_evidence",
                ProviderToolClass::OtherKnown,
                true,
            ),
        ] {
            let identity = known_provider_tool(name).expect("known provider tool");
            assert_eq!(identity.core_name(), Some(canonical), "{name}");
            assert_eq!(identity.class(), class, "{name}");
            assert_eq!(identity.is_control_backed(), control_backed, "{name}");
            assert_eq!(canonical_tool_name(name), Some(canonical), "{name}");
        }

        assert_eq!(known_provider_tool("CustomTool"), None);
        assert_eq!(canonical_tool_name("CustomTool"), None);
    }

    #[test]
    fn detects_obvious_tty_command_risk_conservatively() {
        for command in [
            "sudo id",
            "/usr/bin/ssh host",
            "vim Cargo.toml",
            "less README.md",
            "python",
            "docker exec -it container sh",
            "kubectl exec --tty pod -- sh",
            "LANG=C sudo id",
        ] {
            assert!(obvious_tty_command_reason(command).is_some(), "{command}");
        }

        for command in [
            "df -h",
            "git status --short",
            "python -c 'print(1)'",
            "node -e 'console.log(1)'",
            "docker ps",
            "kubectl get pods",
            "top -b -n1",
            "top -l 1 -stats pid,mem,command",
        ] {
            assert!(obvious_tty_command_reason(command).is_none(), "{command}");
        }
    }

    #[test]
    fn command_interaction_profile_decouples_pty_from_approval_risk() {
        for command in [
            "less README.md",
            "man ls",
            "top",
            "python",
            "node",
            "ssh host",
            "docker exec -it container sh",
            "kubectl exec --tty pod -- sh",
        ] {
            let profile = classify_command_interaction(command);
            assert_eq!(
                profile.pty_requirement,
                PtyRequirement::Required,
                "{command}"
            );
            assert_eq!(
                profile.output_stability,
                OutputStability::UnstableInteractive,
                "{command}"
            );
            assert_eq!(profile.approval_risk, ApprovalRisk::Medium, "{command}");
        }

        for command in ["vim Cargo.toml", "sudo id", "rm -rf target", "kill 1234"] {
            assert_eq!(
                classify_command_interaction(command).approval_risk,
                ApprovalRisk::High,
                "{command}"
            );
        }

        for command in ["df -h", "top -b -n1", "top -l 1 -stats pid,mem,command"] {
            let profile = classify_command_interaction(command);
            assert_eq!(
                profile.pty_requirement,
                PtyRequirement::NotRequired,
                "{command}"
            );
            assert_eq!(
                profile.output_stability,
                OutputStability::StableSnapshot,
                "{command}"
            );
            assert_eq!(profile.approval_risk, ApprovalRisk::Medium, "{command}");
        }

        assert_agent_forensics_commands_disable_implicit_pagers();
        assert_agent_explicit_interactive_commands_inherit_the_user_pager();
        assert_unknown_ordinary_commands_are_not_treated_as_explicit_interactive();
    }

    fn assert_agent_forensics_commands_disable_implicit_pagers() {
        for command in [
            "git log",
            "git log --since=\"1 day ago\" --oneline",
            "git show HEAD",
            "git diff HEAD~1",
            "cd repo && git log",
            "systemctl status nginx",
            "journalctl -u nginx",
            "df -h",
            "top -b -n1",
        ] {
            assert_eq!(
                agent_implicit_pager_policy(command),
                ImplicitPagerPolicy::Disable,
                "{command}"
            );
        }
    }

    fn assert_agent_explicit_interactive_commands_inherit_the_user_pager() {
        for command in [
            "less README.md",
            "cd repo && less README.md",
            "more output.log",
            "man ls",
            "top",
            "htop",
            "ssh host",
        ] {
            assert_eq!(
                agent_implicit_pager_policy(command),
                ImplicitPagerPolicy::Inherit,
                "{command}"
            );
        }
    }

    fn assert_unknown_ordinary_commands_are_not_treated_as_explicit_interactive() {
        for command in [
            "fake-forensics-tool --json",
            "./scripts/collect-evidence.sh",
            "mystery-binary",
        ] {
            assert_eq!(
                classify_command_interaction(command).pty_requirement,
                PtyRequirement::NotRequired,
                "{command}"
            );
            assert_eq!(
                agent_implicit_pager_policy(command),
                ImplicitPagerPolicy::Disable,
                "{command}"
            );
        }
    }
}
