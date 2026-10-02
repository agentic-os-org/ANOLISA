//! Deterministic grammar for session slash-command arguments.

#[derive(Debug, PartialEq, Eq)]
pub(super) enum SessionCommand<'a> {
    OpenPicker,
    New,
    Status,
    List { all: bool },
    Resume(&'a str),
    Clear(Vec<String>),
    Compact(Option<&'a str>),
    Usage,
}

pub(super) fn parse_session_command(arguments: &str) -> SessionCommand<'_> {
    let tokens = arguments.split_whitespace().collect::<Vec<_>>();
    match tokens.as_slice() {
        [] | ["resume"] => SessionCommand::OpenPicker,
        ["new"] => SessionCommand::New,
        ["status"] => SessionCommand::Status,
        ["list"] => SessionCommand::List { all: false },
        ["list", "--all"] => SessionCommand::List { all: true },
        ["resume", session_id] => SessionCommand::Resume(session_id),
        ["clear", "--all"] => SessionCommand::Clear(vec!["--all".to_string()]),
        ["clear", session_ids @ ..]
            if !session_ids.is_empty() && !session_ids.contains(&"--all") =>
        {
            SessionCommand::Clear(
                session_ids
                    .iter()
                    .map(|session_id| (*session_id).to_string())
                    .collect(),
            )
        }
        ["compact"] => SessionCommand::Compact(None),
        ["compact", subcommand] => SessionCommand::Compact(Some(subcommand)),
        [session_id] if is_bare_session_id(session_id) => SessionCommand::Resume(session_id),
        _ => SessionCommand::Usage,
    }
}

fn is_bare_session_id(value: &str) -> bool {
    !value.starts_with('-')
        && !matches!(
            value,
            "new" | "status" | "list" | "resume" | "clear" | "compact"
        )
}

#[cfg(test)]
mod tests {
    use super::{parse_session_command, SessionCommand};

    const SESSION_ID: &str = "00000000-0000-4000-8000-000000000000";

    #[test]
    fn parses_session_command_grammar_without_fallthrough() {
        let cases = [
            ("", SessionCommand::OpenPicker),
            ("new", SessionCommand::New),
            ("status", SessionCommand::Status),
            ("list", SessionCommand::List { all: false }),
            ("list --all", SessionCommand::List { all: true }),
            ("resume", SessionCommand::OpenPicker),
            (
                "resume 00000000-0000-4000-8000-000000000000",
                SessionCommand::Resume(SESSION_ID),
            ),
            (
                "clear 00000000-0000-4000-8000-000000000000 second-id",
                SessionCommand::Clear(vec![SESSION_ID.to_string(), "second-id".to_string()]),
            ),
            (
                "clear --all",
                SessionCommand::Clear(vec!["--all".to_string()]),
            ),
            ("compact", SessionCommand::Compact(None)),
            ("compact status", SessionCommand::Compact(Some("status"))),
            ("compact cancel", SessionCommand::Compact(Some("cancel"))),
            (SESSION_ID, SessionCommand::Resume(SESSION_ID)),
            ("new extra", SessionCommand::Usage),
            ("status extra", SessionCommand::Usage),
            ("list extra", SessionCommand::Usage),
            ("list --all extra", SessionCommand::Usage),
            ("--all", SessionCommand::Usage),
            ("clear", SessionCommand::Usage),
            ("resume status extra", SessionCommand::Usage),
            ("clear --all extra", SessionCommand::Usage),
            ("clear first --all", SessionCommand::Usage),
            ("compact status extra", SessionCommand::Usage),
            ("-reserved", SessionCommand::Usage),
        ];

        for (arguments, expected) in cases {
            assert_eq!(
                parse_session_command(arguments),
                expected,
                "unexpected parse for {arguments:?}"
            );
        }
    }
}
