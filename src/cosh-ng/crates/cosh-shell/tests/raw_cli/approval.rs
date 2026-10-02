use super::*;

fn run_raw_cli_ask_with_delayed_input(chunks: Vec<(Vec<u8>, Duration)>) -> String {
    run_raw_cli_ask_with_args_and_delayed_input(&[], chunks)
}

fn run_raw_cli_ask_with_args_and_delayed_input(
    args: &[&str],
    chunks: Vec<(Vec<u8>, Duration)>,
) -> String {
    run_raw_cli_ask_with_args_env_and_delayed_input(args, &[], chunks)
}

fn run_raw_cli_ask_with_args_env_and_delayed_input(
    args: &[&str],
    extra_env: &[(&str, &str)],
    chunks: Vec<(Vec<u8>, Duration)>,
) -> String {
    let home = temp_shell_home("approval-cards");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    write_executable(&bin_dir.join("git"), "#!/bin/sh\nexit 0\n");
    write_cosh_config(
        &home,
        r#"[shell]
readonly_disabled = ["git status", "pwd"]"#,
    );
    let home_str = home.to_string_lossy().to_string();
    let existing_path = std::env::var("PATH").unwrap_or_default();
    let path = format!("{}:{existing_path}", bin_dir.display());
    let mut env = vec![("HOME", home_str.as_str()), ("PATH", path.as_str())];
    env.extend_from_slice(extra_env);
    run_raw_cli_with_args_env_and_delayed_input("fake", args, &env, chunks)
}

fn run_raw_cli_ask_with_args_and_marker_input(args: &[&str], steps: &[(&str, &[u8])]) -> String {
    let home = temp_shell_home("approval-marker-cards");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    write_executable(&bin_dir.join("git"), "#!/bin/sh\nexit 0\n");
    write_cosh_config(
        &home,
        r#"[shell]
readonly_disabled = ["git status", "pwd"]"#,
    );
    let home_str = home.to_string_lossy().to_string();
    let existing_path = std::env::var("PATH").unwrap_or_default();
    let path = format!("{}:{existing_path}", bin_dir.display());
    run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        args,
        &[("HOME", home_str.as_str()), ("PATH", path.as_str())],
        Path::new(env!("CARGO_MANIFEST_DIR")),
        steps,
    )
}

#[path = "approval/details.rs"]
mod details;
#[path = "approval/foreground.rs"]
mod foreground;
#[path = "approval/input.rs"]
mod input;
#[path = "approval/slash_removed.rs"]
mod slash_removed;
#[path = "approval/status.rs"]
mod status;
