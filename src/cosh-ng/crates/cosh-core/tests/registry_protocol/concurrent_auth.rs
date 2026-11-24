use super::*;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

#[test]
fn registry_auth_concurrent_configure_preserves_successful_providers() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    for guarded in [false, true] {
        for writers in [1, 3] {
            let home = tempfile::tempdir().unwrap();
            let config_dir = home.path().join(".copilot-shell");
            std::fs::create_dir(&config_dir).unwrap();
            std::fs::set_permissions(&config_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
            let config_path = config_dir.join("config.toml");
            std::fs::write(
                &config_path,
                r#"[ai]
active_provider = "keepme"
output_language = "en-US"
thinking = "high"
[ai.providers.keepme]
type = "openai_compat"
base_url = "https://keep.example/v1"
api_key = "sk-keep"
model = "keep-model"
[ui]
language = "zh-CN"
"#,
            )
            .unwrap();
            std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o600)).unwrap();
            let original_metadata = std::fs::metadata(&config_path).unwrap();
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
            let server = std::thread::spawn(move || {
                let mut streams = Vec::new();
                for _ in 0..writers {
                    let mut stream = accept_with_timeout(&listener, SERVER_ACCEPT_TIMEOUT).unwrap();
                    let mut request = [0_u8; 2048];
                    assert!(stream.read(&mut request).unwrap() > 0);
                    streams.push(stream);
                }
                let body = r#"{"id":"home-model","object":"model"}"#;
                for mut stream in streams {
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                    .unwrap();
                }
            });
            std::thread::scope(|scope| {
                let requests: Vec<_> = (0..writers)
                    .map(|writer| {
                        let base_url = &base_url;
                        let home = home.path();
                        scope.spawn(move || {
                            run_registry_request_with_context(
                                "auth",
                                "configure",
                                serde_json::json!({
                                    "provider_id": format!("p{writer}"),
                                    "provider_type": "openai_compat",
                                    "guarded_writes": guarded,
                                    "values": {
                                        "base_url": base_url,
                                        "api_key": format!("sk-p{writer}"),
                                        "model": "home-model"
                                    }
                                }),
                                home,
                                Some(home),
                            )
                        })
                    })
                    .collect();
                for request in requests {
                    let response = request.join().unwrap();
                    assert_eq!(response["success"], true, "{response}");
                }
            });
            server.join().unwrap();

            let saved: toml::Value =
                toml::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
            let providers = saved["ai"]["providers"].as_table().unwrap();
            assert_eq!(providers.len(), writers + 1, "guarded={guarded}: {saved}");
            for writer in 0..writers {
                assert_eq!(
                    providers[&format!("p{writer}")]["api_key"].as_str(),
                    Some(format!("sk-p{writer}").as_str())
                );
            }
            assert_eq!(providers["keepme"]["api_key"].as_str(), Some("sk-keep"));
            assert_eq!(saved["ai"]["output_language"].as_str(), Some("en-US"));
            assert_eq!(saved["ai"]["thinking"].as_str(), Some("high"));
            assert_eq!(saved["ui"]["language"].as_str(), Some("zh-CN"));
            let metadata = std::fs::metadata(&config_path).unwrap();
            assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
            assert_eq!(metadata.uid(), original_metadata.uid());
            assert_eq!(metadata.gid(), original_metadata.gid());
        }
    }
}

#[test]
fn registry_auth_configure_preserves_changes_committed_during_preflight() {
    for guarded in [false, true] {
        let home = tempfile::tempdir().unwrap();
        let config_dir = home.path().join(".copilot-shell");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&config_dir)
            .unwrap();
        let config_path = config_dir.join("config.toml");
        std::fs::write(
            &config_path,
            r#"[ai]
output_language = "en-US"
thinking = "high"
custom_setting = "retained"
[ai.providers.keepme]
type = "openai_compat"
base_url = "https://keep.example/v1"
api_key = "sk-old"
model = "old-model"
[ai.providers.removeme]
type = "openai_compat"
api_key = "sk-remove"
"#,
        )
        .unwrap();
        std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
        let (entered_sender, entered_receiver) = std::sync::mpsc::channel();
        let (release_sender, release_receiver) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let mut stream = accept_with_timeout(&listener, SERVER_ACCEPT_TIMEOUT).unwrap();
            let mut request = [0_u8; 2048];
            assert!(stream.read(&mut request).unwrap() > 0);
            entered_sender.send(()).unwrap();
            release_receiver.recv_timeout(REGISTRY_TIMEOUT).unwrap();
            let body = r#"{"id":"home-model","object":"model"}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        });
        std::thread::scope(|scope| {
            let home_path = home.path();
            let delayed = scope.spawn(move || {
                run_registry_request_with_context(
                    "auth",
                    "configure",
                    serde_json::json!({
                        "provider_id": "new-provider",
                        "provider_type": "openai_compat",
                        "guarded_writes": guarded,
                        "values": {"base_url": base_url, "api_key": "sk-new", "model": "home-model"}
                    }),
                    home_path,
                    Some(home_path),
                )
            });
            entered_receiver.recv_timeout(REGISTRY_TIMEOUT).unwrap();
            let (updated_url, updated_server) = model_server();
            let updated = run_registry_request_with_context(
                "auth",
                "configure",
                serde_json::json!({
                    "provider_id": "keepme",
                    "provider_type": "openai_compat",
                    "guarded_writes": guarded,
                    "values": {"base_url": updated_url, "api_key": "sk-updated", "model": "home-model"}
                }),
                home.path(),
                Some(home.path()),
            );
            updated_server.join().unwrap();
            assert_eq!(updated["success"], true, "{updated}");
            let removed = run_registry_request_with_context(
                "auth",
                "delete",
                serde_json::json!({"provider_id": "removeme", "guarded_writes": guarded}),
                home.path(),
                Some(home.path()),
            );
            assert_eq!(removed["success"], true, "{removed}");
            release_sender.send(()).unwrap();
            let response = delayed.join().unwrap();
            assert_eq!(response["success"], true, "{response}");
        });
        server.join().unwrap();
        let saved: toml::Value =
            toml::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
        let providers = saved["ai"]["providers"].as_table().unwrap();
        assert_eq!(providers.len(), 2, "{saved}");
        assert_eq!(providers["keepme"]["api_key"].as_str(), Some("sk-updated"));
        assert_eq!(
            providers["new-provider"]["api_key"].as_str(),
            Some("sk-new")
        );
    }
}

#[test]
fn registry_auth_configure_does_not_replace_malformed_config() {
    for guarded in [false, true] {
        let home = tempfile::tempdir().unwrap();
        let config_dir = home.path().join(".copilot-shell");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&config_dir)
            .unwrap();
        let config_path = config_dir.join("config.toml");
        let original = "[ai.providers.keepme]\napi_key = \"unterminated\n";
        std::fs::write(&config_path, original).unwrap();
        std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let (base_url, server) = model_server();
        let response = run_registry_request_with_context(
            "auth",
            "configure",
            serde_json::json!({
                "provider_id": "new-provider",
                "provider_type": "openai_compat",
                "guarded_writes": guarded,
                "values": {"base_url": base_url, "api_key": "sk-new", "model": "home-model"}
            }),
            home.path(),
            Some(home.path()),
        );
        server.join().unwrap();
        assert_eq!(response["success"], false, "{response}");
        assert_eq!(response["data"]["error_code"], "persistence_failed");
        assert_eq!(std::fs::read_to_string(config_path).unwrap(), original);
    }
}
