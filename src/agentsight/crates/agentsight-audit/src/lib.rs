//! System-audit domain models, persistence, and application service.

mod models;
mod service;
mod store;

pub use models::{
    AuditCountBy, AuditEventFilter, AuditEventPage, AuditSession, AuditSessionPage, AuditSummary,
    ContainmentAction, ContainmentFailureStage, ContainmentLifecycle, RiskCase, RiskCaseDetail,
    RiskCaseStatus, RiskCaseSummary, RiskSeverity,
};
pub use service::{AuditService, AuditServiceError};
#[cfg(target_os = "linux")]
pub use store::DueContainmentAction;
pub use store::{
    AuditError, AuditEventStore, AuditMaintenancePolicy, AuditMaintenanceReport, AuditStore,
    ContainmentActivationResult, ContainmentClaimResult,
};
