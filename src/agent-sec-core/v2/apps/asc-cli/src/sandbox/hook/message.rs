//! Present only the confirmation requirements bound to the final sandbox invocation.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use asc_daemon_protocol::authorization::{
    ApprovalTarget, AuthorizationRequest, Decision, DecisionVerdict, PathFact,
};
use asc_os_permission_types::{FileRight, FilesystemPermissions, NetworkMode, RuleEffect};

pub(super) fn format(
    request: &AuthorizationRequest,
    decision: &Decision,
    facts: &[PathFact],
) -> String {
    if decision.verdict == DecisionVerdict::StepUp {
        return approval(decision, facts);
    }
    let mut lines = vec![match decision.verdict {
        DecisionVerdict::Allow => "Sandbox: command allowed.".into(),
        DecisionVerdict::Modify => "Sandbox: policy adjusted this command's execution.".into(),
        DecisionVerdict::StepUp => unreachable!("approval was rendered above"),
        DecisionVerdict::Deny => {
            format!(
                "Sandbox blocked execution: {}",
                display_text(&decision.reason)
            )
        }
        DecisionVerdict::Defer => format!(
            "Sandbox cannot run this command yet: {}",
            display_text(&decision.reason)
        ),
    }];
    for warning in &decision.warnings {
        lines.push(format!("Warning: {}", display_text(warning)));
    }
    if matches!(
        decision.verdict,
        DecisionVerdict::Deny | DecisionVerdict::Defer
    ) {
        return lines.join("\n");
    }

    let changed = decision.verdict == DecisionVerdict::Modify;
    // Other hooks may still ask; Cosh's main preview contains only the handoff wrapper.
    let command = command(decision);
    lines.push(format!("Final command: {command}"));
    lines.push(format!("Directory: {}", display_text(&decision.launch.cwd)));
    if let Some(permissions) = &decision.effective_permissions {
        let requested = &request.requested_permissions;
        if changed && permissions.network != requested.network {
            lines.push(match permissions.network.mode {
                NetworkMode::Off => "Network: blocked (IPv4/IPv6).".into(),
                NetworkMode::Full => "Network: allowed (host network).".into(),
            });
        }
        if changed && permissions.filesystem != requested.filesystem {
            file_access(&mut lines, &permissions.filesystem);
        }
        if changed && permissions.execution != requested.execution {
            for path in &permissions.execution.deny_paths {
                lines.push(format!("No direct execution: {}", display_text(path)));
            }
        }
    }
    lines.join("\n")
}

fn command(decision: &Decision) -> String {
    std::iter::once(&decision.launch.resolved_program)
        .chain(&decision.launch.args)
        .map(|arg| quote_argument(arg))
        .collect::<Vec<_>>()
        .join(" ")
}

fn approval(decision: &Decision, facts: &[PathFact]) -> String {
    let mut items: BTreeMap<&ApprovalTarget, BTreeSet<&str>> = BTreeMap::new();
    for item in &decision.approval_requirements {
        items.entry(&item.target).or_default().insert(
            item.message
                .as_deref()
                .filter(|message| !message.trim().is_empty())
                .unwrap_or("策略要求人工确认。"),
        );
    }
    let execution_only = items
        .keys()
        .all(|target| matches!(target, ApprovalTarget::Execution { .. }));
    let mut lines = vec![
        if execution_only {
            "允许本次执行吗？"
        } else {
            "允许本次命令使用以下权限吗？"
        }
        .into(),
        String::new(),
        format!("    {}", command(decision)),
    ];
    if items.is_empty() {
        // Older decisions lack per-item provenance; do not infer permissions from their reason.
        let reason = match decision.reason.as_str() {
            "" | "the final invocation requires explicit confirmation" => "策略要求人工确认。",
            reason => reason,
        };
        lines.push(String::new());
        lines.push(format!("原因：{}", display_text(reason)));
    }
    for (target, reasons) in items {
        lines.push(String::new());
        let original = matches!(
            target,
            ApprovalTarget::Execution {
                original_request: true
            }
        );
        match target {
            ApprovalTarget::Execution { .. } if !execution_only => lines.push("执行命令".into()),
            ApprovalTarget::Execution { .. } => {}
            ApprovalTarget::Filesystem {
                path,
                right,
                excluded_paths,
            } => {
                let action = match right {
                    FileRight::Read => "读",
                    FileRight::Write => "写",
                };
                lines.push(format!(
                    "{action}{} {}",
                    path_kind(path, facts),
                    display_text(path)
                ));
                for excluded in excluded_paths {
                    lines.push(format!("  不含：{}", display_text(excluded)));
                }
            }
            ApprovalTarget::Network => lines.push("联网（不限目标或端口）".into()),
        }
        let reason = reasons
            .into_iter()
            .map(display_text)
            .collect::<Vec<_>>()
            .join("；");
        let indent = if execution_only { "" } else { "  " };
        let label = if original {
            "原因（原请求）"
        } else {
            "原因"
        };
        lines.push(format!("{indent}{label}：{reason}"));
    }
    lines.join("\n")
}

