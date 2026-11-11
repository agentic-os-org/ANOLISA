use super::*;
use crate::config::guide::GuideStore;
use crate::diagnostics::health::{
    HealthFact, HealthFactCategory, HealthFactSource, HealthFactValue, HealthScanReport,
};
use crate::ui::RatatuiInlineRenderer;
use crate::Language;
use std::os::unix::fs::PermissionsExt;

fn args(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

fn fixture() -> (tempfile::TempDir, GuideStore) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let store = GuideStore::open(root.join("guide")).unwrap();
    (dir, store)
}

fn report() -> HealthScanReport {
    let mut report = HealthScanReport::new("banner-fixture", 1000);
    report.host = Some("fixture-host".to_string());
    report.facts.push(HealthFact {
        id: "root-used".to_string(),
        key: "filesystem.root_used_ratio".to_string(),
        category: HealthFactCategory::Disk,
        value: HealthFactValue::Float(0.42),
        unit: None,
        source: HealthFactSource::Fixture,
        elapsed_ms: 0,
    });
    report
}

#[test]
fn parser_accepts_only_banner_management_and_budget_narrowing() {
    assert_eq!(
        parse_action(&args(&["banner"])),
        Ok(Action::Render(Options {
            budget: Duration::from_millis(500),
            disabled: vec![],
            absolute_boottime_us: None,
        }))
    );
    assert_eq!(
        parse_action(&args(&["banner", "off"])),
        Ok(Action::Set(false))
    );
    assert_eq!(
        parse_action(&args(&["banner", "on"])),
        Ok(Action::Set(true))
    );
    assert_eq!(
        parse_action(&args(&["banner", "status"])),
        Ok(Action::Status)
    );
    assert_eq!(
        parse_action(&args(&[
            "banner",
            "--budget-ms",
            "200",
            "--disable",
            "kernel.oom",
            "--disable",
            "filesystem.mounts"
        ])),
        Ok(Action::Render(Options {
            budget: Duration::from_millis(200),
            disabled: vec!["kernel.oom".to_string(), "filesystem.mounts".to_string()],
            absolute_boottime_us: None,
        }))
    );
    for values in [
        vec![],
        vec!["other"],
        vec!["banner", "--budget-ms", "501"],
        vec!["banner", "--budget-ms", "0"],
        vec!["banner", "--disable"],
        vec!["banner", "off", "--disable", "kernel.oom"],
        vec!["banner", "--budget-ms", "1", "--budget-ms", "2"],
        vec!["banner", "--enable", "kernel.oom"],
    ] {
        assert!(parse_action(&args(&values)).is_err(), "{values:?}");
    }
}

#[test]
fn noninteractive_stdio_entry_is_silent_without_opening_state_or_starting_a_worker() {
    use std::io::IsTerminal;
    assert!(!std::io::stdout().is_terminal());
    assert!(!std::io::stderr().is_terminal());
    assert_eq!(run_cli(&args(&["banner"])), 0);
}

#[test]
fn management_changes_only_banner_preference_and_status_reports_it() {
    let (_dir, store) = fixture();
    let text = manage(Action::Set(false), &store, Language::EnUs).unwrap();
    assert!(text.contains("off"));
    let preferences = store.load_preferences().unwrap().unwrap();
    assert!(!preferences.banner_enabled);
    assert!(preferences.guide_enabled);
    assert!(manage(Action::Status, &store, Language::EnUs)
        .unwrap()
        .contains("off"));
    assert!(manage(Action::Set(true), &store, Language::EnUs)
        .unwrap()
        .contains("on"));
}

