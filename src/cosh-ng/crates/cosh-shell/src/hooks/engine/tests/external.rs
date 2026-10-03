use super::*;

#[cfg(unix)]
#[test]
fn external_hook_nonzero_exit_is_no_finding() {
    let (dir, path) = write_executable_hook(
        "cosh_hook_test_external_nonzero",
        "nonzero.sh",
        "#!/bin/sh\n# cosh-hook: nonzero-hook\n# match-commands: echo\nprintf '{\"hook_id\":\"nonzero-hook\",\"severity\":\"warning\",\"title\":\"t\",\"description\":\"d\",\"suggestion\":\"s\"}'\nexit 7\n",
    );
    let config = parse_hook_header(&path).unwrap();
    let mut engine = HookEngine::new();
    engine.register_external(config);

    assert!(engine.evaluate(&make_block("echo hi")).is_empty());

    let _ = fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn external_hook_malformed_json_is_no_finding() {
    let (dir, path) = write_executable_hook(
        "cosh_hook_test_external_malformed",
        "malformed.sh",
        "#!/bin/sh\n# cosh-hook: malformed-hook\n# match-commands: echo\nprintf 'not-json'\n",
    );
    let config = parse_hook_header(&path).unwrap();
    let mut engine = HookEngine::new();
    engine.register_external(config);

    assert!(engine.evaluate(&make_block("echo hi")).is_empty());

    let _ = fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn external_hook_empty_or_stderr_only_output_is_no_finding() {
    let (empty_dir, empty_path) = write_executable_hook(
        "cosh_hook_test_external_empty",
        "empty.sh",
        "#!/bin/sh\n# cosh-hook: empty-hook\n# match-commands: echo\n",
    );
    let (stderr_dir, stderr_path) = write_executable_hook(
        "cosh_hook_test_external_stderr",
        "stderr.sh",
        "#!/bin/sh\n# cosh-hook: stderr-hook\n# match-commands: echo\necho noisy >&2\n",
    );
    let mut engine = HookEngine::new();
    engine.register_external(parse_hook_header(&empty_path).unwrap());
    engine.register_external(parse_hook_header(&stderr_path).unwrap());

    assert!(engine.evaluate(&make_block("echo hi")).is_empty());

    let _ = fs::remove_dir_all(&empty_dir);
    let _ = fs::remove_dir_all(&stderr_dir);
}

#[cfg(unix)]
#[test]
fn external_hook_timeout_is_killed_and_no_finding() {
    let (dir, path) = write_executable_hook(
        "cosh_hook_test_external_timeout",
        "timeout.sh",
        "#!/bin/sh\n# cosh-hook: timeout-hook\n# match-commands: echo\n# timeout: 20ms\nsleep 2\nprintf '{\"hook_id\":\"timeout-hook\",\"severity\":\"warning\",\"title\":\"t\",\"description\":\"d\",\"suggestion\":\"s\"}'\n",
    );
    let config = parse_hook_header(&path).unwrap();
    let mut engine = HookEngine::new();
    engine.register_external(config);

    let started = std::time::Instant::now();
    assert!(engine.evaluate(&make_block("echo hi")).is_empty());
    assert!(started.elapsed() < std::time::Duration::from_secs(1));

    let _ = fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn external_payload_cannot_forge_builtin_provenance() {
    let (dir, path) = write_executable_hook(
        "cosh_hook_test_external_provenance",
        "memory.sh",
        "#!/bin/sh\n# cosh-hook: external-memory\n# match-commands: echo\nprintf '{\"hook_id\":\"memory-pressure\",\"severity\":\"warning\",\"title\":\"t\",\"description\":\"d\",\"suggestion\":\"s\",\"builtin_facts\":{\"MemoryPressure\":{\"available_ratio\":0.01}}}'\n",
    );
    let mut engine = HookEngine::new();
    engine.register_external(parse_hook_header(&path).unwrap());

    let findings = engine.evaluate(&make_block("echo hi"));

    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].hook_id, "memory-pressure");
    assert!(matches!(
        findings[0].provenance(),
        HookProvenance::External { .. }
    ));
    assert!(findings[0].builtin_facts.is_none());

    let _ = fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn different_external_registrations_have_distinct_provenance() {
    let (first_dir, first_path) = write_executable_hook(
        "cosh_hook_test_external_registration_first",
        "first.sh",
        "#!/bin/sh\n# cosh-hook: duplicate\n# match-commands: echo\nprintf '{\"hook_id\":\"duplicate\",\"severity\":\"warning\",\"title\":\"t\",\"description\":\"d\",\"suggestion\":\"s\"}'\n",
    );
    let (second_dir, second_path) = write_executable_hook(
        "cosh_hook_test_external_registration_second",
        "second.sh",
        "#!/bin/sh\n# cosh-hook: duplicate\n# match-commands: echo\nprintf '{\"hook_id\":\"duplicate\",\"severity\":\"warning\",\"title\":\"t\",\"description\":\"d\",\"suggestion\":\"s\"}'\n",
    );
    let mut engine = HookEngine::new();
    engine.register_external(parse_hook_header(&first_path).unwrap());
    engine.register_external(parse_hook_header(&second_path).unwrap());

    let findings = engine.evaluate(&make_block("echo hi"));

    assert_eq!(findings.len(), 2);
    assert_ne!(findings[0].provenance(), findings[1].provenance());

    let _ = fs::remove_dir_all(&first_dir);
    let _ = fs::remove_dir_all(&second_dir);
}
