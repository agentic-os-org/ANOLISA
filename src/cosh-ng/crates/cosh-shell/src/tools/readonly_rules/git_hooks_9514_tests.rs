// Approval cards must disclose repo-embedded hook execution for the git family.
use crate::tools::command_risk::is_high_risk_explanation;
use crate::tools::{
    assess_shell_command, launcher_walk_count, reset_launcher_walk_count, AssessmentPolicy,
    AssessmentSource, CommandAssessment, ExecutionDecision, RiskImpact,
};

const DISCLOSURE: &str = "git-repo-config-execution";

fn auto(command: &str) -> CommandAssessment {
    assess_shell_command(
        command,
        AssessmentPolicy::auto_with_guarded_diagnostics(AssessmentSource::ProviderShellTool),
    )
}

fn ask(command: &str) -> CommandAssessment {
    assess_shell_command(
        command,
        AssessmentPolicy::ask(AssessmentSource::ProviderShellTool),
    )
}

fn assert_disclosed(assessment: &CommandAssessment, command: &str) {
    assert_eq!(
        assessment.execution,
        ExecutionDecision::AskUser,
        "{command}"
    );
    assert_eq!(assessment.impact, RiskImpact::High, "{command}");
    assert_eq!(
        assessment.reasons.first().copied(),
        Some(DISCLOSURE),
        "{command}: {:?}",
        assessment.reasons
    );
    assert!(is_high_risk_explanation(DISCLOSURE));
}

fn assert_disclosed_both_modes(command: &str) {
    for assessment in [auto(command), ask(command)] {
        assert_disclosed(&assessment, command);
    }
}

#[test]
fn issue_9514_git_diff_card_discloses_repo_config_execution() {
    for command in ["git diff", "git show HEAD", "git log -p"] {
        assert_disclosed_both_modes(command);
    }
}

#[test]
fn issue_9514_git_family_cards_disclose_repo_config_execution() {
    for command in [
        "git status --short",
        "git diff --cached",
        "git blame a.txt",
        "git describe --dirty --always",
        "git ls-files",
        "git reflog show -p",
        "git remote update",
    ] {
        assert_disclosed_both_modes(command);
    }
}

#[test]
fn issue_9514_interceptor_wrapped_git_diff_discloses() {
    for command in [
        "env TOKENLESS_AGENT_ID=a TOKENLESS_SESSION_ID=b /usr/bin/rtk git diff",
        "/usr/bin/rtk git show HEAD",
        "env TOKENLESS_AGENT_ID=a /usr/bin/tokenless rtk git log -p",
    ] {
        assert_disclosed_both_modes(command);
    }
}

#[test]
fn issue_9514_escalated_wrapper_keeps_privilege_primary() {
    let assessment = ask("sudo git diff");
    assert_eq!(assessment.impact, RiskImpact::High);
    assert_eq!(
        assessment.reasons.first().copied(),
        Some("privilege-escalation")
    );
    assert!(
        assessment.reasons.contains(&DISCLOSURE),
        "{:?}",
        assessment.reasons
    );
}

#[test]
fn issue_9514_hook_free_forms_get_generic_approval() {
    for command in [
        "git count-objects",
        "git config --get user.name",
        "git config --list",
        "git config --global --get user.name",
        "git remote",
        "git remote -v",
        "git remote get-url origin",
    ] {
        let assessment = ask(command);
        assert_eq!(
            assessment.execution,
            ExecutionDecision::AskUser,
            "{command}"
        );
        assert_ne!(assessment.impact, RiskImpact::High, "{command}");
        assert!(!assessment.reasons.contains(&DISCLOSURE), "{command}");
        let auto_assessment = auto(command);
        assert_eq!(
            auto_assessment.execution,
            ExecutionDecision::AskUser,
            "{command}"
        );
        assert!(!auto_assessment.reasons.contains(&DISCLOSURE), "{command}");
    }
}

#[test]
fn issue_9514_joint_auto_gating_with_demotion() {
    for command in [
        "git diff",
        "git show HEAD",
        "git status --short",
        "git log --oneline -n 5",
        "git branch",
        "git cat-file -p HEAD:a.txt",
    ] {
        let assessment = auto(command);
        assert_eq!(
            assessment.execution,
            ExecutionDecision::AskUser,
            "{command}"
        );
        assert_eq!(assessment.impact, RiskImpact::High, "{command}");
        assert_eq!(
            assessment.reasons.first().copied(),
            Some(DISCLOSURE),
            "{command}: {:?}",
            assessment.reasons
        );
    }
}

#[test]
fn issue_9514_config_object_source_discloses() {
    for command in [
        "git config --blob=HEAD:.gitconfig --get user.name",
        "git config --blob HEAD:.gitconfig --list",
        "git config --get --blob=abc123 user.name",
        "git config --bl=HEAD:.gitconfig --list",
        "git config --bl HEAD:.gitconfig --list",
        "git config --blo=HEAD:.gitconfig --get user.name",
    ] {
        for assessment in [auto(command), ask(command)] {
            assert_eq!(
                assessment.execution,
                ExecutionDecision::AskUser,
                "{command}"
            );
            assert_eq!(assessment.impact, RiskImpact::High, "{command}");
            assert!(
                assessment.reasons.contains(&DISCLOSURE),
                "{command}: {:?}",
                assessment.reasons
            );
        }
    }
}