#[test]
fn guide_off_retains_facts_and_banner_off_has_its_own_entry_claim() {
    let (_dir, store) = fixture();
    store.set_guide(None, false).unwrap();
    let snapshot = store.snapshot(None).unwrap();
    let report = report();
    let renderer = RatatuiInlineRenderer::plain_with_width(80);
    let prepared = presentation::prepare(
        &report,
        &renderer,
        (&store, None, &snapshot),
        Language::EnUs,
        true,
        1000,
    );
    assert!(prepared.final_text(&store, None).contains("42%"));
    assert!(!prepared.final_text(&store, None).contains("cosh --ask"));
    assert!(!prepared.final_text(&store, None).contains("cosh guide off"));
    assert!(prepared.final_text(&store, None).contains("cosh guide"));
    store.set_guide(None, true).unwrap();
    store.set_banner(false).unwrap();
    let snapshot = store.snapshot(None).unwrap();
    let prepared = presentation::prepare(
        &report,
        &renderer,
        (&store, None, &snapshot),
        Language::EnUs,
        true,
        1000,
    );
    assert!(prepared.final_text(&store, None).contains("Run cosh"));
    assert!(!prepared.final_text(&store, None).contains("42%"));
    let next = presentation::prepare(
        &report,
        &renderer,
        (&store, None, &snapshot),
        Language::EnUs,
        true,
        1001,
    );
    assert!(next.final_text(&store, None).is_empty());
}

#[test]
fn off_after_claim_discards_invitation_at_the_final_write_boundary() {
    let (_dir, store) = fixture();
    let snapshot = store.snapshot(None).unwrap();
    let prepared = presentation::prepare(
        &report(),
        &RatatuiInlineRenderer::plain_with_width(80),
        (&store, None, &snapshot),
        Language::EnUs,
        true,
        1000,
    );
    assert!(prepared
        .final_text(&store, None)
        .contains("Type cosh to ask AI"));
    store.set_guide(None, false).unwrap();
    assert!(!prepared.final_text(&store, None).contains("cosh --ask"));
    assert!(!prepared.final_text(&store, None).contains("cosh guide off"));
    assert!(prepared.final_text(&store, None).contains("cosh guide"));
    assert!(prepared.final_text(&store, None).contains("42%"));
}

#[test]
fn suppressed_or_corrupt_primary_invitation_does_not_fall_back_to_generic_promotion() {
    use crate::diagnostics::health::{HealthMessageId, HealthTryItem, HealthTryKind};
    let (dir, store) = fixture();
    let snapshot = store.snapshot(None).unwrap();
    let mut report = report();
    report.try_items.push(HealthTryItem {
        id: "memory-primary".to_string(),
        label_id: HealthMessageId::HealthTryAnalyzeMemoryPressure,
        label_args: Default::default(),
        prompt_id: Some(HealthMessageId::HealthPromptAnalyzeMemoryPressure),
        prompt_args: Default::default(),
        kind: HealthTryKind::AskAgent,
        command: None,
        reason_id: HealthMessageId::HealthTryReasonMemoryLow,
        reason_args: Default::default(),
        score: 100,
        finding_id: String::new(),
    });
    let renderer = RatatuiInlineRenderer::plain_with_width(80);
    let first = presentation::prepare(
        &report,
        &renderer,
        (&store, None, &snapshot),
        Language::EnUs,
        true,
        1000,
    );
    assert!(first.final_text(&store, None).contains("cosh --ask"));
    let second = presentation::prepare(
        &report,
        &renderer,
        (&store, None, &snapshot),
        Language::EnUs,
        true,
        1001,
    );
    assert!(second.final_text(&store, None).contains("42%"));
    assert!(!second.final_text(&store, None).contains("cosh --ask"));
    assert!(!second.final_text(&store, None).contains("cosh guide off"));
    std::fs::write(
        dir.path()
            .canonicalize()
            .unwrap()
            .join("guide/preferences.json"),
        b"corrupt",
    )
    .unwrap();
    let corrupt = presentation::prepare(
        &report,
        &renderer,
        (&store, None, &snapshot),
        Language::EnUs,
        true,
        1002,
    );
    assert!(corrupt.final_text(&store, None).contains("42%"));
    assert!(!corrupt.final_text(&store, None).contains("cosh --ask"));
    assert!(!corrupt.final_text(&store, None).contains("cosh guide off"));
}

