use super::*;
use crate::config::{private_state, Language};
use std::fs;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{symlink, PermissionsExt};

fn fixture(runtime: bool) -> (tempfile::TempDir, UserPaths) {
    #[cfg(target_os = "linux")]
    let temp = tempfile::tempdir_in("/dev/shm").unwrap();
    #[cfg(not(target_os = "linux"))]
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let runtime_base = runtime.then(|| root.join("runtime"));
    if let Some(base) = &runtime_base {
        private_state::ensure_directory(base).unwrap();
    }
    let paths = UserPaths {
        cosh_dir: Some(root.join(".copilot-shell/cosh")),
        runtime_base,
    };
    (temp, paths)
}

fn store(paths: &UserPaths) -> GuideStore {
    GuideStore::open(paths.cosh_dir.as_ref().unwrap().join("guide")).unwrap()
}

fn run(args: &[&str], paths: &UserPaths) -> CommandOutput {
    let args = args.iter().map(OsString::from).collect::<Vec<_>>();
    run_with_paths(&args, paths, I18n::new(Language::EnUs))
}

#[test]
fn invalid_leaf_arguments_are_usage_errors_without_state_access_or_input_echo() {
    let (_temp, paths) = fixture(true);
    for args in [
        vec!["private-input"],
        vec!["on", "extra"],
        vec!["off", "--user", "someone"],
    ] {
        let result = run(&args, &paths);
        assert_eq!(result.exit_code, 2);
        assert!(result.stdout.is_empty());
        assert!(result.stderr.contains("cosh guide off"));
        assert!(!result.stderr.contains("private-input"));
    }
    let invalid = [OsString::from_vec(vec![0xff])];
    assert_eq!(
        run_with_paths(&invalid, &paths, I18n::new(Language::EnUs)).exit_code,
        2
    );
    assert!(!paths.cosh_dir.as_ref().unwrap().exists());
    assert!(!paths
        .runtime_base
        .as_ref()
        .unwrap()
        .join("cosh-guide")
        .exists());
}

#[test]
fn off_without_runtime_persists_and_keeps_banner_independent() {
    let (_temp, paths) = fixture(false);
    let result = run(&["off"], &paths);
    assert_eq!(result.exit_code, 0);
    assert!(result.stdout.contains("off"));
    assert!(result.stdout.contains("unconfirmed"));
    assert!(result.stdout.contains("cosh health banner"));
    assert!(!result.stdout.contains("uninstalled"));
    let preferences = store(&paths).load_preferences().unwrap().unwrap();
    assert!(!preferences.guide_enabled);
    assert!(preferences.banner_enabled);
}

#[test]
fn on_and_off_leave_claims_and_existing_banner_preference_untouched() {
    let (_temp, paths) = fixture(false);
    let store = store(&paths);
    store.set_banner(false).unwrap();
    let snapshot = store.snapshot(None).unwrap();
    store
        .claim(
            None,
            &snapshot,
            super::super::ClaimRequest {
                kind: super::super::InviteKind::LoginEntry,
                host: "host",
                problem: "",
                session: "s",
            },
            100,
        )
        .unwrap();
    let claims_path = store.root.join("claims.json");
    let before = fs::read(&claims_path).unwrap();
    assert_eq!(run(&["off"], &paths).exit_code, 0);
    assert_eq!(run(&["on"], &paths).exit_code, 0);
    assert_eq!(fs::read(&claims_path).unwrap(), before);
    let preferences = store.load_preferences().unwrap().unwrap();
    assert!(preferences.guide_enabled);
    assert!(!preferences.banner_enabled);
}

#[test]
fn status_distinguishes_first_use_from_corrupt_preferences() {
    let (_temp, paths) = fixture(false);
    let first = run(&["status"], &paths);
    assert_eq!(first.exit_code, 0);
    assert!(first.stdout.contains("default"));
    let preferences_path = store(&paths).root.join("preferences.json");
    assert!(!preferences_path.exists());
    private_state::atomic_write(&preferences_path, b"private-secret-input", 4096).unwrap();
    let corrupt = run(&["status"], &paths);
    assert_eq!(corrupt.exit_code, 1);
    assert!(!corrupt.stdout.contains("default"));
    assert!(corrupt.stderr.contains("corrupt"));
    assert!(!corrupt.stderr.contains("private-secret-input"));
}