fn path_kind(path: &str, facts: &[PathFact]) -> &'static str {
    if let Some(fact) = facts.iter().find(|fact| fact.path == path) {
        return if fact.is_directory {
            "目录"
        } else {
            "文件"
        };
    }
    // A policy boundary may be inside a coalesced mount. Stat only for its display label.
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => "目录",
        Ok(metadata) if metadata.is_file() => "文件",
        _ => "路径",
    }
}

fn file_access(lines: &mut Vec<String>, filesystem: &FilesystemPermissions) {
    lines.push("File access (other paths blocked; restrictions override grants):".into());
    // Show mutating access first; read grants are not necessarily read-only subtrees.
    for category in ["Read/write", "Read", "No access", "No writes"] {
        for rule in &filesystem.rules {
            let label = match rule.effect {
                RuleEffect::Allow if rule.rights.contains(&FileRight::Write) => "Read/write",
                RuleEffect::Allow => "Read",
                RuleEffect::Deny if rule.rights.contains(&FileRight::Read) => "No access",
                RuleEffect::Deny => "No writes",
            };
            if label == category {
                lines.push(format!("  {label}: {}", display_text(&rule.path)));
            }
        }
    }
}

fn quote_argument(value: &str) -> String {
    if !value.is_empty()
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_./-:=@%+,".contains(&c))
    {
        value.into()
    } else if value.chars().any(char::is_control) {
        // Bash ANSI-C quoting keeps multiline arguments exact without creating fake UI rows.
        let mut quoted = String::from("$'");
        for c in value.chars() {
            match c {
                '\\' => quoted.push_str("\\\\"),
                '\'' => quoted.push_str("\\'"),
                '\n' => quoted.push_str("\\n"),
                '\r' => quoted.push_str("\\r"),
                '\t' => quoted.push_str("\\t"),
                c if c.is_control() => {
                    // String's fmt::Write implementation cannot return an error.
                    write!(quoted, "\\u{:04x}", u32::from(c))
                        .expect("writing to a String cannot fail");
                }
                c => quoted.push(c),
            }
        }
        quoted.push('\'');
        quoted
    } else {
        super::shell_quote(value)
    }
}

