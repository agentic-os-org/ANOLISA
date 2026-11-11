use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::super::model::{HealthCollector, HealthFactCoverage, HealthFactScope};
use super::job::JobDeadline;
use crate::config::{ConsentedProjectHook, CoshConfig, HealthConfig};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(super) enum ScanContext {
    Resources,
    LoginBanner,
    Startup,
    OnDemand,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct EnvironmentSnapshot {
    adapter: String,
    language: String,
    approval_mode: String,
    trusted_roots: Vec<PathBuf>,
    consented_hooks: Vec<(PathBuf, Option<String>)>,
    pub(super) cwd: PathBuf,
}

impl EnvironmentSnapshot {
    pub(super) fn capture(config: &CoshConfig, cwd: &std::path::Path) -> Self {
        Self {
            adapter: config.adapter_default.clone(),
            language: config.language.clone(),
            approval_mode: config.approval_mode.label().to_string(),
            trusted_roots: config.trusted_project_roots.clone(),
            consented_hooks: config
                .consented_project_hooks
                .iter()
                .map(|hook| (hook.root.clone(), hook.digest.clone()))
                .collect(),
            cwd: cwd.to_path_buf(),
        }
    }

    pub(super) fn config(&self, health: HealthConfig) -> Option<CoshConfig> {
        let mut config = CoshConfig::default();
        config.adapter_default = self.adapter.clone();
        config.language = self.language.clone();
        config.approval_mode = crate::types::CoshApprovalMode::parse(&self.approval_mode)?;
        config.trusted_project_roots = self.trusted_roots.clone();
        config.consented_project_hooks = self
            .consented_hooks
            .iter()
            .map(|(root, digest)| ConsentedProjectHook {
                root: root.clone(),
                digest: digest.clone(),
            })
            .collect();
        config.health = health;
        Some(config)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(super) enum ConfigSource {
    #[default]
    CallerSnapshot,
    LoginConfigProjection,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WorkerRequest {
    pub(super) scan_id: String,
    pub(super) started_at_ms: u128,
    pub(super) now_epoch_seconds: u64,
    pub(super) deadline: JobDeadline,
    pub(super) context: ScanContext,
    pub(super) config: HealthConfig,
    pub(super) environment: Option<EnvironmentSnapshot>,
    #[serde(default)]
    pub(super) disabled: Vec<String>,
    #[serde(default)]
    pub(super) config_source: ConfigSource,
    #[serde(default)]
    pub(super) fixture_name: Option<String>,
}

pub(super) struct CheckSpec {
    pub(super) id: &'static str,
    pub(super) collector: HealthCollector,
    pub(super) source: super::super::plan::SampleSource,
    pub(super) budget_ms: u64,
    pub(super) enabled: bool,
    pub(super) scope: HealthFactScope,
    pub(super) coverage: HealthFactCoverage,
}

impl ScanContext {
    pub(super) fn policy_id(self) -> &'static str {
        match self {
            Self::LoginBanner => "login-banner",
            Self::Startup => "session-startup",
            Self::Resources | Self::OnDemand => "on-demand",
        }
    }
    pub(super) fn passive(self) -> bool {
        matches!(self, Self::LoginBanner | Self::Startup)
    }
}

pub(super) fn checks(request: &WorkerRequest) -> Vec<CheckSpec> {
    use HealthCollector::*;
    super::super::plan::CHECKS
        .iter()
        .map(|definition| {
            let collector = definition.collector;
            let environment = matches!(collector, Provider | Config | Hooks | Pty | Permissions);
            let default_enabled = match definition.id {
                "filesystem.mounts" if request.context.passive() => {
                    request.context == ScanContext::Startup
                        && request
                            .config
                            .scan_mounts
                            .as_ref()
                            .is_some_and(|mounts| !mounts.is_empty())
                }
                _ => true,
            };
            let selected = request
                .config
                .checks
                .get(definition.id)
                .copied()
                .unwrap_or(default_enabled)
                && !request.disabled.iter().any(|id| id == definition.id)
                && !request
                    .config
                    .policies
                    .get(request.context.policy_id())
                    .is_some_and(|disabled| disabled.iter().any(|id| id == definition.id));
            let scope_allowed = match definition.id {
                "filesystem.root" if !request.context.passive() => request
                    .config
                    .scan_mounts
                    .as_ref()
                    .is_none_or(|mounts| mounts.iter().any(|path| path == "/")),
                "filesystem.mounts" if request.context.passive() => request
                    .config
                    .scan_mounts
                    .as_ref()
                    .is_some_and(|mounts| mounts.iter().any(|path| path != "/")),
                "filesystem.mounts" => request
                    .config
                    .scan_mounts
                    .as_ref()
                    .is_none_or(|mounts| mounts.iter().any(|path| path != "/")),
                _ => true,
            };
            let enabled = selected
                && scope_allowed
                && if environment {
                    request.context == ScanContext::OnDemand && request.environment.is_some()
                } else {
                    request.config.enabled
                        && (!request.context.passive() || definition.passive)
                        && (collector != ConfiguredService || !request.config.services.is_empty())
                };
            let scope = if environment {
                HealthFactScope::Environment
            } else if definition.id == "filesystem.root" {
                HealthFactScope::Path("/".into())
            } else if matches!(collector, Disk | KernelSignal) {
                HealthFactScope::Unknown
            } else {
                HealthFactScope::Host
            };
            let coverage = if collector == KernelSignal {
                HealthFactCoverage::ReadWindow {
                    since_ms: None,
                    until_ms: Some(u128::from(request.now_epoch_seconds) * 1000),
                    max_records: 200,
                    complete: false,
                }
            } else {
                HealthFactCoverage::Snapshot
            };
            CheckSpec {
                id: definition.id,
                collector,
                source: definition.source,
                budget_ms: if request.context.passive() {
                    definition.budget_ms
                } else if collector == ConfiguredService {
                    800
                } else {
                    750
                },
                enabled,
                scope,
                coverage,
            }
        })
        .collect()
}

pub(super) fn sources(
    request: &WorkerRequest,
) -> std::collections::BTreeSet<super::super::plan::SampleSource> {
    checks(request)
        .into_iter()
        .filter(|check| check.enabled)
        .map(|check| check.source)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn memory_and_swap_are_separate_checks_of_one_collector() {
        let request = WorkerRequest {
            scan_id: "health-1".into(),
            started_at_ms: 1,
            now_epoch_seconds: 1,
            deadline: JobDeadline::after(Duration::from_secs(1)).unwrap(),
            context: ScanContext::Resources,
            config: HealthConfig::default(),
            environment: None,
            disabled: Vec::new(),
            config_source: crate::diagnostics::health::runtime::plan::ConfigSource::CallerSnapshot,
            fixture_name: None,
        };
        assert_eq!(
            checks(&request)
                .iter()
                .filter(|check| check.collector == HealthCollector::Memory && check.enabled)
                .count(),
            2
        );
    }

    #[test]
    fn active_explicit_mount_scope_does_not_silently_add_root() {
        let request = WorkerRequest {
            scan_id: "health-1".into(),
            started_at_ms: 1,
            now_epoch_seconds: 1,
            deadline: JobDeadline::after(Duration::from_secs(1)).unwrap(),
            context: ScanContext::Resources,
            config: HealthConfig {
                scan_mounts: Some(vec!["/data".into()]),
                ..HealthConfig::default()
            },
            environment: None,
            disabled: Vec::new(),
            config_source: ConfigSource::CallerSnapshot,
            fixture_name: None,
        };
        assert!(
            !checks(&request)
                .iter()
                .find(|check| check.id == "filesystem.root")
                .unwrap()
                .enabled
        );
        assert!(
            checks(&request)
                .iter()
                .find(|check| check.id == "filesystem.mounts")
                .unwrap()
                .enabled
        );
    }
}
