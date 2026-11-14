use super::*;
use crate::config::guide::cli::{self, UserPaths};
use crate::config::Language;
use crate::i18n::{I18n, MessageId};
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;

fn fixture() -> (tempfile::TempDir, UserPaths) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let paths = UserPaths {
        cosh_dir: Some(root.join(".copilot-shell/cosh")),
        runtime_base: None,
    };
    (temp, paths)
}

fn os(args: &[&str]) -> Vec<OsString> {
    args.iter().map(OsString::from).collect()
}

#[test]
fn terminal_ready_depends_on_capability_not_terminal_name() {
    use std::ffi::OsStr;
    for term in ["xterm-256color", "linux", "screen", "vt100"] {
        assert!(
            terminal_ready(Some(OsStr::new(term)), Some((80, 24))),
            "{term}"
        );
    }
    for term in [None, Some(OsStr::new("")), Some(OsStr::new("dumb"))] {
        assert!(!terminal_ready(term, Some((80, 24))), "{term:?}");
    }
    assert!(!terminal_ready(Some(OsStr::new("xterm")), None));
    assert!(!terminal_ready(Some(OsStr::new("xterm")), Some((39, 24))));
    assert!(!terminal_ready(Some(OsStr::new("xterm")), Some((80, 15))));
    assert!(terminal_ready(Some(OsStr::new("xterm")), Some((40, 16))));
}

#[test]
fn only_the_bare_shape_opens_the_wizard() {
    assert!(wants_wizard(&[]));
    for args in [
        vec!["--plain"],
        vec!["--help"],
        vec!["on"],
        vec!["off"],
        vec!["status"],
    ] {
        assert!(!wants_wizard(&os(&args)), "{args:?}");
    }
}

#[test]
fn static_output_is_plain_bilingual_and_within_the_content_width() {
    for language in [Language::EnUs, Language::ZhCn] {
        let i18n = I18n::new(language);
        for text in [plain_text(i18n), usage_text(i18n)] {
            assert!(!text.contains('\x1b'), "{text}");
            for line in text.lines() {
                assert!(crate::ui::display_width(line) <= 76, "{line}");
            }
            for command in [
                "cosh guide off",
                "cosh health banner off",
                "cosh login set",
                "cosh login restore",
                "cosh login status",
                "--user alice",
            ] {
                assert!(text.contains(command), "{command} missing: {text}");
            }
        }
    }
    let english = plain_text(I18n::new(Language::EnUs));
    let chinese = plain_text(I18n::new(Language::ZhCn));
    assert!(english.contains("cosh --ask 'one question'"));
    assert!(chinese.contains("cosh --ask '你的问题'"));
    assert_ne!(english, chinese);
}

#[test]
fn usage_text_keeps_the_synopsis_ahead_of_the_shared_body() {
    let i18n = I18n::new(Language::EnUs);
    let usage = usage_text(i18n);
    assert!(usage.starts_with(i18n.t(MessageId::GuideUsage)));
    assert!(usage.contains("cosh guide --plain"));
    assert!(plain_text(i18n).contains("Enter the AI Shell"));
}

#[test]
fn static_shapes_never_create_config_or_runtime_state() {
    let (temp, paths) = fixture();
    let root = temp.path().canonicalize().unwrap();
    for args in [vec![], vec!["--plain"], vec!["--help"], vec!["-h"]] {
        let output = cli::run_with_paths(&os(&args), &paths, I18n::new(Language::EnUs));
        assert_eq!(output.exit_code, 0, "{}", output.stderr);
        assert!(output.stderr.is_empty());
        assert!(!output.stdout.is_empty());
    }
    assert!(!paths.cosh_dir.as_ref().unwrap().exists());
    assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
}

#[test]
fn illegal_wizard_combinations_are_usage_errors() {
    let (_temp, paths) = fixture();
    for args in [
        vec!["--plain", "on"],
        vec!["--plain", "--help"],
        vec!["--plain", "--plain"],
        vec!["--unknown"],
    ] {
        let output = cli::run_with_paths(&os(&args), &paths, I18n::new(Language::EnUs));
        assert_eq!(output.exit_code, 2, "{args:?}");
        assert!(output.stdout.is_empty());
        assert!(output.stderr.contains("cosh guide off"));
    }
    assert!(!paths.cosh_dir.as_ref().unwrap().exists());
}