#[test]
fn off_reports_runtime_closed_when_persistence_fails() {
    let (_temp, paths) = fixture(true);
    let store = store(&paths);
    private_state::atomic_write(&store.root.join("preferences.json"), b"corrupt", 4096).unwrap();
    let result = run(&["off"], &paths);
    assert_eq!(result.exit_code, 1);
    assert!(result.stderr.contains("off"));
    assert!(result.stderr.contains("saving"));
    let runtime = RuntimeState::open(paths.runtime_base.as_ref().unwrap()).unwrap();
    assert!(!runtime.read().unwrap().unwrap().enabled);
}

#[test]
fn off_reports_disable_marker_when_generation_publish_fails() {
    let (_temp, paths) = fixture(true);
    let store = store(&paths);
    let runtime = RuntimeState::open(paths.runtime_base.as_ref().unwrap()).unwrap();
    store.initialize_runtime(&runtime).unwrap();
    fs::set_permissions(
        runtime.directory().join("current"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    let result = run(&["off"], &paths);
    assert_eq!(result.exit_code, 1);
    assert!(result.stderr.contains("disable marker"));
    assert!(result.stderr.contains("not saved"));
    assert!(runtime.directory().join("disabled").exists());
}

#[test]
fn invalid_runtime_candidates_do_not_create_home_or_tmp_fallbacks() {
    let (temp, mut paths) = fixture(false);
    let root = temp.path().canonicalize().unwrap();
    let missing = root.join("missing-runtime");
    let unsafe_dir = root.join("unsafe-runtime");
    fs::create_dir(&unsafe_dir).unwrap();
    fs::set_permissions(&unsafe_dir, fs::Permissions::from_mode(0o755)).unwrap();
    let private_dir = root.join("private-runtime");
    private_state::ensure_directory(&private_dir).unwrap();
    let link = root.join("linked-runtime");
    symlink(&private_dir, &link).unwrap();
    for candidate in [&missing, &unsafe_dir, &link] {
        paths.runtime_base = Some(candidate.clone());
        let result = run(&["off"], &paths);
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("unconfirmed"));
    }
    assert!(!missing.exists());
    assert!(!private_dir.join("cosh-guide").exists());
    assert!(!paths.cosh_dir.as_ref().unwrap().join("cosh-guide").exists());
    assert!(
        !store(&paths)
            .load_preferences()
            .unwrap()
            .unwrap()
            .guide_enabled
    );
}

#[test]
fn status_reports_persisted_on_and_effective_runtime_close_without_hook_claim() {
    let (_temp, paths) = fixture(true);
    let store = store(&paths);
    store.set_guide(None, true).unwrap();
    let runtime = RuntimeState::open(paths.runtime_base.as_ref().unwrap()).unwrap();
    store.initialize_runtime(&runtime).unwrap();
    runtime.publish(false).unwrap();
    let result = run(&["status"], &paths);
    assert_eq!(result.exit_code, 0);
    assert!(result.stdout.contains("on"));
    assert!(result.stdout.contains("saved"));
    assert!(result.stdout.contains("disabled"));
    assert!(!result.stdout.contains("installed"));
    assert!(!result.stdout.contains("XDG"));
}

#[test]
fn missing_user_state_path_is_an_error_without_runtime_mutation() {
    let (_temp, mut paths) = fixture(true);
    paths.cosh_dir = None;
    let result = run(&["off"], &paths);
    assert_eq!(result.exit_code, 1);
    assert!(result.stderr.contains("HOME"));
    assert!(!paths
        .runtime_base
        .as_ref()
        .unwrap()
        .join("cosh-guide")
        .exists());
}

#[test]
fn chinese_output_uses_the_shared_catalog() {
    let (_temp, paths) = fixture(false);
    let args = [OsString::from("off")];
    let result = run_with_paths(&args, &paths, I18n::new(Language::ZhCn));
    assert_eq!(result.exit_code, 0);
    assert!(result.stdout.contains("已保存"));
    assert!(result.stdout.contains("手动运行 cosh"));
}

#[test]
fn user_facing_guide_feedback_names_invitations_and_the_independent_summary_control() {
    let (_temp, paths) = fixture(false);
    for action in ["off", "status", "on"] {
        let output = run(&[action], &paths);
        assert_eq!(output.exit_code, 0);
        assert!(output.stdout.contains("AI tips"), "{}", output.stdout);
        assert!(
            output.stdout.contains("cosh health banner off"),
            "{}",
            output.stdout
        );
        assert!(output.stdout.contains("login summary"), "{}", output.stdout);
    }
}

#[test]
fn usage_help_succeeds_without_home_or_runtime() {
    let paths = UserPaths {
        cosh_dir: None,
        runtime_base: None,
    };
    for args in [vec![], vec!["--help"], vec!["-h"]] {
        let result = run(&args, &paths);
        assert_eq!(result.exit_code, 0, "{}", result.stderr);
        assert!(result.stderr.is_empty());
        assert!(result.stdout.contains("cosh --ask 'one question'"));
        assert!(result.stdout.contains("cosh guide off"));
        assert!(result.stdout.contains("cosh health banner off"));
        assert!(result.stdout.contains("cosh login set"));
        assert!(result.stdout.contains("cosh login restore"));
        assert!(result.stdout.contains("cosh login status"));
        assert!(result.stdout.contains("--user alice"));
        assert!(result.stdout.contains("administrator"));
        assert!(result.stdout.contains("new logins"));
        assert!(result.stdout.contains("existing sessions"));
    }
}

#[test]
fn usage_help_does_not_access_or_create_config_and_runtime_state() {
    let (temp, mut paths) = fixture(false);
    let root = temp.path().canonicalize().unwrap();
    let unavailable = root.join("not-a-directory");
    fs::write(&unavailable, b"private-secret-input").unwrap();
    paths.cosh_dir = Some(unavailable.join("config"));
    paths.runtime_base = Some(unavailable.join("runtime"));
    for args in [vec![], vec!["--help"], vec!["-h"]] {
        let result = run(&args, &paths);
        assert_eq!(result.exit_code, 0, "{}", result.stderr);
        assert!(result.stderr.is_empty());
        assert!(!result.stdout.contains("private-secret-input"));
    }
    assert_eq!(fs::read(&unavailable).unwrap(), b"private-secret-input");
    assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
}

#[test]
fn usage_help_is_bilingual_and_describes_interactive_entry() {
    let paths = UserPaths {
        cosh_dir: None,
        runtime_base: None,
    };
    let english = run_with_paths(&[], &paths, I18n::new(Language::EnUs));
    assert_eq!(english.exit_code, 0);
    assert!(english.stdout.contains("AI Shell"));
    assert!(english.stdout.contains("interactive"));
    assert!(english
        .stdout
        .contains("systems supporting login switching"));
    let chinese = run_with_paths(&[], &paths, I18n::new(Language::ZhCn));
    assert_eq!(chinese.exit_code, 0);
    assert!(chinese.stdout.contains("AI Shell"));
    assert!(chinese.stdout.contains("交互"));
    assert!(chinese.stdout.contains("管理员"));
    assert!(chinese.stdout.contains("新登录"));
    assert!(chinese.stdout.contains("已有会话"));
    assert!(chinese.stdout.contains("仅支持登录切换的系统可用"));
    assert!(chinese.stdout.contains("cosh --ask '你的问题'"));
    assert_ne!(english.stdout, chinese.stdout);
}

#[test]
fn usage_help_is_readable_without_wide_description_columns() {
    let paths = UserPaths {
        cosh_dir: None,
        runtime_base: None,
    };
    for language in [crate::Language::ZhCn, crate::Language::EnUs] {
        let result = run_with_paths(&[], &paths, I18n::new(language));
        for line in result.stdout.lines() {
            assert!(crate::ui::display_width(line) <= 76, "{line}");
        }
        let text = result.stdout;
        assert!(
            text.contains(if language == crate::Language::ZhCn {
                "不影响手动使用 Cosh"
            } else {
                "Manual Cosh use stays available"
            }),
            "{text}"
        );
    }
}
