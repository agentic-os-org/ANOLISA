use crate::runtime::prelude::{
    visible_slash_commands, ShellEvent, ShellEventKind, SlashCommandSpec,
};

pub(super) fn slash_input(event: &ShellEvent) -> Option<&str> {
    if event.kind != ShellEventKind::UserInputIntercepted {
        return None;
    }
    if event.component.as_deref() != Some("slash") {
        return None;
    }
    event.input.as_deref()
}

pub(super) enum SlashCommand<'a> {
    Noop,
    Help,
    Agent,
    Auth,
    Audit(&'a str),
    Hooks(Option<&'a str>, Option<&'a str>, Option<&'a str>),
    Mode(Option<&'a str>, Option<&'a str>, Option<&'a str>),
    Config(Option<&'a str>, Option<&'a str>),
    Debug(Option<&'a str>),
    #[allow(dead_code)]
    Info(SlashInfoCommand),
    Health,
    Status,
    Stats(&'a str),
    Removed(RemovedCommand<'a>),
    Hint(&'a str),
    Unknown(&'a str),
    Extensions(&'a str),
    Skills(Option<&'a str>, Option<&'a str>),
    Mcp(Option<&'a str>, Option<&'a str>, Option<&'a str>),
    Session(&'a str),
    Task(&'a str),
    Recommendations(Option<&'a str>, Option<&'a str>, Option<&'a str>),
}

/// Explains why a parser-owned slash command could not be decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SlashParseError {
    /// Single- and double-quoted arguments are intentionally unsupported.
    QuotedArgumentsUnsupported,
}

impl<'a> SlashCommand<'a> {
    pub(super) fn parse(input: &'a str) -> Result<Option<Self>, SlashParseError> {
        let mut parts = input.split_whitespace();
        let Some(token) = parts.next() else {
            return Ok(None);
        };
        // Most parser-owned commands split arguments on whitespace, so quotes
        // would silently produce wrong tokens. `/extensions` is the exception:
        // it forwards its raw argument string to a quote-aware tokenizer.
        let supports_quoted_arguments = matches!(token, "/extensions" | "/task");
        if parser_owned_command(token) && !supports_quoted_arguments && input.contains(['\'', '"'])
        {
            return Err(SlashParseError::QuotedArgumentsUnsupported);
        }
        Ok(match token {
            "/help" => Some(Self::Help),
            "/agent" => Some(Self::Agent),
            "/auth" => Some(Self::Auth),
            "/hooks" => {
                let sub = parts.next();
                let arg = parts.next();
                let extra = parts.next();
                Some(Self::Hooks(sub, arg, extra))
            }
            "/mode" => {
                let first = parts.next();
                let second = parts.next();
                let third = parts.next();
                Some(Self::Mode(first, second, third))
            }
            "/approval-mode" => Some(Self::Removed(RemovedCommand::ApprovalMode(parts.next()))),
            "/allow" | "/approve" | "/deny" => {
                Some(Self::Removed(RemovedCommand::ApprovalDecision(token)))
            }
            "/answer" => Some(Self::Removed(RemovedCommand::QuestionAnswer)),
            "/audit" => Some(Self::Audit(
                input.strip_prefix("/audit").unwrap_or_default().trim(),
            )),
            "/config" => {
                let sub = parts.next();
                let value = parts.next();
                Some(Self::Config(sub, value))
            }
            "/debug" => Some(Self::Debug(parts.next())),
            "/health" => Some(Self::Health),
            "/status" | "/about" => Some(Self::Status),
            "/stats" => Some(Self::Stats(
                input.strip_prefix("/stats").unwrap_or_default().trim(),
            )),
            "/extensions" => {
                let args = input
                    .strip_prefix("/extensions")
                    .expect("matched slash command prefix")
                    .trim();
                Some(Self::Extensions(args))
            }
            "/skills" => {
                let sub = parts.next();
                let arg = parts.next();
                Some(Self::Skills(sub, arg))
            }
            "/mcp" => {
                let sub = parts.next();
                let arg = parts.next();
                let extra = parts.next();
                Some(Self::Mcp(sub, arg, extra))
            }
            "/session" => Some(Self::Session(
                input.strip_prefix("/session").unwrap_or_default().trim(),
            )),
            "/task" => Some(Self::Task(
                input.strip_prefix("/task").unwrap_or_default().trim(),
            )),
            // Compatibility alias: `/new` routes to the same session parser and
            // dispatch as `/session new`. Stripping only the leading slash
            // keeps the `new` keyword (and any extra tokens) in the arguments,
            // so `/new extra` renders usage exactly like `/session new extra`.
            "/new" => Some(Self::Session(
                input.strip_prefix('/').unwrap_or_default().trim(),
            )),
            "/resume" => Some(Self::Session(
                input.strip_prefix("/resume").unwrap_or_default().trim(),
            )),
            "/recommendations" => Some(Self::Recommendations(
                parts.next(),
                parts.next(),
                parts.next(),
            )),
            "/cancel" | "/clear" | "/copy" | "/details" | "/explain" | "/select"
            | "/send-to-shell" | "/shell" => None,
            "/" => Some(Self::Noop),
            token if token.starts_with('/') => {
                if slash_command_hints(token).is_empty() {
                    Some(Self::Unknown(token))
                } else {
                    Some(Self::Hint(token))
                }
            }
            _ => None,
        })
    }
}

fn parser_owned_command(token: &str) -> bool {
    matches!(
        token,
        "/help"
            | "/agent"
            | "/auth"
            | "/hooks"
            | "/mode"
            | "/approval-mode"
            | "/allow"
            | "/approve"
            | "/deny"
            | "/answer"
            | "/audit"
            | "/config"
            | "/debug"
            | "/health"
            | "/status"
            | "/about"
            | "/stats"
            | "/extensions"
            | "/skills"
            | "/mcp"
            | "/session"
            | "/task"
            | "/new"
            | "/resume"
            | "/recommendations"
            | "/"
    )
}

#[derive(Debug, Clone, Copy)]
pub(super) enum SlashInfoCommand {
    #[allow(dead_code)]
    Audit,
    Config,
}

pub(super) enum RemovedCommand<'a> {
    ApprovalMode(Option<&'a str>),
    ApprovalDecision(&'a str),
    QuestionAnswer,
}

pub(super) fn slash_command_hints(prefix: &str) -> Vec<&'static SlashCommandSpec> {
    visible_slash_commands()
        .filter(|hint| prefix == "/" || hint.name.starts_with(prefix))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{RemovedCommand, SlashCommand, SlashCommandSpec, SlashParseError};

    #[test]
    fn removed_draft_alias_is_unknown() {
        assert!(matches!(
            SlashCommand::parse("/draft"),
            Ok(Some(SlashCommand::Unknown("/draft")))
        ));
        assert!(matches!(
            SlashCommand::parse("/agent"),
            Ok(Some(SlashCommand::Agent))
        ));
    }

    #[test]
    fn removed_decision_commands_parse_as_removed_not_unknown() {
        for command in ["/allow 1", "/approve 1", "/deny 1"] {
            match SlashCommand::parse(command) {
                Ok(Some(SlashCommand::Removed(RemovedCommand::ApprovalDecision(token)))) => {
                    assert_eq!(token, command.split_whitespace().next().unwrap());
                }
                _ => panic!("{command} did not parse as removed approval decision"),
            }
        }

        match SlashCommand::parse("/answer yes") {
            Ok(Some(SlashCommand::Removed(RemovedCommand::QuestionAnswer))) => {}
            _ => panic!("/answer did not parse as removed question answer"),
        }
    }

    #[test]
    fn session_commands_and_resume_alias_share_parser_path() {
        match SlashCommand::parse("/session resume abc") {
            Ok(Some(SlashCommand::Session(arguments))) => assert_eq!(arguments, "resume abc"),
            _ => panic!("/session did not parse as a session command"),
        }
        match SlashCommand::parse("/resume abc") {
            Ok(Some(SlashCommand::Session(arguments))) => assert_eq!(arguments, "abc"),
            _ => panic!("/resume did not parse as a session command"),
        }
    }

    #[test]
    fn new_alias_and_session_new_share_the_same_session_parser_path() {
        for command in ["/session new", "/new"] {
            match SlashCommand::parse(command) {
                Ok(Some(SlashCommand::Session(arguments))) => assert_eq!(arguments, "new"),
                _ => panic!("{command} did not parse as the session `new` path"),
            }
        }
        // Extra tokens must survive so the session grammar can render usage.
        for command in ["/session new extra", "/new extra"] {
            match SlashCommand::parse(command) {
                Ok(Some(SlashCommand::Session(arguments))) => assert_eq!(arguments, "new extra"),
                _ => panic!("{command} dropped its extra argument"),
            }
        }
    }

    #[test]
    fn recommendations_preserves_subcommand_and_rejectable_extra_arguments() {
        match SlashCommand::parse("/recommendations on unexpected extra") {
            Ok(Some(SlashCommand::Recommendations(sub, arg, extra))) => {
                assert_eq!(sub, Some("on"));
                assert_eq!(arg, Some("unexpected"));
                assert_eq!(extra, Some("extra"));
            }
            _ => panic!("recommendations command did not parse"),
        }
    }

    #[test]
    fn status_about_and_stats_parse_as_read_only_commands() {
        for command in ["/status", "/about"] {
            assert!(
                matches!(SlashCommand::parse(command), Ok(Some(SlashCommand::Status))),
                "{command}"
            );
        }
        match SlashCommand::parse("/stats tools") {
            Ok(Some(SlashCommand::Stats(arguments))) => assert_eq!(arguments, "tools"),
            _ => panic!("/stats tools did not parse as stats"),
        }
    }

    #[test]
    fn quoted_arguments_are_rejected_for_parser_owned_commands() {
        for command in [
            "/mode approval \"trust confirm\"",
            "/mode approval 'trust confirm'",
            "/config language \"en US\"",
            "/health \"quick\"",
            "/stats \"model\"",
            "/recommendations \"on\"",
            "/mcp connect \"my server\"",
            "/mcp inspect 'my server'",
        ] {
            assert!(
                matches!(
                    SlashCommand::parse(command),
                    Err(SlashParseError::QuotedArgumentsUnsupported)
                ),
                "{command}"
            );
        }
    }

    #[test]
    fn task_goal_preserves_quoted_natural_language() {
        match SlashCommand::parse(r#"/task update the "serde" dependency"#) {
            Ok(Some(SlashCommand::Task(goal))) => {
                assert_eq!(goal, r#"update the "serde" dependency"#)
            }
            _ => panic!("quoted Task goal did not parse"),
        }
    }

    #[test]
    fn extensions_forwards_quoted_arguments_to_its_own_tokenizer() {
        match SlashCommand::parse(r#"/extensions new "my extension" --template mcp"#) {
            Ok(Some(SlashCommand::Extensions(arguments))) => {
                assert_eq!(arguments, r#"new "my extension" --template mcp"#);
            }
            _ => panic!("quoted /extensions argument was not forwarded"),
        }
    }

    #[test]
    fn unquoted_arguments_keep_the_existing_token_contract() {
        match SlashCommand::parse("/mode approval trust confirm") {
            Ok(Some(SlashCommand::Mode(Some("approval"), Some("trust"), Some("confirm")))) => {}
            _ => panic!("unquoted trust confirmation did not parse"),
        }

        match SlashCommand::parse("/mode routing shell-only") {
            Ok(Some(SlashCommand::Mode(Some("routing"), Some("shell-only"), None))) => {}
            _ => panic!("routing mode did not parse"),
        }
    }

    #[test]
    fn shell_commands_with_quotes_are_not_slash_parse_errors() {
        assert!(matches!(
            SlashCommand::parse("printf '\"hello world\"\\n'"),
            Ok(None)
        ));
    }

    #[test]
    fn hidden_and_contextual_commands_are_not_public_hints() {
        assert!(slash_hints("/co").iter().any(|hint| hint.name == "/config"));
        for prefix in ["/ag", "/ca", "/de", "/au", "/se", "/co", "/send", "/debug"] {
            let hints = slash_hints(prefix);
            assert!(
                hints.iter().all(|hint| matches!(
                    hint.name,
                    "/config"
                        | "/session"
                        | "/mode"
                        | "/hooks"
                        | "/extensions"
                        | "/skills"
                        | "/auth"
                        | "/agent"
                )),
                "{prefix} returned non-public hints: {:?}",
                hints.iter().map(|hint| hint.name).collect::<Vec<_>>()
            );
        }
        // /au matches the public /auth but must never surface the contextual /audit
        assert!(slash_hints("/au").iter().any(|hint| hint.name == "/auth"));
        assert!(slash_hints("/au").iter().all(|hint| hint.name != "/audit"));
        // /ex and /skill now match public commands
        assert!(slash_hints("/ex")
            .iter()
            .any(|hint| hint.name == "/extensions"));
        assert!(slash_hints("/skill")
            .iter()
            .any(|hint| hint.name == "/skills"));
    }

    fn slash_hints(prefix: &str) -> Vec<&'static SlashCommandSpec> {
        super::slash_command_hints(prefix)
    }
}
