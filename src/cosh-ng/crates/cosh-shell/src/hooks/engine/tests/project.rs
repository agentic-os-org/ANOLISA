use super::*;

#[cfg(unix)]
#[test]
fn untrusted_project_hook_is_discovered_but_not_executed() {
    let project = std::env::temp_dir().join("cosh_hook_test_project_untrusted");
    let hooks_dir = project.join(".cosh/hooks");
    let _ = fs::remove_dir_all(&project);
    fs::create_dir_all(&hooks_dir).unwrap();
    let marker = project.join("executed.marker");
    let hook = hooks_dir.join("project.sh");
    let body = format!(
        "#!/bin/sh\n# cosh-hook: project-hook\n# match-commands: echo\ntouch '{}'\nprintf '{{\"hook_id\":\"project-hook\",\"severity\":\"warning\",\"title\":\"t\",\"description\":\"d\",\"suggestion\":\"s\"}}'\n",
        marker.display()
    );
    fs::write(&hook, body).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    }

    let mut engine = HookEngine::new();
    engine.load_project_hooks_from_root(&project, false);

    assert_eq!(engine.external_hooks().len(), 1);
    assert_eq!(
        engine.external_hooks()[0].source,
        ExternalHookSource::Project
    );
    assert!(!engine.external_hooks()[0].trusted);
    assert!(engine.evaluate(&make_block("echo hi")).is_empty());
    assert!(!marker.exists());

    let infos = engine.registered_hook_infos();
    assert_eq!(infos[0].source, HookSourceInfo::ExternalProject);
    assert_eq!(infos[0].trusted, Some(false));

    let _ = fs::remove_dir_all(&project);
}

#[cfg(unix)]
#[test]
fn trusted_project_hook_executes_after_match() {
    let project = std::env::temp_dir().join("cosh_hook_test_project_trusted");
    let hooks_dir = project.join(".cosh/hooks");
    let _ = fs::remove_dir_all(&project);
    fs::create_dir_all(&hooks_dir).unwrap();
    let marker = project.join("executed.marker");
    let hook = hooks_dir.join("project.sh");
    let body = format!(
        "#!/bin/sh\n# cosh-hook: project-hook\n# match-commands: echo\ntouch '{}'\nprintf '{{\"hook_id\":\"project-hook\",\"severity\":\"warning\",\"title\":\"t\",\"description\":\"d\",\"suggestion\":\"s\"}}'\n",
        marker.display()
    );
    fs::write(&hook, body).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    }

    let mut engine = HookEngine::new();
    engine.load_project_hooks_from_root(&project, true);

    let findings = engine.evaluate(&make_block("echo hi"));
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].hook_id, "project-hook");
    assert!(marker.exists());
    let infos = engine.registered_hook_infos();
    assert_eq!(infos[0].source, HookSourceInfo::ExternalProject);
    assert_eq!(infos[0].trusted, Some(true));

    let _ = fs::remove_dir_all(&project);
}

