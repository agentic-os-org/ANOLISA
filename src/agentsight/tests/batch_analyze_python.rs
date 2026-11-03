//! Run the offline batch converter regressions in the Cargo test gate.

use std::path::Path;
use std::process::Command;

#[test]
fn batch_converter_python_regressions() {
    let tests = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/benchmark/tests");
    let output = Command::new("python3")
        .args(["-m", "unittest", "discover", "-s"])
        .arg(tests)
        .args(["-p", "test_batch_*_regressions.py", "-v"])
        .output()
        .expect("python3 must be available to test the batch converter");
    assert!(
        output.status.success(),
        "batch converter regressions failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}
