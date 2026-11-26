use super::command_risk::{stage_assessment, CommandShape, OutputExposure};
use super::command_risk_build::basename;
use super::command_risk_launcher::{
    launcher_payload_offset,
    output::{env_outputs_values, launcher_runs_login_shell},
};
use super::command_risk_parser::{is_env_assignment, parse_command};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ShellOutputFacts {
    pub(crate) environment: bool,
    pub(crate) sensitive: bool,
    pub(crate) arguments: bool,
    pub(crate) unknown: bool,
}

pub(crate) fn shell_output_facts(command: &str) -> ShellOutputFacts {
    let parsed = parse_command(command.trim());
    let mut facts = ShellOutputFacts {
        unknown: !literal_output_input(command)
            || parsed.requires_shell_expansion
            || !parsed.null_redirections.is_empty()
            || !matches!(
                parsed.shape,
                CommandShape::Simple
                    | CommandShape::EnvSimple
                    | CommandShape::Pipeline
                    | CommandShape::Sequence
                    | CommandShape::AndOrList
            ),
        ..Default::default()
    };
    let segments = if parsed.segments.is_empty() {
        vec![parsed.stages]
    } else {
        parsed.segments
    };
    let mut found_stage = false;
    for segment in segments {
        let mut ordinary_input = false;
        for stage in segment {
            found_stage = true;
            let start = stage.iter().position(|token| !is_env_assignment(token));
            let Some(start) = start else {
                facts.unknown = true;
                continue;
            };
            let tokens = &stage[start..];
            facts.sensitive |= tokens.iter().any(|token| super::is_sensitive_target(token));
            let payload = launcher_payload_offset(tokens)
                .or_else(|| {
                    let mut probe = tokens.to_vec();
                    probe.push("__output_probe__".to_string());
                    launcher_payload_offset(&probe)
                        .filter(|offset| *offset == tokens.len())
                        .and_then(|_| tokens.iter().rposition(|token| basename(token) == "env"))
                })
                .unwrap_or(0);
            facts.unknown |= tokens[..payload].iter().any(|token| {
                matches!(basename(token), "xargs" | "time")
                    || matches!(token.as_str(), "-v" | "--verbose" | "-t")
            });
            facts.unknown |= launcher_runs_login_shell(tokens);
            let tokens = &tokens[payload..];
            let program = basename(&tokens[0]);
            let args = &tokens[1..];
            let exposure = stage_assessment(program, tokens).output_exposure;
            facts.sensitive |= exposure == OutputExposure::MayContainSecrets
                || tokens.iter().any(|token| super::is_sensitive_target(token));
            facts.environment |= match program {
                "printenv" => !args
                    .iter()
                    .take_while(|arg| arg.as_str() != "--")
                    .any(|arg| arg == "--help" || arg == "--version"),
                "env" => env_outputs_values(args),
                _ => false,
            };
            let process_fields = (program == "ps").then(|| process_output_fields(args));
            facts.arguments |= (program == "ps"
                && args.iter().enumerate().any(|(index, arg)| {
                    let value = if matches!(arg.as_str(), "-o" | "-eo" | "-Ao" | "--format") {
                        args.get(index + 1).map(String::as_str)
                    } else {
                        arg.strip_prefix("--format=")
                            .or_else(|| arg.strip_prefix("-eo"))
                            .or_else(|| arg.strip_prefix("-Ao"))
                            .or_else(|| arg.strip_prefix("-o"))
                    };
                    value.is_some_and(|value| {
                        value
                            .split(|character: char| character == ',' || character.is_whitespace())
                            .any(|field| {
                                matches!(process_field_name(field), "args" | "command" | "cmd")
                            })
                    })
                }))
                || (program == "ps"
                    && args
                        .iter()
                        .any(|arg| matches!(arg.as_str(), "aux" | "-aux" | "-ef" | "-f")));
            let ordinary = match program {
                "uptime" | "nproc" => args.is_empty(),
                "free" => args
                    .iter()
                    .all(|arg| matches!(arg.as_str(), "-h" | "-m" | "-k" | "-b" | "--human")),
                "df" => args.iter().all(|arg| {
                    matches!(
                        arg.as_str(),
                        "-h" | "-H" | "-k" | "-P" | "-T" | "--human-readable"
                    ) || arg.starts_with('/')
                }),
                "printf" | "echo" => {
                    !args.is_empty()
                        && args.iter().all(|arg| {
                            arg.chars().all(|character| {
                                character.is_ascii_punctuation()
                                    || character.is_whitespace()
                                    || matches!(character, 'n' | 't' | 'r')
                            })
                        })
                }
                "ps" => process_fields.as_ref().is_some_and(|fields| {
                    !fields.is_empty()
                        && fields.iter().all(|field| {
                            matches!(
                                field.as_str(),
                                "pid"
                                    | "ppid"
                                    | "user"
                                    | "uid"
                                    | "%cpu"
                                    | "%mem"
                                    | "pcpu"
                                    | "pmem"
                                    | "rss"
                                    | "vsz"
                                    | "etime"
                                    | "comm"
                                    | "stat"
                            )
                        })
                }),
                "head" => {
                    ordinary_input
                        && args.len() == 2
                        && args[0] == "-n"
                        && args[1].parse::<usize>().is_ok()
                }
                _ => false,
            };
            facts.unknown |= !ordinary;
            ordinary_input = ordinary;
        }
    }
    facts.unknown |= !found_stage;
    facts
}

