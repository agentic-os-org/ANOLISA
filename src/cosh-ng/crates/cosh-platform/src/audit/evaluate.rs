//! Policy decision point (PDP).
//!
//! Given an `Action` and a `LoadedPolicy`, returns a `Decision`. The first
//! matching rule wins; if no rule matches, `policy.default` is used.
//! See `docs/audit-design.md` §4.

use cosh_types::audit::{Action, ActionSubsystem, ArgMatch, Decision, Match, StringMatch};

use super::glob::glob_match;
use super::policy::LoadedPolicy;

pub fn evaluate(action: &Action, loaded: &LoadedPolicy) -> Decision {
    for rule in &loaded.policy.rules {
        if rule_matches(action, &rule.matches) {
            return Decision {
                outcome: rule.outcome,
                reason: rule
                    .reason
                    .clone()
                    .unwrap_or_else(|| format!("matched rule '{}'", rule.name)),
                matched_rule: Some(rule.name.clone()),
                policy_version: loaded.policy_version.clone(),
            };
        }
    }
    Decision {
        outcome: loaded.policy.default,
        reason: format!("no rule matched, default = {:?}", loaded.policy.default),
        matched_rule: None,
        policy_version: loaded.policy_version.clone(),
    }
}

fn rule_matches(action: &Action, m: &Match) -> bool {
    if let Some(sub) = &m.subsystem {
        if !subsystem_eq(sub, &action.subsystem) {
            return false;
        }
    }
    if let Some(op) = &m.operation {
        if !str_match(op, &action.operation) {
            return false;
        }
    }
    if let Some(tgt) = &m.target {
        match action.target.as_deref() {
            None => return false,
            Some(t) => {
                if !str_match(tgt, t) {
                    return false;
                }
            }
        }
    }
    for am in &m.arg {
        if !arg_match_finds(action, am) {
            return false;
        }
    }
    true
}

fn arg_match_finds(action: &Action, am: &ArgMatch) -> bool {
    action
        .args
        .iter()
        .any(|(k, v)| str_match(&am.key, k) && am.value.as_ref().is_none_or(|vm| str_match(vm, v)))
}

fn subsystem_eq(a: &ActionSubsystem, b: &ActionSubsystem) -> bool {
    a.as_str().eq_ignore_ascii_case(b.as_str())
}

