//! Core planning, manifest, state, and lifecycle primitives for ANOLISA.
//!
//! The crate is deliberately CLI-agnostic: callers provide manifests,
//! distribution indexes, environment facts, and filesystem layout, then use
//! these APIs to plan, execute, audit, and roll back lifecycle operations.

pub mod adapter;
pub mod backup;
pub mod capability;
pub mod central_log;
pub mod component;
pub mod component_snapshot;
pub mod daemon_server;
pub mod distribution;
pub mod domain;
pub mod download;
pub mod execution;
pub mod executor;
pub mod facts;
pub mod feature_flags;
pub mod health;
pub mod hooks;
pub mod install_runner;

pub mod integrity;
pub mod lifecycle;
pub mod lock;
pub mod manifest;
pub mod osbase_install;
pub mod owned_executor;
pub mod path_safety;
pub mod planner;
pub mod process;
pub mod providers;
pub mod provisioner;
pub mod record_sink;
pub mod register;
pub mod resolver;
pub mod sandbox_manifest;
pub mod self_update;
pub mod service;
pub mod state;
mod state_identity;
pub mod state_migration;
pub mod state_store;
pub mod system_helper;
pub mod telemetry;
pub mod transaction;

pub use adapter::claim::{AdapterClaim, ClaimResource, ClaimResourceKind, ClaimStatus};
pub use adapter::driver::{AdapterStatusReport, AdapterSummary, ConditionStatus, DriverPlan};
pub use adapter::manager::{
    AdapterManager, DisableOutcome, EnableOutcome, ScanReport, StatusReport,
};
pub use adapter::registry::DriverRegistry;
pub use adapter::{
    AdapterError, DetectResult, detect_framework, expand_layout_placeholders,
    expand_layout_placeholders_content,
};
pub use backup::{BackupEntry, BackupSet};
pub use capability::{
    CapabilityError, CapabilityManager, CapabilityOutcome, CapabilityRequest, CapabilityRunOutcome,
    FakeCapabilityManager, NotSupportedCapabilityManager, SetcapManager, apply_capabilities,
    for_install_mode as capability_for_install_mode,
};
pub use central_log::{
    CentralLog, CentralLogError, LogFilter, LogKind, LogRecord, LogStatus, Severity,
};
pub use component::{Component, ComponentMeta, ComponentStatus};
pub use component_snapshot::{
    AdapterObservation, AdapterProvenance, AdapterSourceSnapshot, ComponentSnapshot,
    ComponentSnapshotObservations, ComponentSnapshotRequest, JournalProvenance,
    ManifestHealthProvenance, ManifestHealthSnapshot, NativePackageProvenance,
    NativePackageSnapshot, OwnedFileObservation, OwnedFilesProvenance, OwnedFilesSnapshot,
    OwnedFilesVerdict, PendingJournalSnapshot, ProbeEvidence, SnapshotContractError, SnapshotProbe,
    StateProvenance, StateRootScope, StateSnapshot, StateVisibilitySnapshot,
};
pub use distribution::{
    ArtifactType, DistributionEntry, DistributionError, DistributionIndex, ResolveError,
    ResolveQuery,
};
pub use download::{DownloadCache, DownloadError, DownloadedArtifact};
pub use execution::{CommandOutcome, CommandOutcomeStatus, ExecutionIntent, PreparedExecution};
pub use feature_flags::FeatureStore;
pub use health::{
    CheckEnv, CheckOutcome, CheckSpec, CheckStatus, Protocol, ServiceProbes, run_check,
};
pub use hooks::{
    HookOutcome, HookPhase, HookRunResult, HookSkipReason, HookSpec, resolve_manifest_hooks,
    run_hook, run_hooks,
};
pub use install_runner::{
    InstallError, InstallOutcome, InstallRunner, InstalledFile, RenderMode, RenderSpec,
    ResolvedInstallFile,
};
pub use integrity::{IntegrityStatus, check_owned_file};
pub use lifecycle::{
    ComponentLifecyclePlan, FileAction, FileActionKind, FileOwner as LifecycleFileOwner,
    HookAction, LifecycleError, LifecycleMode, LifecycleOperation, LifecyclePhase, LifecyclePlan,
    LifecycleTargetKind, ResolvedLifecycleHooks, RiskLevel, ServiceAction, ServiceActionKind,
};
pub use lock::{InstallLock, LockError};
pub use manifest::{
    AdapterNotice, AdapterSpec, ComponentManifest, DependencyKind, DistributionSelector, FileKind,
    NoticeLevel, NoticeWhen, PackageNames, RuntimeDependency, ServiceScope, declared_unit_scope,
};
pub use provisioner::{
    ManualDependency, ProvisionOutcome, ProvisionPlan, ProvisionStrategy, ProvisionablePackage,
    UnresolvableDependency,
};
pub use register::{
    ConsentState, HistoryAction, HistoryEntry, RegisterRecord, RegisterSource, RegisterState,
    RegistrationManager, SubscriptionError, current_operator, generate_link_id, require_root,
};
pub use resolver::{
    DependencyProbeError, DependencyResolution, DependencyResolver, DependencyStatus,
    ResolutionPlan, ResolverEnv, ResolverError,
};
pub use self_update::{
    ReleaseArtifact, ReleaseManifest, SelfUpdateError, check_update, update_url,
};
pub use service::{
    DeactivationOutcome, FakeServiceManager, NotSupportedServiceManager, ServiceActivation,
    ServiceError, ServiceManager, ServiceOp, ServiceOutcome, ServiceRequest, ServiceRunOutcome,
    ServiceState, SystemdServiceManager, apply_services, deactivate_services,
    for_install_mode as service_for_install_mode, user_service_for_install_mode,
};
pub use state::{
    BackupRecord, ExternalModifiedFile, FileOwner, HealthEntry, InstallMode, InstalledObject,
    InstalledState, ObjectKind, ObjectStatus, OperationRecord, OwnedFile, OwnedFileKind, Ownership,
    RpmMetadata, STATE_SCHEMA_VERSION, ServiceRef, StateError, SubscriptionScope,
};
pub use telemetry::instance::{InstanceInfo, InstanceProber, InstanceSnapshot};
pub use telemetry::metadata::MetadataClient;
pub use telemetry::{
    DISABLE_MARKER_PATH, Endpoint, FileOffset, LegacyAccountsConfig, LegacyIlogtail, ProductType,
    TelemetryChannel, TelemetryConfig, TelemetryError, Uploader, UploaderConfig, UploaderError,
};
pub use transaction::{
    DelegatedRecordAction, DelegatedRecoveryContext, JOURNAL_SCHEMA_VERSION, RollbackAction,
    RollbackActionKind, Transaction, TransactionError, TransactionOutcomeStatus, TransactionStep,
    TransactionStepStatus,
};
