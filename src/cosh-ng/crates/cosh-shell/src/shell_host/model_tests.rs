use super::model::ShellIntegration;

#[test]
fn enhanced_config_is_marker_enabled() {
    let integration = ShellIntegration::parse_config("enhanced").expect("integration");
    assert_eq!(integration, ShellIntegration::Enhanced);
    assert!(integration.uses_markers());
}
