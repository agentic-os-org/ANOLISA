#[macro_use]
mod startup;
#[macro_use]
mod help;
#[macro_use]
mod config;
#[macro_use]
mod hooks;
#[macro_use]
mod hook_action;
#[macro_use]
mod debug;
#[macro_use]
mod modes;
#[macro_use]
mod agent;
#[macro_use]
mod insight;
#[macro_use]
mod hook_details;
#[macro_use]
mod activity;
#[macro_use]
mod recommendation;
#[macro_use]
mod health;
#[macro_use]
mod question;
#[macro_use]
mod approval;
#[macro_use]
mod session;
#[macro_use]
mod auth;

macro_rules! define_message_id {
    ($($id:ident,)*) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum MessageId {
            $($id,)*
        }

        impl MessageId {
            pub const ALL: &'static [MessageId] = &[
                $(MessageId::$id,)*
            ];
        }
    };
}

macro_rules! collect_message_ids {
    ([$segment:ident $(, $remaining:ident)* $(,)?], $($ids:ident,)*) => {
        $segment!(collect_message_ids, [$($remaining),*], $($ids,)*);
    };
    ([], $($ids:ident,)*) => {
        define_message_id!($($ids,)*);
    };
}

// Segment order preserves the public fieldless enum's existing discriminants.
collect_message_ids!([
    startup_ids,
    help_core_ids,
    config_ids,
    hooks_command_ids,
    debug_ids,
    legacy_approval_mode_ids,
    removed_command_ids,
    mode_ids,
    agent_ids,
    hook_insight_ids,
    agent_queue_ids,
    hook_details_ids,
    activity_ids,
    recommendation_ids,
    health_ids,
    tool_summary_ids,
    question_ids,
    approval_ids,
    help_session_ids,
    session_ids,
    help_registry_ids,
    session_compaction_ids,
    compaction_queue_ids,
    slash_parse_error_ids,
    question_hardening_ids,
    question_interaction_ids,
    session_picker_ids,
    session_fresh_ids,
    approval_reason_ids,
    routing_insight_ids,
    activity_untracked_ids,
    approval_turn_consent_ids,
    status_query_ids,
    prompt_soft_newline_ids,
    tool_argument_status_ids,
    multiline_entry_ids,
    mcp_registry_ids,
    approval_foreground_interactive_ids,
    approval_turn_extension_ids,
    capture_notice_ids,
    agent_recovery_reason_ids,
    auth_ids,
    agent_recovery_retry_ids,
    hook_notification_ids,
    approval_system_control_ids,
    input_wait_hint_ids,
    startup_auth_hint_ids,
    session_picker_footer_ids,
    agent_composer_ids,
    approval_trust_catalog_ids,
    hook_action_ids,
    enhanced_routing_mode_ids,
    managed_task_ids,
    composer_control_submit_ids,
    auth_ecs_ids,
],);
