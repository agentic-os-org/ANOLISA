use super::login_effect::LoginEffectSource;
use super::model::{ShellHostConfig, ShellIntegration};

#[test]
fn shell_host_config_owns_one_shared_login_effect_guard() {
    let config = ShellHostConfig::new("guard-owner", "/tmp/cosh-guard-owner");
    let first = config.login_effect_guard();
    let second = config.login_effect_guard();

    first.mark_possible(LoginEffectSource::ManagedShell);
    assert!(second.has(LoginEffectSource::ManagedShell));
}

#[test]
fn enhanced_config_is_marker_enabled() {
    let integration = ShellIntegration::parse_config("enhanced").expect("integration");
    assert_eq!(integration, ShellIntegration::Enhanced);
    assert!(integration.uses_markers());
}
