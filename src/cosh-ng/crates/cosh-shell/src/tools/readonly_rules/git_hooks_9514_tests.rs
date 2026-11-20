// Approval cards must disclose repo-embedded hook execution for the git family.
use crate::tools::command_risk::is_high_risk_explanation;
use crate::tools::{
    assess_shell_command, AssessmentPolicy, AssessmentSource, CommandAssessment, ExecutionDecision,
    RiskImpact,
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
fn issue_9514_disclosure_phrase_is_localized_within_budget() {
    use crate::{I18n, Language, MessageId};
    let zh = I18n::new(Language::ZhCn).t(MessageId::ApprovalRiskPhraseGitRepoConfigExecution);
    let en = I18n::new(Language::EnUs).t(MessageId::ApprovalRiskPhraseGitRepoConfigExecution);
    assert_eq!(zh, "可能执行仓库内嵌脚本");
    assert_eq!(en, "may run repo-embedded scripts");
    assert!(zh.chars().count() * 2 <= 32, "{zh}");
    assert!(en.chars().count() <= 32, "{en}");
}
