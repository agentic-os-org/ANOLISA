use super::loader::{parse_hook_header, parse_timeout};
use super::matcher::matches_command;
use super::*;
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

fn make_input(command: &str, exit_code: i32) -> HookInput {
    HookInput {
        command: command.to_string(),
        cwd: "/tmp".to_string(),
        exit_code,
        duration_ms: 100,
        output_ref: None,
        output_bytes: 0,
        output_preview: String::new(),
    }
}

fn make_matcher(commands: Vec<&str>, patterns: Vec<&str>, trigger: HookTrigger) -> HookMatcher {
    HookMatcher {
        id: "test".to_string(),
        commands: commands.into_iter().map(String::from).collect(),
        command_patterns: patterns.into_iter().map(String::from).collect(),
        command_regex: None,
        min_output_bytes: None,
        exit_codes: None,
        trigger,
    }
}

fn make_block(command: &str) -> CommandBlock {
    CommandBlock {
        id: "b1".to_string(),
        session_id: "s1".to_string(),
        command: command.to_string(),
        origin: Default::default(),
        cwd: "/tmp".to_string(),
        end_cwd: "/tmp".to_string(),
        started_at_ms: 0,
        ended_at_ms: 100,
        duration_ms: 100,
        exit_code: 0,
        status: crate::types::CommandStatus::Completed,
        output: crate::types::OutputRefs {
            terminal_output_ref: None,
            terminal_output_bytes: 0,
        },
        shell_environment_generation: None,
        audit_identity: None,
    }
}

#[cfg(unix)]
fn write_executable_hook(dir_name: &str, file_name: &str, body: &str) -> (PathBuf, PathBuf) {
    use std::os::unix::fs::PermissionsExt;

    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("{dir_name}-{}-{unique}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(file_name);
    fs::write(&path, body).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    (dir, path)
}

#[path = "tests/evaluation.rs"]
mod evaluation;
#[path = "tests/external.rs"]
mod external;
#[path = "tests/loader.rs"]
mod loader;
#[path = "tests/matcher.rs"]
mod matcher;
#[path = "tests/parser.rs"]
mod parser;
#[path = "tests/project.rs"]
mod project;
