use super::*;

#[test]
fn managed_login_skips_profile_probe_and_marks_native_fallback_internal() {
    let mut session = FailopenSession::spawn_with_profile(
        &[
            ("COSH_SHELL_BOOTSTRAP_PATH", "1"),
            ("COSH_INTERNAL_SHELL", "0"),
            ("COSH_FAILOPEN_FORCE_PANIC", "pre-spawn"),
        ],
        "printf '%s' \"${COSH_INTERNAL_SHELL-unset}\" > \"$HOME/.probe_internal_marker\"\n",
    );
    assert!(
        session.wait_for("panicked before shell start"),
        "{}",
        session.text()
    );
    session.finish_output();
    assert_eq!(
        fs::read(session._root.path().join(".probe_internal_marker")).unwrap(),
        b"1"
    );
    assert!(
        session.text().contains("falling back to bash"),
        "{}",
        session.text()
    );
}
