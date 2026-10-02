use super::prelude::first_program_token;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CommandIntent {
    Diagnostic,
    Lookup,
    Pipeline,
    Script,
    Wrapper,
    Interactive,
    Other,
}

pub(super) fn command_intent_key(command: &str) -> &str {
    let trimmed = command.trim_start();
    if trimmed.starts_with("top") {
        "top"
    } else if trimmed.starts_with("free") {
        "free"
    } else if trimmed.starts_with("ps") {
        "ps"
    } else {
        first_program_token(command)
    }
}

pub(super) fn should_downgrade_success_finding(command: &str) -> bool {
    matches!(
        classify_command_intent(command),
        CommandIntent::Lookup
            | CommandIntent::Pipeline
            | CommandIntent::Script
            | CommandIntent::Wrapper
            | CommandIntent::Interactive
    )
}

pub(crate) fn classify_command_intent(command: &str) -> CommandIntent {
    let trimmed = command.trim_start();
    if trimmed.is_empty() {
        return CommandIntent::Other;
    }
    if has_shell_sequence_operator(trimmed) {
        return CommandIntent::Script;
    }
    if is_wrapper_command(trimmed) {
        return CommandIntent::Wrapper;
    }
    if is_shell_script_command(trimmed) {
        return CommandIntent::Script;
    }

    let program = first_program_token(trimmed);
    let intent_command = strip_env_assignment_prefix(trimmed);
    if is_lookup_intent(intent_command, program) {
        return CommandIntent::Lookup;
    }
    if program == "top" && is_top_metadata_command_text(trimmed) {
        return CommandIntent::Other;
    }
    if program == "top" && !is_batch_top_command_text(trimmed) {
        return CommandIntent::Interactive;
    }
    if is_memory_diagnostic_command(trimmed, program) {
        return CommandIntent::Diagnostic;
    }
    if trimmed.contains('|') {
        return CommandIntent::Pipeline;
    }
    CommandIntent::Other
}

fn has_shell_sequence_operator(command: &str) -> bool {
    command.contains('\n')
        || command.contains(';')
        || command.contains("&&")
        || command.contains("||")
}

fn is_wrapper_command(command: &str) -> bool {
    let first = command
        .split_whitespace()
        .find(|token| !is_env_assignment_token(token))
        .unwrap_or("");
    if first == "sudo" || first == "env" || first == "watch" || first == "ssh" {
        return true;
    }
    matches!(
        first_program_token(command),
        "docker" | "kubectl" | "podman" | "nsenter" | "chroot" | "systemd-run"
    )
}

fn is_shell_script_command(command: &str) -> bool {
    let command = strip_env_assignment_prefix(command);
    let mut tokens = command.split_whitespace();
    let Some(program) = tokens.next() else {
        return false;
    };
    if !matches!(program, "bash" | "sh" | "zsh") {
        return false;
    }
    let Some(first_arg) = tokens.next() else {
        return false;
    };
    !matches!(first_arg, "--help" | "--version" | "-h" | "-?")
}

fn strip_env_assignment_prefix(command: &str) -> &str {
    let mut rest = command.trim_start();
    loop {
        let Some(token) = rest.split_whitespace().next() else {
            return "";
        };
        if !is_env_assignment_token(token) {
            return rest;
        }
        rest = rest[rest.find(token).unwrap() + token.len()..].trim_start();
    }
}

fn is_lookup_intent(command: &str, program: &str) -> bool {
    program == "pgrep"
        || program == "pidof"
        || command.starts_with("ps -p")
        || command.starts_with("ps --pid")
        || command.contains("| grep")
        || command.contains("|grep")
}

fn is_memory_diagnostic_command(command: &str, program: &str) -> bool {
    match program {
        "free" => {
            !command.contains('|')
                && !is_free_sampling_command_text(command)
                && !is_free_line_mode_command_text(command)
                && !is_free_metadata_command_text(command)
        }
        "top" => is_batch_top_command_text(command),
        "ps" => {
            !is_ps_header_suppressed_command_text(command)
                && !is_ps_pipeline_header_removed_command_text(command)
                && (command.contains("--sort")
                    || command.contains(" -eo ")
                    || command.contains(" -e -o ")
                    || command.starts_with("ps -eo ")
                    || command.starts_with("ps aux --sort")
                    || command.starts_with("ps -aux --sort"))
        }
        _ => false,
    }
}

