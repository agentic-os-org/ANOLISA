//! Exercise the embedded engine without an independently installed RTK.

#[cfg(unix)]
use std::process::Stdio;
use std::process::{Command, Output};

struct Fixture(tempfile::TempDir);

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join(".claude")).unwrap();
        Self(root)
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_tokenless"));
        command
            .current_dir(self.0.path())
            .env("HOME", self.0.path())
            .env("CLAUDE_CONFIG_DIR", self.0.path().join(".claude"))
            .env("XDG_CONFIG_HOME", self.0.path().join("config"))
            .env("XDG_DATA_HOME", self.0.path().join("data"))
            .env("TOKENLESS_DATA_DIR", self.0.path().join("state"))
            .env("TOKENLESS_STATS_ENABLED", "0")
            .env_remove("TOKENLESS_STATS_DB");
        command
    }

    fn rtk(&self, args: &[&str]) -> Output {
        self.command().arg("rtk").args(args).output().unwrap()
    }
}

#[test]
fn versions_distinguish_host_and_engine() {
    let f = Fixture::new();
    let host = f.command().arg("--version").output().unwrap();
    assert!(host.status.success());
    assert_eq!(
        String::from_utf8(host.stdout).unwrap().trim(),
        concat!("tokenless ", env!("CARGO_PKG_VERSION"))
    );
    let engine = f.rtk(&["--version"]);
    assert!(engine.status.success());
    assert_eq!(engine.stdout, b"rtk 0.49.0\n");
}

#[test]
fn rewrite_preserves_four_outcomes_and_quoted_data() {
    let f = Fixture::new();
    for (rule, code) in [("allow", 0), ("ask", 3), ("deny", 2)] {
        std::fs::write(
            f.0.path().join(".claude/settings.json"),
            serde_json::json!({"permissions":{rule:["Bash(git status)"]}}).to_string(),
        )
        .unwrap();
        let result = f.rtk(&["rewrite", "git status"]);
        assert_eq!(result.status.code(), Some(code), "{rule}: {result:?}");
        let separated = f.rtk(&["rewrite", "--", "git status"]);
        assert_eq!(separated.status.code(), result.status.code());
        assert_eq!(separated.stdout, result.stdout);
        assert_eq!(
            result.stdout,
            if code == 2 {
                b"".as_slice()
            } else {
                b"tokenless rtk git status".as_slice()
            }
        );
    }
    let unsupported = f.rtk(&["rewrite", "printf hello"]);
    assert_eq!(unsupported.status.code(), Some(1));
    assert!(unsupported.stdout.is_empty());
    let help = f.rtk(&["rewrite", "--help"]);
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("tokenless rtk rewrite"));
    std::fs::remove_file(f.0.path().join(".claude/settings.json")).unwrap();
    for command in [
        "tokenless rtk git status",
        "'/a path/tokenless' rtk git status",
    ] {
        let result = f.rtk(&["rewrite", command]);
        assert!(result.stdout.is_empty() || String::from_utf8_lossy(&result.stdout) == command);
    }
    let result = f.rtk(&["rewrite", "git log --grep='rtk git status' | head -5"]);
    assert!(matches!(result.status.code(), Some(0 | 3)), "{result:?}");
    let rewritten = String::from_utf8(result.stdout).unwrap();
    assert!(
        rewritten.starts_with("tokenless rtk git log"),
        "{rewritten}"
    );
    assert!(rewritten.contains("'rtk git status'"), "{rewritten}");
}

#[test]
fn rewriting_is_text_only_but_execution_enforces_legacy_hook_integrity() {
    let f = Fixture::new();
    let hooks = f.0.path().join(".claude/hooks");
    std::fs::create_dir(&hooks).unwrap();
    std::fs::write(hooks.join("rtk-rewrite.sh"), "modified hook").unwrap();
    std::fs::write(
        hooks.join(".rtk-hook.sha256"),
        format!("{}  rtk-rewrite.sh\n", "0".repeat(64)),
    )
    .unwrap();
    let rewrite = f.rtk(&["rewrite", "git status"]);
    assert!(matches!(rewrite.status.code(), Some(0 | 3)), "{rewrite:?}");
    for args in [
        vec!["run", "-c", "echo should-not-run"],
        vec!["unknown-native-command"],
    ] {
        let output = f.rtk(&args);
        assert!(!output.status.success(), "{output:?}");
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("standalone upstream RTK build"), "{stderr}");
        assert!(
            stderr.contains("embedded-rtk.md#legacy-hook-integrity"),
            "{stderr}"
        );
        assert!(!stderr.contains("rtk init"), "{stderr}");
        assert!(!stderr.contains("rtk verify"), "{stderr}");
    }
}

#[test]
fn existing_standalone_rtk_calls_are_not_adopted() {
    let f = Fixture::new();
    let output = f.rtk(&["rewrite", "rtk git status"]);
    assert!(matches!(output.status.code(), Some(0 | 3)), "{output:?}");
    assert_eq!(output.stdout, b"rtk git status");
    let proxy = f.rtk(&["proxy"]);
    assert!(!proxy.status.success());
    assert!(String::from_utf8_lossy(&proxy.stderr).contains("Usage: tokenless rtk proxy"));
}

#[test]
#[cfg(unix)]
fn management_names_never_reach_native_fallback() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    for name in [
        "init",
        "hook",
        "hook-audit",
        "gain",
        "cc-economics",
        "discover",
        "session",
        "telemetry",
        "learn",
        "config",
        "trust",
        "untrust",
        "verify",
    ] {
        let program = f.0.path().join(name);
        std::fs::write(&program, "#!/bin/sh\ntouch forbidden-executed\n").unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        for args in [
            vec![name],
            vec![name, "--invalid"],
            vec!["--", name],
            vec!["-v", name, "--invalid"],
        ] {
            let result = f
                .command()
                .arg("rtk")
                .args(&args)
                .env("PATH", f.0.path())
                .output()
                .unwrap();
            assert!(!result.status.success(), "{args:?}: {result:?}");
            assert!(!f.0.path().join("forbidden-executed").exists());
        }
    }
}

#[test]
#[cfg(unix)]
fn proxy_and_fallback_preserve_arguments_stdin_and_failure_status() {
    use std::io::Write;
    use std::os::unix::ffi::OsStringExt;
    let f = Fixture::new();
    for args in [
        vec![
            "proxy",
            "sh",
            "-c",
            "printf '%s' \"$1\"; exit 7",
            "sh",
            "a b",
        ],
        vec!["sh", "-c", "printf '%s' \"$1\"; exit 7", "sh", "a b"],
    ] {
        let result = f.rtk(&args);
        assert_eq!(result.status.code(), Some(7), "{result:?}");
        assert_eq!(result.stdout, b"a b");
    }
    let result = f
        .command()
        .args(["rtk", "/usr/bin/printf", "%s"])
        .arg(std::ffi::OsString::from_vec(vec![b'a', 0xff]))
        .output()
        .unwrap();
    assert!(result.status.success(), "{result:?}");
    assert_eq!(result.stdout, [b'a', 0xff]);
    let mut child = f
        .command()
        .args(["rtk", "proxy", "cat"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"unmodified stdin\n")
        .unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(result.status.success());
    assert_eq!(result.stdout, b"unmodified stdin\n");
}
