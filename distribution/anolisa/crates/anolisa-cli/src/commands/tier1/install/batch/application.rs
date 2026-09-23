//! Application orchestration for `install --all`.

use std::collections::{HashMap, HashSet};

use anolisa_core::execution::ExecutionIntent;
use anolisa_core::planner::Step;

use crate::context::CliContext;
use crate::progress::{self, Activity};
use crate::response::CliError;

use super::super::application as install_application;
use super::super::{
    InstallArgs, InstallOutcome, RpmdbProbe, host_backends, normalized_repo_override,
    plan_component,
};
use super::{
    MergedItem, execute_merged_group, merged_package, per_component_args, resolve_all_components,
};

/// Batch command input plus whether the caller requested preview or apply.
pub(super) struct BatchRequest<'a> {
    pub(super) args: &'a InstallArgs,
    pub(super) intent: ExecutionIntent,
}

/// Typed disposition of one batch member.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BatchMemberStatus {
    /// Effects completed for this member.
    Installed,
    /// The member has an executable plan, but no effects ran.
    Planned,
    /// Existing state already covers the requested install.
    AlreadyInstalled,
    /// Effects occurred or cannot be excluded, so repair is required.
    Partial,
    /// The member failed without a known partial state.
    Failed,
    /// Fail-fast prevented this member from being attempted.
    Skipped,
}

impl BatchMemberStatus {
    /// Returns the stable label used by the existing batch wire format.
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Installed => "installed",
            Self::Planned => "planned",
            Self::AlreadyInstalled => "already-installed",
            Self::Partial | Self::Failed => "failed",
            Self::Skipped => "skipped",
        }
    }

    /// Returns whether this member makes the aggregate batch unsuccessful.
    pub(super) fn is_unsuccessful(self) -> bool {
        matches!(self, Self::Partial | Self::Failed)
    }
}

/// Typed result for one component in index order.
#[derive(Debug)]
pub(super) struct BatchMemberOutcome {
    pub(super) component: String,
    pub(super) status: BatchMemberStatus,
    pub(super) reason: Option<String>,
    /// Only merged preview members expose their existing per-member plan.
    pub(super) plan: Option<Vec<Step>>,
}

/// Complete batch result consumed by the command renderer.
#[derive(Debug)]
pub(super) struct BatchApplicationOutcome {
    pub(super) intent: ExecutionIntent,
    pub(super) merged_transaction: Option<Vec<String>>,
    pub(super) items: Vec<BatchMemberOutcome>,
}

/// Ordered presentation facts consumed by the command renderer.
#[derive(Debug)]
pub(super) enum BatchOutputEvent {
    /// Announces members sharing one native transaction.
    MergedGroup { members: String },
    /// Shows one merged member's plan in the legacy human format.
    PreviewPlan { component: String, steps: Vec<Step> },
    /// Announces a member using the single-component application path.
    Member { component: String },
    /// Surfaces a non-fatal application warning.
    Warning(String),
}

/// Effects returned by the merged transaction boundary.
pub(super) struct BatchEffectOutcome {
    pub(super) items: Vec<BatchMemberOutcome>,
}

/// Batch-facing classification of a single-component application result.
pub(super) enum InstallBatchOutcome {
    /// The member completed or required no change.
    Completed(InstallOutcome),
    /// Effects occurred or cannot be excluded, so repair is required.
    Partial { reason: String },
    /// The member failed without a known partial state.
    Failed { reason: String },
}

/// Per-member fallback result returned to merged transaction orchestration.
pub(super) struct MemberApplicationOutcome {
    pub(super) outcome: InstallBatchOutcome,
    pub(super) warnings: Vec<String>,
}

impl BatchApplicationOutcome {
    /// Returns whether any member failed.
    pub(super) fn has_failures(&self) -> bool {
        self.items.iter().any(|item| item.status.is_unsuccessful())
    }

    /// Returns whether this batch was prepared without applying effects.
    pub(super) fn is_preview(&self) -> bool {
        matches!(self.intent, ExecutionIntent::Plan)
    }
}

