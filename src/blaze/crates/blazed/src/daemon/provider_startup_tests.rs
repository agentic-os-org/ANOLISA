// SPDX-License-Identifier: Apache-2.0

use super::*;
use async_trait::async_trait;
use blaze_provider_api::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use uuid::Uuid;

struct StartupProvider {
    before_probe: ProviderDescriptor,
    after_probe: ProviderDescriptor,
    probes: AtomicUsize,
    prepares: AtomicUsize,
}

#[async_trait]
impl DataPlaneProvider for StartupProvider {
    fn descriptor(&self) -> ProviderDescriptor {
        if self.probes.load(Ordering::Acquire) == 0 {
            self.before_probe
        } else {
            self.after_probe
        }
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }

    async fn probe(&self) -> std::result::Result<(), ProviderError> {
        self.probes.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }

    async fn prepare(
        &self,
        _: PrepareRequest,
    ) -> std::result::Result<PreparedLease, ProviderError> {
        self.prepares.fetch_add(1, Ordering::AcqRel);
        Err(ProviderError::Unsupported)
    }

    async fn inspect(
        &self,
        _: InspectRequest,
    ) -> std::result::Result<ObservedLease, ProviderError> {
        panic!("startup rejection must not inspect resources")
    }

    async fn commit(&self, _: CommitRequest) -> std::result::Result<CommittedLease, ProviderError> {
        panic!("startup rejection must not commit resources")
    }

    async fn finalize(
        &self,
        _: FinalizeRequest,
    ) -> std::result::Result<FinalizedLease, ProviderError> {
        panic!("startup rejection must not finalize resources")
    }

    async fn abort(&self, _: AbortRequest) -> std::result::Result<AbortResult, ProviderError> {
        panic!("startup rejection must not abort resources")
    }

    async fn stop(&self, _: StopRequest) -> std::result::Result<StoppedLease, ProviderError> {
        panic!("startup rejection must not stop resources")
    }

    async fn release(
        &self,
        _: ReleaseRequest,
    ) -> std::result::Result<ReleaseResult, ProviderError> {
        panic!("startup rejection must not release resources")
    }
}

async fn assert_startup_rejection(
    before_probe: ProviderDescriptor,
    after_probe: ProviderDescriptor,
    expected_probes: usize,
    diagnostic: &str,
) {
    let temp = tempfile::tempdir().expect("test directory");
    let mut config = DaemonConfig::default();
    config.template.dir = temp.path().join("catalog");
    config.template.import_root = Some(temp.path().join("imports"));
    config.storage.images_dir = temp.path().join("images");
    config.storage.instances_dir = temp.path().join("instances");
    config.policy.dir = temp.path().join("policies");
    config.daemon.state_dir = temp.path().join("state");
    config.daemon.socket = temp.path().join("run/api.sock");
    let path = temp.path().join("config.toml");
    std::fs::write(&path, toml::to_string(&config).expect("config TOML")).expect("config file");
    let provider = Arc::new(StartupProvider {
        before_probe,
        after_probe,
        probes: AtomicUsize::new(0),
        prepares: AtomicUsize::new(0),
    });

    let error = tokio::time::timeout(
        Duration::from_secs(2),
        run_with_provider(&path, provider.clone()),
    )
    .await
    .expect("startup must not enter the serving loop")
    .expect_err("invalid descriptor must reject startup");
    assert!(error.to_string().contains(diagnostic), "{error}");
    assert_eq!(provider.probes.load(Ordering::Acquire), expected_probes);
    assert_eq!(provider.prepares.load(Ordering::Acquire), 0);
    let entries: Vec<_> = std::fs::read_dir(temp.path())
        .expect("test directory contents")
        .map(|entry| entry.expect("directory entry").path())
        .collect();
    assert_eq!(
        entries,
        vec![path],
        "startup must not allocate directories or sockets"
    );
}

#[tokio::test]
async fn incompatible_contract_rejects_startup_before_probe_or_allocation() {
    let descriptor = ProviderDescriptor {
        contract_version: PROVIDER_CONTRACT_VERSION + 1,
        provider_instance_id: Uuid::new_v4(),
    };
    assert_startup_rejection(descriptor, descriptor, 0, "provider is incompatible").await;
}

#[tokio::test]
async fn empty_provider_identity_rejects_startup_before_probe_or_allocation() {
    let descriptor = ProviderDescriptor {
        contract_version: PROVIDER_CONTRACT_VERSION,
        provider_instance_id: Uuid::nil(),
    };
    assert_startup_rejection(descriptor, descriptor, 0, "provider is incompatible").await;
}

#[tokio::test]
async fn descriptor_drift_after_probe_rejects_startup_before_allocation() {
    let before = ProviderDescriptor {
        contract_version: PROVIDER_CONTRACT_VERSION,
        provider_instance_id: Uuid::new_v4(),
    };
    for after in [
        ProviderDescriptor {
            provider_instance_id: Uuid::new_v4(),
            ..before
        },
        ProviderDescriptor {
            contract_version: PROVIDER_CONTRACT_VERSION + 1,
            ..before
        },
    ] {
        assert_startup_rejection(before, after, 1, "identity changed during startup").await;
    }
}
