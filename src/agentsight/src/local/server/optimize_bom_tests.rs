use super::*;

#[test]
fn marked_config_survives_startup_update_and_restart() {
    for prefix in ["", "\u{feff}"] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CONFIG_FILE_NAME);
        let original = format!(
            "{prefix}{}",
            serde_json::json!({"api_key":"synthetic-key-123456789",
                "base_url":"https://example.test/v1","model":"original",
                "search_timeout_secs":42})
        );
        std::fs::write(&path, &original).unwrap();
        let state = OptimizeState::init(dir.path(), None);
        let mut config = state.snapshot();
        assert_eq!(config.api_key.as_deref(), Some("synthetic-key-123456789"));
        let response = config_response(&config);
        assert_eq!(response["base_url"], "https://example.test/v1");
        assert_eq!(response["search_timeout_secs"], 42);
        assert_ne!(response["api_key"], "synthetic-key-123456789");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);

        apply_config_update(
            &mut config,
            &UpdateOptConfig {
                api_key: None,
                current_api_key: None,
                base_url: None,
                model: Some("updated".into()),
                search_timeout_secs: None,
            },
        )
        .unwrap();
        config.save(&path).unwrap();
        let restarted = OptimizeState::init(dir.path(), None).snapshot();
        assert_eq!(restarted.api_key, config.api_key);
        assert_eq!(restarted.base_url, config.base_url);
        assert_eq!(restarted.model.as_deref(), Some("updated"));
        assert_eq!(restarted.search_timeout_secs, Some(42));
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            1,
            "a valid marked config must not create a corruption backup"
        );
    }
}

#[test]
fn valid_marked_config_does_not_trigger_the_corruption_backup() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(CONFIG_FILE_NAME);
    std::fs::write(&path, "\u{feff}{\"model\":\"old\"}").unwrap();
    preserve_unparseable_config(&path).unwrap();
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn normalization_still_preserves_invalid_original_bytes() {
    for invalid in [
        "\u{feff}{",
        "\u{feff}\u{feff}{}",
        "\u{feff}{\"search_timeout_secs\":\"42\"}",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CONFIG_FILE_NAME);
        std::fs::write(&path, invalid).unwrap();
        OptLlmConfig::default().save(&path).unwrap();
        let backups: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|file| file != &path)
            .collect();
        assert_eq!(backups.len(), 1);
        assert_eq!(std::fs::read_to_string(&backups[0]).unwrap(), invalid);
        assert!(
            serde_json::from_str::<OptLlmConfig>(&std::fs::read_to_string(&path).unwrap()).is_ok()
        );
    }
}