fn literal_output_input(command: &str) -> bool {
    let mut quote = None;
    for character in command.chars() {
        if quote == Some('\'') {
            if character == '\'' {
                quote = None;
            }
            continue;
        }
        if matches!(character, '$' | '`' | '!' | '\\')
            || (quote.is_none() && matches!(character, '*' | '?' | '[' | '#' | '~'))
        {
            return false;
        }
        if character == '"' {
            quote = if quote == Some('"') { None } else { Some('"') };
        } else if character == '\'' && quote.is_none() {
            quote = Some('\'');
        }
    }
    quote.is_none()
}

fn process_output_fields(args: &[String]) -> Vec<String> {
    let mut fields = Vec::new();
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        let value = if matches!(arg.as_str(), "-o" | "-eo" | "-Ao" | "--format") {
            index += 1;
            args.get(index).map(String::as_str)
        } else if let Some(value) = arg
            .strip_prefix("--format=")
            .or_else(|| arg.strip_prefix("-eo"))
            .or_else(|| arg.strip_prefix("-Ao"))
            .or_else(|| arg.strip_prefix("-o"))
        {
            Some(value)
        } else if matches!(arg.as_str(), "-e" | "-A") {
            None
        } else if let Some(sort) = arg.strip_prefix("--sort=") {
            if !ordinary_process_sort(sort) {
                return Vec::new();
            }
            None
        } else if arg == "--sort" {
            index += 1;
            if !args
                .get(index)
                .is_some_and(|sort| ordinary_process_sort(sort))
            {
                return Vec::new();
            }
            None
        } else if matches!(arg.as_str(), "-p" | "--pid") {
            index += 1;
            if !args.get(index).is_some_and(|value| {
                value
                    .chars()
                    .all(|character| character.is_ascii_digit() || character == ',')
            }) {
                return Vec::new();
            }
            None
        } else {
            return Vec::new();
        };
        if let Some(value) = value {
            fields.extend(
                value
                    .split(|character: char| character == ',' || character.is_whitespace())
                    .filter(|field| !field.is_empty())
                    .map(ToString::to_string),
            );
        }
        index += 1;
    }
    fields
}

fn process_field_name(field: &str) -> &str {
    field.split(['=', ':']).next().unwrap_or(field)
}

fn ordinary_process_sort(value: &str) -> bool {
    value.split(',').all(|field| {
        matches!(
            field.trim_start_matches(['-', '+']),
            "pid" | "ppid" | "%cpu" | "%mem" | "pcpu" | "pmem" | "rss" | "vsz" | "etime" | "comm"
        )
    })
}

#[cfg(test)]
library_tests! {
    #[path = "command_risk_output_tests.rs"]
    mod tests;
}