#[test]
fn issue_9514_log_forms_disclose() {
    for command in [
        "git log -sp",
        "git log -ps",
        "git log -m",
        "git log -c",
        "git log --cc",
        "git log --patch",
        "git log --diff-merges=separate",
        "git log --patch-with-stat",
        "git log -U3",
        "git log --unified=2",
        "git log -u",
        "git log --show-signature",
        "git log --oneline",
        "git log --oneline -n 5",
    ] {
        assert_disclosed_both_modes(command);
    }
}

#[test]
fn issue_9514_launcher_wrapped_git_diff_discloses() {
    for command in ["env -i git diff", "timeout 5 git diff", "command git diff"] {
        assert_disclosed_both_modes(command);
    }
}

#[test]
fn issue_9514_global_flag_forms_disclose() {
    for flag in [
        "-p",
        "-P",
        "--paginate",
        "--no-pager",
        "--no-replace-objects",
        "--no-lazy-fetch",
        "--no-optional-locks",
        "--no-advice",
        "--bare",
        "--literal-pathspecs",
        "--glob-pathspecs",
        "--noglob-pathspecs",
        "--icase-pathspecs",
    ] {
        let command = format!("git {flag} diff");
        assert_disclosed_both_modes(&command);
    }
    for flag in [
        "-c k=v",
        "-C /tmp",
        "--git-dir=/tmp/.git",
        "--work-tree=/tmp",
        "--namespace=x",
        "--super-prefix=x",
        "--config-env=k=E",
        "--exec-path=/usr/lib/git",
        "--attr-source=HEAD",
    ] {
        let command = format!("git {flag} diff");
        assert_disclosed_both_modes(&command);
    }
}

#[test]
fn issue_9514_pipeline_git_family_discloses() {
    for command in [
        "git diff | head",
        "git status | head",
        "sudo git diff | head",
        "git log -p | head",
    ] {
        let assessment = ask(command);
        assert_eq!(
            assessment.execution,
            ExecutionDecision::AskUser,
            "{command}"
        );
        assert_eq!(assessment.impact, RiskImpact::High, "{command}");
        assert!(
            assessment.reasons.contains(&DISCLOSURE),
            "{command}: {:?}",
            assessment.reasons
        );
    }
}

#[test]
fn issue_9514_env_inside_wrapper_discloses() {
    let command = "/usr/bin/rtk env TOKENLESS_AGENT_ID=a git diff";
    assert_disclosed_both_modes(command);
}

#[test]
fn issue_9514_shortlog_stash_forms_disclose() {
    for command in [
        "git shortlog --show-signature",
        "git shortlog -sn",
        "git stash list -p",
        "git stash show -p",
    ] {
        assert_disclosed_both_modes(command);
    }
}

#[test]
fn issue_9514_ref_listing_object_read_forms_disclose() {
    for command in [
        "git branch",
        "git branch --sort=-committerdate",
        "git tag",
        "git tag -v v1",
        "git for-each-ref --format=%(signature)",
        "git name-rev HEAD",
        "git rev-parse HEAD",
        "git rev-list HEAD",
        "git ls-tree HEAD",
        "git cat-file -p HEAD:a.txt",
        "git cat-file --batch-check",
    ] {
        assert_disclosed_both_modes(command);
    }
}

#[test]
fn issue_9514_pipeline_env_assignment_discloses() {
    for command in ["LANG=C git diff | head", "FOO=1 BAR=2 git status | head"] {
        for assessment in [auto(command), ask(command)] {
            assert_eq!(
                assessment.execution,
                ExecutionDecision::AskUser,
                "{command}"
            );
            assert_eq!(assessment.impact, RiskImpact::High, "{command}");
            assert!(
                assessment.reasons.contains(&DISCLOSURE),
                "{command}: {:?}",
                assessment.reasons
            );
        }
    }
    assert_disclosed_both_modes("LANG=C git diff");
}

#[test]
fn issue_10499_plumbing_patch_and_signature_forms_disclose() {
    for command in [
        "git diff-tree -p HEAD",
        "git diff-tree --no-commit-id --name-only -r HEAD",
        "git diff-index --cached HEAD",
        "git diff-files -p",
        "git verify-tag v1.0.0",
        "git verify-commit HEAD",
    ] {
        assert_disclosed_both_modes(command);
    }
}

