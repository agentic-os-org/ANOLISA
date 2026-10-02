use std::sync::{OnceLock, RwLock};

use super::readonly_rules::{self, RuntimeReadonlyConfig};
use crate::CoshConfig;

#[cfg(test)]
use std::time::Duration;

static READONLY_CONFIG: OnceLock<RwLock<RuntimeReadonlyConfig>> = OnceLock::new();

pub fn apply_readonly_config(config: &CoshConfig) {
    set_readonly_config(config.readonly_config().clone());
}

pub(crate) fn set_readonly_config(config: RuntimeReadonlyConfig) {
    let lock = READONLY_CONFIG.get_or_init(|| RwLock::new(RuntimeReadonlyConfig::default()));
    if let Ok(mut guard) = lock.write() {
        *guard = config;
    }
}

pub fn can_run_approved_bash_tool(command: &str) -> Result<(), String> {
    readonly_tokens(command).map(|_| ())
}

#[cfg(test)]
fn user_approved_shell_command(command: &str) -> Result<(), String> {
    if command.is_empty() {
        return Err("empty tool command".to_string());
    }
    if command.contains('\0') {
        return Err("blocked NUL byte in shell command".to_string());
    }

    Ok(())
}

#[cfg(test)]
fn parse_user_approved_tool_timeout(value: Option<&str>) -> Option<Duration> {
    value
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|secs| *secs > 0)
        .map(Duration::from_secs)
}

fn readonly_tokens(command: &str) -> Result<Vec<String>, String> {
    let tokens = direct_exec_tokens(command)?;

    if configured_readonly_command(&tokens) {
        Ok(tokens)
    } else {
        Err("command is not in the read-only tool allowlist".to_string())
    }
}

pub(crate) fn configured_readonly_command(tokens: &[String]) -> bool {
    if let Some(lock) = READONLY_CONFIG.get() {
        return match lock.read() {
            Ok(config) => readonly_rules::is_readonly_command_with_config(tokens, &config),
            Err(_) => false,
        };
    }

    readonly_rules::is_readonly_command(tokens)
}

fn direct_exec_tokens(command: &str) -> Result<Vec<String>, String> {
    if command.is_empty() {
        return Err("empty tool command".to_string());
    }
    if command.contains('\0') {
        return Err("blocked NUL byte in tool command".to_string());
    }
    if command.chars().any(is_shell_meta) {
        return Err("blocked shell metacharacter; tool broker does not use a shell".to_string());
    }

    let tokens = command
        .split_ascii_whitespace()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    if tokens.is_empty() {
        return Err("empty tool command".to_string());
    }

    Ok(tokens)
}

fn is_shell_meta(ch: char) -> bool {
    matches!(
        ch,
        ';' | '|'
            | '&'
            | '>'
            | '<'
            | '$'
            | '`'
            | '('
            | ')'
            | '{'
            | '}'
            | '\''
            | '"'
            | '\\'
            | '\n'
            | '\r'
    )
}

#[cfg(test)]
mod tests {
    use super::readonly_rules::{
        PathMode, RuntimeGenericSpec, RuntimeReadonlyConfig, RuntimeReadonlySpec, RuntimeValidator,
    };

    use super::{
        parse_user_approved_tool_timeout, readonly_tokens, set_readonly_config,
        user_approved_shell_command,
    };

    #[test]
    fn readonly_broker_allows_simple_git_status() {
        assert!(readonly_tokens("git status --short").is_ok());
    }

    #[test]
    fn readonly_broker_blocks_shell_metas_and_mutation() {
        let piped = readonly_tokens("ps aux | head").unwrap_err();
        let mutation = readonly_tokens("touch /tmp/cosh-shell-broker-should-not-run").unwrap_err();

        assert!(piped.contains("metacharacter"));
        assert!(mutation.contains("allowlist"));

        // Issue #2184 vector-level pin: ANSI-C `$'...'` quoting is denied
        // by the shell-meta gate before any path check, so the quoting
        // bypass never reaches the readonly path predicate.
        for command in ["cat $'/proc/version'", "cat $'/etc/shadow'"] {
            let err = readonly_tokens(command).unwrap_err();
            assert!(err.contains("metacharacter"), "{command}: {err}");
        }
    }

    #[test]
    fn readonly_broker_uses_configured_rules_after_hard_gate() {
        set_readonly_config(RuntimeReadonlyConfig {
            overrides: vec![RuntimeReadonlySpec {
                command: "cosh_readonly_echo".to_string(),
                validator: RuntimeValidator::Generic(RuntimeGenericSpec {
                    short_flags: String::new(),
                    long_flags: Vec::new(),
                    value_flags: Vec::new(),
                    deny_flags: Vec::new(),
                    path_mode: PathMode::Unchecked,
                    bare_number_max: 0,
                }),
            }],
            ..RuntimeReadonlyConfig::default()
        });

        assert!(readonly_tokens("cosh_readonly_echo hello").is_ok());

        let chained = readonly_tokens("cosh_readonly_echo hello && touch /tmp/nope").unwrap_err();
        assert!(chained.contains("metacharacter"));
    }

