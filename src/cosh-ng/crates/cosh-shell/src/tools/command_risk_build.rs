use super::command_risk::{
    AssessmentConfidence, AssessmentSource, AutoAllowEvidence, CommandAssessment, CommandShape,
    ExecutionDecision, InteractionRequirement, OutputExposure, OutputStability, RiskImpact,
    SideEffectClass,
};
use super::command_risk_parser::is_env_assignment;

pub(super) fn has_interpreter_inline_code(program: &str, tokens: &[String]) -> bool {
    interpreter_program_source(program, tokens) == Some(InterpreterProgramSource::Inline)
}

pub(super) fn interpreter_consumes_stdin_as_program(program: &str, tokens: &[String]) -> bool {
    interpreter_program_source(program, tokens) == Some(InterpreterProgramSource::Stdin)
}

pub(super) fn downloaded_program_file<'a>(program: &str, tokens: &'a [String]) -> Option<&'a str> {
    if let Some(InterpreterProgramSource::File(path)) = interpreter_program_source(program, tokens)
    {
        return Some(path);
    }
    if !matches!(program, "sh" | "bash" | "zsh" | "fish") {
        return None;
    }
    command_args(tokens)
        .iter()
        .find(|arg| !arg.starts_with('-'))
        .map(String::as_str)
}

pub(super) fn network_download_effect(
    program: &str,
    tokens: &[String],
) -> Option<(bool, Vec<String>)> {
    match program {
        "curl" => Some(curl_download_effect(command_args(tokens))),
        "wget" => Some(wget_download_effect(command_args(tokens))),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InterpreterProgramSource<'a> {
    Inline,
    Stdin,
    File(&'a str),
    Other,
}

fn interpreter_program_source<'a>(
    program: &str,
    tokens: &'a [String],
) -> Option<InterpreterProgramSource<'a>> {
    if !matches!(program, "python" | "python3" | "node" | "ruby" | "perl") {
        return None;
    }

    let args = interpreter_args(tokens);
    let mut check_only = false;
    let mut index = 0;
    while let Some(arg) = args.get(index).map(String::as_str) {
        if arg == "-" {
            return Some(if check_only {
                InterpreterProgramSource::Other
            } else {
                InterpreterProgramSource::Stdin
            });
        }
        if arg == "--" {
            return Some(match args.get(index + 1).map(String::as_str) {
                None | Some("-") if !check_only => InterpreterProgramSource::Stdin,
                Some(source) if !check_only => InterpreterProgramSource::File(source),
                _ => InterpreterProgramSource::Other,
            });
        }
        if interpreter_exits_without_program(program, arg) {
            return Some(InterpreterProgramSource::Other);
        }
        if interpreter_check_only_option(program, arg) {
            check_only = true;
            index += 1;
            continue;
        }
        if let Some(consumes_next) = interpreter_inline_code_option(program, arg) {
            if consumes_next && args.get(index + 1).is_none() {
                return Some(InterpreterProgramSource::Other);
            }
            return Some(if check_only {
                InterpreterProgramSource::Other
            } else {
                InterpreterProgramSource::Inline
            });
        }
        if interpreter_program_option(program, arg) {
            return Some(InterpreterProgramSource::Other);
        }
        if !arg.starts_with('-') {
            return Some(if check_only {
                InterpreterProgramSource::Other
            } else {
                InterpreterProgramSource::File(arg)
            });
        }
        if interpreter_option_consumes_next(program, arg) {
            if args.get(index + 1).is_none() {
                return Some(InterpreterProgramSource::Other);
            }
            index += 2;
        } else {
            index += 1;
        }
    }
    Some(if check_only {
        InterpreterProgramSource::Other
    } else {
        InterpreterProgramSource::Stdin
    })
}

fn interpreter_args(tokens: &[String]) -> &[String] {
    command_args(tokens)
}

fn command_args(tokens: &[String]) -> &[String] {
    let program_index = tokens
        .iter()
        .position(|token| !is_env_assignment(token))
        .unwrap_or(tokens.len());
    tokens.get(program_index + 1..).unwrap_or_default()
}

