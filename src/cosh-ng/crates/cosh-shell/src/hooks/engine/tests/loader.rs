use super::*;

#[test]
fn load_hooks_from_dir_skips_non_executable() {
    let dir = std::env::temp_dir().join("cosh_hook_test_noexec");
    let _ = fs::remove_dir_all(&dir);
    let _ = fs::create_dir_all(&dir);

    // Non-executable file
    let path = dir.join("no-exec.sh");
    fs::write(&path, "#!/bin/bash\n# cosh-hook: no-exec\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    }

    // Executable file
    let path2 = dir.join("exec.sh");
    fs::write(&path2, "#!/bin/bash\n# cosh-hook: exec-hook\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path2, fs::Permissions::from_mode(0o755)).unwrap();
    }

    let mut engine = HookEngine::new();
    engine.load_hooks_from_dir(&dir);

    assert_eq!(engine.external_hooks().len(), 1);
    assert_eq!(engine.external_hooks()[0].matcher.id, "exec-hook");

    let _ = fs::remove_dir_all(&dir);

    #[cfg(unix)]
    {
        assert_loader_skips_symlinked_hooks();
        assert_loader_resists_replacement_symlink_race();
    }
}

#[test]
fn load_project_hooks_missing_dir_is_noop() {
    let project = std::env::temp_dir().join("cosh_hook_test_project_missing_dir");
    let _ = fs::remove_dir_all(&project);
    fs::create_dir_all(&project).unwrap();

    let mut engine = HookEngine::new();
    engine.load_project_hooks_from_root(&project, false);

    assert!(engine.external_hooks().is_empty());
    assert!(engine.registered_hook_infos().is_empty());

    let _ = fs::remove_dir_all(&project);
}

#[cfg(unix)]
fn assert_loader_skips_symlinked_hooks() {
    use std::os::unix::fs::symlink;

    let (outside_dir, outside_path) = write_executable_hook(
        "loader-symlink-target",
        "outside.sh",
        "#!/bin/sh\n# cosh-hook: outside\n",
    );
    let (dir, path) = write_executable_hook(
        "loader-symlink-dir",
        "link.sh",
        "#!/bin/sh\n# cosh-hook: placeholder\n",
    );
    fs::remove_file(&path).unwrap();
    symlink(&outside_path, &path).unwrap();

    let mut engine = HookEngine::new();
    engine.load_hooks_from_dir(&dir);

    assert!(engine.external_hooks().is_empty());
    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&outside_dir);
}

#[cfg(unix)]
fn assert_loader_resists_replacement_symlink_race() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

    let (outside_dir, outside_path) = write_executable_hook(
        "loader-race-target",
        "outside.sh",
        "#!/bin/sh\n# cosh-hook: outside\n",
    );
    let (dir, path) = write_executable_hook(
        "loader-race-dir",
        "candidate.sh",
        "#!/bin/sh\n# cosh-hook: trusted\n",
    );
    let stop = Arc::new(AtomicBool::new(false));
    let stop_replacer = Arc::clone(&stop);
    let replacement_path = path.clone();
    let replacement_target = outside_path.clone();
    let replacer = std::thread::spawn(move || {
        while !stop_replacer.load(Ordering::Relaxed) {
            let _ = fs::remove_file(&replacement_path);
            let _ = symlink(&replacement_target, &replacement_path);
            std::thread::yield_now();
            let _ = fs::remove_file(&replacement_path);
            if fs::write(&replacement_path, "#!/bin/sh\n# cosh-hook: trusted\n").is_ok() {
                let _ = fs::set_permissions(&replacement_path, fs::Permissions::from_mode(0o755));
            }
        }
    });

    for _ in 0..200 {
        let mut engine = HookEngine::new();
        engine.load_hooks_from_dir(&dir);
        assert!(engine
            .external_hooks()
            .iter()
            .all(|hook| hook.matcher.id == "trusted"));
    }

    stop.store(true, Ordering::Relaxed);
    replacer.join().unwrap();
    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&outside_dir);
}