    #[test]
    fn user_approved_broker_allows_non_allowlisted_command_after_confirmation() {
        assert!(user_approved_shell_command("true").is_ok());
    }

    #[test]
    fn user_approved_broker_allows_shell_syntax_after_confirmation() {
        for command in [
            "printf 'alpha\\nbeta\\n' | grep beta",
            "echo ok >/dev/null",
            "git status&&pwd",
        ] {
            assert!(user_approved_shell_command(command).is_ok(), "{command}");
        }
    }

    #[test]
    fn user_approved_broker_has_no_default_timeout() {
        assert_eq!(parse_user_approved_tool_timeout(None), None);
        assert_eq!(
            parse_user_approved_tool_timeout(Some("2")),
            Some(std::time::Duration::from_secs(2))
        );
        assert_eq!(parse_user_approved_tool_timeout(Some("0")), None);
        assert_eq!(parse_user_approved_tool_timeout(Some("invalid")), None);
    }

    #[test]
    fn user_approved_broker_rejects_empty_or_nul_command() {
        for command in ["", "printf ok\0printf bad"] {
            assert!(user_approved_shell_command(command).is_err(), "{command:?}");
        }
    }

    #[test]
    fn readonly_broker_tokenizes_tabs_but_rejects_newlines_and_unspaced_metas() {
        let tabbed = readonly_tokens("git\tstatus\t--short");
        let newline = readonly_tokens("git status\npwd").unwrap_err();
        let chained = readonly_tokens("git status&&pwd").unwrap_err();
        let redirected =
            readonly_tokens("git status>/tmp/cosh-shell-broker-should-not-run").unwrap_err();

        assert!(tabbed.is_ok());
        assert!(newline.contains("metacharacter"));
        assert!(chained.contains("metacharacter"));
        assert!(redirected.contains("metacharacter"));
    }

    #[test]
    fn readonly_broker_allows_bounded_cpu_diagnostics() {
        for command in [
            "top -l 1 -n 15 -s 0",
            "top -l 1 -o cpu -n 20",
            "top -b -n 1 -o %CPU",
            "top -n 1 -b -o %CPU",
            "ps -Ao pid,pcpu,pmem,comm -r",
            "sysctl -n hw.ncpu",
            "sysctl -n machdep.cpu.brand_string",
        ] {
            assert!(readonly_tokens(command).is_ok(), "{command}");
        }
    }

    #[test]
    fn readonly_broker_allows_disk_usage_diagnostics() {
        for command in ["df", "df -h", "df -hi", "df -h ."] {
            assert!(readonly_tokens(command).is_ok(), "{command}");
        }
    }

    #[test]
    fn readonly_broker_rejects_unbounded_or_chained_cpu_diagnostics() {
        for command in [
            "top",
            "top -l 2 -n 15",
            "top -l 1 -n 1000",
            "top -l 1 -n 15 | head -30",
            "sysctl -a",
            "sysctl -w hw.ncpu=1",
            "sysctl -n hw.ncpu$(echo x)",
            "sysctl -n machdep.cpu.brand_string && echo ok",
        ] {
            assert!(readonly_tokens(command).is_err(), "{command}");
        }
    }

    #[test]
    fn readonly_broker_rejects_risky_per_command_arguments() {
        for command in [
            "git -c core.pager=cat status",
            "git diff --ext-diff",
            "git show --textconv HEAD:README.md",
            "git diff --output=/tmp/cosh-shell-git-diff.txt",
            "ps -o command=",
            "find . -exec echo {} ;",
            "find . -delete",
            "find /proc -name cpuinfo",
            "find . -maxdepth 100 -name Cargo.toml",
            "cat /dev/zero",
            "cat /proc/cpuinfo",
            "cat \"中文.md\"",
            "head -n 100000 README.md",
            "tail -f README.md",
            "grep -R cosh .",
            "grep cosh /proc/cpuinfo",
            "rg --pre cat cosh .",
            "rg --pre=cat cosh .",
            "rg -n cosh /dev",
            "ls /dev/zero",
            "df --output=source",
            "df /dev/zero",
        ] {
            assert!(readonly_tokens(command).is_err(), "{command}");
        }
    }

    #[test]
    fn readonly_broker_allows_safe_per_command_arguments() {
        for command in [
            "git status --short",
            "git diff --stat",
            "git diff --name-only --",
            "git log --oneline -n 5",
            "git show --stat HEAD",
            "ps -Ao pid,pcpu,pmem,comm -r",
            "ls -la .",
            "cat README.md",
            "cat 中文.md",
            "head -n 20 README.md",
            "head -20 README.md",
            "tail -n 20 README.md",
            "grep -n cosh README.md",
            "grep -e cosh README.md",
            "rg -n cosh crates/cosh-shell",
            "rg --files crates/cosh-shell",
            "find . -maxdepth 2 -type f -name Cargo.toml -print",
            "df -h .",
            "uname -a",
            "id -u",
        ] {
            assert!(readonly_tokens(command).is_ok(), "{command}");
        }
    }
}
