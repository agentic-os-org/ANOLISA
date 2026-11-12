use super::*;
use std::os::unix::fs::symlink;

const DEFAULT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../packaging/sandbox/permissions.json.template"
));

#[test]
fn installed_defaults_protect_git_and_include_temporary_writes() {
    let root = tempfile::tempdir().unwrap();
    let cwd = root.path().join("quote' and \"space");
    fs::create_dir(&cwd).unwrap();
    fs::create_dir(cwd.join(".git")).unwrap();
    let temporary = root.path().join("temporary");
    fs::create_dir(&temporary).unwrap();
    fs::create_dir(temporary.join(".git")).unwrap();
    let permissions = expand(DEFAULT, &cwd, temporary.to_str()).unwrap();
    assert_eq!(
        permissions.network.mode,
        asc_os_permission_types::NetworkMode::Off
    );
    let canonical = fs::canonicalize(&cwd).unwrap();
    assert!(permissions.execution.deny_paths.is_empty());
    for path in [canonical.as_path(), Path::new("/tmp"), temporary.as_path()] {
        assert!(
            permissions
                .filesystem
                .rules
                .iter()
                .any(|rule| rule.path == path.to_str().unwrap()
                    && rule.effect == RuleEffect::Allow
                    && rule.rights.contains(&FileRight::Write))
        );
    }
    for path in [cwd.join(".git"), temporary.join(".git")] {
        assert!(denied(&permissions, &path));
    }
}

fn denied(permissions: &OsPermissionSet, path: &Path) -> bool {
    permissions.filesystem.rules.iter().any(|rule| {
        rule.path == path.to_str().unwrap()
            && rule.effect == RuleEffect::Deny
            && rule.rights == [FileRight::Write]
    })
}

#[test]
fn absent_git_path_does_not_create_an_unbindable_rule() {
    let root = tempfile::tempdir().unwrap();
    let permissions = expand(DEFAULT, root.path(), None).unwrap();
    assert!(!denied(&permissions, &root.path().join(".git")));
}

#[test]
fn git_pointer_preserves_path_whitespace_without_following_commondir() {
    let root = tempfile::tempdir().unwrap();
    let cwd = root.path().join("work");
    let gitdir = cwd.join("metadata ");
    fs::create_dir_all(&gitdir).unwrap();
    fs::write(gitdir.join("commondir"), "not-read").unwrap();
    for target in ["metadata ".to_owned(), gitdir.to_str().unwrap().to_owned()] {
        fs::write(cwd.join(".git"), format!("gitdir: {target}\r\n")).unwrap();
        let permissions = expand(DEFAULT, &cwd, None).unwrap();
        assert!(denied(&permissions, &gitdir));
        assert!(denied(&permissions, &cwd.join(".git")));
    }
}

#[test]
fn protected_symlinks_and_nonregular_git_pointers_fail_closed() {
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("target");
    fs::create_dir(&target).unwrap();
    let cwd = root.path().join("work");
    fs::create_dir(&cwd).unwrap();
    let path = cwd.join(".git");
    for destination in [&target, &root.path().join("missing")] {
        symlink(destination, &path).unwrap();
        assert!(expand(DEFAULT, &cwd, None).is_err());
        fs::remove_file(&path).unwrap();
    }
    let pointer = cwd.join(".git");
    nix::unistd::mkfifo(&pointer, nix::sys::stat::Mode::S_IRUSR).unwrap();
    assert!(expand(DEFAULT, &cwd, None).is_err());
    fs::remove_file(&pointer).unwrap();
    for value in ["gitdir: ", "gitdir: missing", "not a pointer"] {
        fs::write(&pointer, value).unwrap();
        assert!(expand(DEFAULT, &cwd, None).is_err());
    }
    let alias = cwd.join("alias");
    symlink(&target, &alias).unwrap();
    fs::write(&pointer, "gitdir: alias").unwrap();
    assert!(expand(DEFAULT, &cwd, None).is_err());
}

