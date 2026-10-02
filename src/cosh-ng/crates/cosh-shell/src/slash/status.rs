//! Read-only runtime identity and session statistics slash commands.
//!
//! `/status` is the canonical command and `/about` is its compatibility alias.
//! `/stats` reports only metrics that cosh-shell owns today. Provider token and
//! latency telemetry are called out as unavailable instead of being inferred.

use std::collections::BTreeMap;

use crate::activity::runtime::{ToolInvocationPhase, ToolInvocationRecord};
use crate::runtime::prelude::*;
use crate::slash::panel::render_notice_panel;

#[derive(Debug, Default, PartialEq, Eq)]
struct RuntimeIdentity {
    provider_id: Option<String>,
    provider_type: Option<String>,
    model: Option<String>,
    provider_details_available: bool,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct ToolStats {
    calls: usize,
    successful: usize,
    failed: usize,
    pending: usize,
}

pub(super) fn render_status_command<W: Write>(
    adapter: &AdapterInstance,
    state: &InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    let i18n = state.i18n();
    let identity = runtime_identity(adapter, state);
    let provider = provider_display(&identity)
        .unwrap_or_else(|| i18n.t(MessageId::SlashValueUnavailable).to_string());
    let model = identity
        .model
        .as_deref()
        .unwrap_or_else(|| i18n.t(MessageId::SlashValueUnavailable));
    let provider_session = adapter.committed_session_id();
    let session = provider_session
        .as_deref()
        .or(state.shell_session_id.as_deref())
        .unwrap_or_else(|| i18n.t(MessageId::SlashValueNotStarted));
    let os = format!("{} {}", std::env::consts::OS, std::env::consts::ARCH);

    let mut body = vec![
        i18n.format(
            MessageId::SlashStatusVersionLine,
            &[("version", env!("CARGO_PKG_VERSION"))],
        ),
        i18n.format(
            MessageId::SlashStatusBackendLine,
            &[("backend", adapter.name())],
        ),
        i18n.format(
            MessageId::SlashStatusProviderLine,
            &[("provider", &provider)],
        ),
        i18n.format(MessageId::SlashStatusModelLine, &[("model", model)]),
        i18n.format(MessageId::SlashStatusSessionLine, &[("session", session)]),
        i18n.format(MessageId::SlashStatusOsLine, &[("os", &os)]),
        i18n.format(
            MessageId::SlashStatusModesLine,
            &[
                ("approval", state.approval_mode.label()),
                ("analysis", state.analysis_mode.label()),
            ],
        ),
    ];
    if !identity.provider_details_available {
        body.push(
            i18n.t(MessageId::SlashStatusProviderUnavailableLine)
                .to_string(),
        );
    }

    render_notice_panel(
        output,
        i18n.t(MessageId::SlashStatusTitle),
        body,
        Some(i18n.t(MessageId::SlashStatusFooter)),
    )
}

pub(super) fn render_stats_command<W: Write>(
    arguments: &str,
    adapter: &AdapterInstance,
    state: &InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    match arguments.split_whitespace().collect::<Vec<_>>().as_slice() {
        [] => render_stats_summary(adapter, state, output),
        ["model"] => render_model_stats(adapter, state, output),
        ["tools"] => render_tool_stats(state, output),
        _ => render_stats_usage(state, output),
    }
}

fn render_stats_summary<W: Write>(
    adapter: &AdapterInstance,
    state: &InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    let i18n = state.i18n();
    let identity = runtime_identity(adapter, state);
    let model = identity
        .model
        .as_deref()
        .unwrap_or_else(|| i18n.t(MessageId::SlashValueUnavailable));
    let active_run_id = state
        .agent_run
        .active
        .as_ref()
        .map(|run| run.request.id.as_str());
    let totals = aggregate_tool_stats(&state.activity.tool_invocations, active_run_id);
    let run_state = if state.agent_run.active.is_some() {
        i18n.t(MessageId::SlashValueActive)
    } else {
        i18n.t(MessageId::SlashValueIdle)
    };
    let body = vec![
        i18n.format(MessageId::SlashStatsModelLine, &[("model", model)]),
        i18n.format(MessageId::SlashStatsRunStateLine, &[("state", run_state)]),
        tool_totals_line(i18n, &totals),
    ];

    render_notice_panel(
        output,
        i18n.t(MessageId::SlashStatsTitle),
        body,
        Some(i18n.t(MessageId::SlashStatsFooter)),
    )
}

fn render_model_stats<W: Write>(
    adapter: &AdapterInstance,
    state: &InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    let i18n = state.i18n();
    let identity = runtime_identity(adapter, state);
    let model = identity
        .model
        .as_deref()
        .unwrap_or_else(|| i18n.t(MessageId::SlashValueUnavailable));
    let run_state = if state.agent_run.active.is_some() {
        i18n.t(MessageId::SlashValueActive)
    } else {
        i18n.t(MessageId::SlashValueIdle)
    };
    let body = vec![
        i18n.format(MessageId::SlashStatsModelLine, &[("model", model)]),
        i18n.format(
            MessageId::SlashStatsBackendLine,
            &[("backend", adapter.name())],
        ),
        i18n.format(MessageId::SlashStatsRunStateLine, &[("state", run_state)]),
        i18n.t(MessageId::SlashStatsTelemetryUnavailable)
            .to_string(),
    ];

    render_notice_panel(
        output,
        i18n.t(MessageId::SlashStatsModelTitle),
        body,
        Some(i18n.t(MessageId::SlashStatsFooter)),
    )
}

fn render_tool_stats<W: Write>(state: &InlineState, output: &mut W) -> std::io::Result<()> {
    let i18n = state.i18n();
    let active_run_id = state
        .agent_run
        .active
        .as_ref()
        .map(|run| run.request.id.as_str());
    let by_name = tool_stats_by_name(&state.activity.tool_invocations, active_run_id);
    let mut body = Vec::new();
    if by_name.is_empty() {
        body.push(i18n.t(MessageId::SlashStatsNoToolCalls).to_string());
    } else {
        for (name, stats) in &by_name {
            body.push(i18n.format(
                MessageId::SlashStatsToolRow,
                &[
                    ("name", name),
                    ("calls", &stats.calls.to_string()),
                    ("successful", &stats.successful.to_string()),
                    ("failed", &stats.failed.to_string()),
                    ("pending", &stats.pending.to_string()),
                ],
            ));
        }
        let totals = by_name
            .values()
            .fold(ToolStats::default(), |mut total, item| {
                total.calls += item.calls;
                total.successful += item.successful;
                total.failed += item.failed;
                total.pending += item.pending;
                total
            });
        body.push(tool_totals_line(i18n, &totals));
    }

    render_notice_panel(
        output,
        i18n.t(MessageId::SlashStatsToolsTitle),
        body,
        Some(i18n.t(MessageId::SlashStatsFooter)),
    )
}

fn render_stats_usage<W: Write>(state: &InlineState, output: &mut W) -> std::io::Result<()> {
    render_notice_panel(
        output,
        state.i18n().t(MessageId::SlashStatsTitle),
        vec![state.i18n().t(MessageId::SlashStatsUsageLine).to_string()],
        Some(state.i18n().t(MessageId::SlashStatsFooter)),
    )
}

fn tool_totals_line(i18n: I18n, totals: &ToolStats) -> String {
    i18n.format(
        MessageId::SlashStatsToolTotalsLine,
        &[
            ("calls", &totals.calls.to_string()),
            ("successful", &totals.successful.to_string()),
            ("failed", &totals.failed.to_string()),
            ("pending", &totals.pending.to_string()),
        ],
    )
}

fn aggregate_tool_stats(
    records: &[ToolInvocationRecord],
    active_run_id: Option<&str>,
) -> ToolStats {
    tool_stats_by_name(records, active_run_id)
        .into_values()
        .fold(ToolStats::default(), |mut total, item| {
            total.calls += item.calls;
            total.successful += item.successful;
            total.failed += item.failed;
            total.pending += item.pending;
            total
        })
}

fn tool_stats_by_name(
    records: &[ToolInvocationRecord],
    active_run_id: Option<&str>,
) -> BTreeMap<String, ToolStats> {
    let mut stats = BTreeMap::<String, ToolStats>::new();
    for record in records {
        let item = stats.entry(record.tool_name.clone()).or_default();
        record_tool_outcome(
            item,
            record.phase,
            &record.status,
            active_run_id == Some(record.run_id.as_str()),
        );
    }
    stats
}

fn record_tool_outcome(
    stats: &mut ToolStats,
    phase: ToolInvocationPhase,
    status: &str,
    call_belongs_to_active_run: bool,
) {
    stats.calls += 1;
    match phase {
        ToolInvocationPhase::Call if call_belongs_to_active_run => stats.pending += 1,
        ToolInvocationPhase::Call => stats.failed += 1,
        ToolInvocationPhase::Result
            if matches!(
                status,
                "error" | "failed" | "interrupted" | "denied" | "cancelled"
            ) =>
        {
            stats.failed += 1;
        }
        ToolInvocationPhase::Result => stats.successful += 1,
    }
}

fn runtime_identity(adapter: &AdapterInstance, state: &InlineState) -> RuntimeIdentity {
    let observed_model = state.personalization.foreground_model.clone();
    match adapter {
        AdapterInstance::Fake(_) => RuntimeIdentity {
            provider_id: Some("fake".to_string()),
            provider_type: Some("test".to_string()),
            model: observed_model.or_else(|| Some("fake".to_string())),
            provider_details_available: true,
        },
        AdapterInstance::ClaudeCode(claude) => RuntimeIdentity {
            provider_id: Some("claude-code".to_string()),
            provider_type: Some("Claude Code".to_string()),
            model: observed_model.or_else(|| Some(claude.model.clone())),
            provider_details_available: true,
        },
        AdapterInstance::QwenCli(_) => RuntimeIdentity {
            provider_id: Some("co".to_string()),
            provider_type: Some("Qwen CLI".to_string()),
            model: observed_model,
            provider_details_available: true,
        },
        AdapterInstance::CoshCore(core) => {
            match core.registry_query("auth", "state", serde_json::Value::Null) {
                Ok(value) => core_identity_from_auth_state(&value, observed_model),
                Err(_) => RuntimeIdentity {
                    provider_id: Some("cosh-core".to_string()),
                    model: observed_model,
                    ..RuntimeIdentity::default()
                },
            }
        }
    }
}

fn core_identity_from_auth_state(
    value: &serde_json::Value,
    observed_model: Option<String>,
) -> RuntimeIdentity {
    let provider_id = value
        .get("active_provider")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    let provider = provider_id.as_deref().and_then(|active| {
        value
            .get("saved_providers")
            .and_then(serde_json::Value::as_array)
            .and_then(|providers| {
                providers.iter().find(|provider| {
                    provider
                        .get("provider_id")
                        .and_then(serde_json::Value::as_str)
                        == Some(active)
                })
            })
    });
    let provider_type = provider
        .and_then(|provider| provider.get("provider_type"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    let configured_model = provider
        .and_then(|provider| provider.get("model"))
        .and_then(serde_json::Value::as_str)
        .filter(|model| !model.trim().is_empty())
        .map(str::to_string);

    RuntimeIdentity {
        provider_details_available: provider_id.is_none() || provider.is_some(),
        provider_id,
        provider_type,
        model: observed_model.or(configured_model),
    }
}

fn provider_display(identity: &RuntimeIdentity) -> Option<String> {
    let provider_id = identity.provider_id.as_deref()?;
    let Some(provider_type) = identity.provider_type.as_deref() else {
        return Some(provider_id.to_string());
    };
    if provider_type == provider_id {
        Some(provider_id.to_string())
    } else {
        Some(format!("{provider_id} ({provider_type})"))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        core_identity_from_auth_state, provider_display, record_tool_outcome, RuntimeIdentity,
        ToolStats,
    };
    use crate::activity::runtime::ToolInvocationPhase;

    #[test]
    fn core_identity_uses_active_provider_and_observed_model() {
        let value = serde_json::json!({
            "active_provider": "prod",
            "saved_providers": [
                {"provider_id": "other", "provider_type": "mock", "model": "wrong"},
                {
                    "provider_id": "prod",
                    "provider_type": "openai_compat",
                    "model": "configured-model"
                }
            ]
        });

        let identity = core_identity_from_auth_state(&value, Some("observed-model".to_string()));
        assert_eq!(
            identity,
            RuntimeIdentity {
                provider_id: Some("prod".to_string()),
                provider_type: Some("openai_compat".to_string()),
                model: Some("observed-model".to_string()),
                provider_details_available: true,
            }
        );
        assert_eq!(
            provider_display(&identity).as_deref(),
            Some("prod (openai_compat)")
        );
    }

    #[test]
    fn core_identity_falls_back_to_configured_model() {
        let value = serde_json::json!({
            "active_provider": "aliyun",
            "saved_providers": [{
                "provider_id": "aliyun",
                "provider_type": "aliyun",
                "model": "qwen3.7-plus"
            }]
        });

        let identity = core_identity_from_auth_state(&value, None);
        assert_eq!(identity.model.as_deref(), Some("qwen3.7-plus"));
        assert_eq!(provider_display(&identity).as_deref(), Some("aliyun"));
    }

    #[test]
    fn missing_active_provider_is_a_valid_unconfigured_state() {
        let identity =
            core_identity_from_auth_state(&serde_json::json!({"saved_providers": []}), None);

        assert_eq!(identity.provider_id, None);
        assert!(identity.provider_details_available);
    }

    #[test]
    fn tool_stats_only_leave_current_run_calls_pending() {
        let mut stats = ToolStats::default();
        record_tool_outcome(&mut stats, ToolInvocationPhase::Call, "called", true);
        record_tool_outcome(&mut stats, ToolInvocationPhase::Call, "called", false);
        record_tool_outcome(&mut stats, ToolInvocationPhase::Result, "completed", false);
        record_tool_outcome(
            &mut stats,
            ToolInvocationPhase::Result,
            "interrupted",
            false,
        );

        assert_eq!(stats.calls, 4);
        assert_eq!(stats.successful, 1);
        assert_eq!(stats.failed, 2);
        assert_eq!(stats.pending, 1);
    }
}
