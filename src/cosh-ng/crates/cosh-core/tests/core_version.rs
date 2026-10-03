use std::process::{Command, Stdio};

fn run_version() -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_cosh-core"))
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .expect("run cosh-core --version")
}

#[test]
fn version_flag_exits_zero_and_prints_name_version() {
    let output = run_version();

    assert!(
        output.status.success(),
        "status={:?}\nstderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let expected = format!("cosh-core {}", env!("CARGO_PKG_VERSION"));
    assert_eq!(
        stdout.trim(),
        expected,
        "stdout={stdout:?} expected={expected:?}"
    );

    assert!(
        !String::from_utf8_lossy(&output.stderr).contains("unexpected argument"),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
}
