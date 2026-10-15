use super::*;
use crate::shell_host::{LoginEffectGuard, LoginEffectSource};

#[test]
fn managed_shell_phase_depends_only_on_managed_shell_effect() {
    let effects = LoginEffectGuard::new();
    assert!(!managed_shell_started(&effects));

    effects.mark_possible(LoginEffectSource::PathBootstrapProbe);
    effects.mark_possible(LoginEffectSource::R2CapabilityProbe);
    assert!(!managed_shell_started(&effects));

    effects.mark_possible(LoginEffectSource::ManagedShell);
    assert!(managed_shell_started(&effects));
}

#[test]
fn temp_session_dir_guard_removes_session_directory_on_drop() {
    let dir = std::env::temp_dir().join(format!(
        "cosh-shell-temp-session-cleanup-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);

    {
        let _cleanup = TempSessionDir::new(dir.clone());
        fs::create_dir_all(dir.join("output-refs")).expect("create output refs");
        fs::write(dir.join("history"), "echo ok\n").expect("write history");
        fs::write(dir.join("output-refs/cmd-1.txt"), "ok\n").expect("write output ref");
    }

    assert!(!dir.exists(), "temp session dir should be removed on drop");
}

#[test]
fn resume_forces_enhanced_marker_path_for_r2() {
    // #R2 A11: a resume under a Native default must be upgraded to Enhanced,
    // and Enhanced must carry markers (the R2 branch is gated on marker_path
    // being Some). If the override regresses to `configured`, the first
    // assert fails; if Enhanced ever stopped emitting markers, the second.
    assert_eq!(
        integration_for_launch(true, ShellIntegration::Native),
        ShellIntegration::Enhanced,
    );
    assert!(integration_for_launch(true, ShellIntegration::Native).uses_markers());
    assert_eq!(
        integration_for_launch(false, ShellIntegration::Native),
        ShellIntegration::Native,
    );
    assert_eq!(
        integration_for_launch(false, ShellIntegration::Enhanced),
        ShellIntegration::Enhanced,
    );
}
