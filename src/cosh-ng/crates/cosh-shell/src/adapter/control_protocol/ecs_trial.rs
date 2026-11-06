use crate::types::{EcsTrialAction, EcsTrialContext, EcsTrialProgress};
use serde_json::{json, Value};

pub(crate) fn parse_progress(line: &str) -> Option<Result<EcsTrialProgress, String>> {
    let value: Value = serde_json::from_str(line).ok()?;
    if value.get("type").and_then(Value::as_str) != Some("ecs_trial_state") {
        return None;
    }
    Some(
        serde_json::from_value::<EcsTrialProgress>(value)
            .map_err(|error| format!("invalid ECS trial state: {error}"))
            .and_then(|progress| {
                let phase_valid = matches!(
                    progress.phase.as_str(),
                    "preparing"
                        | "waiting_activation"
                        | "credits_preparing"
                        | "ready"
                        | "paused"
                        | "failed"
                        | "terminated"
                );
                let dispatch_valid = matches!(
                    progress.dispatch_state.as_str(),
                    "not_dispatched" | "accepted" | "unknown"
                );
                if progress.run_id.is_empty()
                    || progress.flow_id.is_empty()
                    || !phase_valid
                    || !dispatch_valid
                    || (progress.resume_allowed
                        && (!progress.local_terminated
                            || progress.dispatch_state != "not_dispatched"))
                {
                    Err("invalid ECS trial state fields".into())
                } else {
                    Ok(progress)
                }
            }),
    )
}

pub(crate) fn serialize_control(progress: &EcsTrialProgress, action: EcsTrialAction) -> String {
    json!({"type":"ecs_trial_control","run_id":progress.run_id,"flow_id":progress.flow_id,"revision":progress.revision,"action":action}).to_string()
}

pub(crate) fn attach_context(
    message: &str,
    context: Option<&EcsTrialContext>,
    capable: bool,
) -> String {
    if !capable || context.is_none() {
        return message.to_string();
    }
    let Ok(mut value) = serde_json::from_str::<Value>(message) else {
        return message.to_string();
    };
    value["ecs_trial_context"] = json!(context);
    value.to_string()
}

#[cfg(test)]
mod tests {
    #[test]
    fn malformed_trial_progress_fails_closed() {
        let progress = serde_json::json!({"type":"ecs_trial_state","run_id":"opaque","flow_id":"flow","revision":1,"phase":"future_phase","dispatch_state":"not_dispatched","local_terminated":false,"resume_allowed":false});
        assert!(super::parse_progress(&progress.to_string())
            .unwrap()
            .is_err());
        let mut progress = progress;
        progress["phase"] = "paused".into();
        progress["dispatch_state"] = "unknown".into();
        progress["resume_allowed"] = true.into();
        assert!(super::parse_progress(&progress.to_string())
            .unwrap()
            .is_err());
    }

    #[test]
    fn cosh_initialize_declares_ecs_trial_without_changing_foreign_initialize() {
        let cosh: serde_json::Value =
            serde_json::from_str(&super::super::serialize_cosh_core_initialize("init")).unwrap();
        assert_eq!(
            cosh["request"]["capabilities"]["can_handle_ecs_trial_v1"],
            true
        );
        let foreign: serde_json::Value =
            serde_json::from_str(&super::super::serialize_initialize("init")).unwrap();
        assert!(foreign["request"]["capabilities"].is_null());
    }
}
