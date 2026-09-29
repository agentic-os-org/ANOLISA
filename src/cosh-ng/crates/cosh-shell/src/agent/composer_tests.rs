//! Binary-only tests for Agent Composer completion behavior.

use std::path::{Path, PathBuf};

use super::composer::{attach_submission, completions, ComposerReferenceRejection};
use crate::input::composer::{is_slash_submission, slash_completions};
use crate::types::{
    AgentMode, AgentRequest, CommandBlock, CommandOrigin, CommandStatus, OutputRefs,
};

fn test_workspace(name: &str) -> PathBuf {
    let path =
        std::env::temp_dir().join(format!("cosh-agent-composer-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(path.join("src")).expect("create workspace");
    std::fs::write(path.join("README.md"), "demo").expect("write README");
    std::fs::write(path.join("src/service.rs"), "demo").expect("write source");
    path
}

fn composer_request(input: String, workspace: &Path) -> AgentRequest {
    AgentRequest {
        id: "composer-test-request".to_string(),
        session_id: "composer-test-session".to_string(),
        command_block: CommandBlock {
            id: "composer-test-command".to_string(),
            session_id: "composer-test-session".to_string(),
            command: input.clone(),
            origin: CommandOrigin::UserInteractive,
            cwd: workspace.to_string_lossy().into_owned(),
            end_cwd: workspace.to_string_lossy().into_owned(),
            started_at_ms: 0,
            ended_at_ms: 0,
            duration_ms: 0,
            exit_code: 0,
            status: CommandStatus::Completed,
            output: OutputRefs {
                terminal_output_ref: None,
                terminal_output_bytes: 0,
            },
            shell_environment_generation: None,
            audit_identity: None,
        },
        context_blocks: Vec::new(),
        context_hints: Vec::new(),
        user_input: Some(input),
        findings: Vec::new(),
        mode: AgentMode::RecommendOnly,
        user_confirmed: true,
        hook_finding: None,
        recommended_skill: None,
    }
}

#[test]
fn path_completion_is_workspace_bounded_and_marks_directories() {
    let workspace = test_workspace("paths");
    let cwd = workspace.to_string_lossy();

    let root = completions("review @", 0, 8, Some(&cwd), &[]);
    assert!(root.iter().any(|item| item.display == "@README.md"));
    assert!(root.iter().any(|item| item.display == "@src/"));

    let nested = completions("review @src/s", 0, 13, Some(&cwd), &[]);
    assert_eq!(nested[0].display, "@src/service.rs");
    assert_eq!(nested[0].replacement, "@src/service.rs ");
    assert!(completions("review @../", 0, 11, Some(&cwd), &[]).is_empty());
    let _ = std::fs::remove_dir_all(workspace);
}

#[test]
fn skill_completion_only_applies_to_the_leading_directive() {
    let skills = vec!["repo-review".to_string(), "release-notes".to_string()];
    let matches = completions("/skill:r", 0, 8, None, &skills);
    assert_eq!(
        matches
            .iter()
            .map(|item| item.display.as_str())
            .collect::<Vec<_>>(),
        vec!["/skill:release-notes", "/skill:repo-review"]
    );
    assert_eq!(matches[0].replacement, "/skill:release-notes ");
    assert_eq!(completions("/skill", 0, 6, None, &skills), matches);
    assert!(completions("/skill", 0, 6, None, &[]).is_empty());
    assert_eq!(
        completions("/skills", 0, 7, None, &skills)[0].display,
        "/skills"
    );
    assert!(completions("inspect /skill:r", 0, 16, None, &skills).is_empty());
}

#[test]
fn invalid_candidates_do_not_hide_a_later_valid_reference() {
    let workspace = test_workspace("invalid-before-valid");
    let invalid = (0..17)
        .map(|index| format!("@missing-{index}"))
        .collect::<Vec<_>>()
        .join(" ");
    let mut request = composer_request(format!("inspect {invalid} @README.md"), &workspace);

    let feedback = attach_submission(&mut request, Some(&workspace.to_string_lossy()));
    let prompt = crate::types::composer::composer_prompt(&request);

    assert_eq!(feedback.rejected_references.len(), 17);
    assert!(prompt.contains("- file: \"README.md\""), "{prompt}");
    assert!(prompt.contains("rejected_reference_count: 17"), "{prompt}");
    let _ = std::fs::remove_dir_all(workspace);
}

#[test]
fn valid_reference_limit_reports_every_excess_reference() {
    let workspace = test_workspace("valid-limit");
    for index in 0..18 {
        std::fs::write(workspace.join(format!("file-{index}.txt")), "demo")
            .expect("write reference");
    }
    let input = (0..18)
        .map(|index| format!("@file-{index}.txt"))
        .collect::<Vec<_>>()
        .join(" ");
    let mut request = composer_request(input, &workspace);

    let feedback = attach_submission(&mut request, Some(&workspace.to_string_lossy()));
    let prompt = crate::types::composer::composer_prompt(&request);

    assert_eq!(feedback.rejected_references.len(), 2);
    assert!(feedback
        .rejected_references
        .iter()
        .all(|item| item.reason == ComposerReferenceRejection::LimitExceeded));
    assert!(prompt.contains("- file: \"file-15.txt\""), "{prompt}");
    assert!(!prompt.contains("- file: \"file-16.txt\""), "{prompt}");
    assert!(prompt.contains("rejected_reference_count: 2"), "{prompt}");
    let _ = std::fs::remove_dir_all(workspace);
}

#[test]
fn composer_commands_are_public_unique_and_prefix_filtered() {
    let all = slash_completions("/", 0, 1);
    assert!(all.contains(&"/help"));
    assert!(all.contains(&"/mcp"));
    assert_eq!(all.iter().filter(|name| **name == "/mode").count(), 1);
    assert!(!all.contains(&"/details"));
    assert!(!all.contains(&"/approve"));
    assert_eq!(slash_completions("/ho", 0, 3), ["/hooks"]);
    assert!(slash_completions("/skill", 0, 6).is_empty());
    assert_eq!(slash_completions("/skills", 0, 7), ["/skills"]);
    assert_eq!(slash_completions("/sta", 0, 4), ["/status", "/stats"]);
    assert!(slash_completions("/tmp/", 0, 5).is_empty());
    for path in ["/tmp", "/etc"] {
        assert!(std::path::Path::new(path).exists());
        for col in 1..=path.len() {
            assert!(slash_completions(path, 0, col).is_empty());
            assert!(completions(path, 0, col, None, &[]).is_empty());
        }
    }
    assert!(slash_completions("/hooks/file", 0, 3).is_empty());
    assert!(slash_completions("/skill:review", 0, 3).is_empty());
    assert!(slash_completions("explain /ho", 0, 11).is_empty());
    assert!(slash_completions("/hooks /", 0, 8).is_empty());
    assert_eq!(slash_completions("\n  /he", 1, 5), ["/help", "/health"]);
}

#[test]
fn composer_routing_keeps_paths_skills_and_explicit_prompts_as_agent_text() {
    for input in [
        "/",
        "/ho",
        " /hooks ",
        "/unknown",
        "/mode approval auto",
        "/cancel",
    ] {
        assert!(is_slash_submission(input), "{input}");
    }
    for input in [
        "",
        "/tmp",
        "/etc explain this",
        "/tmp/file explain this",
        "/skill:repo-review inspect",
        "/skill:team/review",
        "?? /help",
        "explain /help",
    ] {
        assert!(!is_slash_submission(input), "{input}");
    }
}
