//! Discover public syntax without loading configuration or starting runtime work.

use clap::{Command, CommandFactory};
use serde::Serialize;
use std::io::Write;

#[derive(Serialize)]
struct Inventory {
    format: u8,
    name: &'static str,
    version: &'static str,
    commands: Vec<Entry>,
}

#[derive(Debug, PartialEq, Serialize)]
struct Entry {
    command: String,
    description: String,
    arguments: Vec<Argument>,
}

#[derive(Debug, PartialEq, Serialize)]
struct Argument {
    id: String,
    short: Option<char>,
    long: Option<String>,
    positional: bool,
    required: bool,
    takes_values: bool,
    value_names: Vec<String>,
    choices: Vec<String>,
    description: String,
}

fn tree() -> Command {
    let mut command = super::Cli::command();
    command.build();
    command
}

fn public_tree(command: &Command) -> Command {
    // Some completion backends enumerate hidden children. Project visible
    // declarations recursively instead of maintaining a separate command list.
    let mut visible = Command::new(command.get_name().to_owned())
        .about(command.get_about().cloned().unwrap_or_default())
        .disable_help_flag(true)
        .disable_version_flag(true)
        .disable_help_subcommand(true)
        .args(
            command
                .get_arguments()
                .filter(|arg| !arg.is_hide_set())
                .cloned(),
        )
        .groups(command.get_groups().cloned())
        .subcommands(
            command
                .get_subcommands()
                .filter(|child| !child.is_hide_set())
                .map(public_tree),
        );
    if let Some(version) = command.get_version() {
        visible = visible.version(version.to_owned());
    }
    visible
}

fn entries(command: &Command, path: &str, result: &mut Vec<Entry>) {
    result.push(Entry {
        command: path.into(),
        description: command
            .get_about()
            .map(ToString::to_string)
            .unwrap_or_default(),
        arguments: command
            .get_arguments()
            .filter(|argument| !argument.is_hide_set())
            .map(|argument| Argument {
                id: argument.get_id().to_string(),
                short: argument.get_short(),
                long: argument.get_long().map(str::to_owned),
                positional: argument.is_positional(),
                // Conditional requirements and conflicts still belong to the
                // parser. This field describes only an unconditional requirement.
                required: argument.is_required_set(),
                takes_values: argument.get_action().takes_values(),
                value_names: argument
                    .get_value_names()
                    .unwrap_or_default()
                    .iter()
                    .map(ToString::to_string)
                    .collect(),
                choices: argument
                    .get_possible_values()
                    .into_iter()
                    .filter(|value| !value.is_hide_set())
                    .map(|value| value.get_name().to_owned())
                    .collect(),
                description: argument
                    .get_help()
                    .map(ToString::to_string)
                    .unwrap_or_default(),
            })
            .collect(),
    });
    for child in command
        .get_subcommands()
        .filter(|child| !child.is_hide_set())
    {
        entries(child, &format!("{path} {}", child.get_name()), result);
    }
}

pub(super) fn commands(json: bool) -> crate::cli::Result<crate::cli::Exit> {
    let mut commands = Vec::new();
    entries(&tree(), "aw", &mut commands);
    let mut stdout = std::io::stdout().lock();
    if json {
        serde_json::to_writer(
            &mut stdout,
            &Inventory {
                format: 1,
                name: "aw",
                version: env!("CARGO_PKG_VERSION"),
                commands,
            },
        )?;
        writeln!(stdout)?;
    } else {
        for command in commands {
            writeln!(stdout, "{}  {}", command.command, command.description)?;
        }
    }
    Ok(crate::cli::Exit::Code(0))
}

pub(super) fn completion(shell: clap_complete::Shell) -> crate::cli::Result<crate::cli::Exit> {
    // Generate into memory because clap_complete's writer adapter can panic on
    // an I/O failure. Only the final publication touches the caller's stdout.
    let mut script = Vec::new();
    clap_complete::generate(shell, &mut public_tree(&tree()), "aw", &mut script);
    std::io::stdout().lock().write_all(&script)?;
    Ok(crate::cli::Exit::Code(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_projection_preserves_every_public_command_and_argument() {
        let declared = tree();
        let mut projected = public_tree(&declared);
        projected.build();
        projected.clone().debug_assert();
        let mut original = Vec::new();
        let mut visible = Vec::new();
        entries(&declared, "aw", &mut original);
        entries(&projected, "aw", &mut visible);
        assert_eq!(original, visible);
        assert!(!projected.get_subcommands().any(Command::is_hide_set));
    }
}
