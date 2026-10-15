//! Translate Tokenless rewrites into a fresh native attempt, never an authorization.

use serde_json::{json, Value};
use std::path::Path;

pub(super) fn supported(tool: &Value) -> bool {
    matches!(
        tool["name"]
            .as_str()
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str(),
        "bash" | "exec" | "terminal" | "shell" | "run_shell_command" | "execute_shell_command"
    ) && tool["input"]["command"]
        .as_str()
        .is_some_and(|s| !s.is_empty())
}

pub(super) fn reply(request: &Value, result: &Value) -> Result<Value, &'static str> {
    let original = &request["event"]["tool"]["input"];
    let neutral = json!({"input_digest":request["input_digest"],"effects":[]});
    match result["action"].as_str() {
        Some("passthrough")
            if result["arguments"] == *original && result["output_optimization"] == "none" =>
        {
            Ok(neutral)
        }
        Some("block_and_suggest") if result["output_optimization"] == "rtk" => {
            let args = &result["arguments"];
            let _command = args["command"]
                .as_str()
                .filter(|s| !s.is_empty())
                .ok_or("invalid_pretool_result")?;
            let mut compared = args.clone();
            compared["command"] = original["command"].clone();
            if compared != *original || args == original {
                return Err("invalid_pretool_result");
            }
            // A new model tool call must traverse every before Hook and the
            // native permission path. This text never changes or executes input.
            Ok(
                json!({"input_digest":request["input_digest"],"effects":[{"type":"block","reason_code":"tokenless_retry","text":format!("[tokenless:rewrite] Retry this tool with arguments: {}", serde_json::to_string(args).map_err(|_| "invalid_pretool_result")?)}]}),
            )
        }
        _ => Err("invalid_pretool_result"),
    }
}

// Like the Tokenless Hermes integration, infer ownership from executed input,
// not from a blocked call ID (the retry has a new ID). Restrict recognition to
// the attributed standalone wrapper; compound output remains eligible for PostTool.
pub(super) fn optimization(event: &Value, data: &Path) -> &'static str {
    if !supported(&event["tool"]) {
        return "none";
    }
    let command = event["tool"]["input"]["command"].as_str().unwrap_or("");
    if !literal_command(command) {
        return "none";
    }
    let Some(words) = shlex::split(command) else {
        return "none";
    };
    let mut words = words.as_slice();
    while !words.is_empty() {
        if words.len() >= 7
            && words[0] == "env"
            && words[1]
                == format!(
                    "TOKENLESS_AGENT_ID={}",
                    event["agent"]["adapter"].as_str().unwrap_or("")
                )
            && words[2]
                == format!(
                    "TOKENLESS_SESSION_ID={}",
                    event["session_id"].as_str().unwrap_or("")
                )
            && words[3].starts_with("TOKENLESS_TOOL_USE_ID=")
            && data_directory_matches(&words[4], data)
            && Path::new(&words[5]).is_absolute()
            && Path::new(&words[5])
                .file_name()
                .is_some_and(|name| name == "rtk")
        {
            return "rtk";
        }
        // Only these literal forms forward the remaining command. Unknown wrappers
        // and flag-bearing forms need their own execution contract, not a token scan.
        words = match words {
            [program, rest @ ..]
                if matches!(
                    program.as_str(),
                    "env" | "sudo" | "noglob" | "command" | "exec"
                ) || environment_assignment(program) =>
            {
                rest
            }
            [program, action, separator, rest @ ..]
                if program == "shadowenv" && action == "exec" && separator == "--" =>
            {
                rest
            }
            _ => return "none",
        };
    }
    "none"
}

fn environment_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        let mut bytes = name.bytes();
        bytes
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == b'_')
            && bytes.all(|c| c.is_ascii_alphanumeric() || c == b'_')
    })
}

// shlex decodes words but does not distinguish shell operators from quoted
// literals. Reject active syntax before decoding, including substitutions in
// double quotes; a quoted or escaped metacharacter remains an ordinary argument.
fn literal_command(command: &str) -> bool {
    let mut quote = None;
    let mut chars = command.chars();
    while let Some(ch) = chars.next() {
        match (quote, ch) {
            (Some('\''), '\'') | (Some('"'), '"') => quote = None,
            (Some('\''), _) => {}
            (_, '\\') => {
                if chars.next().is_none() {
                    return false;
                }
            }
            (_, '$' | '`') => return false,
            (None, '\'' | '"') => quote = Some(ch),
            (None, ';' | '&' | '|' | '\n' | '\r' | '<' | '>' | '(' | ')' | '#') => {
                return false;
            }
            _ => {}
        }
    }
    quote.is_none()
}

