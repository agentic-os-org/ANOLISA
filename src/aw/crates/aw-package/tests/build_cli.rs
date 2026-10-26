//! Maintainer build CLI rejects invalid process arguments without panicking.

use std::{ffi::OsStr, os::unix::ffi::OsStrExt, process::Command};

#[test]
fn non_utf8_build_arguments_are_rejected_without_panicking() {
    let output = Command::new(env!("CARGO_BIN_EXE_aw-build"))
        .arg("--output")
        .arg(OsStr::from_bytes(b"/tmp/output-\xff"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("aw-build:"));
    assert!(stderr.contains("UTF-8"));
    assert!(!stderr.contains("panicked"));
}