pub(super) fn command_requires_tty(program: &str, tokens: &[String]) -> bool {
    matches!(
        program,
        "less" | "more" | "man" | "htop" | "ssh" | "scp" | "sftp"
    ) || matches!(program, "python" | "python3" | "node" | "irb" | "ruby") && !has_eval_arg(tokens)
        || matches!(program, "docker" | "podman" | "kubectl") && has_tty_arg(tokens)
}

fn has_eval_arg(tokens: &[String]) -> bool {
    tokens
        .iter()
        .skip(1)
        .any(|arg| matches!(arg.as_str(), "-c" | "-e" | "--eval" | "--command"))
}

pub(super) fn has_tty_arg(tokens: &[String]) -> bool {
    tokens.iter().any(|arg| {
        matches!(
            arg.as_str(),
            "-it" | "-ti" | "-i" | "-t" | "--interactive" | "--tty"
        ) || arg.starts_with("--interactive=")
            || arg.starts_with("--tty=")
    })
}

fn attached_short_option(arg: &str, option: &str) -> bool {
    arg.strip_prefix(option)
        .is_some_and(|value| !value.is_empty())
}

fn interpreter_inline_code_option(program: &str, arg: &str) -> Option<bool> {
    match program {
        "python" | "python3" => python_inline_option(arg),
        "node" => long_or_short_inline_option(arg, &["-e", "-p"], &["--eval", "--print"]),
        "ruby" => short_inline_option(arg, "-e"),
        "perl" => perl_inline_option(arg),
        _ => None,
    }
}

fn python_inline_option(arg: &str) -> Option<bool> {
    let flags = arg
        .strip_prefix('-')
        .filter(|flags| !flags.starts_with('-'))?;
    for (index, flag) in flags.char_indices() {
        if flag == 'c' {
            return Some(index + flag.len_utf8() == flags.len());
        }
        if !matches!(
            flag,
            'b' | 'B' | 'd' | 'E' | 'i' | 'I' | 'O' | 'P' | 'q' | 's' | 'S' | 'u' | 'v' | 'x'
        ) {
            return None;
        }
    }
    None
}

fn short_inline_option(arg: &str, option: &str) -> Option<bool> {
    (arg == option)
        .then_some(true)
        .or_else(|| attached_short_option(arg, option).then_some(false))
}

fn long_or_short_inline_option(
    arg: &str,
    short_options: &[&str],
    long_options: &[&str],
) -> Option<bool> {
    if short_options.contains(&arg) || long_options.contains(&arg) {
        return Some(true);
    }
    if short_options
        .iter()
        .any(|option| attached_short_option(arg, option))
        || long_options.iter().any(|option| {
            arg.strip_prefix(option)
                .is_some_and(|value| value.starts_with('='))
        })
    {
        return Some(false);
    }
    None
}

fn perl_inline_option(arg: &str) -> Option<bool> {
    let flags = arg
        .strip_prefix('-')
        .filter(|flags| !flags.starts_with('-'))?;
    let mut iter = flags.char_indices().peekable();
    while let Some((index, flag)) = iter.next() {
        match flag {
            'e' | 'E' => return Some(index + flag.len_utf8() == flags.len()),
            'a' | 'c' | 'n' | 'p' | 's' | 'S' | 't' | 'T' | 'u' | 'U' | 'w' | 'W' => {}
            '0' => {
                if iter.peek().is_some_and(|(_, next)| *next == 'x') {
                    iter.next();
                    while iter
                        .peek()
                        .is_some_and(|(_, next)| next.is_ascii_hexdigit())
                    {
                        iter.next();
                    }
                } else {
                    while iter
                        .peek()
                        .is_some_and(|(_, next)| matches!(next, '0'..='7'))
                    {
                        iter.next();
                    }
                }
            }
            'l' => {
                while iter
                    .peek()
                    .is_some_and(|(_, next)| matches!(next, '0'..='7'))
                {
                    iter.next();
                }
            }
            // These switches consume the remainder of the current argument,
            // so an `e` inside their operand is data rather than an eval flag.
            'C' | 'D' | 'F' | 'I' | 'M' | 'V' | 'd' | 'm' | 'x' => return None,
            _ => return None,
        }
    }
    None
}

