//! Package-independent application consumer of the negotiated SDK extension.
#![cfg(target_os = "linux")]

#[allow(dead_code)]
#[path = "launcher/support.rs"]
mod support;

use std::{path::Path, process::Command};

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
    let fixture = support::Fixture::new();
    let script =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../sdk/tests/typescript_package.py");
    let mut command = Command::new("python3");
    command.arg(script).arg(env!("CARGO_BIN_EXE_aw"));
    assert_eq!(fixture.successful(command)["status"], "passed");
}

#[test]
fn python_sdk_contracts_and_out_of_tree_wheel() {
    let fixture = support::Fixture::new();
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../sdk/tests/python_package.py");
    let mut command = Command::new("python3");
    command.arg(script).arg(env!("CARGO_BIN_EXE_aw"));
    assert_eq!(fixture.successful(command)["status"], "passed");
}