#[test]
fn private_absolute_budget_rejects_expired_or_invalid_deadlines_without_rearming() {
    let mut options = Options {
        budget: Duration::from_millis(500),
        disabled: vec![],
        absolute_boottime_us: None,
    };
    let now = Instant::now();
    assert_eq!(
        deadline_for(&options, now),
        Some(now + Duration::from_millis(500))
    );
    assert!(deadline_for(&options, now - Duration::from_secs(1)).is_none());
    for absolute in [1, u64::MAX] {
        options.absolute_boottime_us = Some(absolute);
        assert!(deadline_for(&options, Instant::now()).is_none());
    }
    let action = parse_action(&args(&[
        "banner",
        "--budget-ms",
        "350",
        "--bash-deadline-boottime-us",
        "12345",
    ]))
    .unwrap();
    assert!(matches!(
        action,
        Action::Render(Options {
            absolute_boottime_us: Some(12345),
            ..
        })
    ));
    assert!(parse_action(&args(&["banner", "--bash-deadline-boottime-us", "0"])).is_err());
}

#[test]
fn user_facing_banner_controls_follow_the_visible_feature_for_all_preferences() {
    for banner in [false, true] {
        for guide in [false, true] {
            let (_dir, store) = fixture();
            store.set_banner(banner).unwrap();
            store.set_guide(None, guide).unwrap();
            let snapshot = store.snapshot(None).unwrap();
            let renderer = RatatuiInlineRenderer::plain_with_width(80);
            let prepared = presentation::prepare(
                &report(),
                &renderer,
                (&store, None, &snapshot),
                Language::EnUs,
                true,
                1000,
            );
            let text = prepared.final_text(&store, None);
            assert_eq!(text.contains("42%"), banner, "{text}");
            assert_eq!(text.contains("cosh guide"), banner || guide, "{text}");
            assert_eq!(
                text.contains("Run cosh") || text.contains("Type cosh to ask AI"),
                guide,
                "{text}"
            );
            assert!(
                !text.contains("guide off") && !text.contains("banner off"),
                "{text}"
            );
        }
    }
}

