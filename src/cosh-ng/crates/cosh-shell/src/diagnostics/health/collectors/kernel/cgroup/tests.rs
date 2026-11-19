use super::*;

#[test]
fn membership_v2_unified_line_parses() {
    assert_eq!(
        parse_membership_v2("0::/user.slice/user-1000.slice/session-2.scope\n"),
        Some("/user.slice/user-1000.slice/session-2.scope".into())
    );
    assert_eq!(parse_membership_v2("0::/\n"), Some("/".into()));
}

#[test]
fn membership_v1_and_hybrid_are_unsupported() {
    assert!(parse_membership_v2("12:memory:/a\n11:cpu:/a\n").is_none());
    assert!(parse_membership_v2("0::/a\n1:name=systemd:/a\n").is_none());
    assert!(parse_membership_v2("0::/a\n0::/b\n").is_none());
    assert!(parse_membership_v2("").is_none());
}

#[test]
fn events_oom_kill_scalar_parses_only_exact_key() {
    let content = "low 0\nhigh 0\nmax 0\noom 0\noom_kill 3\noom_group_kill 0\n";
    assert_eq!(parse_events_oom_kill(content), Some(3));
    assert_eq!(parse_events_oom_kill("oom 5\n"), None);
    assert_eq!(parse_events_oom_kill("oom_kill x\n"), None);
    assert_eq!(parse_events_oom_kill(""), None);
}

#[test]
fn domain_match_is_exact_or_descendant_never_ancestor_or_sibling() {
    let current = "/user.slice/user-1000.slice/session-2.scope";
    assert!(in_current_domain(current, current));
    assert!(in_current_domain(&format!("{current}/child"), current));
    assert!(!in_current_domain("/user.slice/user-1000.slice", current));
    assert!(!in_current_domain(
        "/user.slice/user-1001.slice/session-2.scope",
        current
    ));
    assert!(!in_current_domain(
        "/user.slice/user-1000.slice/session-20.scope",
        current
    ));
    assert!(!in_current_domain("/system.slice/sshd.service", current));
    assert!(in_current_domain("/anything", "/"));
}

#[test]
fn path_normalization_handles_trailing_slash_and_relative() {
    assert_eq!(normalize_path("/a/b/"), "/a/b");
    assert_eq!(normalize_path("a/b"), "/a/b");
    assert_eq!(normalize_path("/"), "/");
    assert_eq!(normalize_path(""), "/");
    assert!(in_current_domain("/a/b/", "/a/b"));
}

#[test]
fn quota_limits_parse_finite_zero_unlimited_and_reject_malformed() {
    assert_eq!(parse_quota_limit("max\n"), Some(QuotaValue::Unlimited));
    assert_eq!(parse_quota_limit("0\n"), Some(QuotaValue::Finite(0)));
    assert_eq!(
        parse_quota_limit("1073741824\n"),
        Some(QuotaValue::Finite(1073741824))
    );
    assert_eq!(parse_quota_limit("max"), Some(QuotaValue::Unlimited));
    assert_eq!(parse_quota_limit("-1"), None);
    assert_eq!(parse_quota_limit("abc"), None);
    assert_eq!(parse_quota_limit(""), None);
    assert_eq!(parse_quota_limit("12.5"), None);
}

#[test]
fn quota_current_parses_bytes_only() {
    assert_eq!(parse_quota_current("0\n"), Some(0));
    assert_eq!(parse_quota_current("1048576"), Some(1048576));
    assert_eq!(parse_quota_current("max"), None);
    assert_eq!(parse_quota_current("-5"), None);
    assert_eq!(parse_quota_current(""), None);
}
