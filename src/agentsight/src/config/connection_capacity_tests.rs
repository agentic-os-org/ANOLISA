//! Check connection-capacity overrides and compatibility with existing configurations.

use super::{AgentsightConfig, DEFAULT_CONNECTION_CAPACITY};

#[test]
fn new_and_bundled_configs_default_to_128_connections() {
    assert_eq!(AgentsightConfig::new().connection_capacity, 128);
    let bundled: serde_json::Value =
        serde_json::from_str(include_str!("../../agentsight.json")).unwrap();
    assert_eq!(bundled["runtime_limits"]["connection_capacity"], 128);
}

#[test]
fn json_connection_capacity_overrides_default() {
    for capacity in [1, 32, 64] {
        let mut config = AgentsightConfig::new();
        config
            .load_from_json(&format!(
                r#"{{"runtime_limits":{{"connection_capacity":{capacity}}}}}"#
            ))
            .unwrap();
        assert_eq!(config.connection_capacity, capacity);
    }
}

#[test]
fn omitted_connection_capacity_preserves_default_and_builder_override() {
    for capacity in [DEFAULT_CONNECTION_CAPACITY, 48] {
        let mut config = AgentsightConfig::new().set_connection_capacity(capacity);
        config
            .load_from_json(r#"{"runtime_limits":{"ring_buffer_mb":16}}"#)
            .unwrap();
        assert_eq!(config.connection_capacity, capacity);
    }
}

#[test]
fn zero_connection_capacity_is_rejected() {
    let mut config = AgentsightConfig::new();
    let error = config
        .load_from_json(r#"{"runtime_limits":{"connection_capacity":0}}"#)
        .unwrap_err();
    assert!(error.contains("runtime_limits.connection_capacity"));
    assert_eq!(config.connection_capacity, DEFAULT_CONNECTION_CAPACITY);
}
