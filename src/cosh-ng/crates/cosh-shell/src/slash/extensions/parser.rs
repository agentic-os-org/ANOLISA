#[derive(Debug, PartialEq, Eq)]
pub(super) enum ExtensionCommand {
    List,
    Info {
        name: String,
    },
    Doctor {
        name: Option<String>,
    },
    New {
        path: String,
        template: String,
    },
    Install {
        source: String,
        git_ref: Option<String>,
    },
    Link {
        source: String,
    },
    Update {
        name: String,
    },
    UpdateAll,
    Uninstall {
        name: String,
    },
    Enable {
        name: String,
    },
    Disable {
        name: String,
    },
    SelectSource {
        name: String,
        source: String,
    },
    SettingsList {
        name: String,
        scope: Option<String>,
    },
    SettingsGet {
        name: String,
        key: String,
        scope: Option<String>,
    },
    SettingsSet {
        name: String,
        key: String,
        value: String,
        scope: String,
    },
    SettingsUnset {
        name: String,
        key: String,
        scope: String,
    },
    Reload,
    Operation {
        operation_id: String,
    },
    Consent {
        operation_id: String,
    },
    Cancel {
        operation_id: String,
    },
    Help,
}

pub(super) fn parse(input: &str) -> Result<ExtensionCommand, String> {
    let args = tokenize(input)?;
    let Some(command) = args.first().map(String::as_str) else {
        return Ok(ExtensionCommand::List);
    };
    match command {
        "list" => no_args(&args, ExtensionCommand::List),
        "info" | "detail" => Ok(ExtensionCommand::Info {
            name: one_arg(&args, "info <name>")?,
        }),
        "doctor" => {
            optional_arg(&args, "doctor [name]").map(|name| ExtensionCommand::Doctor { name })
        }
        "new" => parse_new(&args),
        "install" => parse_install(&args),
        "link" => Ok(ExtensionCommand::Link {
            source: one_arg(&args, "link <path>")?,
        }),
        "update" if args.get(1).map(String::as_str) == Some("--all") => {
            no_extra(&args, 2, "update --all")?;
            Ok(ExtensionCommand::UpdateAll)
        }
        "update" => Ok(ExtensionCommand::Update {
            name: one_arg(&args, "update <name>")?,
        }),
        "uninstall" => Ok(ExtensionCommand::Uninstall {
            name: one_arg(&args, "uninstall <name>")?,
        }),
        "enable" => Ok(ExtensionCommand::Enable {
            name: one_arg(&args, "enable <name>")?,
        }),
        "disable" => Ok(ExtensionCommand::Disable {
            name: one_arg(&args, "disable <name>")?,
        }),
        "select-source" => {
            no_extra(&args, 3, "select-source <name> <user|system>")?;
            let name = required(&args, 1, "select-source <name> <user|system>")?;
            let source = required(&args, 2, "select-source <name> <user|system>")?;
            if source != "user" && source != "system" {
                return Err("source must be user or system".to_string());
            }
            Ok(ExtensionCommand::SelectSource { name, source })
        }
        "settings" => parse_settings(&args),
        "reload" => no_args(&args, ExtensionCommand::Reload),
        "operation" => Ok(ExtensionCommand::Operation {
            operation_id: one_arg(&args, "operation <id>")?,
        }),
        "consent" => Ok(ExtensionCommand::Consent {
            operation_id: one_arg(&args, "consent <id>")?,
        }),
        "cancel" => Ok(ExtensionCommand::Cancel {
            operation_id: one_arg(&args, "cancel <id>")?,
        }),
        "help" | "--help" | "-h" => no_args(&args, ExtensionCommand::Help),
        _ => Err(format!("unknown extensions command: {command}")),
    }
}