// Tokenless canonicalizes the existing ancestor even when the state directory
// does not exist yet. Compare the same path identity without creating state.
fn data_directory_matches(assignment: &str, data: &Path) -> bool {
    let Some(actual) = assignment.strip_prefix("TOKENLESS_DATA_DIR=") else {
        return false;
    };
    if !data.is_absolute()
        || data
            .components()
            .any(|c| c == std::path::Component::ParentDir)
    {
        return false;
    }
    for ancestor in data.ancestors() {
        match ancestor.symlink_metadata() {
            Ok(_) => {
                return ancestor.is_dir()
                    && ancestor.canonicalize().ok().is_some_and(|root| {
                        data.strip_prefix(ancestor)
                            .is_ok_and(|suffix| root.join(suffix) == Path::new(actual))
                    });
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return false,
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn optimization_uses_executed_attribution_without_granting_authority() {
        let prefix = "env TOKENLESS_AGENT_ID=hermes TOKENLESS_SESSION_ID='session one' TOKENLESS_TOOL_USE_ID=old-call TOKENLESS_DATA_DIR='/state space' /opt/rtk git status";
        let mut event = json!({"agent":{"adapter":"hermes"},"session_id":"session one","tool":{"name":"terminal","call_id":"new-call","input":{"command":prefix}}});
        assert_eq!(optimization(&event, Path::new("/state space")), "rtk");
        for command in [
            "rtk git status".to_owned(),
            format!("echo {prefix}"),
            format!("{prefix} && echo other"),
            prefix.replace("AGENT_ID=hermes", "AGENT_ID=qoder"),
            prefix.replace("'session one'", "other"),
            prefix.replace("'/state space'", "/other"),
            prefix.replace("/opt/rtk", "rtk"),
            prefix.replace("/opt/rtk", "/opt/rtk-lookalike"),
            "'unclosed".into(),
        ] {
            event["tool"]["input"]["command"] = json!(command);
            assert_eq!(
                optimization(&event, Path::new("/state space")),
                "none",
                "{command}"
            );
        }
    }

    #[test]
    fn quoted_shell_literals_preserve_ownership_but_shell_operations_do_not() {
        let prefix = "env TOKENLESS_AGENT_ID=hermes TOKENLESS_SESSION_ID='session|one' TOKENLESS_TOOL_USE_ID=old-call TOKENLESS_DATA_DIR='/state space' /opt/rtk rg";
        let mut event = json!({"agent":{"adapter":"hermes"},"session_id":"session|one","tool":{"name":"terminal","input":{"command":""}}});
        for (argument, expected) in [
            ("'foo|bar' src", "rtk"),
            (r#""foo|bar" src"#, "rtk"),
            (r"foo\|bar src", "rtk"),
            ("'a;$HOME`id`<>\n&b' src", "rtk"),
            (r#""a\"|b" src"#, "rtk"),
            (r#""\$HOME\`id\`" src"#, "rtk"),
            (r"'it'\''s|literal' src", "rtk"),
            ("foo | cat", "none"),
            ("foo && echo other", "none"),
            ("foo; echo other", "none"),
            ("foo\necho other", "none"),
            ("foo > output", "none"),
            ("foo < input", "none"),
            ("\"$(id)\"", "none"),
            ("\"$HOME\"", "none"),
            ("`id`", "none"),
            ("<(cat input)", "none"),
            ("'unclosed", "none"),
            ("foo\\", "none"),
        ] {
            event["tool"]["input"]["command"] = json!(format!("{prefix} {argument}"));
            assert_eq!(
                optimization(&event, Path::new("/state space")),
                expected,
                "{argument}"
            );
        }
    }

    #[test]
    fn ownership_matches_canonical_state_paths_without_creating_them() {
        let build = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/tokenless-validation");
        std::fs::create_dir_all(&build).unwrap();
        let root = tempfile::tempdir_in(build.canonicalize().unwrap()).unwrap();
        let real = root.path().join("real");
        let link = root.path().join("link");
        std::fs::create_dir(&real).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        for suffix in ["", "not-created/nested"] {
            let data = link.join(suffix);
            let canonical = real.canonicalize().unwrap().join(suffix);
            let mut event = json!({"agent":{"adapter":"hermes"},"session_id":"session","tool":{"name":"terminal","input":{"command":format!("env TOKENLESS_AGENT_ID=hermes TOKENLESS_SESSION_ID=session TOKENLESS_TOOL_USE_ID=old TOKENLESS_DATA_DIR={} /opt/rtk git status", shlex::try_quote(canonical.to_str().unwrap()).unwrap())}}});
            assert_eq!(optimization(&event, &data), "rtk");
            event["tool"]["input"]["command"] = json!(event["tool"]["input"]["command"]
                .as_str()
                .unwrap()
                .replace("TOKENLESS_DATA_DIR=", "TOKENLESS_DATA_DIR=/other"));
            assert_eq!(optimization(&event, &data), "none");
        }
        assert!(!real.join("not-created").exists());
    }

    #[test]
    fn transparent_execution_prefixes_preserve_ownership() {
        let wrapper = "env TOKENLESS_AGENT_ID=hermes TOKENLESS_SESSION_ID=session TOKENLESS_TOOL_USE_ID=old TOKENLESS_DATA_DIR=/state /opt/rtk git status";
        let mut event = json!({"agent":{"adapter":"hermes"},"session_id":"session","tool":{"name":"terminal","input":{"command":""}}});
        for (prefix, expected) in [
            ("sudo ", "rtk"),
            ("sudo noglob ", "rtk"),
            ("RUST_BACKTRACE=1 ", "rtk"),
            ("env LANG=C ", "rtk"),
            ("env TOKENLESS_AGENT_ID=other sudo ", "rtk"),
            ("command ", "rtk"),
            ("exec ", "rtk"),
            ("shadowenv exec -- ", "rtk"),
            ("echo ", "none"),
            ("printf '%s' ", "none"),
            ("sudo -l ", "none"),
            ("command -v ", "none"),
            ("shadowenv other -- ", "none"),
            ("custom-wrapper ", "none"),
        ] {
            event["tool"]["input"]["command"] = json!(format!("{prefix}{wrapper}"));
            assert_eq!(
                optimization(&event, Path::new("/state")),
                expected,
                "{prefix}"
            );
        }
        event["tool"]["input"]["command"] = json!(format!("sudo {wrapper} && echo other"));
        assert_eq!(optimization(&event, Path::new("/state")), "none");
    }
}
