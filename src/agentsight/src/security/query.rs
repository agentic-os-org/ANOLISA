//! Compatibility aliases for system-audit domain models.

pub use agentsight_audit::{
    AuditCountBy as SecurityCountBy, AuditEventFilter as SecurityEventFilter,
    AuditEventPage as SecurityEventPage, AuditSession as SecuritySession,
    AuditSessionPage as SecuritySessionPage, AuditSummary as SecuritySummary, ContainmentAction,
    ContainmentFailureStage, ContainmentLifecycle, RiskCase, RiskCaseDetail, RiskCaseStatus,
    RiskSeverity,
};