/// Run the batch application against production host dependencies.
pub(super) fn run(
    request: BatchRequest<'_>,
    ctx: &CliContext,
    output: &mut dyn FnMut(BatchOutputEvent),
) -> Result<BatchApplicationOutcome, CliError> {
    let mut activity = Activity::start(
        progress::feedback_for_stderr(ctx.json, ctx.quiet),
        "Preparing batch installation...",
    );
    let index_base_override = normalized_repo_override(request.args)?;
    let names = resolve_all_components(
        ctx,
        request.args.backend.as_deref(),
        index_base_override.as_deref(),
    )?;
    if names.is_empty() {
        activity.finish();
        return Ok(BatchApplicationOutcome {
            intent: request.intent,
            merged_transaction: None,
            items: Vec::new(),
        });
    }

    // Per-member applications return typed outcomes; batch owns the only
    // final renderer, so their human and JSON output stays suppressed.
    let mut suppressed_ctx = ctx.clone();
    suppressed_ctx.json = false;
    suppressed_ctx.quiet = true;
    suppressed_ctx.dry_run = matches!(request.intent, ExecutionIntent::Plan);

    let mut merged: Vec<MergedItem> = Vec::new();
    let mut per_item: Vec<String> = Vec::new();
    for (index, name) in names.iter().enumerate() {
        activity.set_message(&format!(
            "Planning {name} ({}/{})...",
            index + 1,
            names.len()
        ));
        let per_args = per_component_args(name, request.args);
        let env = anolisa_env::EnvService::detect();
        let rpmdb = RpmdbProbe::for_host(&env);
        let candidate = host_backends(name, &per_args, &suppressed_ctx)
            .and_then(|(query, txn)| {
                plan_component(name, &per_args, &suppressed_ctx, &env, &rpmdb, &query, &txn)
            })
            .ok()
            .and_then(|planned| {
                merged_package(&planned).map(|package| MergedItem {
                    name: name.clone(),
                    package,
                    planned,
                })
            });
        match candidate {
            Some(item) => merged.push(item),
            None => per_item.push(name.clone()),
        }
    }

    if merged.len() < 2 {
        per_item.clone_from(&names);
        merged.clear();
    }
    let merged_transaction =
        (!merged.is_empty()).then(|| merged.iter().map(|item| item.package.clone()).collect());

    let mut results: HashMap<String, BatchMemberOutcome> = HashMap::with_capacity(names.len());
    let mut planned_components: HashSet<String> = HashSet::new();
    let mut fail_fast_tripped = false;
    let preview = matches!(request.intent, ExecutionIntent::Plan);

    if !merged.is_empty() {
        let members = merged
            .iter()
            .map(|item| item.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let message = if preview {
            format!("Preparing install plan for {members}...")
        } else {
            format!("Installing {members}...")
        };
        activity.set_message(&message);
        output(BatchOutputEvent::MergedGroup {
            members: members.clone(),
        });
        if preview {
            let per_args = per_component_args(&merged[0].name, request.args);
            let packages: Vec<&str> = merged.iter().map(|item| item.package.as_str()).collect();
            let preflight = host_backends(&merged[0].name, &per_args, &suppressed_ctx).and_then(
                |(query, txn)| {
                    super::super::check_rpm_install(
                        &anolisa_core::providers::DelegatedProvider::new(&query, &txn),
                        &packages,
                        "install --all",
                    )
                },
            );
            for item in &merged {
                if let Err(err) = &preflight {
                    results.insert(item.name.clone(), failed_item(&item.name, err.reason()));
                    fail_fast_tripped = request.args.fail_fast;
                    continue;
                }

                let steps = item.planned.route.steps().to_vec();
                output(BatchOutputEvent::PreviewPlan {
                    component: item.name.clone(),
                    steps: steps.clone(),
                });
                results.insert(
                    item.name.clone(),
                    BatchMemberOutcome {
                        component: item.name.clone(),
                        status: BatchMemberStatus::Planned,
                        reason: None,
                        plan: Some(steps),
                    },
                );
                planned_components.insert(item.name.clone());
            }
        } else {
            let effect = execute_merged_group(merged, request.args, ctx, output);
            let any_failed = effect
                .items
                .iter()
                .any(|item| item.status.is_unsuccessful());
            for item in effect.items {
                results.insert(item.component.clone(), item);
            }
            if request.args.fail_fast && any_failed {
                fail_fast_tripped = true;
            }
        }
    }

    for name in &per_item {
        if fail_fast_tripped {
            break;
        }
        let message = if preview {
            format!("Preparing install plan for {name}...")
        } else {
            format!("Installing {name}...")
        };
        activity.set_message(&message);
        output(BatchOutputEvent::Member {
            component: name.clone(),
        });
        let per_args = per_component_args(name, request.args);
        let member = member_application_outcome(install_application::run_with_planned_components(
            install_application::InstallRequest {
                component: name,
                args: &per_args,
                intent: request.intent,
            },
            &suppressed_ctx,
            &planned_components,
        ));
        for warning in member.warnings {
            output(BatchOutputEvent::Warning(warning));
        }
        if preview
            && matches!(
                member.outcome,
                InstallBatchOutcome::Completed(InstallOutcome::Installed)
            )
        {
            planned_components.insert(name.clone());
        }
        let item = project_member_application(name, request.intent, member.outcome);
        if request.args.fail_fast && item.status.is_unsuccessful() {
            fail_fast_tripped = true;
        }
        results.insert(name.clone(), item);
    }

    let items = names
        .iter()
        .map(|name| {
            results.remove(name).unwrap_or_else(|| BatchMemberOutcome {
                component: name.clone(),
                status: BatchMemberStatus::Skipped,
                reason: Some("--fail-fast: not attempted".to_string()),
                plan: None,
            })
        })
        .collect();
    activity.finish();

    Ok(BatchApplicationOutcome {
        intent: request.intent,
        merged_transaction,
        items,
    })
}

pub(super) fn member_application_outcome(
    result: Result<
        install_application::InstallApplicationOutcome,
        install_application::ApplicationFailure,
    >,
) -> MemberApplicationOutcome {
    match result {
        Ok(outcome) => MemberApplicationOutcome {
            outcome: InstallBatchOutcome::Completed(outcome.batch_outcome()),
            warnings: outcome.warnings().to_vec(),
        },
        Err(install_application::ApplicationFailure::Partial(error)) => MemberApplicationOutcome {
            outcome: InstallBatchOutcome::Partial {
                reason: error.reason(),
            },
            warnings: Vec::new(),
        },
        Err(install_application::ApplicationFailure::Failed(error)) => MemberApplicationOutcome {
            outcome: InstallBatchOutcome::Failed {
                reason: error.reason(),
            },
            warnings: Vec::new(),
        },
    }
}

pub(super) fn project_member_application(
    name: &str,
    intent: ExecutionIntent,
    outcome: InstallBatchOutcome,
) -> BatchMemberOutcome {
    match outcome {
        InstallBatchOutcome::Completed(outcome) => BatchMemberOutcome {
            component: name.to_string(),
            status: batch_status(outcome, intent),
            reason: None,
            plan: None,
        },
        InstallBatchOutcome::Partial { reason } => partial_item(name, reason),
        InstallBatchOutcome::Failed { reason } => failed_item(name, reason),
    }
}

pub(super) fn failed_item(name: &str, reason: String) -> BatchMemberOutcome {
    BatchMemberOutcome {
        component: name.to_string(),
        status: BatchMemberStatus::Failed,
        reason: Some(reason),
        plan: None,
    }
}

pub(super) fn partial_item(name: &str, reason: String) -> BatchMemberOutcome {
    BatchMemberOutcome {
        component: name.to_string(),
        status: BatchMemberStatus::Partial,
        reason: Some(reason),
        plan: None,
    }
}

pub(super) fn batch_status(outcome: InstallOutcome, intent: ExecutionIntent) -> BatchMemberStatus {
    match (outcome, intent) {
        (InstallOutcome::Installed, ExecutionIntent::Apply) => BatchMemberStatus::Installed,
        (InstallOutcome::Installed, ExecutionIntent::Plan) => BatchMemberStatus::Planned,
        (InstallOutcome::AlreadyInstalled, _) => BatchMemberStatus::AlreadyInstalled,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_status_is_driven_by_execution_intent() {
        assert_eq!(
            batch_status(InstallOutcome::Installed, ExecutionIntent::Apply),
            BatchMemberStatus::Installed
        );
        assert_eq!(
            batch_status(InstallOutcome::Installed, ExecutionIntent::Plan),
            BatchMemberStatus::Planned
        );
        for intent in [ExecutionIntent::Plan, ExecutionIntent::Apply] {
            assert_eq!(
                batch_status(InstallOutcome::AlreadyInstalled, intent),
                BatchMemberStatus::AlreadyInstalled
            );
        }
    }

    #[test]
    fn aggregate_failure_is_typed() {
        for item in [
            partial_item("cosh", "partial".to_string()),
            failed_item("cosh", "failed".to_string()),
        ] {
            let outcome = BatchApplicationOutcome {
                intent: ExecutionIntent::Apply,
                merged_transaction: None,
                items: vec![item],
            };

            assert!(outcome.has_failures());
            assert!(!outcome.is_preview());
        }
    }

    #[test]
    fn partial_and_failed_keep_the_legacy_wire_label() {
        assert_eq!(BatchMemberStatus::Partial.as_str(), "failed");
        assert_eq!(BatchMemberStatus::Failed.as_str(), "failed");
        assert!(BatchMemberStatus::Partial.is_unsuccessful());
        assert!(BatchMemberStatus::Failed.is_unsuccessful());
    }
}
