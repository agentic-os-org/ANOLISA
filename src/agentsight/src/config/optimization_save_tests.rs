use super::OptLlmConfig;

#[cfg(unix)]
#[test]
fn failed_save_preserves_previous_file() {
    const CHILD_PATH: &str = "AGENTSIGHT_ATOMIC_SAVE_FAILURE_PATH";
    if let Some(path) = std::env::var_os(CHILD_PATH) {
        let replacement = OptLlmConfig {
            model: Some("replacement".repeat(1024)),
            ..Default::default()
        };
        // Apply the file-size limit only in this subprocess, after setup.
        // SIGXFSZ must be ignored so write(2) returns EFBIG to the real save.
        unsafe {
            libc::signal(libc::SIGXFSZ, libc::SIG_IGN);
            let limit = libc::rlimit {
                rlim_cur: 64,
                rlim_max: 64,
            };
            assert_eq!(libc::setrlimit(libc::RLIMIT_FSIZE, &limit), 0);
        }
        let error = replacement.save(std::path::Path::new(&path)).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::EFBIG));
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("optimization_config.json");
    OptLlmConfig {
        base_url: Some("https://example.test/v1".into()),
        model: Some("original".into()),
        search_timeout_secs: Some(42),
        ..Default::default()
    }
    .save(&path)
    .unwrap();
    let original = std::fs::read(&path).unwrap();
    let test_name = format!(
        "{}::failed_save_preserves_previous_file",
        module_path!().split_once("::").unwrap().1
    );
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", &test_name, "--nocapture"])
        .env(CHILD_PATH, &path)
        .output()
        .unwrap();
    assert!(
        child.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&child.stdout),
        String::from_utf8_lossy(&child.stderr)
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        original,
        "a failed replacement must leave the previous config intact"
    );
    assert_eq!(
        std::fs::read_dir(dir.path()).unwrap().count(),
        1,
        "failed temporary writes must be removed"
    );
}

#[test]
fn successful_save_replaces_config_and_keeps_private_permissions() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("optimization_config.json");
    OptLlmConfig {
        model: Some("original".into()),
        ..Default::default()
    }
    .save(&path)
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    }
    OptLlmConfig {
        model: Some("replacement".into()),
        ..Default::default()
    }
    .save(&path)
    .unwrap();
    let stored: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(stored["model"], "replacement");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[cfg(unix)]
#[test]
fn saving_through_a_symlink_preserves_the_link_and_updates_its_target() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("actual.json");
    let link = dir.path().join("optimization_config.json");
    OptLlmConfig::default().save(&target).unwrap();
    std::os::unix::fs::symlink("actual.json", &link).unwrap();
    OptLlmConfig {
        model: Some("linked".into()),
        ..Default::default()
    }
    .save(&link)
    .unwrap();
    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    let stored: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&target).unwrap()).unwrap();
    assert_eq!(stored["model"], "linked");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
}
