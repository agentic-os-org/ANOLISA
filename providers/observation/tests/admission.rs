//! Default-build admission must reject observation before touching target or output.

#[cfg(not(feature = "unsafe-lab"))]
#[test]
fn default_build_refuses_even_without_a_permit() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_aw-observation-ptrace-poc"))
        .args(["observe", "--permit", "/nonexistent-ptrace-test-permit"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("observation_disabled"));
}