fn is_ps_header_suppressed_command_text(command: &str) -> bool {
    if first_program_token(command) != "ps" {
        return false;
    }
    let mut seen_program = false;
    let mut next_token_is_format = false;
    let mut skip_next_ps_arg = false;
    for token in command.split_whitespace() {
        if !seen_program {
            if is_env_assignment_token(token) || token == "sudo" {
                continue;
            }
            let basename = token
                .rsplit_once('/')
                .map(|(_, name)| name)
                .unwrap_or(token);
            if basename == "ps" {
                seen_program = true;
            }
            continue;
        }
        if matches!(token, "|" | ";" | "&&" | "||") {
            break;
        }
        if skip_next_ps_arg {
            skip_next_ps_arg = false;
            continue;
        }
        if is_ps_no_header_option_token(token) {
            return true;
        }
        if next_token_is_format {
            if ps_format_suppresses_required_header(token) {
                return true;
            }
            next_token_is_format = false;
            continue;
        }
        if matches!(token, "-o" | "--format") {
            next_token_is_format = true;
            continue;
        }
        if is_ps_option_with_next_arg(token) {
            skip_next_ps_arg = true;
            continue;
        }
        if let Some(format) = token.strip_prefix("--format=") {
            if ps_format_suppresses_required_header(format) {
                return true;
            }
            continue;
        }
        if token.starts_with('-') && !token.starts_with("--") {
            if let Some(o_pos) = token.find('o') {
                let format = &token[o_pos + 1..];
                if format.is_empty() {
                    next_token_is_format = true;
                } else if ps_format_suppresses_required_header(format) {
                    return true;
                }
            }
        }
    }
    false
}

fn is_ps_pipeline_header_removed_command_text(command: &str) -> bool {
    if first_program_token(command) != "ps" {
        return false;
    }
    command.split('|').skip(1).any(|stage| {
        let stage = stage.trim_start();
        (stage.starts_with("tail ") && stage.contains("+2"))
            || stage.starts_with("sed 1d")
            || stage.starts_with("sed '1d'")
            || stage.starts_with("sed \"1d\"")
            || (stage.starts_with("grep ")
                && stage.contains("-v")
                && contains_ps_header_token(stage))
    })
}

fn contains_ps_header_token(text: &str) -> bool {
    ["PID", "USER", "%MEM", "COMMAND", "ARGS"]
        .iter()
        .any(|token| text.contains(token))
}

fn is_ps_option_with_next_arg(token: &str) -> bool {
    matches!(
        token,
        "-C" | "-G"
            | "-g"
            | "-p"
            | "-q"
            | "-s"
            | "-t"
            | "-u"
            | "-U"
            | "--Group"
            | "--User"
            | "--group"
            | "--pid"
            | "--ppid"
            | "--quick-pid"
            | "--sid"
            | "--tty"
            | "--user"
    )
}

fn is_ps_no_header_option_token(token: &str) -> bool {
    if matches!(token, "--no-headers" | "h" | "-h") {
        return true;
    }
    let short_options = if let Some(short_options) = token.strip_prefix('-') {
        if short_options.starts_with('-') {
            return false;
        }
        short_options
    } else {
        token
    };
    short_options.contains('h')
        && short_options.bytes().all(is_ps_bsd_option_byte)
        && short_options
            .bytes()
            .any(|b| matches!(b, b'a' | b'x' | b'u' | b'e' | b'o'))
}

fn is_ps_bsd_option_byte(byte: u8) -> bool {
    matches!(
        byte,
        b'a' | b'A' | b'x' | b'X' | b'u' | b'U' | b'e' | b'E' | b'o' | b'O' | b'h' | b'H'
    )
}

fn ps_format_suppresses_required_header(format: &str) -> bool {
    format.split(',').any(|spec| {
        let Some((field, label)) = spec.trim().split_once('=') else {
            return false;
        };
        label.is_empty()
            && matches!(
                field.to_ascii_lowercase().as_str(),
                "pid" | "%mem" | "pmem" | "rss" | "rsz" | "rssize"
            )
    })
}

fn is_free_sampling_command_text(command: &str) -> bool {
    if first_program_token(command) != "free" {
        return false;
    }
    let mut seen_program = false;
    for token in command.split_whitespace() {
        if !seen_program {
            if is_env_assignment_token(token) || token == "sudo" {
                continue;
            }
            let basename = token
                .rsplit_once('/')
                .map(|(_, name)| name)
                .unwrap_or(token);
            if basename == "free" {
                seen_program = true;
            }
            continue;
        }
        if matches!(token, "|" | ";" | "&&" | "||") {
            break;
        }
        if matches!(token, "-s" | "--seconds" | "-c" | "--count")
            || token.starts_with("--seconds=")
            || token.starts_with("--count=")
        {
            return true;
        }
        if let Some(short_options) = token.strip_prefix('-') {
            if !short_options.starts_with('-')
                && (short_options.contains('s') || short_options.contains('c'))
            {
                return true;
            }
        }
    }
    false
}

