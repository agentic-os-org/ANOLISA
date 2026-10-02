use std::path::Path;

use super::personal_context::{build_activity_context, discover_repo_context};

#[test]
fn repo_context_persists_digest_name_and_relative_cwd_only() {
    let key = [7_u8; 32];
    let context = build_activity_context(
        &key,
        "host-secret-label",
        Path::new("/Users/alice/work/payment/services/payment-api"),
        Some(Path::new("/Users/alice/work/payment")),
        Some("git@github.example:team/payment.git"),
        Path::new("/Users/alice"),
    );

    assert!(context
        .host_id
        .as_deref()
        .unwrap()
        .starts_with("host:hmac:v1:"));
    assert!(context
        .repo_id
        .as_deref()
        .unwrap()
        .starts_with("repo:hmac:v1:"));
    assert_eq!(context.repo_name.as_deref(), Some("payment"));
    assert_eq!(
        context.cwd_relative.as_deref(),
        Some("services/payment-api")
    );
    let json = serde_json::to_string(&context).unwrap();
    assert!(!json.contains("host-secret-label"));
    assert!(!json.contains("github.example"));
    assert!(!json.contains("/Users/alice"));
}

#[test]
fn outside_repo_normalizes_home_without_hostname_label() {
    let context = build_activity_context(
        &[9_u8; 32],
        "host-secret-label",
        Path::new("/Users/alice/tmp/diagnosis"),
        None,
        None,
        Path::new("/Users/alice"),
    );

    assert!(context.repo_id.is_none());
    assert!(context.repo_name.is_none());
    assert_eq!(context.cwd_relative.as_deref(), Some("$HOME/tmp/diagnosis"));
}

#[test]
fn discovers_origin_and_worktree_common_identity_without_running_git() {
    let root = std::env::temp_dir().join(format!(
        "cosh-personal-context-{}",
        crate::recommendation::personal_crypto::random_hex(6).unwrap()
    ));
    let common = root.join("repo/.git");
    let worktree = root.join("feature");
    let gitdir = common.join("worktrees/feature");
    std::fs::create_dir_all(&gitdir).unwrap();
    std::fs::create_dir_all(worktree.join("src")).unwrap();
    std::fs::write(
        common.join("config"),
        "[remote \"origin\"]\n\turl = git@github.com:Acme/Payments.git\n",
    )
    .unwrap();
    std::fs::write(
        worktree.join(".git"),
        format!("gitdir: {}\n", gitdir.display()),
    )
    .unwrap();
    std::fs::write(gitdir.join("commondir"), "../..\n").unwrap();

    let discovered = discover_repo_context(&worktree.join("src")).unwrap();

    assert_eq!(discovered.root, worktree);
    assert_eq!(
        discovered.normalized_identity.as_deref(),
        Some("github.com:acme/payments")
    );
    std::fs::remove_dir_all(root).unwrap();
}
