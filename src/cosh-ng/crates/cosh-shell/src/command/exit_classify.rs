/// Exit code semantic classification for shell commands.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitCodeCategory {
    Success,
    UserInterrupt,
    PipelineNormal,
    AbnormalSignal,
    CommandNotFound,
    PermissionDenied,
    CommandSpecificNormal,
    GenericError,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandOutcome {
    Completed,
    Failed,
    Interrupted,
    TimedOut,
    NotExecuted,
}

impl CommandOutcome {
    pub fn status(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Interrupted => "interrupted",
            Self::TimedOut => "timed_out",
            Self::NotExecuted => "not_executed",
        }
    }
}

pub fn classify_exit(exit_code: i32, command: &str) -> ExitCodeCategory {
    match exit_code {
        0 => ExitCodeCategory::Success,
        126 => ExitCodeCategory::PermissionDenied,
        127 => ExitCodeCategory::CommandNotFound,
        130 | 143 => ExitCodeCategory::UserInterrupt,
        141 => ExitCodeCategory::PipelineNormal,
        132 | 134 | 135 | 136 | 137 | 139 => ExitCodeCategory::AbnormalSignal,
        c if c > 128 => ExitCodeCategory::GenericError,
        1 if is_normal_exit_one(command) => ExitCodeCategory::CommandSpecificNormal,
        _ => ExitCodeCategory::GenericError,
    }
}

pub fn classify_executed_command_outcome(exit_code: i32, command: &str) -> CommandOutcome {
    match classify_exit(exit_code, command) {
        ExitCodeCategory::Success => CommandOutcome::Completed,
        ExitCodeCategory::UserInterrupt => CommandOutcome::Interrupted,
        _ => CommandOutcome::Failed,
    }
}

pub fn classify_shell_handoff_command_outcome(
    exit_code: i32,
    command: &str,
    timeout_interrupt_sent: bool,
) -> CommandOutcome {
    if timeout_interrupt_sent {
        CommandOutcome::TimedOut
    } else {
        classify_executed_command_outcome(exit_code, command)
    }
}

pub fn first_program_token(command: &str) -> &str {
    let mut rest = command;
    let mut after_sudo = false;
    let mut after_env = false;
    let mut skip_next_sudo_arg = false;
    loop {
        let token = match rest.split_whitespace().next() {
            Some(t) => t,
            None => return "",
        };
        // Advance past this token for possible next iteration.
        rest = rest[rest.find(token).unwrap() + token.len()..].trim_start();

        if is_env_assignment(token) {
            continue;
        }
        let basename = match token.rsplit_once('/') {
            Some((_, basename)) => basename,
            None => token,
        };
        if basename == "sudo" {
            after_sudo = true;
            continue;
        }
        if basename == "env" {
            after_env = true;
            continue;
        }
        if skip_next_sudo_arg {
            skip_next_sudo_arg = false;
            continue;
        }
        if after_sudo && is_sudo_option_token(token, &mut skip_next_sudo_arg) {
            continue;
        }
        if after_env && token == "--" {
            continue;
        }
        // Strip path: /usr/bin/grep → grep
        return basename;
    }
}

fn is_normal_exit_one(command: &str) -> bool {
    const NORMAL_EXIT_ONE: &[&str] = &[
        "grep",
        "egrep",
        "fgrep",
        "rg",
        "ag",
        "diff",
        "colordiff",
        "vimdiff",
        "test",
        "[",
        "cmp",
        "which",
        "whence",
        "false",
    ];
    let prog = first_program_token(command);
    NORMAL_EXIT_ONE.contains(&prog)
}

fn is_env_assignment(token: &str) -> bool {
    let eq_pos = match token.find('=') {
        Some(pos) if pos > 0 => pos,
        _ => return false,
    };
    let name = &token[..eq_pos];
    name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        && !name.bytes().next().unwrap_or(0).is_ascii_digit()
}