fn interpreter_option_consumes_next(program: &str, arg: &str) -> bool {
    // These tables encode source-selection CLI contracts, not every runtime
    // flag. Audit option arity and source modes when supported majors change.
    let options = match program {
        "python" | "python3" => &["-W", "-X", "--check-hash-based-pycs"][..],
        "node" => &[
            "-C",
            "-r",
            "--conditions",
            "--env-file",
            "--env-file-if-exists",
            "--experimental-loader",
            "--import",
            "--input-type",
            "--loader",
            "--require",
            "--title",
        ][..],
        "ruby" => &[
            "-C",
            "-E",
            "-I",
            "-r",
            "--encoding",
            "--external-encoding",
            "--internal-encoding",
        ][..],
        "perl" => &["-F", "-I", "-M", "-m"][..],
        _ => &[],
    };
    options.contains(&arg)
}

fn interpreter_program_option(program: &str, arg: &str) -> bool {
    match program {
        "python" | "python3" => arg == "-m" || attached_short_option(arg, "-m"),
        "node" => arg == "--run" || arg.starts_with("--run=") || arg == "--test",
        "ruby" => arg == "-S" || attached_short_option(arg, "-S"),
        _ => false,
    }
}

fn interpreter_exits_without_program(program: &str, arg: &str) -> bool {
    arg == "-h"
        || arg.starts_with("--help")
        || matches!(
            (program, arg),
            ("python" | "python3", "-V" | "--version")
                | ("node", "-v" | "--version")
                | ("ruby", "-v" | "--version" | "--copyright")
                | ("perl", "-v" | "-V")
        )
}

fn interpreter_check_only_option(program: &str, arg: &str) -> bool {
    // Perl `-c` still runs compile-time blocks, so it remains executable stdin code.
    matches!((program, arg), ("node", "-c" | "--check"))
        || program == "ruby" && ruby_check_only_option(arg)
}

fn ruby_check_only_option(arg: &str) -> bool {
    let Some(flags) = arg
        .strip_prefix('-')
        .filter(|flags| !flags.starts_with('-'))
    else {
        return false;
    };
    flags.find('c').is_some_and(|index| {
        !flags[..index].chars().any(|flag| {
            matches!(
                flag,
                'C' | 'E' | 'F' | 'I' | 'K' | 'S' | 'T' | 'W' | 'e' | 'i' | 'r' | 'x'
            )
        })
    })
}

fn curl_download_effect(args: &[String]) -> (bool, Vec<String>) {
    let mut explicit_destinations = 0;
    let mut explicit_stdout = false;
    let mut output_files = Vec::new();
    let mut remote_name_all = false;
    let mut remote_urls = 0;
    let mut index = 0;
    while let Some(arg) = args.get(index).map(String::as_str) {
        if arg == "--" {
            remote_urls += args[index + 1..]
                .iter()
                .filter(|value| looks_like_remote_url(value))
                .count();
            break;
        }
        if looks_like_remote_url(arg) {
            remote_urls += 1;
        } else if arg == "--remote-name" {
            explicit_destinations += 1;
        } else if arg == "--remote-name-all" {
            remote_name_all = true;
        } else if matches!(arg, "--no-remote-name-all" | "--next") {
            remote_name_all = false;
        } else if arg == "--dump-header" {
            if args.get(index + 1).is_some_and(|output| output == "-") {
                explicit_stdout = true;
            }
            index += 1;
        } else if arg == "-D-" || arg == "--dump-header=-" {
            explicit_stdout = true;
        } else if arg == "--output" {
            let Some(output) = args.get(index + 1) else {
                return (true, output_files);
            };
            record_download_destination(
                output,
                &mut explicit_destinations,
                &mut explicit_stdout,
                &mut output_files,
            );
            index += 1;
        } else if let Some(output) = arg.strip_prefix("--output=") {
            record_download_destination(
                output,
                &mut explicit_destinations,
                &mut explicit_stdout,
                &mut output_files,
            );
        } else if let Some(short_options) = arg
            .strip_prefix('-')
            .filter(|value| !value.starts_with('-'))
        {
            for (option_index, option) in short_options.char_indices() {
                match option {
                    'o' => {
                        let attached = &short_options[option_index + 1..];
                        if attached.is_empty() {
                            let Some(output) = args.get(index + 1) else {
                                return (true, output_files);
                            };
                            record_download_destination(
                                output,
                                &mut explicit_destinations,
                                &mut explicit_stdout,
                                &mut output_files,
                            );
                            index += 1;
                        } else {
                            record_download_destination(
                                attached,
                                &mut explicit_destinations,
                                &mut explicit_stdout,
                                &mut output_files,
                            );
                        }
                        break;
                    }
                    'O' => explicit_destinations += 1,
                    // Only traverse switches known not to consume the rest of
                    // a combined short-option token. Unknown forms stay
                    // conservative instead of hiding a possible stdout body.
                    'f' | 'g' | 'G' | 'i' | 'I' | 'J' | 'k' | 'L' | 'N' | 'q' | 'R' | 's' | 'S' => {
                    }
                    _ => return (true, output_files),
                }
            }
        }
        index += 1;
    }
    let has_default_stdout = !remote_name_all && explicit_destinations < remote_urls;
    (
        explicit_stdout || has_default_stdout || remote_urls == 0,
        output_files,
    )
}

