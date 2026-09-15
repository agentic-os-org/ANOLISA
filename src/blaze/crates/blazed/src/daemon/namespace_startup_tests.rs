// SPDX-License-Identifier: Apache-2.0

use super::*;
use blaze_core::lifecycle::{SandboxInstance, SandboxState};
use blaze_core::policy::WorkloadClass;
use blaze_provider_api::{PROVIDER_CONTRACT_VERSION, ProviderDescriptor};
use std::os::unix::fs::MetadataExt;
use uuid::Uuid;

async fn check_standard_listener_boundary(provider_state: Option<bool>) {
    let temporary = tempfile::tempdir().expect("daemon test directory");
    let root = temporary.path();
    let mut config = DaemonConfig::default();
    config.template.dir = root.join("catalog");
    config.template.import_root = Some(root.join("imports"));
    config.storage.images_dir = root.join("images");
    config.storage.instances_dir = root.join("instances");
    config.policy.dir = root.join("policies");
    config.daemon.state_dir = root.join("state");
    config.daemon.socket = root.join("api.sock");
    config.backends.clear();
    std::fs::create_dir(root.join("imports")).expect("import directory");
    std::fs::create_dir(&config.policy.dir).expect("policy directory");
    std::fs::create_dir(&config.daemon.state_dir).expect("state directory");

    // A reserved TCP address makes the success control return before serving.
    // The existing Unix socket reveals whether startup touched its listeners.
    let tcp_owner = std::net::TcpListener::bind("127.0.0.1:0").expect("reserved TCP address");
    config.listen.http_addr = tcp_owner.local_addr().expect("TCP address").to_string();
    let _socket_owner = std::os::unix::net::UnixListener::bind(&config.daemon.socket)
        .expect("preexisting Unix listener");
    let socket_before = std::fs::symlink_metadata(&config.daemon.socket).expect("socket metadata");
    let path = root.join("config.toml");
    std::fs::write(&path, toml::to_string(&config).expect("config TOML")).expect("config file");

    let mut retained_record = None;
    if let Some(corrupt) = provider_state {
        let namespace = build_time_provider_state_namespace(ProviderDescriptor {
            contract_version: PROVIDER_CONTRACT_VERSION,
            provider_instance_id: Uuid::new_v4(),
        });
        let provider =
            StateStore::open_provider_namespace(config.daemon.state_dir.clone(), &namespace)
                .expect("provider state owner");
        let mut instance = SandboxInstance::new(
            BackendKind::Mock,
            WorkloadClass::AgentTool,
            "sha256:test".into(),
            "default".into(),
        );
        instance
            .transition(SandboxState::Creating)
            .expect("create transition");
        instance
            .transition(SandboxState::Running)
            .expect("running transition");
        provider.persist(&instance).expect("active provider record");
        let record = config
            .daemon
            .state_dir
            .join(namespace)
            .join(instance.id.to_string())
            .join("state.json");
        if corrupt {
            std::fs::write(&record, b"{unknown-record").expect("uncertain provider record");
        }
        let bytes = std::fs::read(&record).expect("record before startup");
        retained_record = Some((record, bytes));
        drop(provider);
    }

    let error = tokio::time::timeout(Duration::from_secs(2), run(&path))
        .await
        .expect("startup must return before serving")
        .expect_err("provider preflight or reserved TCP address must stop startup");
    let socket_after = std::fs::symlink_metadata(&config.daemon.socket).expect("retained socket");
    match provider_state {
        Some(corrupt) => {
            if corrupt {
                assert!(
                    matches!(error, BlazeDaemonError::RecoveryRequired(_)),
                    "{error}"
                );
            } else {
                assert!(matches!(error, BlazeDaemonError::Conflict(_)), "{error}");
            }
            assert_eq!(socket_before.dev(), socket_after.dev());
            assert_eq!(socket_before.ino(), socket_after.ino());
            let (record, bytes) = retained_record.expect("provider record");
            assert_eq!(std::fs::read(record).expect("retained record"), bytes);
        }
        None => {
            assert!(
                matches!(error, BlazeDaemonError::Internal(ref message)
                if message.contains("bind TCP")),
                "{error}"
            );
            assert_ne!(socket_before.ino(), socket_after.ino());
        }
    }
}

#[tokio::test]
async fn standard_startup_rejects_active_provider_before_touching_listeners() {
    check_standard_listener_boundary(Some(false)).await;
}

#[tokio::test]
async fn standard_startup_rejects_uncertain_provider_before_touching_listeners() {
    check_standard_listener_boundary(Some(true)).await;
}

#[tokio::test]
async fn standard_startup_without_foreign_state_reaches_listener_setup() {
    check_standard_listener_boundary(None).await;
}
