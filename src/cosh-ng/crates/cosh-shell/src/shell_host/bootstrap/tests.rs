use super::*;

#[test]
fn failed_shell_spawn_keeps_login_effects_armed() {
    let dir = tempfile::tempdir().expect("create shell work directory");
    let mut config = ShellHostConfig::new("spawn-failure", dir.path());
    let effects = config.login_effect_guard();
    config.integration = crate::shell_host::ShellIntegration::Native;
    config.native_mode = false;
    config.bash_path = "/definitely/missing/cosh-shell".to_string();

    let result = start_bash_session(&config);

    assert!(result.is_err());
    assert!(!effects.may_have_started());
}

#[test]
fn login_inject_body_clears_posix_restores_env_then_marker() {
    // S2 + environment hygiene: inject = `set +o posix` (line 1) → restore
    // HOME, HISTFILE, and ENV from child inputs/defaults → marker verbatim.
    // Ordering is load-bearing: all restoration precedes profile replay.
    let marker = "# cosh marker\n_cosh_prompt_ready() { :; }\n";
    let body = login_inject_body(marker);
    assert_eq!(
        body.lines().next(),
        Some("set +o posix"),
        "inject line 1 must be the posix-off directive, got {body:?}"
    );
    // ENV restore/unset sits between posix-off and the marker.
    let posix_end = "set +o posix\n".len();
    let marker_start = body
        .find(marker)
        .expect("marker must be present verbatim in the inject body");
    let restore = &body[posix_end..marker_start];
    assert!(
        restore.contains("unset ENV") && restore.contains("COSH_PRIOR_ENV"),
        "ENV must be restored/unset before the marker, got {restore:?}"
    );
    assert!(
        restore.contains("unset COSH_LOGIN_INJECT"),
        "inject path indirection must be cleared before the marker, got {restore:?}"
    );
    assert!(
        restore.contains("COSH_PRIOR_BASHOPTS") && restore.contains("shopt -u inherit_errexit"),
        "inherit_errexit must match the caller before the marker, got {restore:?}"
    );
    let clear_allexport = restore
        .find("unset COSH_RESTORE_ALLEXPORT")
        .expect("missing stale allexport marker cleanup");
    let suspend_allexport = restore.find("set +a").expect("missing allexport suspend");
    let restore_home = restore
        .find("if [ -z \"${HOME+x}\" ]; then HOME=~; fi")
        .expect("missing HOME recovery");
    let restore_histfile = restore
        .find("COSH_PRIOR_HISTFILE")
        .expect("missing HISTFILE recovery");
    let restore_allexport = restore.rfind("set -a").expect("missing allexport restore");
    assert!(
        clear_allexport < suspend_allexport
            && suspend_allexport < restore_home
            && restore_home < restore_histfile
            && restore_histfile < restore_allexport
            && restore.contains("HISTFILE=\"$HOME/.bash_history\""),
        "HOME/HISTFILE must recover with allexport suspended, got {restore:?}"
    );
    assert!(
        body.ends_with(marker),
        "marker script must follow verbatim, unaltered"
    );

    let overrides = vec![
        ("ENV".to_string(), "/first".to_string()),
        ("OTHER".to_string(), "ignored".to_string()),
        ("ENV".to_string(), "".to_string()),
    ];
    assert_eq!(
        final_child_env_value(&overrides, "ENV", Some(OsStr::new("/inherited"))),
        Some(OsString::new()),
        "last override wins, including an explicit empty value"
    );
    assert_eq!(
        final_child_env_value(&[], "ENV", Some(OsStr::new("/inherited"))),
        Some(OsString::from("/inherited"))
    );
    assert_eq!(final_child_env_value(&[], "ENV", None), None);
}

#[test]
fn cleanup_expired_output_refs_removes_old_files_only() {
    let dir = std::env::temp_dir().join(format!(
        "cosh-shell-output-ref-cleanup-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create dir");
    let stale = dir.join("cmd-old.txt");
    let subdir = dir.join("nested");
    fs::write(&stale, "old\n").expect("write stale");
    fs::create_dir_all(&subdir).expect("create subdir");

    cleanup_expired_output_refs_at(
        &dir,
        Duration::ZERO,
        SystemTime::now() + Duration::from_secs(1),
    )
    .expect("cleanup");

    assert!(!stale.exists(), "stale output ref should be removed");
    assert!(subdir.exists(), "cleanup must not remove directories");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn cleanup_expired_output_refs_keeps_recent_files() {
    let dir = std::env::temp_dir().join(format!(
        "cosh-shell-output-ref-retain-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create dir");
    let recent = dir.join("cmd-recent.txt");
    fs::write(&recent, "recent\n").expect("write recent");

    cleanup_expired_output_refs_at(&dir, Duration::from_secs(60 * 60), SystemTime::now())
        .expect("cleanup");

    assert!(recent.exists(), "recent output ref should be retained");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn close_on_exec_sets_fd_flag() {
    let pty = openpty(None, None).expect("open pty");
    let fd = pty.master.as_raw_fd();

    set_close_on_exec(fd).expect("set close on exec");

    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    assert!(flags >= 0);
    assert_ne!(flags & libc::FD_CLOEXEC, 0);
}
