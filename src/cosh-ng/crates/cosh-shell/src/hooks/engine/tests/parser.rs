use super::*;

#[test]
fn parse_timeout_seconds() {
    assert_eq!(parse_timeout("3s"), 3000);
    assert_eq!(parse_timeout("10s"), 10000);
}

#[test]
fn parse_timeout_milliseconds() {
    assert_eq!(parse_timeout("500ms"), 500);
    assert_eq!(parse_timeout("2500ms"), 2500);
}

#[test]
fn parse_timeout_raw_number() {
    assert_eq!(parse_timeout("4000"), 4000);
}

#[test]
fn parse_timeout_invalid_falls_back() {
    assert_eq!(parse_timeout("bogus"), 5000);
    assert_eq!(parse_timeout(""), 5000);
}

#[test]
fn parse_hook_header_full() {
    let dir = std::env::temp_dir().join("cosh_hook_test_full");
    let _ = fs::create_dir_all(&dir);
    let path = dir.join("my-hook.sh");
    fs::write(
        &path,
        "#!/bin/bash\n# cosh-hook: my-hook-id\n# match-commands: docker, kubectl\n# trigger: on_fail\n# timeout: 3s\necho hello\n",
    )
    .unwrap();

    let config = parse_hook_header(&path).unwrap();
    assert_eq!(config.matcher.id, "my-hook-id");
    assert_eq!(config.matcher.commands, vec!["docker", "kubectl"]);
    assert_eq!(config.matcher.trigger, HookTrigger::OnFail);
    assert_eq!(config.timeout_ms, 3000);
    assert_eq!(config.path, path);

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn parse_hook_header_defaults() {
    let dir = std::env::temp_dir().join("cosh_hook_test_defaults");
    let _ = fs::create_dir_all(&dir);
    let path = dir.join("simple.sh");
    fs::write(&path, "#!/bin/bash\n# cosh-hook: simple\n").unwrap();

    let config = parse_hook_header(&path).unwrap();
    assert_eq!(config.matcher.id, "simple");
    assert!(config.matcher.commands.is_empty());
    assert_eq!(config.matcher.trigger, HookTrigger::OnComplete);
    assert_eq!(config.timeout_ms, 5000);

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn parse_hook_header_missing_id_returns_none() {
    let dir = std::env::temp_dir().join("cosh_hook_test_no_id");
    let _ = fs::create_dir_all(&dir);
    let path = dir.join("no-id.sh");
    fs::write(&path, "#!/bin/bash\n# match-commands: git\n").unwrap();

    assert!(parse_hook_header(&path).is_none());

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn parse_hook_header_on_success_trigger() {
    let dir = std::env::temp_dir().join("cosh_hook_test_success");
    let _ = fs::create_dir_all(&dir);
    let path = dir.join("ok.sh");
    fs::write(
        &path,
        "#!/bin/bash\n# cosh-hook: ok-hook\n# trigger: on_success\n",
    )
    .unwrap();

    let config = parse_hook_header(&path).unwrap();
    assert_eq!(config.matcher.trigger, HookTrigger::OnSuccess);

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn parse_hook_header_on_complete_trigger() {
    let dir = std::env::temp_dir().join("cosh_hook_test_complete");
    let _ = fs::create_dir_all(&dir);
    let path = dir.join("complete.sh");
    fs::write(
        &path,
        "#!/bin/bash\n# cosh-hook: c-hook\n# trigger: on_complete\n",
    )
    .unwrap();

    let config = parse_hook_header(&path).unwrap();
    assert_eq!(config.matcher.trigger, HookTrigger::OnComplete);

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn parse_hook_header_timeout_ms_format() {
    let dir = std::env::temp_dir().join("cosh_hook_test_tms");
    let _ = fs::create_dir_all(&dir);
    let path = dir.join("tms.sh");
    fs::write(
        &path,
        "#!/bin/bash\n# cosh-hook: tms-hook\n# timeout: 1500ms\n",
    )
    .unwrap();

    let config = parse_hook_header(&path).unwrap();
    assert_eq!(config.timeout_ms, 1500);

    let _ = fs::remove_dir_all(&dir);
}
