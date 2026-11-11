use super::*;

#[test]
fn health_selection_preserves_explicit_scope_and_policy() {
    let mut config = CoshConfig::default();
    parse_toml_config(
        r#"
[health]
scan_mounts = ["/", "/data"]
[health.checks]
"resource.memory" = false
"resource.swap" = true
[health.policies.login-banner]
disable = ["kernel.oom"]
"#,
        &mut config,
    );
    assert_eq!(
        config.health.scan_mounts,
        Some(vec!["/".into(), "/data".into()])
    );
    assert_eq!(config.health.checks.get("resource.memory"), Some(&false));
    assert_eq!(
        config.health.policies.get("login-banner"),
        Some(&vec!["kernel.oom".to_string()])
    );
    assert_eq!(config.health.critical_mounts, ["/"]);
}