#[test]
fn tmpdir_is_optional_and_admin_can_disable_default_exclusions() {
    let root = tempfile::tempdir().unwrap();
    let base = expand(DEFAULT, root.path(), None).unwrap();
    for value in ["", "relative"] {
        assert_eq!(expand(DEFAULT, root.path(), Some(value)).unwrap(), base);
    }
    assert!(expand(DEFAULT, root.path(), root.path().join("absent").to_str()).is_err());
    let file = root.path().join("file");
    fs::write(&file, "not a directory").unwrap();
    assert!(expand(DEFAULT, root.path(), file.to_str()).is_err());
    fs::create_dir(root.path().join(".git")).unwrap();
    let mut template: serde_json::Value = serde_json::from_str(DEFAULT).unwrap();
    template["include_tmpdir"] = false.into();
    template["read_only_subpaths"] = serde_json::json!([]);
    let permissions = expand(&template.to_string(), root.path(), file.to_str()).unwrap();
    assert!(!denied(&permissions, &root.path().join(".git")));
    template["read_only_subpaths"] = serde_json::json!(["../escape"]);
    assert!(expand(&template.to_string(), root.path(), None).is_err());
}

#[test]
fn root_and_home_paths_remain_policy_decisions() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    fs::create_dir(&home).unwrap();
    let alias = root.path().join("home-alias");
    symlink(&home, &alias).unwrap();
    for cwd in [Path::new("/"), root.path(), home.as_path(), alias.as_path()] {
        let canonical = fs::canonicalize(cwd).unwrap();
        let permissions = expand(DEFAULT, cwd, None).unwrap();
        assert!(
            permissions
                .filesystem
                .rules
                .iter()
                .any(|rule| rule.path == canonical.to_str().unwrap()
                    && rule.rights.contains(&FileRight::Write))
        );
    }
    assert_eq!(expand_path("${cwd}/private", "/").unwrap(), "/private");
}

#[test]
fn invalid_template_contracts_and_placeholders_fail() {
    let root = tempfile::tempdir().unwrap();
    for text in [
        DEFAULT.replace("permissions-template.v1", "permissions-template.v2"),
        DEFAULT.replacen('{', "{\"unknown\":true,", 1),
        DEFAULT.replace("${cwd}", "${HOME}"),
        DEFAULT.replace("${cwd}", "$(pwd)"),
        DEFAULT.replace("${cwd}", "${cwd}suffix"),
        DEFAULT.replace("${cwd}", "${cwd}/../escape"),
        DEFAULT.replace("\"off\"", "\"sometimes\""),
        "{}".into(),
    ] {
        assert!(expand(&text, root.path(), None).is_err(), "accepted {text}");
    }
    let file = root.path().join("ordinary-file");
    fs::write(&file, "not a directory").unwrap();
    assert!(expand(DEFAULT, &file, None).is_err());
    assert!(expand(DEFAULT, &root.path().join("missing"), None).is_err());
}

#[test]
fn template_trust_and_input_limits_fail_closed() {
    assert!(trusted(0, 0o100_644));
    assert!(!trusted(1000, 0o100_644));
    assert!(!trusted(0, 0o100_664));
    assert!(!trusted(0, 0o100_646));
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("permissions.json.template");
    fs::write(&source, DEFAULT).unwrap();
    assert!(
        read(&source, root.path(), None).is_err(),
        "untrusted parent was accepted"
    );
    fs::write(
        &source,
        vec![b' '; usize::try_from(MAX_PERMISSION_BYTES).unwrap() + 1],
    )
    .unwrap();
    assert!(matches!(
        read_bounded(File::open(&source).unwrap()),
        Err(SandboxError::TooLarge)
    ));
    fs::write(&source, [0xff]).unwrap();
    assert!(read_bounded(File::open(&source).unwrap()).is_err());
}