fn parse_settings(args: &[String]) -> Result<ExtensionCommand, String> {
    let subcommand = required(args, 1, "settings <list|get|set|unset> <extension> ...")?;
    let syntax = match subcommand.as_str() {
        "list" => "settings list <extension> [--scope user|workspace]",
        "get" => "settings get <extension> <key> [--scope user|workspace]",
        "set" => "settings set <extension> <key> <value> [--scope user|workspace]",
        "unset" => "settings unset <extension> <key> [--scope user|workspace]",
        _ => return Err(format!("unknown settings command: {subcommand}")),
    };
    let expected_positionals = match subcommand.as_str() {
        "list" => 1,
        "get" | "unset" => 2,
        "set" => 3,
        _ => unreachable!("settings subcommand validated above"),
    };
    let mut positionals = Vec::new();
    let mut scope = None;
    let mut index = 2;
    let mut options = true;
    while index < args.len() {
        match args[index].as_str() {
            "--" if options => options = false,
            "--scope" if options => {
                index += 1;
                if scope.is_some() {
                    return Err("duplicate option: --scope".to_string());
                }
                let value = required(args, index, syntax)?;
                if value != "user" && value != "workspace" {
                    return Err("scope must be user or workspace".to_string());
                }
                scope = Some(value);
            }
            option if options && option.starts_with('-') => {
                return Err(format!("unknown option: {option}"))
            }
            value => positionals.push(value.to_string()),
        }
        index += 1;
    }
    if positionals.len() != expected_positionals {
        if positionals.len() > expected_positionals {
            return Err(format!(
                "unexpected argument: {}",
                positionals[expected_positionals]
            ));
        }
        return Err(usage(syntax));
    }
    let mut values = positionals.into_iter();
    let name = values.next().expect("validated positional count");
    match subcommand.as_str() {
        "list" => Ok(ExtensionCommand::SettingsList { name, scope }),
        "get" => Ok(ExtensionCommand::SettingsGet {
            name,
            key: values.next().expect("validated positional count"),
            scope,
        }),
        "set" => Ok(ExtensionCommand::SettingsSet {
            name,
            key: values.next().expect("validated positional count"),
            value: values.next().expect("validated positional count"),
            scope: scope.unwrap_or_else(|| "user".to_string()),
        }),
        "unset" => Ok(ExtensionCommand::SettingsUnset {
            name,
            key: values.next().expect("validated positional count"),
            scope: scope.unwrap_or_else(|| "user".to_string()),
        }),
        _ => unreachable!("settings subcommand validated above"),
    }
}

fn parse_new(args: &[String]) -> Result<ExtensionCommand, String> {
    let mut path = None;
    let mut template = "minimal".to_string();
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--template" => {
                index += 1;
                template = required(args, index, "new <path> [--template <name>]")?;
            }
            "--" => {
                index += 1;
                path = Some(required(args, index, "new <path> [--template <name>]")?);
            }
            option if option.starts_with('-') => return Err(format!("unknown option: {option}")),
            value if path.is_none() => path = Some(value.to_string()),
            value => return Err(format!("unexpected argument: {value}")),
        }
        index += 1;
    }
    Ok(ExtensionCommand::New {
        path: path.ok_or_else(|| usage("new <path> [--template <name>]"))?,
        template,
    })
}

fn parse_install(args: &[String]) -> Result<ExtensionCommand, String> {
    let mut source = None;
    let mut git_ref = None;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--ref" => {
                index += 1;
                git_ref = Some(required(args, index, "install <source> [--ref <ref>]")?);
            }
            "--" => {
                index += 1;
                source = Some(required(args, index, "install <source> [--ref <ref>]")?);
            }
            option if option.starts_with('-') => return Err(format!("unknown option: {option}")),
            value if source.is_none() => source = Some(value.to_string()),
            value => return Err(format!("unexpected argument: {value}")),
        }
        index += 1;
    }
    Ok(ExtensionCommand::Install {
        source: source.ok_or_else(|| usage("install <source> [--ref <ref>]"))?,
        git_ref,
    })
}

fn no_args(args: &[String], command: ExtensionCommand) -> Result<ExtensionCommand, String> {
    no_extra(args, 1, args[0].as_str())?;
    Ok(command)
}

fn one_arg(args: &[String], syntax: &str) -> Result<String, String> {
    no_extra(args, 2, syntax)?;
    required(args, 1, syntax)
}

fn optional_arg(args: &[String], _syntax: &str) -> Result<Option<String>, String> {
    if args.len() > 2 {
        Err(format!("unexpected argument: {}", args[2]))
    } else {
        Ok(args.get(1).cloned())
    }
}