#[test]
fn closing_guidance_and_banner_does_not_wait_for_language_fifo() {
    use std::os::unix::fs::OpenOptionsExt;
    let _guard = crate::diagnostics::test_env::env_guard();
    struct Environment(Vec<(&'static str, Option<std::ffi::OsString>)>);
    impl Drop for Environment {
        fn drop(&mut self) {
            for (name, value) in &self.0 {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
    }
    let _saved = Environment(
        ["HOME", "XDG_RUNTIME_DIR", "COSH_SHELL_LANG", "LC_ALL"]
            .into_iter()
            .map(|name| (name, std::env::var_os(name)))
            .collect(),
    );
    for banner in [false, true] {
        let home = tempfile::tempdir().unwrap();
        std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::create_dir(home.path().join(".copilot-shell")).unwrap();
        std::fs::set_permissions(
            home.path().join(".copilot-shell"),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        let path = home.path().join(".copilot-shell/config.toml");
        nix::unistd::mkfifo(&path, nix::sys::stat::Mode::from_bits_truncate(0o600)).unwrap();
        std::env::set_var("HOME", home.path().canonicalize().unwrap());
        std::env::remove_var("XDG_RUNTIME_DIR");
        std::env::remove_var("COSH_SHELL_LANG");
        std::env::set_var("LC_ALL", "C");
        let (tx, rx) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            let code = if banner {
                crate::runtime::health_banner::run_cli(&["banner".into(), "off".into()])
            } else {
                crate::config::guide::run_cli(&["off".into()])
            };
            tx.send(code).unwrap();
        });
        let early = rx.recv_timeout(std::time::Duration::from_millis(250));
        if early.is_err() {
            let _writer = std::fs::OpenOptions::new()
                .write(true)
                .custom_flags(nix::libc::O_NONBLOCK)
                .open(&path)
                .unwrap();
        }
        thread.join().unwrap();
        let store = crate::config::guide::GuideStore::open(
            home.path()
                .canonicalize()
                .unwrap()
                .join(".copilot-shell/cosh/guide"),
        )
        .unwrap();
        let prefs = store.load_preferences().unwrap().unwrap();
        assert!(
            early.is_ok(),
            "off waited for configuration FIFO: banner={banner}"
        );
        assert!(!if banner {
            prefs.banner_enabled
        } else {
            prefs.guide_enabled
        });
    }
}

#[test]
fn private_health_claim_worker_requires_supervision_before_reading_state() {
    assert_eq!(
        run_cli(&[crate::diagnostics::health::suppression::claim::WORKER_FLAG.into()]),
        1
    );
}

#[cfg(target_os = "linux")]
#[test]
fn management_commands_do_not_open_the_language_config() {
    use std::io::Read;
    use std::os::fd::FromRawFd;
    use std::os::unix::ffi::OsStrExt;

    let _guard = crate::diagnostics::test_env::env_guard();
    struct Environment(Vec<(&'static str, Option<std::ffi::OsString>)>);
    impl Drop for Environment {
        fn drop(&mut self) {
            for (name, value) in &self.0 {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
    }
    let _saved = Environment(
        ["HOME", "XDG_RUNTIME_DIR", "COSH_SHELL_LANG", "LC_ALL"]
            .into_iter()
            .map(|name| (name, std::env::var_os(name)))
            .collect(),
    );
    let home = tempfile::tempdir().unwrap();
    let home = home.path().canonicalize().unwrap();
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::create_dir(home.join(".copilot-shell")).unwrap();
    std::fs::set_permissions(
        home.join(".copilot-shell"),
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    let path = home.join(".copilot-shell/config.toml");
    std::fs::write(&path, "[ui]\nlanguage = 'zh-CN'\n").unwrap();
    std::env::set_var("HOME", &home);
    std::env::remove_var("XDG_RUNTIME_DIR");
    std::env::remove_var("COSH_SHELL_LANG");
    std::env::set_var("LC_ALL", "C");

    let fd = unsafe { nix::libc::inotify_init1(nix::libc::IN_NONBLOCK | nix::libc::IN_CLOEXEC) };
    assert!(fd >= 0);
    let mut notifications = unsafe { std::fs::File::from_raw_fd(fd) };
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    assert!(unsafe { nix::libc::inotify_add_watch(fd, path.as_ptr(), nix::libc::IN_OPEN) } >= 0);

    let mut opened = Vec::new();
    for banner in [false, true] {
        for action in ["off", "status", "on"] {
            let code = if banner {
                run_cli(&args(&["banner", action]))
            } else {
                crate::config::guide::run_cli(&args(&[action]))
            };
            assert_eq!(code, 0, "banner={banner}, action={action}");
            let mut bytes = [0; 1024];
            match notifications.read(&mut bytes) {
                Ok(count) if count > 0 => opened.push((banner, action)),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                result => panic!("unexpected config notification result: {result:?}"),
            }
        }
    }
    assert!(
        opened.is_empty(),
        "management opened language config: {opened:?}"
    );
}

#[test]
fn suppressed_invitation_keeps_only_the_guide_inside_the_card() {
    let (_dir, store) = fixture();
    store.set_guide(None, false).unwrap();
    let snapshot = store.snapshot(None).unwrap();
    let report = report();
    let renderer = RatatuiInlineRenderer::plain_with_width(80);
    let prepared = presentation::prepare(
        &report,
        &renderer,
        (&store, None, &snapshot),
        Language::EnUs,
        true,
        1000,
    );
    let text = prepared.final_text(&store, None);
    assert_eq!(text.matches("cosh guide").count(), 1, "{text}");
}
