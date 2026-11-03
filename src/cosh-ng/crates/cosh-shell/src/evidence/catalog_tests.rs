use super::redaction::{provider_safe_command_facts, redact_sensitive_text};
use crate::types::{CommandBlock, CommandStatus, OutputRefs};

#[test]
fn redaction_boundaries_catalog_forms_and_precision() {
    for key in [
        "secret_key",
        "secret_access_key",
        "private_key",
        "access_key",
        "auth_token",
        "session_token",
        "credentials",
        "credential",
        "bearer",
    ] {
        for input in [
            format!("{key}=catalog-canary-material"),
            format!(
                "tool --{} 'catalog canary words' --region cn",
                key.replace('_', "-")
            ),
        ] {
            let (safe, changed) = redact_sensitive_text(&input);
            assert!(
                changed
                    && !safe.contains("catalog")
                    && !safe.contains("canary")
                    && !safe.contains("words"),
                "catalog masking failed"
            );
        }
    }
    let ordinary =
        "cargo test --region cn tokenizer=ordinary_value api_key_id_shape=ordinary_value";
    assert!(
        redact_sensitive_text(ordinary) == (ordinary.to_string(), false),
        "ordinary command changed"
    );
}

#[test]
fn redaction_boundaries_actual_command_facts() {
    let block = CommandBlock {
        id: "catalog".to_string(), session_id: "catalog-session".to_string(),
        command: "tool --auth-token facts-canary --access-key facts-access --region cn SECRET_KEY=facts-assignment".to_string(),
        origin: Default::default(), cwd: "/ordinary".to_string(), end_cwd: "/ordinary".to_string(),
        started_at_ms: 0, ended_at_ms: 1, duration_ms: 1, exit_code: 0,
        status: CommandStatus::Completed,
        output: OutputRefs { terminal_output_ref: None, terminal_output_bytes: 0 },
        shell_environment_generation: None, audit_identity: None,
    };
    let facts = provider_safe_command_facts(&block);
    assert!(
        !facts.command.contains("facts-"),
        "provider facts masking failed"
    );
    assert!(
        facts.command.contains("--region cn") && facts.cwd == "/ordinary",
        "ordinary facts changed"
    );
}

#[test]
fn redaction_boundaries_actual_output_preview() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("output.txt");
    std::fs::write(&path, "SECRET_KEY=preview-canary-material\nprivate_key='preview canary words'\n--auth-token preview-flag-canary\naccess_key=preview-access-canary\nordinary-preview-control").unwrap();
    let preview = super::output_text::provider_output_preview(
        path.to_str(),
        "terminal-output://catalog/preview",
    );
    let text = preview.text.expect("actual preview produced");
    assert!(!text.contains("canary"), "output preview masking failed");
    assert!(
        text.contains("ordinary-preview-control")
            && preview.complete
            && preview.redaction_status == "preview_redacted",
        "ordinary preview changed"
    );
}
