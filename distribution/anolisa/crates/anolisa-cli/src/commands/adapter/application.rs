//! Application boundary for adapter enable and disable mutations.

use anolisa_core::adapter::claim::AdapterClaim;
use anolisa_core::adapter::driver::DriverPlan;
use anolisa_core::adapter::manager::{DisableOutcome, EnableOptions, EnableOutcome};
use anolisa_core::execution::{CommandOutcome, CommandOutcomeStatus, ExecutionIntent};
use anolisa_core::manifest::{AdapterNotice, NoticeWhen};

use crate::commands::common;
use crate::context::CliContext;
use crate::response::CliError;

/// Adapter mutation request after CLI arguments have been normalized.
pub(super) enum AdapterRequest<'a> {
    /// Enables one component adapter.
    Enable {
        /// Component name or package alias to resolve.
        component: &'a str,
        /// Explicit framework, or `None` to use manager resolution.
        framework: Option<&'a str>,
        /// Existing enable options forwarded without reinterpretation.
        options: EnableOptions,
        /// Selects read-only preview or state-changing application.
        intent: ExecutionIntent,
    },
    /// Disables one component adapter.
    Disable {
        /// Component name or package alias to resolve.
        component: &'a str,
        /// Explicit framework, or `None` to use manager resolution.
        framework: Option<&'a str>,
        /// Selects read-only preview or state-changing application.
        intent: ExecutionIntent,
    },
}

/// Typed adapter changes produced by an applied mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum AdapterChange {
    /// An adapter claim was enabled for the resolved framework.
    Enabled {
        /// Resolved component identity.
        component: String,
        /// Resolved framework identity.
        framework: String,
    },
    /// An adapter claim was removed after cleanup completed.
    Disabled {
        /// Resolved component identity.
        component: String,
        /// Resolved framework identity.
        framework: String,
    },
    /// Cleanup remained incomplete and the claim was retained for retry.
    CleanupFailed {
        /// Resolved component identity.
        component: String,
        /// Resolved framework identity.
        framework: String,
    },
}

/// Read-only adapter result without a terminal execution outcome.
pub(super) enum AdapterPreview {
    /// Enable plan and notices to display without applying driver effects.
    Enable {
        /// Driver plan generated from the current framework state.
        plan: DriverPlan,
        /// Preview notices kept separate from terminal status.
        notices: Vec<AdapterNotice>,
    },
    /// Disable preview produced by the existing manager contract.
    Disable(DisableOutcome),
}

/// Applied manager result and display-only notices.
pub(super) enum AdapterApplied {
    /// Enabled claim and post-enable notices.
    Enable {
        /// Persisted claim returned by the manager.
        claim: Box<AdapterClaim>,
        /// Post-enable notices kept separate from outcome warnings.
        notices: Vec<AdapterNotice>,
    },
    /// Disable result produced by the existing manager contract.
    Disable(DisableOutcome),
}

/// Typed application result separating previews from applied mutations.
pub(super) enum AdapterApplicationOutcome {
    /// Read-only result that never carries an applied command outcome.
    Preview(AdapterPreview),
    /// Applied result paired with its terminal status and typed changes.
    Applied {
        /// Manager result used to preserve existing CLI output.
        result: AdapterApplied,
        /// Terminal status and changes; operation ID remains absent.
        outcome: CommandOutcome<AdapterChange>,
    },
}

/// Resolves the target, dispatches the manager, and projects its typed outcome.
///
/// # Errors
///
/// Returns the existing CLI error when target resolution or manager execution
/// fails.
pub(super) fn run(
    request: AdapterRequest<'_>,
    ctx: &CliContext,
) -> Result<AdapterApplicationOutcome, CliError> {
    match request {
        AdapterRequest::Enable {
            component,
            framework,
            options,
            intent,
        } => {
            const COMMAND: &str = "adapter enable";
            let (component, view) = common::resolve_adapter_target(component, ctx, COMMAND)?;
            let manager = common::build_adapter_manager_from_view(ctx, &view);
            let result = manager
                .enable_with_options(&component, framework, is_preview(intent), options)
                .map_err(|err| super::map_err(COMMAND, err))?;
            Ok(project_enable(result))
        }
        AdapterRequest::Disable {
            component,
            framework,
            intent,
        } => {
            const COMMAND: &str = "adapter disable";
            let (component, view) = common::resolve_adapter_target(component, ctx, COMMAND)?;
            let manager = common::build_adapter_manager_from_view(ctx, &view);
            let result = manager
                .disable(&component, framework, is_preview(intent))
                .map_err(|err| super::map_err(COMMAND, err))?;
            Ok(project_disable(result))
        }
    }
}