fn required(args: &[String], index: usize, syntax: &str) -> Result<String, String> {
    args.get(index).cloned().ok_or_else(|| usage(syntax))
}

fn no_extra(args: &[String], expected: usize, syntax: &str) -> Result<(), String> {
    if args.len() > expected {
        Err(format!("unexpected argument: {}", args[expected]))
    } else if args.len() < expected {
        Err(usage(syntax))
    } else {
        Ok(())
    }
}

fn usage(syntax: &str) -> String {
    format!("usage: /extensions {syntax}")
}

fn tokenize(input: &str) -> Result<Vec<String>, String> {
    let mut tokens = Vec::new();
    let mut token = String::new();
    let mut quote = None;
    let mut escaped = false;
    for ch in input.chars() {
        if escaped {
            token.push(ch);
            escaped = false;
            continue;
        }
        if ch == '\\' && quote != Some('\'') {
            escaped = true;
            continue;
        }
        if let Some(active) = quote {
            if ch == active {
                quote = None;
            } else {
                token.push(ch);
            }
            continue;
        }
        match ch {
            '\'' | '"' => quote = Some(ch),
            value if value.is_whitespace() => {
                if !token.is_empty() {
                    tokens.push(std::mem::take(&mut token));
                }
            }
            value => token.push(value),
        }
    }
    if escaped {
        return Err("trailing escape in extensions command".to_string());
    }
    if quote.is_some() {
        return Err("unterminated quote in extensions command".to_string());
    }
    if !token.is_empty() {
        tokens.push(token);
    }
    Ok(tokens)
}

#[cfg(test)]
mod tests {
    use super::{parse, ExtensionCommand};

    #[test]
    fn parses_quoted_paths_flags_and_double_dash() {
        assert_eq!(
            parse("new \"my extension\" --template mcp").unwrap(),
            ExtensionCommand::New {
                path: "my extension".to_string(),
                template: "mcp".to_string(),
            }
        );
        assert_eq!(
            parse("install --ref main -- -leading-path").unwrap(),
            ExtensionCommand::Install {
                source: "-leading-path".to_string(),
                git_ref: Some("main".to_string()),
            }
        );
    }

    #[test]
    fn detail_is_an_info_alias_and_invalid_input_fails_closed() {
        assert_eq!(
            parse("detail demo").unwrap(),
            ExtensionCommand::Info {
                name: "demo".to_string(),
            }
        );
        assert!(parse("enable demo extra").is_err());
        assert!(parse("new demo --unknown").is_err());
        assert!(parse("install \"unterminated").is_err());
    }

    #[test]
    fn doctor_accepts_an_optional_extension_name() {
        assert_eq!(
            parse("doctor").unwrap(),
            ExtensionCommand::Doctor { name: None }
        );
        assert_eq!(
            parse("doctor demo").unwrap(),
            ExtensionCommand::Doctor {
                name: Some("demo".to_string()),
            }
        );
        assert_eq!(
            parse("doctor demo extra").unwrap_err(),
            "unexpected argument: extra"
        );
    }

    #[test]
    fn parses_settings_quotes_scopes_and_double_dash() {
        assert_eq!(
            parse("settings set example.ops region 'cn hangzhou' --scope workspace").unwrap(),
            ExtensionCommand::SettingsSet {
                name: "example.ops".to_string(),
                key: "region".to_string(),
                value: "cn hangzhou".to_string(),
                scope: "workspace".to_string(),
            }
        );
        assert_eq!(
            parse("settings set example.ops label -- -private").unwrap(),
            ExtensionCommand::SettingsSet {
                name: "example.ops".to_string(),
                key: "label".to_string(),
                value: "-private".to_string(),
                scope: "user".to_string(),
            }
        );
        assert_eq!(
            parse("settings get example.ops region --scope user").unwrap(),
            ExtensionCommand::SettingsGet {
                name: "example.ops".to_string(),
                key: "region".to_string(),
                scope: Some("user".to_string()),
            }
        );
        assert!(parse("settings set example.ops region value extra").is_err());
        assert!(parse("settings list example.ops --scope project").is_err());
    }
}
