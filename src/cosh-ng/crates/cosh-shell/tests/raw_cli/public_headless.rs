use super::*;
use std::path::PathBuf;

fn public_shell_with_core(core_script: &str) -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).expect("public headless fixture");
    let shell = root.path().join("cosh");
    fs::copy(env!("CARGO_BIN_EXE_cosh-shell"), &shell).expect("copy actual Shell");
    write_executable(&root.path().join("cosh-core"), core_script);
    (root, shell)
}

fn run_public_shell(shell: &Path, root: &Path) -> std::process::Output {
    Command::new(shell)
        .arg0("cosh")
        .args(["--headless", "hello"])
        .env("HOME", root)
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::null())
        .output()
        .expect("run public Shell")
}

#[test]
fn public_headless_rejects_a_mismatched_core_before_task_execution() {
    let (root, shell) = public_shell_with_core(
        "#!/bin/sh\n\
         if [ \"$1\" = --version ]; then printf 'cosh-core 0.0.0\\n'; exit 0; fi\n\
         printf started > \"$(dirname \"$0\")/core-task-started\"\n\
         exit 0\n",
    );

    let output = run_public_shell(&shell, root.path());

    assert_eq!(output.status.code(), Some(126), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("incompatible cosh-core version"),
        "{output:?}"
    );
    assert!(!root.path().join("core-task-started").exists());
}

#[test]
fn public_headless_executes_a_matching_core_after_version_validation() {
    let (root, shell) = public_shell_with_core(&format!(
        "#!/bin/sh\n\
         if [ \"$1\" = --version ]; then printf 'cosh-core {}\\n'; exit 0; fi\n\
         printf '%s\\n' \"$@\" > \"$(dirname \"$0\")/core-task-argv\"\n\
         printf 'matching-core\\n'\n",
        env!("CARGO_PKG_VERSION")
    ));

    let output = run_public_shell(&shell, root.path());

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "matching-core\n");
    let argv = fs::read_to_string(root.path().join("core-task-argv")).expect("Core argv");
    assert!(argv.starts_with("--headless\n--execution-profile=public-headless-v1\n"));
    assert!(argv.ends_with("hello\n"));
}
