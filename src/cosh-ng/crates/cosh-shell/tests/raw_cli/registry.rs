use std::time::Duration;

use super::*;

#[test]
fn slash_extensions_with_fake_backend_shows_degradation() {
    let output = run_raw_cli_with_input("fake", "/extensions\necho after-ext\nexit\n");

    // Fake adapter cannot query registry, should show degradation
    assert!(
        output.contains("cosh-core") || output.contains("后端"),
        "expected degradation message: {output}"
    );
    assert!(output.contains("after-ext"), "{output}");
    assert!(!output.contains("bash: /extensions"), "{output}");
}

#[test]
fn slash_skills_with_fake_backend_shows_degradation() {
    let output = run_raw_cli_with_input("fake", "/skills\necho after-skills\nexit\n");

    // Fake adapter cannot query registry, should show degradation
    assert!(
        output.contains("cosh-core") || output.contains("后端"),
        "expected degradation message: {output}"
    );
    assert!(output.contains("after-skills"), "{output}");
    assert!(!output.contains("bash: /skills"), "{output}");
}

#[test]
fn slash_extensions_with_fake_backend_zh_shows_degradation() {
    let output = run_raw_cli_with_env(
        "fake",
        "/extensions\necho after-ext-zh\nexit\n",
        &[("COSH_SHELL_LANG", "zh-CN")],
    );

    assert!(
        output.contains("cosh-core") || output.contains("后端"),
        "expected zh degradation: {output}"
    );
    assert!(output.contains("after-ext-zh"), "{output}");
}

#[test]
fn slash_skills_with_fake_backend_zh_shows_degradation() {
    let output = run_raw_cli_with_env(
        "fake",
        "/skills\necho after-skills-zh\nexit\n",
        &[("COSH_SHELL_LANG", "zh-CN")],
    );

    assert!(
        output.contains("cosh-core") || output.contains("后端"),
        "expected zh degradation: {output}"
    );
    assert!(output.contains("after-skills-zh"), "{output}");
}

#[test]
fn slash_hooks_with_fake_backend_shows_shell_hooks_section() {
    let output = run_raw_cli_with_input("fake", "/hooks\necho after-hooks\nexit\n");

    // Should show Shell Hooks section header
    assert!(
        output.contains("Shell Hooks"),
        "expected Shell Hooks section: {output}"
    );
    // Agent Hooks section should show unavailable
    assert!(
        output.contains("Agent Hooks"),
        "expected Agent Hooks section header: {output}"
    );
    assert!(output.contains("after-hooks"), "{output}");
    assert!(!output.contains("bash: /hooks"), "{output}");
}

#[test]
fn slash_extensions_not_intercepted_as_shell_command() {
    let output = run_raw_cli_with_delayed_input(
        "fake",
        vec![
            (b"/extensions\n".to_vec(), Duration::ZERO),
            (b"echo done\n".to_vec(), Duration::from_millis(200)),
            (b"exit\n".to_vec(), Duration::from_millis(100)),
        ],
    );

    // Should not leak to bash as a file path
    assert!(!output.contains("bash: /extensions"), "{output}");
    assert!(
        !output.contains("No such file or directory: /extensions"),
        "{output}"
    );
    assert!(output.contains("done"), "{output}");
}