fn is_sudo_option_token(token: &str, skip_next_arg: &mut bool) -> bool {
    match token {
        "--" => return true,
        "-u" | "-g" | "-h" | "-p" | "-C" | "-T" | "--user" | "--group" | "--host" | "--prompt"
        | "--close-from" | "--command-timeout" => {
            *skip_next_arg = true;
            return true;
        }
        "--non-interactive" | "--preserve-env" | "--set-home" | "-E" | "-H" | "-n" => {
            return true;
        }
        _ => {}
    }
    if token.starts_with("--user=")
        || token.starts_with("--group=")
        || token.starts_with("--host=")
        || token.starts_with("--prompt=")
        || token.starts_with("--close-from=")
        || token.starts_with("--command-timeout=")
        || token.starts_with("--preserve-env=")
    {
        return true;
    }
    if token.len() > 2 && matches!(&token[..2], "-u" | "-g" | "-h" | "-p" | "-C" | "-T") {
        return true;
    }
    token
        .strip_prefix('-')
        .filter(|opts| !opts.starts_with('-') && !opts.is_empty())
        .is_some_and(|opts| opts.chars().all(|ch| "EHn".contains(ch)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn success() {
        assert_eq!(classify_exit(0, "ls"), ExitCodeCategory::Success);
    }

    #[test]
    fn permission_denied() {
        assert_eq!(
            classify_exit(126, "./script.sh"),
            ExitCodeCategory::PermissionDenied
        );
    }

    #[test]
    fn command_not_found() {
        assert_eq!(
            classify_exit(127, "nonexistent"),
            ExitCodeCategory::CommandNotFound
        );
    }

    #[test]
    fn user_interrupt_sigint() {
        assert_eq!(
            classify_exit(130, "sleep 100"),
            ExitCodeCategory::UserInterrupt
        );
    }

    #[test]
    fn command_outcome_statuses_cover_executed_commands() {
        for (exit_code, command, expected) in [
            (0, "df -h", CommandOutcome::Completed),
            (1, "make build", CommandOutcome::Failed),
            (1, "sudo -n df -h", CommandOutcome::Failed),
            (130, "sleep 100", CommandOutcome::Interrupted),
        ] {
            assert_eq!(
                classify_executed_command_outcome(exit_code, command),
                expected,
                "{command}"
            );
        }
        assert_eq!(
            classify_shell_handoff_command_outcome(0, "sleep 10", true),
            CommandOutcome::TimedOut
        );
        assert_eq!(
            classify_shell_handoff_command_outcome(130, "sleep 100", false),
            CommandOutcome::Interrupted
        );
        assert_eq!(CommandOutcome::Completed.status(), "completed");
        assert_eq!(CommandOutcome::Failed.status(), "failed");
        assert_eq!(CommandOutcome::Interrupted.status(), "interrupted");
        assert_eq!(CommandOutcome::TimedOut.status(), "timed_out");
        assert_eq!(CommandOutcome::NotExecuted.status(), "not_executed");
    }

    #[test]
    fn user_interrupt_sigterm() {
        assert_eq!(
            classify_exit(143, "tail -f /var/log/syslog"),
            ExitCodeCategory::UserInterrupt
        );
        assert_eq!(
            classify_executed_command_outcome(143, "tail -f /var/log/syslog"),
            CommandOutcome::Interrupted
        );
    }

    #[test]
    fn pipeline_normal() {
        assert_eq!(
            classify_exit(141, "yes | head -1"),
            ExitCodeCategory::PipelineNormal
        );
    }

    #[test]
    fn abnormal_sigkill() {
        assert_eq!(
            classify_exit(137, "oom-process"),
            ExitCodeCategory::AbnormalSignal
        );
    }

    #[test]
    fn abnormal_sigsegv() {
        assert_eq!(
            classify_exit(139, "buggy"),
            ExitCodeCategory::AbnormalSignal
        );
    }

    #[test]
    fn abnormal_sigabrt() {
        assert_eq!(
            classify_exit(134, "assert-fail"),
            ExitCodeCategory::AbnormalSignal
        );
    }

    #[test]
    fn abnormal_sigfpe() {
        assert_eq!(
            classify_exit(136, "divzero"),
            ExitCodeCategory::AbnormalSignal
        );
    }

    #[test]
    fn fatal_signal_matrix_is_explicit() {
        for exit_code in [132, 134, 135, 136, 137, 139] {
            assert_eq!(
                classify_exit(exit_code, "crashed"),
                ExitCodeCategory::AbnormalSignal,
                "exit {exit_code}"
            );
        }
    }

    #[test]
    fn unknown_signal_conservative() {
        assert_eq!(
            classify_exit(142, "something"),
            ExitCodeCategory::GenericError
        );
        assert_eq!(
            classify_exit(200, "something"),
            ExitCodeCategory::GenericError
        );
        assert_eq!(
            classify_executed_command_outcome(200, "something"),
            CommandOutcome::Failed
        );
    }

    #[test]
    fn grep_exit_one_is_normal() {
        assert_eq!(
            classify_exit(1, "grep pattern file.txt"),
            ExitCodeCategory::CommandSpecificNormal
        );
    }

    #[test]
    fn diff_exit_one_is_normal() {
        assert_eq!(
            classify_exit(1, "diff a.txt b.txt"),
            ExitCodeCategory::CommandSpecificNormal
        );
    }

    #[test]
    fn test_builtin_exit_one_is_normal() {
        assert_eq!(
            classify_exit(1, "test -f missing.txt"),
            ExitCodeCategory::CommandSpecificNormal
        );
        assert_eq!(
            classify_exit(1, "[ -f missing.txt ]"),
            ExitCodeCategory::CommandSpecificNormal
        );
    }

    #[test]
    fn false_exit_one_is_normal() {
        assert_eq!(
            classify_exit(1, "false"),
            ExitCodeCategory::CommandSpecificNormal
        );
    }

    #[test]
    fn rg_exit_one_is_normal() {
        assert_eq!(
            classify_exit(1, "rg pattern"),
            ExitCodeCategory::CommandSpecificNormal
        );
    }

    #[test]
    fn which_exit_one_is_normal() {
        assert_eq!(
            classify_exit(1, "which nonexistent"),
            ExitCodeCategory::CommandSpecificNormal
        );
    }

    #[test]
    fn generic_error_exit_one_unknown_command() {
        assert_eq!(
            classify_exit(1, "make build"),
            ExitCodeCategory::GenericError
        );
    }

    #[test]
    fn generic_error_exit_two() {
        assert_eq!(
            classify_exit(2, "ls --bad-flag"),
            ExitCodeCategory::GenericError
        );
    }

    #[test]
    fn first_program_token_simple() {
        assert_eq!(first_program_token("ls -la"), "ls");
    }

    #[test]
    fn first_program_token_with_path() {
        assert_eq!(first_program_token("/usr/bin/grep pattern"), "grep");
    }

    #[test]
    fn first_program_token_with_sudo() {
        assert_eq!(first_program_token("sudo apt install foo"), "apt");
    }

    #[test]
    fn first_program_token_with_sudo_options() {
        assert_eq!(first_program_token("sudo -n -E free -m"), "free");
        assert_eq!(
            first_program_token("sudo -u root /usr/bin/top -b -n1"),
            "top"
        );
        assert_eq!(first_program_token("/usr/bin/sudo -n free -m"), "free");
        assert_eq!(
            first_program_token("sudo --non-interactive -- free -m"),
            "free"
        );
        assert_eq!(first_program_token("LANG=C sudo -n free -m"), "free");
        assert_eq!(
            first_program_token("sudo --definitely-unknown free -m"),
            "--definitely-unknown"
        );
    }

    #[test]
    fn first_program_token_with_env() {
        assert_eq!(first_program_token("FOO=bar grep pattern"), "grep");
    }

    #[test]
    fn first_program_token_with_env_and_sudo() {
        assert_eq!(first_program_token("LANG=C sudo /usr/bin/diff a b"), "diff");
    }

    #[test]
    fn first_program_token_empty() {
        assert_eq!(first_program_token(""), "");
        assert_eq!(first_program_token("   "), "");
    }

    #[test]
    fn first_program_token_env_only() {
        assert_eq!(first_program_token("FOO=bar"), "");
    }

    #[test]
    fn is_env_assignment_positive() {
        assert!(is_env_assignment("FOO=bar"));
        assert!(is_env_assignment("MY_VAR=123"));
        assert!(is_env_assignment("A="));
    }

    #[test]
    fn is_env_assignment_negative() {
        assert!(!is_env_assignment("grep"));
        assert!(!is_env_assignment("=bar"));
        assert!(!is_env_assignment("1BAD=x"));
        assert!(!is_env_assignment("foo-bar=x"));
    }

    #[test]
    fn grep_variants_exit_one() {
        assert_eq!(
            classify_exit(1, "egrep pattern file"),
            ExitCodeCategory::CommandSpecificNormal
        );
        assert_eq!(
            classify_exit(1, "fgrep pattern file"),
            ExitCodeCategory::CommandSpecificNormal
        );
    }

    #[test]
    fn grep_exit_two_is_generic_error() {
        assert_eq!(
            classify_exit(2, "grep --bad-flag"),
            ExitCodeCategory::GenericError
        );
    }

    #[test]
    fn env_prefix_with_path_command() {
        assert_eq!(
            classify_exit(1, "LC_ALL=C /usr/local/bin/rg needle"),
            ExitCodeCategory::CommandSpecificNormal
        );
    }
}
