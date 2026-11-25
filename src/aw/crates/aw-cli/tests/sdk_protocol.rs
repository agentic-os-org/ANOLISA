//! Package-independent application consumer of the negotiated SDK extension.
#![cfg(target_os = "linux")]

#[allow(dead_code)]
#[path = "launcher/support.rs"]
mod support;

use std::{path::Path, process::Command, time::Duration};

#[test]
fn direct_sdk_consumer_registers_executes_and_exports_native_facts() {
    let fixture = support::Fixture::new();
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../sdk/tests/direct_contract.py");
    let mut command = Command::new("python3");
    command.arg(script).arg(env!("CARGO_BIN_EXE_aw"));
    assert_eq!(fixture.successful(command)["status"], "passed");
}

#[test]
fn typescript_sdk_contracts_and_out_of_tree_package() {
    check_sdk_package("typescript_package.py", Duration::from_secs(90));
}

#[test]
fn python_sdk_contracts_and_out_of_tree_wheel() {
    // The wheel suite builds and installs the core and both optional adapters.
    check_sdk_package("python_package.py", Duration::from_secs(90));
}

fn check_sdk_package(script: &str, timeout: Duration) {
    let fixture = support::Fixture::new();
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../sdk/tests")
        .join(script);
    let mut command = Command::new("python3");
    command.arg(script).arg(env!("CARGO_BIN_EXE_aw"));
    let output = support::Process::spawn(&fixture, command).finish_with_timeout(timeout);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["status"], "passed");
}