fn str_match(m: &StringMatch, s: &str) -> bool {
    match m {
        StringMatch::Exact(e) => e == s,
        StringMatch::OneOf { one_of } => one_of.iter().any(|o| o == s),
        StringMatch::Glob { glob } => glob_match(glob, s),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::action::parse_action_string;
    use crate::audit::builtin;
    use cosh_types::audit::{ActionSubsystem, Outcome};

    fn balanced() -> LoadedPolicy {
        builtin::balanced()
    }

    fn evaluate_str(s: &str) -> Decision {
        let action = parse_action_string(s).expect("test input must parse");
        evaluate(&action, &balanced())
    }

    fn assert_outcome(s: &str, expected: Outcome) {
        let d = evaluate_str(s);
        assert_eq!(
            d.outcome, expected,
            "for input {:?}: expected {:?}, got {:?} (matched={:?}, reason={})",
            s, expected, d.outcome, d.matched_rule, d.reason
        );
    }

    // ---- Allow path ----------------------------------------------------

    #[test]
    fn balanced_allows_readonly_singletons() {
        for cmd in [
            "uptime",
            "ls -la",
            "cat /etc/hosts",
            "ps aux",
            "df -h",
            "echo hello",
        ] {
            assert_outcome(cmd, Outcome::Allow);
        }
    }

    #[test]
    fn balanced_allows_git_readonly() {
        for cmd in [
            "git status",
            "git log --oneline -10",
            "git diff",
            "git show HEAD",
            "git blame file.rs",
        ] {
            assert_outcome(cmd, Outcome::Allow);
        }
    }

    #[test]
    fn balanced_allows_git_branch_readonly() {
        // git branch / git branch -v are read-only
        assert_outcome("git branch", Outcome::Allow);
        assert_outcome("git branch -v", Outcome::Allow);
    }

    #[test]
    fn balanced_allows_git_stash_readonly() {
        assert_outcome("git stash", Outcome::Allow);
        assert_outcome("git stash list", Outcome::Allow);
        assert_outcome("git stash show", Outcome::Allow);
    }

    #[test]
    fn balanced_allows_safe_pairs() {
        for cmd in [
            "systemctl status sshd",
            "apt list --installed",
            "dnf list installed",
            "docker ps",
            "kubectl get pods",
            "cargo --version",
        ] {
            assert_outcome(cmd, Outcome::Allow);
        }
    }

    // ---- Deny path -----------------------------------------------------

    #[test]
    fn balanced_denies_destructive_singletons() {
        for cmd in [
            "rm -rf /",
            "sudo ls",
            "shutdown -h now",
            "dd if=/dev/zero of=/dev/sda",
            "mkfs.ext4 /dev/sda1",
            "tee /tmp/output.log",
        ] {
            assert_outcome(cmd, Outcome::Deny);
        }
    }

    #[test]
    fn balanced_denies_git_mutating() {
        for cmd in [
            "git push origin main",
            "git push --force",
            "git reset --hard HEAD~1",
            "git clean -fd",
            "git checkout .",
            "git rebase main",
        ] {
            assert_outcome(cmd, Outcome::Deny);
        }
    }

    #[test]
    fn balanced_denies_git_branch_mutating() {
        for cmd in [
            "git branch -D feature",
            "git branch -m old new",
            "git branch -M new",
            "git branch --delete feature",
            "git branch --force feature",
        ] {
            assert_outcome(cmd, Outcome::Deny);
        }
    }

    #[test]
    fn balanced_denies_git_stash_mutating() {
        for cmd in [
            "git stash drop",
            "git stash clear",
            "git stash pop",
            "git stash apply",
        ] {
            assert_outcome(cmd, Outcome::Deny);
        }
    }

    #[test]
    fn balanced_denies_sed_inplace_variants() {
        for cmd in ["sed -i s/a/b/ file", "sed --in-place s/a/b/ f"] {
            assert_outcome(cmd, Outcome::Deny);
        }
        // sed without -i is allowed (single-token readonly)
        assert_outcome("sed s/old/new/ file.txt", Outcome::Allow);
        assert_outcome("sed -n 1,10p file.txt", Outcome::Allow);
    }

    #[test]
    fn balanced_denies_sed_inplace_glob_variants() {
        // `-i.bak`, `-iEXT`, and `--in-place=...` should all be caught by the
        // glob-key arg-match rules.
        let tab_friendly_cmds = ["sed -i.bak s/a/b/ file", "sed --in-place=.bak s/a/b/ file"];
        for cmd in tab_friendly_cmds {
            assert_outcome(cmd, Outcome::Deny);
        }
    }

    #[test]
    fn balanced_denies_find_destructive_actions() {
        // `find . -exec rm {} +` would contain `{` / `}` shell metas that
        // parse_action_string rejects — but `-delete` and `-fprint` are
        // pure flags that pass parsing.
        assert_outcome("find . -delete", Outcome::Deny);
        assert_outcome("find . -fprint /tmp/log", Outcome::Deny);
        assert_outcome("find . -name *.rs", Outcome::Allow);
        assert_outcome("find . -type f", Outcome::Allow);
    }

    // ---- Sensitive-path guard -----------------------------------------

    #[test]
    fn balanced_require_approval_for_sensitive_system_files() {
        for cmd in [
            "cat /etc/passwd",
            "cat /etc/shadow",
            "cat /etc/gshadow",
            "cat /etc/sudoers",
            "head /etc/passwd",
            "tail /etc/shadow",
            "grep root /etc/passwd",
            "wc /etc/passwd",
        ] {
            assert_outcome(cmd, Outcome::RequireApproval);
        }
    }

    #[test]
    fn balanced_require_approval_for_ssh_keys() {
        for cmd in [
            "cat ~/.ssh/id_rsa",
            "cat ~/.ssh/id_ed25519",
            "head ~/.ssh/config",
            "cat /root/.ssh/authorized_keys",
            "cat /home/user/.ssh/id_rsa",
            "head /home/alice/.ssh/config",
        ] {
            assert_outcome(cmd, Outcome::RequireApproval);
        }
    }

    #[test]
    fn balanced_require_approval_for_proc_and_sudoersd() {
        for cmd in [
            "cat /proc/self/environ",
            "cat /proc/self/cmdline",
            "cat /etc/sudoers.d/myuser",
        ] {
            assert_outcome(cmd, Outcome::RequireApproval);
        }
    }

    #[test]
    fn balanced_require_approval_for_shell_history() {
        for cmd in [
            "cat ~/.bash_history",
            "cat /root/.bash_history",
            "cat /home/user/.bash_history",
            "head /home/alice/.bash_history",
        ] {
            assert_outcome(cmd, Outcome::RequireApproval);
        }
    }

    #[test]
    fn balanced_require_approval_for_flag_before_sensitive_path() {
        // Flags before the path: target would be the flag, but arg matching
        // still catches the sensitive path in args.
        assert_outcome("cat -n /etc/passwd", Outcome::RequireApproval);
        assert_outcome("head -5 /etc/shadow", Outcome::RequireApproval);
    }

    #[test]
    fn balanced_still_allows_non_sensitive_readonly() {
        for cmd in [
            "cat /etc/hosts",
            "cat /tmp/test.txt",
            "head README.md",
            "ls -la",
            "echo hello",
        ] {
            assert_outcome(cmd, Outcome::Allow);
        }
    }

    #[test]
    fn balanced_destructive_on_sensitive_path_still_denied() {
        // rm /etc/passwd is Deny'd by the destructive rule before the
        // sensitive-path guard is reached.
        assert_outcome("rm /etc/passwd", Outcome::Deny);
        assert_outcome("sudo cat /etc/shadow", Outcome::Deny);
    }

    #[test]
    fn strict_denies_sensitive_paths() {
        // Strict mode denies direct access to sensitive system and credential
        // files; non-sensitive read-only commands still fall through to Allow.
        let loaded = builtin::strict();
        for cmd in [
            "cat /etc/passwd",
            "cat /etc/shadow",
            "head /etc/sudoers",
            "cat /etc/sudoers.d/myuser",
            "cat /proc/self/environ",
            "cat /proc/self/cmdline",
            "cat ~/.ssh/id_rsa",
            "cat /root/.ssh/authorized_keys",
            "cat /home/user/.ssh/id_rsa",
            "cat ~/.bash_history",
            "cat /root/.bash_history",
            "cat /home/user/.bash_history",
        ] {
            let action = parse_action_string(cmd).unwrap();
            let d = evaluate(&action, &loaded);
            assert_eq!(
                d.outcome,
                Outcome::Deny,
                "for input {:?}: expected Deny, got {:?} (matched={:?})",
                cmd,
                d.outcome,
                d.matched_rule
            );
        }
    }

    #[test]
    fn strict_still_allows_non_sensitive_readonly() {
        let action = parse_action_string("cat /etc/hosts").unwrap();
        let loaded = builtin::strict();
        let d = evaluate(&action, &loaded);
        assert_eq!(d.outcome, Outcome::Allow);
    }

    // ---- Pkg / Svc / Checkpoint ---------------------------------------

    #[test]
    fn balanced_pkg_search_allow_install_approve() {
        assert_outcome("pkg search nginx", Outcome::Allow);
        assert_outcome("pkg list", Outcome::Allow);
        assert_outcome("pkg install nginx", Outcome::RequireApproval);
        assert_outcome("pkg remove nginx", Outcome::RequireApproval);
    }

    #[test]
    fn balanced_svc_status_allow_start_approve() {
        assert_outcome("svc status sshd", Outcome::Allow);
        assert_outcome("svc list", Outcome::Allow);
        assert_outcome("svc start sshd", Outcome::RequireApproval);
        assert_outcome("svc restart sshd", Outcome::RequireApproval);
    }

    #[test]
    fn balanced_checkpoint_list_allow_create_approve() {
        assert_outcome("checkpoint list", Outcome::Allow);
        assert_outcome("checkpoint status", Outcome::Allow);
        assert_outcome("checkpoint create", Outcome::RequireApproval);
        assert_outcome("checkpoint restore", Outcome::RequireApproval);
    }

    // ---- Default fall-through -----------------------------------------

    #[test]
    fn unknown_command_falls_through_to_default_require_approval() {
        // `touch /tmp/x` and unknown tools are not in any allow / deny rule.
        assert_outcome("touch /tmp/x", Outcome::RequireApproval);
        assert_outcome("myproprietarytool --run", Outcome::RequireApproval);
    }

    #[test]
    fn decision_includes_policy_version() {
        let d = evaluate_str("uptime");
        assert!(d.policy_version.starts_with("builtin-balanced@"));
        assert!(d.policy_version.contains("+sha256:"));
    }

    // ---- Match-block invariants ---------------------------------------

    #[test]
    fn target_required_by_rule_means_action_without_target_no_match() {
        // git stash deny rule requires arg containing "drop" — bare "git stash"
        // has no such arg, so falls through to the git-fallback Allow.
        let action = Action {
            subsystem: ActionSubsystem::Shell,
            operation: "git".to_string(),
            target: Some("stash".to_string()),
            args: vec![],
            raw: None,
        };
        let d = evaluate(&action, &balanced());
        assert_eq!(d.outcome, Outcome::Allow);
    }
}