fn is_free_line_mode_command_text(command: &str) -> bool {
    if first_program_token(command) != "free" {
        return false;
    }
    let mut seen_program = false;
    for token in command.split_whitespace() {
        if !seen_program {
            if is_env_assignment_token(token) || token == "sudo" {
                continue;
            }
            let basename = token
                .rsplit_once('/')
                .map(|(_, name)| name)
                .unwrap_or(token);
            if basename == "free" {
                seen_program = true;
            }
            continue;
        }
        if matches!(token, "|" | ";" | "&&" | "||") {
            break;
        }
        if matches!(token, "-L" | "--line") {
            return true;
        }
        if let Some(short_options) = token.strip_prefix('-') {
            if !short_options.starts_with('-') && short_options.contains('L') {
                return true;
            }
        }
    }
    false
}

fn is_free_metadata_command_text(command: &str) -> bool {
    if first_program_token(command) != "free" {
        return false;
    }
    let mut seen_program = false;
    for token in command.split_whitespace() {
        if !seen_program {
            if is_env_assignment_token(token) || token == "sudo" {
                continue;
            }
            let basename = token
                .rsplit_once('/')
                .map(|(_, name)| name)
                .unwrap_or(token);
            if basename == "free" {
                seen_program = true;
            }
            continue;
        }
        if matches!(token, "|" | ";" | "&&" | "||") {
            break;
        }
        if matches!(token, "--help" | "-V" | "--version") {
            return true;
        }
        if let Some(short_options) = token.strip_prefix('-') {
            if !short_options.starts_with('-') && short_options.contains('V') {
                return true;
            }
        }
    }
    false
}

fn is_top_metadata_command_text(command: &str) -> bool {
    if first_program_token(command) != "top" {
        return false;
    }
    let mut seen_program = false;
    for token in command.split_whitespace() {
        if !seen_program {
            if is_env_assignment_token(token) || token == "sudo" {
                continue;
            }
            let basename = token
                .rsplit_once('/')
                .map(|(_, name)| name)
                .unwrap_or(token);
            if basename == "top" {
                seen_program = true;
            }
            continue;
        }
        if matches!(token, "|" | ";" | "&&" | "||") {
            break;
        }
        if matches!(token, "--help" | "-h" | "-v" | "-V" | "-O" | "--version") {
            return true;
        }
    }
    false
}

fn is_batch_top_command_text(command: &str) -> bool {
    if first_program_token(command) != "top" {
        return false;
    }
    let mut seen_program = false;
    let mut skip_next_iteration_arg = false;
    let mut seen_batch = false;
    let mut seen_one_shot_iteration = false;
    for token in command.split_whitespace() {
        if !seen_program {
            if is_env_assignment_token(token) || token == "sudo" {
                continue;
            }
            let basename = token
                .rsplit_once('/')
                .map(|(_, name)| name)
                .unwrap_or(token);
            if basename == "top" {
                seen_program = true;
            }
            continue;
        }
        if matches!(token, "|" | ";" | "&&" | "||") {
            break;
        }
        if skip_next_iteration_arg {
            seen_one_shot_iteration |= is_one_shot_top_iteration_text(token);
            skip_next_iteration_arg = false;
            continue;
        }
        if token == "--batch" {
            seen_batch = true;
            continue;
        }
        if matches!(token, "-n" | "--iterations") {
            skip_next_iteration_arg = true;
            continue;
        }
        if let Some(iterations) = token.strip_prefix("--iterations=") {
            seen_one_shot_iteration |= is_one_shot_top_iteration_text(iterations);
            continue;
        }
        if let Some(short_options) = token.strip_prefix('-') {
            if short_options.starts_with('-') {
                continue;
            }
            if short_options.contains('b') {
                seen_batch = true;
            }
            if short_options == "n" {
                skip_next_iteration_arg = true;
                continue;
            }
            if let Some(n_pos) = short_options.find('n') {
                let iterations = &short_options[n_pos + 1..];
                if iterations.is_empty() {
                    skip_next_iteration_arg = true;
                } else {
                    seen_one_shot_iteration |= is_one_shot_top_iteration_text(iterations);
                }
            }
        }
    }
    seen_batch && seen_one_shot_iteration
}

fn is_one_shot_top_iteration_text(value: &str) -> bool {
    value.trim_start_matches('=').trim() == "1"
}

fn is_env_assignment_token(token: &str) -> bool {
    let Some(eq_pos) = token.find('=') else {
        return false;
    };
    if eq_pos == 0 {
        return false;
    }
    let name = &token[..eq_pos];
    name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        && !name.bytes().next().unwrap_or(0).is_ascii_digit()
}