fn wget_download_effect(args: &[String]) -> (bool, Vec<String>) {
    let mut index = 0;
    while let Some(arg) = args.get(index).map(String::as_str) {
        if arg == "--" {
            break;
        }
        if arg == "--output-document" || arg == "-O" {
            return output_document_effect(args.get(index + 1).map(String::as_str));
        }
        if let Some(output) = arg.strip_prefix("--output-document=") {
            return output_document_effect(Some(output));
        }
        if let Some(short_options) = arg
            .strip_prefix('-')
            .filter(|value| !value.starts_with('-'))
        {
            for (option_index, option) in short_options.char_indices() {
                match option {
                    'q' => {}
                    'O' => {
                        let attached = &short_options[option_index + 1..];
                        return if attached.is_empty() {
                            output_document_effect(args.get(index + 1).map(String::as_str))
                        } else {
                            output_document_effect(Some(attached))
                        };
                    }
                    // Unknown combined forms stay conservative instead of
                    // hiding a later `O-` stdout destination.
                    _ => return (true, Vec::new()),
                }
            }
        }
        index += 1;
    }
    (false, Vec::new())
}

fn record_download_destination(
    output: &str,
    explicit_destinations: &mut usize,
    explicit_stdout: &mut bool,
    output_files: &mut Vec<String>,
) {
    *explicit_destinations += 1;
    if output == "-" {
        *explicit_stdout = true;
    } else {
        output_files.push(output.to_string());
    }
}

fn output_document_effect(output: Option<&str>) -> (bool, Vec<String>) {
    match output {
        Some("-") | None => (true, Vec::new()),
        Some(path) => (false, vec![path.to_string()]),
    }
}

fn looks_like_remote_url(value: &str) -> bool {
    matches!(
        value.split_once("://").map(|(scheme, _)| scheme),
        Some("http" | "https" | "ftp" | "ftps")
    )
}

pub(super) fn basename(program: &str) -> &str {
    program
        .rsplit_once('/')
        .map(|(_, name)| name)
        .unwrap_or(program)
}

/// Retains a stderr-only argv grant; other output suppression needs approval.
pub(super) fn apply_null_redirection_policy(result: &mut CommandAssessment) {
    result.reasons.push("output-suppressed");
    result.reasons = dedupe_reasons(std::mem::take(&mut result.reasons));
    if result.auto_allow == Some(AutoAllowEvidence::StderrSuppressedReadonly) {
        return;
    }
    if result.execution == ExecutionDecision::AutoAllow {
        result.execution = ExecutionDecision::AskUser;
    }
    result.auto_allow = None;
}