#[test]
fn issue_10499_carried_escalated_command_keeps_privilege_primary() {
    for command in [
        "su -c 'git diff'",
        "su -c \"git status --short\"",
        "sudo su -c 'git log -p'",
    ] {
        for assessment in [auto(command), ask(command)] {
            assert_eq!(
                assessment.execution,
                ExecutionDecision::AskUser,
                "{command}"
            );
            assert_eq!(assessment.impact, RiskImpact::High, "{command}");
            assert_eq!(
                assessment.reasons.first().copied(),
                Some("privilege-escalation"),
                "{command}: {:?}",
                assessment.reasons
            );
            assert!(
                assessment.reasons.contains(&DISCLOSURE),
                "{command}: {:?}",
                assessment.reasons
            );
        }
    }
}

#[test]
fn issue_10499_carried_interpreter_command_discloses() {
    // An unescalated interpreter carrier has no stronger explanation of
    // its own, so the git capability takes the card primary slot.
    for command in [
        "sh -c 'git diff'",
        "bash -c 'git log -p'",
        "dash -c 'git status --short'",
    ] {
        assert_disclosed_both_modes(command);
    }
}

#[test]
fn issue_10499_carried_rest_command_takes_card_priority() {
    for command in [
        "eval 'git diff'",
        "eval 'echo ok; git status --short'",
        "eval 'git diff-tree -p HEAD'",
    ] {
        assert_disclosed_both_modes(command);
    }
}

#[test]
fn issue_10499_carried_command_pipeline_discloses() {
    for command in ["su -c 'git diff' | head", "sh -c 'git diff' | head"] {
        for assessment in [auto(command), ask(command)] {
            assert_eq!(
                assessment.execution,
                ExecutionDecision::AskUser,
                "{command}"
            );
            assert_eq!(assessment.impact, RiskImpact::High, "{command}");
            assert!(
                assessment.reasons.contains(&DISCLOSURE),
                "{command}: {:?}",
                assessment.reasons
            );
        }
    }
}

#[test]
fn issue_10499_nested_carrier_disclosure_stays_bounded() {
    // Disclosure may walk the launcher chain once per carrier level. Judging
    // the carried command inside every disclosure step re-ran the whole
    // recursive walk per level, so an 800-deep 4KB carrier chain stalled the
    // approval card for tens of seconds (#10499 review). Count walker
    // invocations instead of wall-clock time so the bound is deterministic.
    const DEPTH: usize = 800;
    // Every carrier level costs a classifier stack frame; give the probe its
    // own stack instead of the 2 MiB test-thread default.
    let (disclosed, disclosed_assessment, disclosed_walks, opaque, opaque_assessment, opaque_walks) =
        std::thread::Builder::new()
            .stack_size(64 * 1024 * 1024)
            .spawn(|| {
                let disclosed = format!("{}git diff", "eval ".repeat(DEPTH));
                reset_launcher_walk_count();
                let disclosed_assessment = ask(&disclosed);
                let disclosed_walks = launcher_walk_count();

                // The cost came from disclosure alone, so a Git-free chain
                // must stay bounded too.
                let opaque = format!("{}:", "eval ".repeat(DEPTH));
                reset_launcher_walk_count();
                let opaque_assessment = ask(&opaque);
                let opaque_walks = launcher_walk_count();
                (
                    disclosed,
                    disclosed_assessment,
                    disclosed_walks,
                    opaque,
                    opaque_assessment,
                    opaque_walks,
                )
            })
            .expect("spawn nested-carrier probe")
            .join()
            .expect("nested-carrier probe panicked");

    assert_disclosed(&disclosed_assessment, &disclosed);
    assert!(
        disclosed_walks <= 3 * DEPTH,
        "{disclosed_walks} walker invocations for {DEPTH} carriers"
    );
    assert_eq!(
        opaque_assessment.execution,
        ExecutionDecision::AskUser,
        "{opaque}"
    );
    assert!(
        opaque_walks <= 3 * DEPTH,
        "{opaque_walks} walker invocations for {DEPTH} carriers"
    );
}

#[test]
fn issue_10499_unknown_global_flag_keeps_generic_approval() {
    // git itself rejects an unknown global flag (exit 129, nothing runs),
    // so the classifier must not invent the git disclosure for it while
    // still keeping the command behind approval.
    for command in [
        "git --not-a-real-global-flag diff",
        "git --bogus-flags diff-tree -p HEAD",
    ] {
        for assessment in [auto(command), ask(command)] {
            assert_eq!(
                assessment.execution,
                ExecutionDecision::AskUser,
                "{command}"
            );
            assert!(
                !assessment.reasons.contains(&DISCLOSURE),
                "{command}: {:?}",
                assessment.reasons
            );
        }
    }
}

#[test]
fn issue_9514_disclosure_phrase_is_localized_within_budget() {
    use crate::{I18n, Language, MessageId};
    let zh = I18n::new(Language::ZhCn).t(MessageId::ApprovalRiskPhraseGitRepoConfigExecution);
    let en = I18n::new(Language::EnUs).t(MessageId::ApprovalRiskPhraseGitRepoConfigExecution);
    assert_eq!(zh, "可能执行仓库内嵌脚本");
    assert_eq!(en, "may run repo-embedded scripts");
    assert!(zh.chars().count() * 2 <= 32, "{zh}");
    assert!(en.chars().count() <= 32, "{en}");
}
