use super::*;

#[test]
fn matches_on_fail_skips_success() {
    let matcher = make_matcher(vec!["cargo"], vec![], HookTrigger::OnFail);
    let input = make_input("cargo test", 0);
    assert!(!matches_command(&matcher, &input));
}

#[test]
fn matches_on_fail_fires_on_nonzero() {
    let matcher = make_matcher(vec!["cargo"], vec![], HookTrigger::OnFail);
    let input = make_input("cargo test", 1);
    assert!(matches_command(&matcher, &input));
}

#[test]
fn matches_on_fail_skips_non_actionable_exit_categories() {
    let matcher = make_matcher(vec!["grep"], vec![], HookTrigger::OnFail);

    assert!(!matches_command(
        &matcher,
        &make_input("grep needle file.txt", 1)
    ));
    assert!(!matches_command(&matcher, &make_input("grep needle", 130)));
    assert!(!matches_command(
        &matcher,
        &make_input("yes | grep needle | head -1", 141)
    ));

    let crash_matcher = make_matcher(vec!["worker"], vec![], HookTrigger::OnFail);
    assert!(matches_command(&crash_matcher, &make_input("worker", 137)));
}

#[test]
fn matches_command_name() {
    let matcher = make_matcher(vec!["git"], vec![], HookTrigger::OnComplete);
    let input = make_input("git status", 0);
    assert!(matches_command(&matcher, &input));
}

#[test]
fn matches_command_name_after_sudo_options() {
    let matcher = make_matcher(vec!["free"], vec![], HookTrigger::OnComplete);
    let input = make_input("sudo -n -E free -m", 0);
    assert!(matches_command(&matcher, &input));

    let env_input = make_input("LANG=C sudo -n free -m", 0);
    assert!(matches_command(&matcher, &env_input));

    let unknown_sudo_input = make_input("sudo --definitely-unknown free -m", 0);
    assert!(!matches_command(&matcher, &unknown_sudo_input));
}

#[test]
fn no_match_wrong_command_name() {
    let matcher = make_matcher(vec!["npm"], vec![], HookTrigger::OnComplete);
    let input = make_input("cargo build", 0);
    assert!(!matches_command(&matcher, &input));
}

#[test]
fn matches_command_pattern_prefix() {
    let matcher = make_matcher(vec![], vec!["cargo test"], HookTrigger::OnComplete);
    let input = make_input("cargo test --workspace", 0);
    assert!(matches_command(&matcher, &input));
}

#[test]
fn no_match_wrong_pattern() {
    let matcher = make_matcher(vec![], vec!["cargo test"], HookTrigger::OnComplete);
    let input = make_input("cargo build", 0);
    assert!(!matches_command(&matcher, &input));
}

#[test]
fn command_regex_is_literal_contains_for_now() {
    let matcher = HookMatcher {
        id: "test".to_string(),
        commands: vec![],
        command_patterns: vec![],
        command_regex: Some("cargo.*test".to_string()),
        min_output_bytes: None,
        exit_codes: None,
        trigger: HookTrigger::OnComplete,
    };

    assert!(matches_command(
        &matcher,
        &make_input("printf 'cargo.*test'", 0)
    ));
    assert!(!matches_command(
        &matcher,
        &make_input("cargo nextest run", 0)
    ));
}