pub(super) fn high_risk_program(
    program: &str,
) -> Option<(SideEffectClass, &'static str, InteractionRequirement)> {
    match program {
        "sudo" | "su" => Some((
            SideEffectClass::PrivilegeEscalation,
            "privilege-escalation",
            InteractionRequirement::CredentialPromptLikely,
        )),
        "passwd" => Some((
            SideEffectClass::CredentialAccess,
            "credential-access",
            InteractionRequirement::CredentialPromptLikely,
        )),
        "vim" | "vi" | "nvim" | "nano" | "emacs" => Some((
            SideEffectClass::FilesystemWrite,
            "interactive-editor",
            InteractionRequirement::TtyRequired,
        )),
        "rm" | "rmdir" => Some((
            SideEffectClass::FilesystemDelete,
            "filesystem-delete",
            InteractionRequirement::None,
        )),
        "mv" | "dd" => Some((
            SideEffectClass::FilesystemWrite,
            "filesystem-write",
            InteractionRequirement::None,
        )),
        "chmod" | "chown" => Some((
            SideEffectClass::PermissionChange,
            "permission-change",
            InteractionRequirement::None,
        )),
        "kill" | "pkill" | "killall" => Some((
            SideEffectClass::ProcessControl,
            "process-control",
            InteractionRequirement::None,
        )),
        // Whole-machine irrecoverable operations: strictly worse than
        // service control (a service can restart; a rebooted host drops
        // every SSH session and loses unsaved work). issue #2064.
        "reboot" | "shutdown" | "poweroff" | "halt" | "init" | "telinit" => Some((
            SideEffectClass::SystemControl,
            "system-control",
            InteractionRequirement::None,
        )),
        "brew" | "apt" | "apt-get" | "dnf" | "yum" => Some((
            SideEffectClass::PackageInstall,
            "package-manager-mutation",
            InteractionRequirement::None,
        )),
        "systemctl" | "launchctl" | "service" => Some((
            SideEffectClass::ServiceControl,
            "service-control",
            InteractionRequirement::None,
        )),
        _ => None,
    }
}

pub(super) fn high_shell_syntax(
    source: AssessmentSource,
    command: &str,
    shape: CommandShape,
    reason: &'static str,
) -> CommandAssessment {
    assessment(
        source,
        command,
        shape,
        ExecutionDecision::AskUser,
        RiskImpact::High,
        AssessmentConfidence::High,
        InteractionRequirement::None,
        OutputStability::StableSnapshot,
        OutputExposure::Normal,
        vec![SideEffectClass::Unknown],
        vec![reason],
        None,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn assessment(
    source: AssessmentSource,
    command: &str,
    shape: CommandShape,
    execution: ExecutionDecision,
    impact: RiskImpact,
    confidence: AssessmentConfidence,
    interaction: InteractionRequirement,
    output_stability: OutputStability,
    output_exposure: OutputExposure,
    side_effects: Vec<SideEffectClass>,
    reasons: Vec<&'static str>,
    auto_allow: Option<AutoAllowEvidence>,
) -> CommandAssessment {
    CommandAssessment {
        source,
        command: command.to_string(),
        shape,
        execution,
        impact,
        confidence,
        interaction,
        output_stability,
        output_exposure,
        side_effects,
        reasons,
        auto_allow,
    }
}

pub(super) fn min_confidence(
    left: AssessmentConfidence,
    right: AssessmentConfidence,
) -> AssessmentConfidence {
    use AssessmentConfidence::*;
    match (left, right) {
        (Low, _) | (_, Low) => Low,
        (Medium, _) | (_, Medium) => Medium,
        (High, High) => High,
    }
}

pub(super) fn max_output_stability(
    left: OutputStability,
    right: OutputStability,
) -> OutputStability {
    use OutputStability::*;
    let rank = |stability| match stability {
        StableSnapshot => 0,
        PotentiallyLarge => 1,
        Streaming => 2,
        UnstableInteractive => 3,
    };
    if rank(right) > rank(left) {
        right
    } else {
        left
    }
}

pub(super) fn max_output_exposure(left: OutputExposure, right: OutputExposure) -> OutputExposure {
    use OutputExposure::*;
    let rank = |exposure| match exposure {
        Normal => 0,
        MayContainCommandLine => 1,
        MayContainEnvironment => 2,
        MayContainSecrets => 3,
    };
    if rank(right) > rank(left) {
        right
    } else {
        left
    }
}

pub(super) fn dedupe_reasons(reasons: Vec<&'static str>) -> Vec<&'static str> {
    let mut out = Vec::new();
    for reason in reasons {
        if !out.contains(&reason) {
            out.push(reason);
        }
    }
    out
}
