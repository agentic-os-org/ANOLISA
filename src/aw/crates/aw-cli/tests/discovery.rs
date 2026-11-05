//! Public command discovery is generated without reading configuration or runtime state.
#![cfg(target_os = "linux")]

use std::process::{Command, Stdio};

fn invoke(arguments: &[&str], directory: &std::path::Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_aw"))
        .args(arguments)
        .current_dir(directory)
        .env("XDG_RUNTIME_DIR", directory.join("absent-runtime"))
        .output()
        .unwrap()
}

#[test]
fn metadata_and_human_inventory_describe_public_syntax_only() {
    let directory = tempfile::tempdir().unwrap();
    let output = invoke(&["commands", "--json"], directory.path());
    assert!(output.status.success(), "{:?}", output);
    assert!(output.stderr.is_empty());
    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(metadata["format"], 1);
    assert_eq!(metadata["name"], "aw");
    assert_eq!(metadata["version"], env!("CARGO_PKG_VERSION"));
    let commands = metadata["commands"].as_array().unwrap();
    let names: Vec<_> = commands
        .iter()
        .map(|entry| entry["command"].as_str().unwrap())
        .collect();
    for public in [
        "aw",
        "aw completion",
        "aw commands",
        "aw config validate",
        "aw service stop",
        "aw list",
        "aw doctor",
    ] {
        assert!(names.contains(&public), "missing {public}");
    }
    for hidden in [
        "aw hook",
        "aw bridge",
        "aw serve",
        "aw status",
        "aw stop",
        "aw validate",
    ] {
        assert!(!names.contains(&hidden), "exposed {hidden}");
    }
    for absent in ["aw provider", "aw migrate", "aw service start"] {
        assert!(!names.contains(&absent));
    }
    let install = commands
        .iter()
        .find(|entry| entry["command"] == "aw install")
        .unwrap();
    let options = install["arguments"].as_array().unwrap();
    let bundle = options
        .iter()
        .find(|argument| argument["long"] == "bundle")
        .unwrap();
    assert_eq!(bundle["takes_values"], true);
    assert_eq!(bundle["value_names"], serde_json::json!(["ABS_DIR"]));
    assert_eq!(bundle["required"], false);
    let human = invoke(&["commands"], directory.path());
    assert!(human.status.success() && human.stderr.is_empty());
    let text = String::from_utf8(human.stdout).unwrap();
    assert_eq!(text.lines().count(), names.len());
    for name in names {
        assert!(text
            .lines()
            .any(|line| line.starts_with(&format!("{name}  "))));
    }
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
}

#[test]
fn every_shell_uses_public_tree_and_does_not_install_a_script() {
    let directory = tempfile::tempdir().unwrap();
    for shell in ["bash", "zsh", "fish", "powershell", "elvish"] {
        let output = invoke(&["completion", shell], directory.path());
        assert!(output.status.success(), "{shell}: {:?}", output);
        assert!(output.stderr.is_empty());
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(
            text.contains("config") && text.contains("completion"),
            "{shell}"
        );
        assert!(text.contains("prefix"), "{shell}");
        for hidden in [
            "aw__hook",
            "aw__bridge",
            "aw,hook",
            "aw,bridge",
            "--adapter",
            "--binding",
        ] {
            assert!(!text.contains(hidden), "{shell}: exposed {hidden}");
        }
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    }
}

#[test]
fn discovery_rejects_unsupported_options_and_shells_before_work() {
    let directory = tempfile::tempdir().unwrap();
    for arguments in [
        vec!["completion", "tcsh"],
        vec!["completion", "bash", "extra"],
        vec!["commands", "--all"],
        vec!["commands", "--json", "--json"],
    ] {
        let output = invoke(&arguments, directory.path());
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty() && !output.stderr.is_empty());
    }
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
}

#[test]
fn closed_stdout_fails_without_a_generator_panic() {
    for arguments in [vec!["completion", "bash"], vec!["commands", "--json"]] {
        let mut child = Command::new(env!("CARGO_BIN_EXE_aw"))
            .args(arguments)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        drop(child.stdout.take());
        let output = child.wait_with_output().unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(!String::from_utf8_lossy(&output.stderr).contains("panicked"));
    }
}
