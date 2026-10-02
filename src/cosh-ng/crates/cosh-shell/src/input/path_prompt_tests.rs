use std::os::unix::fs::PermissionsExt;

use crate::input::path_prompt::{
    is_slash_bearing_han_prompt, path_provably_missing, ShellPathCommandNames,
};
use crate::input::{AssistanceControl, InputClassifier, InterceptReason, PathPromptIntercept};
use crate::raw_input::MainPromptGate;

#[test]
fn initial_prompt_seed_is_scoped_to_path_routing() {
    let gate = MainPromptGate::default();

    gate.seed_initial_prompt();
    assert!(!gate.is_at_prompt());
    assert!(gate.is_path_prompt_ready());

    gate.set_at_prompt(false);
    assert!(!gate.is_path_prompt_ready());
    gate.set_at_prompt(true);
    assert!(gate.is_at_prompt());
    assert!(gate.is_path_prompt_ready());
}

#[test]
fn shell_path_command_names_require_a_complete_snapshot() {
    let names = ShellPathCommandNames::default();

    assert!(!names.excludes_first_token("路径/run 帮我运行一下"));
    names.set(Some(Vec::new()), Some(Vec::new()));
    assert!(names.excludes_first_token("路径/run 帮我运行一下"));
    names.set(Some(vec!["路径/run".to_string()]), Some(Vec::new()));
    assert!(!names.excludes_first_token("路径/run 帮我运行一下"));
    names.set(Some(Vec::new()), Some(vec!["txt".to_string()]));
    assert!(!names.excludes_first_token("/missing/path.txt 帮我运行一下"));
}

#[test]
fn path_prompt_routes_only_with_assisted_prompt_cwd() {
    let base = tempfile::tempdir().expect("temp dir");
    let cwd = base
        .path()
        .canonicalize()
        .expect("canonical temp dir")
        .to_string_lossy()
        .into_owned();
    let input = "打开./nonexistent-cosh-2913/SKILL.md";
    let classifier = InputClassifier::default();
    classifier.prompt_cwd().set(Some(cwd.clone()));

    assert_eq!(
        classifier.classify_missing_path_submission(input),
        Some(PathPromptIntercept {
            input: input.to_string(),
            reason: InterceptReason::NaturalLanguage,
            cwd,
        })
    );
    assert_eq!(
        classifier
            .clone()
            .with_shell_passthrough(true)
            .classify_missing_path_submission(input),
        None
    );
    assert_eq!(
        classifier
            .clone()
            .with_ai_enabled(false)
            .classify_missing_path_submission(input),
        None
    );

    let control = AssistanceControl::enabled(base.path().join("assistance-enabled"));
    control.set_enabled(false).expect("disable assistance");
    assert_eq!(
        classifier
            .clone()
            .with_assistance_control(control)
            .classify_missing_path_submission(input),
        None
    );

    let classifier_without_prompt_cwd = InputClassifier::default();
    assert_eq!(
        classifier_without_prompt_cwd.classify_missing_path_submission(input),
        None
    );
}

#[test]
fn path_prompt_classifier_keeps_command_shapes_shell_owned() {
    let base = tempfile::tempdir().expect("temp dir");
    let base_path = base.path().canonicalize().expect("canonical temp dir");
    let missing = base_path.join("missing/SKILL.md");
    let cases = [
        (
            format!("你读一下，并安装这个skill：{}", missing.display()),
            true,
        ),
        ("打开./config.toml".to_string(), true),
        (format!("{} 帮我读一下", missing.display()), true),
        ("看看../logs/app.log".to_string(), true),
        ("~/脚本啊".to_string(), false),
        ("/usr/bin/gooo".to_string(), false),
        ("https://example.com/foo".to_string(), false),
        ("打开./config.toml | cat".to_string(), false),
        ("./run.sh --all".to_string(), false),
        ("打开./x FOO=bar".to_string(), true),
        ("打开./不存在 --dry-run \"x (preview)\"".to_string(), true),
    ];
    for (input, expected) in cases {
        assert_eq!(
            is_slash_bearing_han_prompt(&input, Some(&base_path)),
            expected,
            "{input:?}"
        );
    }

    let existing_han_dir = base_path.join("打开.");
    std::fs::create_dir_all(&existing_han_dir).expect("Han path dir");
    std::fs::write(existing_han_dir.join("config.toml"), "x\n").expect("Han path file");
    assert!(!is_slash_bearing_han_prompt(
        "打开./config.toml",
        Some(&base_path)
    ));
    assert!(!is_slash_bearing_han_prompt(
        "\"打开./missing\"",
        Some(&base_path)
    ));
}

#[test]
fn path_provably_missing_requires_enoent_proof() {
    let base = tempfile::tempdir().expect("temp dir");
    let base_path = base.path().canonicalize().expect("canonical temp dir");
    let existing = base_path.join("existing.txt");
    std::fs::write(&existing, "x\n").expect("existing file");
    let dangling = base_path.join("dangling-link");
    std::os::unix::fs::symlink(base_path.join("no-such-target"), &dangling).expect("symlink");
    let opaque = base_path.join("opaque");
    std::fs::create_dir_all(&opaque).expect("opaque dir");
    let opaque_file = opaque.join("real-file");
    std::fs::write(&opaque_file, "x\n").expect("opaque file");
    std::fs::set_permissions(&opaque, std::fs::Permissions::from_mode(0o000))
        .expect("chmod opaque");

    assert!(path_provably_missing(
        base_path.join("missing.txt").to_str().expect("utf8 path"),
        None
    ));
    assert!(path_provably_missing(
        base_path
            .join("missing-dir/child")
            .to_str()
            .expect("utf8 path"),
        None
    ));
    assert!(!path_provably_missing(
        existing.to_str().expect("utf8 path"),
        None
    ));
    assert!(!path_provably_missing(
        dangling.to_str().expect("utf8 path"),
        None
    ));
    assert!(!path_provably_missing(
        opaque_file.to_str().expect("utf8 path"),
        None
    ));
    assert!(!path_provably_missing(
        existing.join("child").to_str().expect("utf8 path"),
        None
    ));

    std::fs::set_permissions(&opaque, std::fs::Permissions::from_mode(0o755))
        .expect("restore opaque");
}

#[test]
fn relative_path_proof_resolves_only_the_trusted_cwd_prefix() {
    let base = tempfile::tempdir().expect("temp dir");
    let real = base.path().join("real-workspace");
    let logical = base.path().join("logical-workspace");
    std::fs::create_dir(&real).expect("real workspace");
    std::os::unix::fs::symlink(&real, &logical).expect("logical cwd symlink");

    assert!(path_provably_missing("./missing/SKILL.md", Some(&logical)));
    let suffix_symlink = real.join("linked");
    std::os::unix::fs::symlink(real.join("missing-target"), &suffix_symlink)
        .expect("suffix symlink");
    assert!(!path_provably_missing("./linked/child", Some(&logical)));
}
