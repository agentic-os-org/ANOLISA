//! Pipe and PTY presentation never changes package work or native protocol flags.

use super::{output::invoke, *};

#[test]
fn quiet_keeps_work_and_errors_while_verbose_separates_scope_details() {
    let f = Fixture::new();
    let output = invoke(f.install().arg("--quiet"));
    assert!(output.status.success());
    assert!(output.stdout.is_empty() && output.stderr.is_empty());
    assert!(f.prefix.join(".aw-packages/aw-core.json").is_file());
    let output = invoke(f.install().arg("--quiet"));
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).starts_with("aw:"));

    let output = invoke(f.init().arg("--verbose"));
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).starts_with("Created "));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "Configuration was created offline; no service reload was requested.\n"
    );
    assert!(!f.state.exists());
    let original = fs::read(&f.config).unwrap();
    let inode = fs::metadata(&f.config).unwrap().ino();
    for action in ["remove-provider", "remove-hook"] {
        let args = if action == "remove-provider" {
            vec!["--name", "absent"]
        } else {
            vec!["--event", "absent", "--id", "absent"]
        };
        let output = invoke(f.edit(action, &args).arg("--verbose"));
        assert!(output.status.success());
        assert_eq!(output.stdout, b"Configuration unchanged\n");
        assert_eq!(
            output.stderr,
            b"Configuration bytes and inode were retained; no runtime reload was requested.\n"
        );
        assert_eq!(fs::read(&f.config).unwrap(), original);
        assert_eq!(fs::metadata(&f.config).unwrap().ino(), inode);
    }
    let output = invoke(
        f.command()
            .args(["uninstall", "--yes", "--prefix"])
            .arg(&f.prefix)
            .arg("--quiet"),
    );
    assert!(output.status.success());
    assert!(output.stdout.is_empty() && output.stderr.is_empty());
    assert!(!f.prefix.join("bin/aw").exists());
    assert_eq!(fs::read(&f.config).unwrap(), original);
}

#[test]
fn output_options_are_strict_local_and_never_approval_flags() {
    let f = Fixture::new();
    for options in [
        vec!["--quiet", "--verbose"],
        vec!["--quiet", "--quiet"],
        vec!["--json", "--quiet"],
        vec!["--quiet", "--json"],
        vec!["--json", "--verbose"],
        vec!["--verbose", "--json"],
        vec!["--json", "--color", "never"],
        vec!["--color", "never", "--json"],
        vec!["--color", "invalid"],
        vec!["--color", "always", "--color", "never"],
        vec!["--yes"],
    ] {
        let output = invoke(f.install().args(options));
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert!(!f.prefix.exists());
    }
    for command in [
        vec!["install", "--config", "aw.yaml", "--agent", "qoder"],
        vec![
            "agent", "connect", "--config", "aw.yaml", "--agent", "qoder",
        ],
        vec!["run", "--config", "aw.yaml", "--agent", "qoder"],
        vec!["config", "show", "--config", "/missing"],
        vec!["config", "validate", "--config", "/missing"],
        vec!["service", "status", "--socket", "/missing"],
        vec!["policy"],
    ] {
        for flag in [vec!["--quiet"], vec!["--verbose"], vec!["--color", "never"]] {
            let output = invoke(f.command().args(&command).args(flag));
            assert_eq!(output.status.code(), Some(1));
            assert!(output.stdout.is_empty());
        }
    }
    assert!(!f.config.exists());
}

#[test]
fn piped_management_metadata_escapes_terminal_and_bidi_controls() {
    let mut f = Fixture::new();
    f.prefix = f.root.join("prefix\n\r\u{1b}[2J\u{85}\u{202e}");
    let output = invoke(&f.install());
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("prefix\\n\\r\\u{1b}[2J\\u{85}\\u{202e}"));
    assert_eq!(stdout.lines().count(), 1);
    assert!(!stdout.contains('\u{1b}') && !stdout.contains('\u{202e}'));
    fs::write(f.prefix.join("bin/aw"), b"modified").unwrap();
    let output = invoke(
        f.command()
            .args(["uninstall", "--yes", "--prefix"])
            .arg(&f.prefix),
    );
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("prefix\\n\\r\\u{1b}[2J\\u{85}\\u{202e}"));
    assert!(!stderr.contains('\u{1b}') && !stderr.contains('\u{202e}'));
}
