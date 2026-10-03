pub(crate) const APPROVAL_ZH_FORBIDDEN_UI: &[&str] = &[
    "Approval required",
    "Subject: Bash",
    "Tool input:",
    "Allow once",
    "Always trust",
    "Approved req-1",
    "Bash tool sent to shell",
    "Approval details",
    "Approval journal",
    "Policy: user approval is required",
    "Keys:",
    "Command block:",
    "Redaction: ref_only",
];

pub(crate) const DETAILS_ZH_FORBIDDEN_UI: &[&str] = &[
    "Activity details",
    "Details unavailable",
    " is not available; use a Details action",
    "Run:",
    "Detail:",
    "Tool output - stdout captured; [Details]",
];

pub(crate) const PROVIDER_NATIVE_ZH_FORBIDDEN_UI: &[&str] = &[
    "Provider-native shell tool allowed",
    "Read-only tools auto-approved; risky requests need confirmation.",
    "run_shell_command requested; [Details]",
    "Tool output - stdout captured; [Details]",
];

pub(crate) const RENDERER_ZH_FORBIDDEN_UI: &[&str] = &[
    "╭ Agent ─",
    "│ ┌ code:",
    "│ ┌ table",
    "No selectable recommendation",
    "No selectable recommendation is available yet",
];

pub(crate) const QUESTION_ZH_FORBIDDEN_UI: &[&str] = &[
    "Agent question",
    "Select one:",
    "Left/Right move",
    "Answer:",
    "Answer sent",
    "Sent to Agent",
];

pub(crate) const SLASH_CONFIG_ZH_FORBIDDEN_UI: &[&str] = &[
    "Slash commands",
    "Slash command hint",
    "Unknown slash command",
    "Did you mean /help?",
    "Use /help to see available commands.",
    "User mode",
    "Invalid language",
    "Unknown config key",
    "Config saved",
    "language is a persistent config",
    "Use /config language [auto|en-US|zh-CN].",
    "│ Registry",
    "组稿一次性 Agent 请求",
    "多行组稿",
    "可组稿多行",
];

pub(crate) const MODE_ZH_FORBIDDEN_UI: &[&str] = &[
    "Trust confirmation required",
    "Trust mode auto-approves provider tool requests",
    "Run /mode approval trust confirm to enable it explicitly.",
    "Recommend or auto mode remains active until confirmation.",
    "User mode",
    "Current: auto",
    "Explain and suggest only",
    "Read-only auto-approved; risky needs confirmation",
    "All tools auto-approved with audit trail",
    "Keys: Left/Right select",
    "Mode set to trust.",
    "Mode set to recommend.",
    "Hooks evaluate on failure; Agent auto-triggered for failed commands.",
    "Hooks and automatic analysis disabled; use slash commands to trigger.",
];

pub(crate) fn assert_no_migrated_english_ui_labels(output: &str, labels: &[&str]) {
    for label in labels {
        assert!(
            !output.contains(label),
            "migrated English UI label leaked: {label}\n{output}"
        );
    }
}
