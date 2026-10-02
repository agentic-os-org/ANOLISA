//! Neutral commands and events for Agent Runtime bridges.

use serde::{de, Deserialize, Deserializer, Serialize};
use thiserror::Error;

use crate::{
    capability::{BrokeredOperation, CapabilityRequest, DenialCode},
    common::{
        BoundedName, BoundedText, ContentPart, ContractHeader, ContractSchema, RuntimeBindingRef,
        WorkspaceRef,
    },
    error::ContractError,
    ids::{
        ApprovalId, CheckpointId, ExecutionId, InputRequestId, RequestId, RunId, RuntimeBindingId,
        RuntimeMessageId, TaskId, ToolUseId, TurnId,
    },
    task::CancelReason,
};

// Keep the wire families in one module namespace so this layout-only split
// neither changes public contract paths nor widens cross-family internals.
include!("runtime/input.rs");
include!("runtime/command.rs");
include!("runtime/brokered.rs");
include!("runtime/outcome.rs");
include!("runtime/event.rs");
include!("runtime/envelope.rs");

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        AgentRuntimeEvent, ExecutionAuthority, RuntimePermissionDecision, RuntimePermissionRef,
        ToolInvocationSnapshot, ToolInvocationStatus, ToolSummary, TurnLimit, TurnOutcome,
    };
    use crate::{
        common::{BoundedName, BoundedText},
        ids::{RequestId, RunId, RuntimeBindingId, ToolUseId, TurnId},
    };

    #[test]
    fn turn_limits_are_not_serialized_as_run_success() {
        let event = AgentRuntimeEvent::Completed {
            turn_id: TurnId::new(),
            outcome: TurnOutcome::LimitReached {
                limit: TurnLimit::Tokens,
            },
        };

        let value = serde_json::to_value(event).expect("turn event serializes");
        assert_eq!(value["event"], "completed");
        assert_eq!(value["outcome"]["outcome"], "limit_reached");
        assert_eq!(value["outcome"]["limit"], "tokens");
        assert_ne!(value["outcome"], json!({"outcome": "succeeded"}));
    }

    #[test]
    fn tool_snapshot_records_observation_only_authority() {
        let event = AgentRuntimeEvent::ToolInvocationUpdated {
            snapshot: ToolInvocationSnapshot {
                turn_id: TurnId::new(),
                tool_use_id: ToolUseId::new(),
                revision: 1,
                summary: ToolSummary {
                    name: BoundedName::new("execute").expect("test name is bounded"),
                    summary: BoundedText::new("Run tests").expect("test text is bounded"),
                },
                status: ToolInvocationStatus::Pending,
                authority: ExecutionAuthority::ProviderNativeObserved,
            },
        };

        let value = serde_json::to_value(event).expect("tool event serializes");
        assert_eq!(value["snapshot"]["authority"], "provider_native_observed");
        assert_ne!(value["snapshot"]["authority"], "cosh_brokered");
    }

    #[test]
    fn provider_native_allow_never_serializes_a_cosh_permit() {
        let value = serde_json::to_value(RuntimePermissionDecision::ProviderNativeAllowOnce)
            .expect("provider decision serializes");
        assert_eq!(value["decision"], "provider_native_allow_once");
        assert!(value.get("permit_id").is_none());
    }

    #[test]
    fn runtime_native_allow_is_distinct_from_provider_native_allow() {
        let value = serde_json::to_value(RuntimePermissionDecision::RuntimeNativeAllowOnce)
            .expect("Runtime decision serializes");
        assert_eq!(value["decision"], "runtime_native_allow_once");
        assert_ne!(value["decision"], "provider_native_allow_once");
        assert!(value.get("permit_id").is_none());
    }

    #[test]
    fn legacy_permission_refs_deserialize_without_new_callback_authority() {
        let legacy = RuntimePermissionRef {
            binding_id: RuntimeBindingId::new(),
            runtime_generation: 1,
            event_sequence: 2,
            run_id: RunId::new(),
            turn_id: TurnId::new(),
            tool_use_id: None,
            request_id: RequestId::new(),
            callback: None,
            core_callback: None,
        };
        let value = serde_json::to_value(&legacy).expect("legacy ref serializes");
        assert!(value.get("callback").is_none());

        let decoded: RuntimePermissionRef =
            serde_json::from_value(value).expect("legacy ref remains readable");
        assert!(decoded.callback.is_none());
        assert!(decoded.core_callback.is_none());
    }
}
