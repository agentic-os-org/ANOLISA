use super::model::ExecutionScope;

const REMOTE_OR_CONTAINER_WRAPPERS: &[&str] = &[
    "chroot",
    "docker",
    "kubectl",
    "lxc-attach",
    "machinectl",
    "mosh",
    "nerdctl",
    "nsenter",
    "podman",
    "ssh",
];

const INDIRECT_COMMAND_WRAPPERS: &[&str] = &[
    "bash", "command", "dash", "doas", "env", "exec", "fish", "ionice", "ksh", "nice", "nohup",
    "setsid", "sh", "stdbuf", "sudo", "taskset", "time", "timeout", "watch", "xargs", "zsh",
];

pub(crate) fn resolve_execution_scope(session_id: &str, command: &str) -> ExecutionScope {
    let Some(program) = direct_program(command) else {
        return ExecutionScope::unknown(session_id);
    };
    if REMOTE_OR_CONTAINER_WRAPPERS.contains(&program)
        || INDIRECT_COMMAND_WRAPPERS.contains(&program)
    {
        ExecutionScope::unknown(session_id)
    } else {
        ExecutionScope::local(session_id)
    }
}

pub(crate) fn direct_program(command: &str) -> Option<&str> {
    if command.trim().is_empty() || has_composite_syntax(command) {
        return None;
    }
    let tokens = command.split_whitespace().collect::<Vec<_>>();
    target_program(&tokens)
}

fn has_composite_syntax(command: &str) -> bool {
    command.chars().any(|ch| {
        matches!(
            ch,
            '\0' | '\n'
                | '\r'
                | '|'
                | ';'
                | '&'
                | '`'
                | '\''
                | '"'
                | '\\'
                | '$'
                | '<'
                | '>'
                | '('
                | ')'
        )
    })
}

fn target_program<'a>(tokens: &[&'a str]) -> Option<&'a str> {
    let mut index = 0;
    skip_assignments(tokens, &mut index);
    if basename(tokens.get(index)?) == "env" {
        index += 1;
        skip_env_prefix(tokens, &mut index)?;
    }
    if basename(tokens.get(index)?) == "sudo" {
        index += 1;
        skip_sudo_options(tokens, &mut index)?;
        if basename(tokens.get(index)?) == "env" {
            index += 1;
            skip_env_prefix(tokens, &mut index)?;
        }
    }
    Some(basename(tokens.get(index)?))
}

fn skip_assignments(tokens: &[&str], index: &mut usize) {
    while tokens.get(*index).is_some_and(|token| is_assignment(token)) {
        *index += 1;
    }
}

fn skip_env_prefix(tokens: &[&str], index: &mut usize) -> Option<()> {
    while let Some(token) = tokens.get(*index) {
        match *token {
            "--" => {
                *index += 1;
                break;
            }
            "-u" | "--unset" | "-C" | "--chdir" | "-S" | "--split-string" => {
                *index += 2;
            }
            "-i" | "--ignore-environment" | "-0" | "--null" => {
                *index += 1;
            }
            value
                if value.starts_with("--unset=")
                    || value.starts_with("--chdir=")
                    || value.starts_with("--split-string=") =>
            {
                *index += 1;
            }
            value if value.starts_with('-') => return None,
            value if is_assignment(value) => *index += 1,
            _ => break,
        }
    }
    tokens.get(*index).map(|_| ())
}

fn skip_sudo_options(tokens: &[&str], index: &mut usize) -> Option<()> {
    while let Some(token) = tokens.get(*index) {
        match *token {
            "--" => {
                *index += 1;
                break;
            }
            "-u" | "-g" | "-h" | "-p" | "-C" | "-T" | "--user" | "--group" | "--host"
            | "--prompt" | "--close-from" | "--command-timeout" => {
                *index += 2;
            }
            "-n" | "--non-interactive" | "-E" | "--preserve-env" | "-H" | "--set-home" => {
                *index += 1;
            }
            value
                if value.starts_with("--user=")
                    || value.starts_with("--group=")
                    || value.starts_with("--host=")
                    || value.starts_with("--prompt=")
                    || value.starts_with("--close-from=")
                    || value.starts_with("--command-timeout=")
                    || (value.starts_with("-u") && value.len() > 2)
                    || (value.starts_with("-g") && value.len() > 2) =>
            {
                *index += 1;
            }
            value if value.starts_with('-') => return None,
            _ => break,
        }
    }
    tokens.get(*index).map(|_| ())
}

fn is_assignment(token: &str) -> bool {
    let Some((name, _)) = token.split_once('=') else {
        return false;
    };
    !name.is_empty()
        && !name.as_bytes()[0].is_ascii_digit()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn basename(token: &str) -> &str {
    token.rsplit('/').next().unwrap_or(token)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::insight::model::ExecutionScopeKind;

    #[test]
    fn direct_env_and_sudo_commands_are_local() {
        for command in [
            "make test",
            "env LANG=C free -m",
            "sudo free -m",
            "sudo -u root ps aux",
        ] {
            let scope = resolve_execution_scope("session-1", command);
            assert_eq!(scope.kind, ExecutionScopeKind::LocalHost, "{command}");
            assert_eq!(scope.identity.as_deref(), Some("session-1"), "{command}");
            assert!(scope.allows_correlation(), "{command}");
        }
    }

    #[test]
    fn remote_container_and_composite_commands_are_unknown() {
        for command in [
            "ssh host free -m",
            "docker exec app ps aux",
            "podman exec app free",
            "kubectl exec pod -- free",
            "nsenter -t 1 -m free",
            "chroot /mnt free",
            "sudo ssh host free",
            "ssh host 'free -m'",
            "ssh host \"free -m\"",
            "echo 'local text'",
            "free -m | cat",
            "free -m > /tmp/free.txt",
            "echo $(free -m)",
        ] {
            let scope = resolve_execution_scope("session-1", command);
            assert_eq!(scope.kind, ExecutionScopeKind::UnknownWrapper, "{command}");
            assert_eq!(scope.identity, None, "{command}");
            assert!(!scope.allows_correlation(), "{command}");
        }
    }

    #[test]
    fn indirect_command_wrappers_are_unknown() {
        for command in [
            "time free -m",
            "nohup free -m",
            "command free -m",
            "xargs free",
            "bash -c free",
            "sudo time free -m",
            "env LANG=C nohup free -m",
        ] {
            let scope = resolve_execution_scope("session-1", command);
            assert_eq!(scope.kind, ExecutionScopeKind::UnknownWrapper, "{command}");
            assert!(!scope.allows_correlation(), "{command}");
        }
    }

    #[test]
    fn unsupported_env_and_sudo_options_are_unknown() {
        for command in [
            "env --unknown free -m",
            "env -Z free -m",
            "sudo -R /mnt free -m",
            "sudo --chroot=/mnt free -m",
            "sudo --unknown free -m",
        ] {
            let scope = resolve_execution_scope("session-1", command);
            assert_eq!(scope.kind, ExecutionScopeKind::UnknownWrapper, "{command}");
            assert!(!scope.allows_correlation(), "{command}");
        }
    }

    #[test]
    fn explicitly_supported_env_and_sudo_options_are_local() {
        for command in [
            "env -i LANG=C free -m",
            "env --unset LANG free -m",
            "env --chdir /tmp free -m",
            "sudo --non-interactive free -m",
            "sudo --user=root free -m",
            "sudo -uroot free -m",
        ] {
            let scope = resolve_execution_scope("session-1", command);
            assert_eq!(scope.kind, ExecutionScopeKind::LocalHost, "{command}");
        }
    }
}