fn is_preview(intent: ExecutionIntent) -> bool {
    matches!(intent, ExecutionIntent::Plan)
}

fn project_enable(result: EnableOutcome) -> AdapterApplicationOutcome {
    match result {
        EnableOutcome::Planned { plan, notices } => {
            AdapterApplicationOutcome::Preview(AdapterPreview::Enable { plan, notices })
        }
        EnableOutcome::Enabled(claim) => {
            let notices = claim
                .notices
                .iter()
                .filter(|notice| notice.when == NoticeWhen::PostEnable)
                .cloned()
                .collect();
            let change = AdapterChange::Enabled {
                component: claim.component.clone(),
                framework: claim.framework.clone(),
            };
            AdapterApplicationOutcome::Applied {
                result: AdapterApplied::Enable { claim, notices },
                outcome: CommandOutcome::new(
                    CommandOutcomeStatus::Completed,
                    None,
                    vec![change],
                    Vec::new(),
                ),
            }
        }
    }
}

fn project_disable(result: DisableOutcome) -> AdapterApplicationOutcome {
    if result.dry_run {
        return AdapterApplicationOutcome::Preview(AdapterPreview::Disable(result));
    }

    let status = if result.report.cleanup_complete {
        CommandOutcomeStatus::Completed
    } else {
        CommandOutcomeStatus::Partial {
            reason: format!(
                "adapter '{}' cleanup incomplete; receipt kept for retry",
                result.component
            ),
        }
    };
    let changes = match (
        result.claim_removed,
        result.report.cleanup_complete,
        result.framework.clone(),
    ) {
        (true, _, Some(framework)) => vec![AdapterChange::Disabled {
            component: result.component.clone(),
            framework,
        }],
        (false, false, Some(framework)) => vec![AdapterChange::CleanupFailed {
            component: result.component.clone(),
            framework,
        }],
        _ => Vec::new(),
    };

    AdapterApplicationOutcome::Applied {
        result: AdapterApplied::Disable(result),
        outcome: CommandOutcome::new(status, None, changes, Vec::new()),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use anolisa_core::adapter::claim::{AdapterClaim, ClaimStatus, DriverPayload, OpenClawClaim};
    use anolisa_core::adapter::driver::{DisableReport, DriverPlan};
    use anolisa_core::adapter::manager::{DisableOutcome, EnableOutcome};
    use anolisa_core::execution::{CommandOutcomeStatus, ExecutionIntent};
    use anolisa_core::manifest::{AdapterNotice, NoticeLevel, NoticeWhen};

    use super::{
        AdapterApplicationOutcome, AdapterApplied, AdapterChange, AdapterPreview, is_preview,
        project_disable, project_enable,
    };

    fn sample_notice(when: NoticeWhen) -> AdapterNotice {
        AdapterNotice {
            when,
            level: NoticeLevel::Warning,
            text: "restart the framework".to_string(),
            command: None,
        }
    }

    fn sample_claim() -> AdapterClaim {
        AdapterClaim {
            claim_schema: 1,
            component: "tokenless".to_string(),
            framework: "openclaw".to_string(),
            plugin_id: None,
            adapter_type: None,
            enabled_at: "2026-09-01T00:00:00Z".to_string(),
            resource_root: PathBuf::from("/tmp/tokenless/openclaw"),
            bundle_digest: None,
            source_revision: None,
            materialized_files: Vec::new(),
            driver_schema: 1,
            status: ClaimStatus::Enabled,
            notices: vec![
                sample_notice(NoticeWhen::PostEnable),
                sample_notice(NoticeWhen::PostDisable),
            ],
            resources: Vec::new(),
            driver_payload: DriverPayload::OpenClaw(OpenClawClaim {
                state_dir_resource: "state".to_string(),
                plugin_resource: "plugin".to_string(),
                skill_resources: Vec::new(),
                config_resources: Vec::new(),
            }),
        }
    }

    fn disable_outcome(
        dry_run: bool,
        claim_removed: bool,
        cleanup_complete: bool,
        notices: Vec<AdapterNotice>,
    ) -> DisableOutcome {
        DisableOutcome {
            component: "tokenless".to_string(),
            framework: Some("openclaw".to_string()),
            report: DisableReport {
                cleanup_complete,
                messages: vec!["driver report".to_string()],
            },
            claim_removed,
            dry_run,
            notices,
        }
    }

    #[test]
    fn execution_intent_maps_only_plan_to_manager_preview() {
        assert!(is_preview(ExecutionIntent::Plan));
        assert!(!is_preview(ExecutionIntent::Apply));
    }

    #[test]
    fn enable_preview_preserves_plan_and_notices() {
        let notice = sample_notice(NoticeWhen::PostEnable);
        let outcome = project_enable(EnableOutcome::Planned {
            plan: DriverPlan {
                framework: "openclaw".to_string(),
                component: "tokenless".to_string(),
                actions: vec!["register plugin".to_string()],
                register_command: Some("openclaw plugins install".to_string()),
            },
            notices: vec![notice.clone()],
        });

        let AdapterApplicationOutcome::Preview(AdapterPreview::Enable { plan, notices }) = outcome
        else {
            panic!("expected enable preview");
        };
        assert_eq!(plan.component, "tokenless");
        assert_eq!(notices, vec![notice]);
    }

    #[test]
    fn enable_applied_returns_completed_change_and_display_notices() {
        let outcome = project_enable(EnableOutcome::Enabled(Box::new(sample_claim())));
        let AdapterApplicationOutcome::Applied { result, outcome } = outcome else {
            panic!("expected applied enable");
        };
        let AdapterApplied::Enable { notices, .. } = result else {
            panic!("expected enable result");
        };

        assert_eq!(outcome.status(), &CommandOutcomeStatus::Completed);
        assert_eq!(
            outcome.changes(),
            &[AdapterChange::Enabled {
                component: "tokenless".to_string(),
                framework: "openclaw".to_string(),
            }]
        );
        assert!(outcome.operation_id().is_none());
        assert!(outcome.warnings().is_empty());
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].when, NoticeWhen::PostEnable);
    }

    #[test]
    fn disable_preview_preserves_report_without_terminal_outcome() {
        let outcome = project_disable(disable_outcome(true, false, true, Vec::new()));
        let AdapterApplicationOutcome::Preview(AdapterPreview::Disable(result)) = outcome else {
            panic!("expected disable preview");
        };
        assert!(result.dry_run);
        assert!(result.report.cleanup_complete);
    }

    #[test]
    fn disable_completed_distinguishes_removed_claim_from_noop() {
        let removed = project_disable(disable_outcome(false, true, true, Vec::new()));
        let AdapterApplicationOutcome::Applied { outcome, .. } = removed else {
            panic!("expected applied disable");
        };
        assert_eq!(outcome.status(), &CommandOutcomeStatus::Completed);
        assert_eq!(
            outcome.changes(),
            &[AdapterChange::Disabled {
                component: "tokenless".to_string(),
                framework: "openclaw".to_string(),
            }]
        );

        let noop = project_disable(disable_outcome(false, false, true, Vec::new()));
        let AdapterApplicationOutcome::Applied { outcome, .. } = noop else {
            panic!("expected applied disable no-op");
        };
        assert_eq!(outcome.status(), &CommandOutcomeStatus::Completed);
        assert!(outcome.changes().is_empty());
    }

    #[test]
    fn cleanup_failure_is_partial_while_notices_remain_non_terminal() {
        let notice = sample_notice(NoticeWhen::PostDisable);
        let outcome = project_disable(disable_outcome(false, false, false, vec![notice.clone()]));
        let AdapterApplicationOutcome::Applied { result, outcome } = outcome else {
            panic!("expected applied disable");
        };
        let AdapterApplied::Disable(result) = result else {
            panic!("expected disable result");
        };

        assert_eq!(
            outcome.status(),
            &CommandOutcomeStatus::Partial {
                reason: "adapter 'tokenless' cleanup incomplete; receipt kept for retry"
                    .to_string(),
            }
        );
        assert_eq!(
            outcome.changes(),
            &[AdapterChange::CleanupFailed {
                component: "tokenless".to_string(),
                framework: "openclaw".to_string(),
            }]
        );
        assert!(outcome.warnings().is_empty());
        assert_eq!(result.notices, vec![notice]);
    }
}