fn display_text(value: &str) -> String {
    let mut text = String::new();
    for c in value.chars() {
        if c.is_control() || c == '\\' {
            text.extend(c.escape_default());
        } else {
            text.push(c);
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use asc_daemon_protocol::authorization::ApprovalRequirement;
    use serde_json::json;

    fn fixture(verdict: DecisionVerdict) -> (AuthorizationRequest, Decision) {
        let request: AuthorizationRequest = serde_json::from_value(json!({
            "type": "os", "request_id": "request", "execution_id": "execution",
            "principal": {"agent_type": "cosh"},
            "launch": {"program": "/bin/bash", "resolved_program": "/usr/bin/bash",
                "args": ["-c", "rm -rf cache"], "cwd": "/work"},
            "requested_permissions": {"version": 1,
                "filesystem": {"default": "deny", "rules": [
                    {"effect": "allow", "path": "/", "rights": ["read"]},
                    {"effect": "allow", "path": "/work", "rights": ["read", "write"]}
                ]}, "execution": {"deny_paths": []}, "network": {"mode": "off"}}
        }))
        .unwrap();
        let decision = Decision {
            decision_id: "decision".into(),
            request_id: request.request_id.clone(),
            execution_id: request.execution_id.clone(),
            request_digest: "digest".into(),
            policy_version: "policy".into(),
            expires_at: 0,
            principal: request.principal.clone(),
            launch: request.launch.clone(),
            verdict,
            effective_permissions: Some(request.requested_permissions.clone()),
            reason: "Review file removal".into(),
            approval_requirements: vec![],
            warnings: vec![],
            require_files: vec![],
        };
        (request, decision)
    }

    #[test]
    fn allow_and_warn_do_not_dump_permissions_or_request_approval() {
        let (request, mut decision) = fixture(DecisionVerdict::Allow);
        assert_eq!(
            format(&request, &decision, &[]),
            "Sandbox: command allowed.\nFinal command: /usr/bin/bash -c 'rm -rf cache'\nDirectory: /work"
        );
        decision.warnings.push("network disabled".into());
        assert_eq!(
            format(&request, &decision, &[]),
            "Sandbox: command allowed.\nWarning: network disabled\n\
             Final command: /usr/bin/bash -c 'rm -rf cache'\nDirectory: /work"
        );
    }

    #[test]
    fn execution_approval_indents_only_the_final_command_and_hides_other_permissions() {
        let (request, mut decision) = fixture(DecisionVerdict::StepUp);
        decision.launch.args[1] = "printf done > final".into();
        let permissions = decision.effective_permissions.as_mut().unwrap();
        permissions.filesystem.rules.extend(
            serde_json::from_value::<Vec<_>>(json!([
                {"effect": "deny", "path": "/private", "rights": ["read", "write"]},
                {"effect": "deny", "path": "/work/input", "rights": ["write"]}
            ]))
            .unwrap(),
        );
        permissions
            .execution
            .deny_paths
            .push("/work/downloads".into());
        decision
            .warnings
            .push("Review the replacement command".into());
        decision.approval_requirements.push(ApprovalRequirement {
            target: ApprovalTarget::Execution {
                original_request: true,
            },
            message: Some("Review file removal".into()),
        });
        let message = format(&request, &decision, &[]);
        assert_eq!(
            message,
            "允许本次执行吗？\n\n    /usr/bin/bash -c 'printf done > final'\n\n原因（原请求）：Review file removal"
        );
        assert!(!message.contains("rm -rf"));
        assert!(!message.contains("version"));
        assert!(!message.contains("/work"));
        assert!(!message.contains("/private"));
        assert!(!message.contains("Review the replacement"));
    }

    #[test]
    fn modification_shows_only_affected_permission_categories() {
        let (request, mut decision) = fixture(DecisionVerdict::Modify);
        decision.launch.cwd = "/elsewhere".into();
        let message = format(&request, &decision, &[]);
        assert!(message.contains("Final command:"));
        assert!(message.contains("Directory: /elsewhere"));
        assert!(!message.contains("Network:"));
        assert!(!message.contains("File access"));

        decision.launch = request.launch.clone();
        decision
            .effective_permissions
            .as_mut()
            .unwrap()
            .filesystem
            .rules[1]
            .rights = vec![FileRight::Read];
        let message = format(&request, &decision, &[]);
        assert!(message.contains("  Read: /work"));
        assert!(!message.contains("Read/write:"));
        assert!(message.contains("Final command:"));
        assert!(!message.contains("Network:"));
    }

    #[test]
    fn blocked_decisions_do_not_describe_permissions_as_granted() {
        for verdict in [DecisionVerdict::Deny, DecisionVerdict::Defer] {
            let (request, mut decision) = fixture(verdict);
            decision.effective_permissions = None;
            decision.reason = "No policy permits this command".into();
            decision.warnings.push("Contact your administrator".into());
            let message = format(&request, &decision, &[]);
            assert!(message.contains(&decision.reason));
            assert!(message.contains("Warning: Contact your administrator"));
            assert!(!message.contains("Final command:"));
            assert!(!message.contains("File access"));
        }
    }

    #[test]
    fn legacy_approval_preserves_reason_without_inventing_resource_items() {
        let (request, mut decision) = fixture(DecisionVerdict::StepUp);
        decision.reason = "请确认删除工作区缓存".into();
        let permissions = decision.effective_permissions.as_mut().unwrap();
        permissions.network.mode = NetworkMode::Full;
        permissions.filesystem.rules[0].rights = vec![FileRight::Write];
        let message = format(&request, &decision, &[]);
        assert_eq!(
            message,
            "允许本次执行吗？\n\n    /usr/bin/bash -c 'rm -rf cache'\n\n原因：请确认删除工作区缓存"
        );
    }

    fn fact(path: &str, is_directory: bool) -> PathFact {
        PathFact {
            path: path.into(),
            canonical_path: path.into(),
            is_directory,
            device: 1,
            inode: 1,
        }
    }

    fn file_item(path: &str, right: FileRight, message: &str) -> ApprovalRequirement {
        ApprovalRequirement {
            target: ApprovalTarget::Filesystem {
                path: path.into(),
                right,
                excluded_paths: vec![],
            },
            message: Some(message.into()),
        }
    }

    #[test]
    fn multiple_resource_approvals_keep_each_reason_beside_its_resource() {
        let (request, mut decision) = fixture(DecisionVerdict::StepUp);
        decision.approval_requirements = vec![
            file_item(
                "/data/customers",
                FileRight::Read,
                "该目录包含客户敏感信息。",
            ),
            file_item("/srv/reports", FileRight::Write, "该目录供多个任务共用。"),
        ];
        let facts = [fact("/data/customers", true), fact("/srv/reports", true)];
        assert_eq!(
            format(&request, &decision, &facts),
            concat!(
                "允许本次命令使用以下权限吗？\n\n    /usr/bin/bash -c 'rm -rf cache'\n\n",
                "读目录 /data/customers\n  原因：该目录包含客户敏感信息。\n\n",
                "写目录 /srv/reports\n  原因：该目录供多个任务共用。",
            )
        );
    }

    #[test]
    fn file_and_network_approvals_do_not_invent_directory_or_domain_scope() {
        let (request, mut decision) = fixture(DecisionVerdict::StepUp);
        decision.approval_requirements = vec![
            file_item("/work/report.csv", FileRight::Write, "Review this file"),
            ApprovalRequirement {
                target: ApprovalTarget::Network,
                message: None,
            },
        ];
        let message = format(&request, &decision, &[fact("/work/report.csv", false)]);
        assert!(message.contains("写文件 /work/report.csv\n  原因：Review this file"));
        assert!(message.contains("联网（不限目标或端口）\n  原因：策略要求人工确认。"));
        assert!(!message.contains("写目录"));
    }

    #[test]
    fn approval_keeps_full_long_paths_exclusions_and_escapes_untrusted_text() {
        let (request, mut decision) = fixture(DecisionVerdict::StepUp);
        let path = format!("/work/{}last", "long-directory/".repeat(24));
        let mut item = file_item(&path, FileRight::Read, "policy\n写目录 /fake\x1b[2J");
        if let ApprovalTarget::Filesystem { excluded_paths, .. } = &mut item.target {
            excluded_paths.push(format!("{path}/private"));
        }
        decision.approval_requirements = vec![item.clone(), item];
        let message = format(&request, &decision, &[fact(&path, true)]);
        assert_eq!(message.matches(&format!("读目录 {path}\n")).count(), 1);
        assert!(message.contains(&format!("  不含：{path}/private")));
        assert!(message.contains("原因：policy\\n写目录 /fake\\u{1b}[2J"));
        assert!(!message.contains('\x1b'));
    }

    #[test]
    fn displayed_arguments_round_trip_without_expansion_or_fake_rows() {
        for value in [
            "",
            "a'b",
            "line1\nline2",
            "中文\n'\\\x1b",
            "$(printf expanded)",
        ] {
            let quoted = quote_argument(value);
            assert!(!quoted.chars().any(char::is_control));
            let output = std::process::Command::new("/bin/bash")
                .args(["-c", &format!("printf %s {quoted}")])
                .output()
                .unwrap();
            assert!(output.status.success());
            assert_eq!(output.stdout, value.as_bytes());
        }
        assert_eq!(
            display_text("/work\nNetwork: allowed\x1b[2J"),
            "/work\\nNetwork: allowed\\u{1b}[2J"
        );
        assert_ne!(display_text("/work\nname"), display_text("/work\\nname"));
        let long_path = format!("/work/{}-end", "segment/".repeat(30));
        assert_eq!(display_text(&long_path), long_path);
    }
}