#[cfg(unix)]
#[test]
fn trusted_project_hook_runs_only_for_user_shell_origins() {
    let project = std::env::temp_dir().join("cosh_hook_test_project_origin_gate");
    let hooks_dir = project.join(".cosh/hooks");
    let _ = fs::remove_dir_all(&project);
    fs::create_dir_all(&hooks_dir).unwrap();
    let marker = project.join("executed.marker");
    let hook = hooks_dir.join("project.sh");
    let body = format!(
        "#!/bin/sh\n# cosh-hook: project-hook\n# match-commands: echo\ntouch '{}'\nprintf '{{\"hook_id\":\"project-hook\",\"severity\":\"warning\",\"title\":\"t\",\"description\":\"d\",\"suggestion\":\"s\"}}'\n",
        marker.display()
    );
    fs::write(&hook, body).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    }

    let mut engine = HookEngine::new();
    engine.load_project_hooks_from_root(&project, true);

    for origin in [
        crate::types::CommandOrigin::UserAnalysisAction,
        crate::types::CommandOrigin::AgentHandoff,
        crate::types::CommandOrigin::ProviderTool,
        crate::types::CommandOrigin::ShellInternal,
        crate::types::CommandOrigin::Unknown,
    ] {
        assert!(
            engine
                .evaluate_with_disabled_and_origin(&make_block("echo hi"), &HashSet::new(), origin)
                .is_empty(),
            "origin {origin:?} should not execute project hook"
        );
        assert!(!marker.exists(), "origin {origin:?} executed project hook");
    }

    for origin in [
        crate::types::CommandOrigin::UserInteractive,
        crate::types::CommandOrigin::UserSendToShell,
    ] {
        let findings = engine.evaluate_with_disabled_and_origin(
            &make_block("echo hi"),
            &HashSet::new(),
            origin,
        );
        assert_eq!(
            findings.len(),
            1,
            "origin {origin:?} should run project hook"
        );
        assert!(marker.exists(), "origin {origin:?} skipped project hook");
        let _ = fs::remove_file(&marker);
    }

    let _ = fs::remove_dir_all(&project);
}

#[cfg(unix)]
#[test]
fn user_external_hook_runs_only_for_user_shell_origins() {
    let (dir, hook) = write_executable_hook(
        "cosh_hook_test_user_origin_gate",
        "user.sh",
        "#!/bin/sh\n# cosh-hook: user-hook\n# match-commands: echo\nprintf '{\"hook_id\":\"user-hook\",\"severity\":\"warning\",\"title\":\"t\",\"description\":\"d\",\"suggestion\":\"s\"}'\n",
    );
    let mut engine = HookEngine::new();
    engine.load_hooks_from_dir(&dir);

    for origin in [
        crate::types::CommandOrigin::UserInteractive,
        crate::types::CommandOrigin::UserSendToShell,
    ] {
        let findings = engine.evaluate_with_disabled_and_origin(
            &make_block("echo hi"),
            &HashSet::new(),
            origin,
        );
        assert_eq!(findings.len(), 1, "origin {origin:?} should run user hook");
    }

    for origin in [
        crate::types::CommandOrigin::UserAnalysisAction,
        crate::types::CommandOrigin::AgentHandoff,
        crate::types::CommandOrigin::ProviderTool,
        crate::types::CommandOrigin::ShellInternal,
        crate::types::CommandOrigin::Unknown,
    ] {
        assert!(
            engine
                .evaluate_with_disabled_and_origin(&make_block("echo hi"), &HashSet::new(), origin)
                .is_empty(),
            "origin {origin:?} should not run user hook"
        );
    }

    let _ = fs::remove_file(hook);
    let _ = fs::remove_dir_all(dir);
}

#[cfg(unix)]
#[test]
fn trusted_project_hook_still_respects_disabled_filter() {
    let project = std::env::temp_dir().join("cosh_hook_test_project_disabled");
    let hooks_dir = project.join(".cosh/hooks");
    let _ = fs::remove_dir_all(&project);
    fs::create_dir_all(&hooks_dir).unwrap();
    let marker = project.join("executed.marker");
    let hook = hooks_dir.join("project.sh");
    let body = format!(
        "#!/bin/sh\n# cosh-hook: project-hook\n# match-commands: echo\ntouch '{}'\nprintf '{{\"hook_id\":\"project-hook\",\"severity\":\"warning\",\"title\":\"t\",\"description\":\"d\",\"suggestion\":\"s\"}}'\n",
        marker.display()
    );
    fs::write(&hook, body).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    }

    let mut engine = HookEngine::new();
    engine.load_project_hooks_from_root(&project, false);
    assert_eq!(engine.set_project_hooks_trusted(true), 1);

    let disabled = HashSet::from(["project-hook".to_string()]);
    assert!(engine
        .evaluate_with_disabled(&make_block("echo hi"), &disabled)
        .is_empty());
    assert!(!marker.exists());

    let _ = fs::remove_dir_all(&project);
}
