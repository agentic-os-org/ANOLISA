//! Detect actual PATH executables without requiring an external lookup tool.

#![cfg(unix)]

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;
use std::process::Command;

use skillfs_core::env::EnvironmentProfile;
use skillfs_core::transform::TransformPipeline;

fn executable(path: &Path) {
    std::fs::write(path, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn probe(case: &str) {
    if std::env::var("SKILLFS_PATH_PROBE").as_deref() != Ok(case) {
        let tmp = tempfile::tempdir().unwrap();
        let first = tmp.path().join("first");
        let second = tmp.path().join(OsString::from_vec(b"second-\xff".to_vec()));
        std::fs::create_dir(&first).unwrap();
        std::fs::create_dir(&second).unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap());
        child
            .env_clear()
            .env("SKILLFS_PATH_PROBE", case)
            .current_dir(&first);
        let path = match case {
            "detect_and_compile_without_which" => {
                executable(&first.join("uv"));
                symlink("/bin/sh", first.join("sh")).unwrap();
                first.as_os_str().to_owned()
            }
            "skip_unusable_candidate_before_executable" => {
                std::fs::write(first.join("uv"), "not executable").unwrap();
                executable(&second.join("uv"));
                std::env::join_paths([&first, &second]).unwrap()
            }
            "follow_executable_symlinks" => {
                executable(&second.join("real-uv"));
                symlink(second.join("real-uv"), first.join("uv")).unwrap();
                first.as_os_str().to_owned()
            }
            "empty_path_entry_searches_current_directory" => {
                executable(&first.join("uv"));
                OsString::new()
            }
            "relative_path_entry_is_supported" => {
                executable(&first.join("uv"));
                OsString::from(".")
            }
            "ignore_nonexecutables_directories_and_broken_links" => {
                std::fs::write(first.join("uv"), "not executable").unwrap();
                std::fs::create_dir(first.join("npm")).unwrap();
                symlink(first.join("absent"), first.join("pnpm")).unwrap();
                first.as_os_str().to_owned()
            }
            _ => unreachable!("the caller supplies a fixed test name"),
        };
        let output = child
            .env("PATH", path)
            .args(["--exact", case, "--nocapture"])
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            stdout.contains("path-probe-completed"),
            "child did not run: {stdout}"
        );
        return;
    }
    assert!(Command::new("which").arg("uv").status().is_err());
    let profile = EnvironmentProfile::detect();
    if case == "ignore_nonexecutables_directories_and_broken_links" {
        for cmd in ["uv", "npm", "pnpm"] {
            assert!(!profile.has_command(cmd), "{cmd} is not executable");
        }
        assert_eq!(
            TransformPipeline::directive_only(profile).run("pip install requests\n"),
            "pip install requests\n"
        );
    } else {
        assert!(
            Command::new("uv").status().unwrap().success(),
            "control: uv really runs through PATH"
        );
        let pipeline = TransformPipeline::directive_only(profile);
        assert_eq!(pipeline.run("<!-- @if has_command(\"uv\") -->\nuv pip install requests\n<!-- @else -->\npip install requests\n<!-- @endif -->\n"), "uv pip install requests\n");
        assert_eq!(
            pipeline.run("pip install requests\n"),
            "uv pip install requests\n"
        );
    }
    println!("path-probe-completed");
}

#[test]
fn detect_and_compile_without_which() {
    probe("detect_and_compile_without_which");
}
#[test]
fn skip_unusable_candidate_before_executable() {
    probe("skip_unusable_candidate_before_executable");
}
#[test]
fn follow_executable_symlinks() {
    probe("follow_executable_symlinks");
}
#[test]
fn empty_path_entry_searches_current_directory() {
    probe("empty_path_entry_searches_current_directory");
}
#[test]
fn relative_path_entry_is_supported() {
    probe("relative_path_entry_is_supported");
}
#[test]
fn ignore_nonexecutables_directories_and_broken_links() {
    probe("ignore_nonexecutables_directories_and_broken_links");
}

#[test]
fn unset_path_uses_platform_search_path() {
    const CASE: &str = "unset_path_uses_platform_search_path";
    if std::env::var("SKILLFS_PATH_PROBE").as_deref() != Ok(CASE) {
        let output = Command::new(std::env::current_exe().unwrap())
            .env_clear()
            .env("SKILLFS_PATH_PROBE", CASE)
            .args(["--exact", CASE, "--nocapture"])
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(stdout.contains("path-probe-completed"));
        return;
    }
    assert!(std::env::var_os("PATH").is_none());
    assert!(
        Command::new("sh")
            .args(["-c", "exit 0"])
            .status()
            .unwrap()
            .success()
    );
    let profile = EnvironmentProfile::detect();
    assert!(profile.has_command("sh"));
    assert!(!profile.has_env("PATH"));
    println!("path-probe-completed");
}
