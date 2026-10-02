use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use tempfile::TempDir;

use crate::runtime::{AcpV1ClientConfig, AcpV1Codec, ProcessExit, RuntimeSupervisor};
use serde_json::Value;

use super::{
    built_in_acp_runtime_profiles, AcpRuntimeProfileId, AcpRuntimeProfileRequest,
    AcpRuntimeProfileResolveError, AcpRuntimeProfileResolver, PROXY_ENVIRONMENT,
};

fn executable(directory: &Path, name: &str) -> PathBuf {
    let path = directory.join(name);
    fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    path
}

#[cfg(target_os = "linux")]
fn marker_executable(directory: &Path, name: &str, marker: &Path, value: &str) -> PathBuf {
    let path = directory.join(name);
    fs::write(
        &path,
        format!(
            "#!/usr/bin/env sh\nprintf '{value}' >> '{}'\nexit 0\n",
            marker.display()
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    path
}

fn wait_for_terminal(supervisor: &mut RuntimeSupervisor) -> ProcessExit {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(terminal) = supervisor.poll_terminal().unwrap() {
            return terminal.exit;
        }
        assert!(Instant::now() < deadline, "adapter child did not exit");
        thread::sleep(Duration::from_millis(5));
    }
}

fn request(
    profile: AcpRuntimeProfileId,
    executable: Option<PathBuf>,
    workspace: &Path,
    environment: BTreeMap<OsString, OsString>,
) -> AcpRuntimeProfileRequest {
    AcpRuntimeProfileRequest {
        profile,
        executable,
        workspace: workspace.to_path_buf(),
        environment,
    }
}

#[test]
fn built_in_profiles_pin_official_adapter_commands() {
    let profiles = built_in_acp_runtime_profiles();
    assert_eq!(profiles.len(), 2);
    assert_eq!(profiles[0].id(), AcpRuntimeProfileId::Codex);
    assert_eq!(profiles[0].executable_name(), "codex-acp");
    assert!(profiles[0].arguments().is_empty());
    assert_eq!(profiles[1].id(), AcpRuntimeProfileId::ClaudeCode);
    assert_eq!(profiles[1].executable_name(), "claude-agent-acp");
    assert!(profiles[1].arguments().is_empty());
}

#[test]
fn resolved_codex_profile_binds_extension_without_package_version_resolution() {
    let root = TempDir::new().unwrap();
    let adapter = executable(root.path(), "codex-acp");
    let resolved = AcpRuntimeProfileResolver::resolve(request(
        AcpRuntimeProfileId::Codex,
        Some(adapter),
        root.path(),
        BTreeMap::new(),
    ))
    .unwrap();
    let config = resolved.bind_client_config(AcpV1ClientConfig::new("test", "1", 4096));
    let mut codec = AcpV1Codec::new(config).unwrap();
    let initialize: Value = serde_json::from_str(&codec.initialize_frame().unwrap()).unwrap();

    assert_eq!(
        initialize["params"]["clientCapabilities"]["_meta"]["jetbrains"]["air"],
        serde_json::json!({"version": 1, "capabilities": ["sessionFailure"]})
    );
}

#[test]
fn rejects_explicit_command_spoofing_and_relative_paths() {
    let root = TempDir::new().unwrap();
    let shell = executable(root.path(), "sh");
    let spoofed = AcpRuntimeProfileResolver::resolve(request(
        AcpRuntimeProfileId::Codex,
        Some(shell),
        root.path(),
        BTreeMap::new(),
    ));
    assert!(matches!(
        spoofed,
        Err(AcpRuntimeProfileResolveError::ExecutableNameMismatch { .. })
    ));

    let relative = AcpRuntimeProfileResolver::resolve(request(
        AcpRuntimeProfileId::Codex,
        Some(PathBuf::from("codex-acp")),
        root.path(),
        BTreeMap::new(),
    ));
    assert!(matches!(
        relative,
        Err(AcpRuntimeProfileResolveError::ExecutableNotAbsolute(_))
    ));
}

#[test]
fn rejects_missing_and_non_executable_adapters() {
    let root = TempDir::new().unwrap();
    let missing = root.path().join("codex-acp");
    let result = AcpRuntimeProfileResolver::resolve(request(
        AcpRuntimeProfileId::Codex,
        Some(missing),
        root.path(),
        BTreeMap::new(),
    ));
    assert!(matches!(
        result,
        Err(AcpRuntimeProfileResolveError::ExecutableUnavailable { .. })
    ));

    let path = root.path().join("codex-acp");
    fs::write(&path, "not executable").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let result = AcpRuntimeProfileResolver::resolve(request(
            AcpRuntimeProfileId::Codex,
            Some(path),
            root.path(),
            BTreeMap::new(),
        ));
        assert!(matches!(
            result,
            Err(AcpRuntimeProfileResolveError::ExecutableNotExecutable(_))
        ));
    }
}

#[cfg(unix)]
#[test]
fn accepts_and_pins_an_npm_style_adapter_symlink() {
    use std::os::unix::fs::symlink;

    let root = TempDir::new().unwrap();
    let target = executable(root.path(), "arbitrary-runtime");
    let adapter = root.path().join("codex-acp");
    symlink(&target, &adapter).unwrap();

    let resolved = AcpRuntimeProfileResolver::resolve(request(
        AcpRuntimeProfileId::Codex,
        Some(adapter),
        root.path(),
        BTreeMap::from([(
            OsString::from("PATH"),
            env_path(&[PathBuf::from("/usr/bin"), PathBuf::from("/bin")]),
        )]),
    ))
    .unwrap();

    assert_eq!(resolved.executable(), fs::canonicalize(target).unwrap());
}

#[cfg(target_os = "linux")]
#[test]
fn canonical_adapter_replacement_cannot_redirect_repeated_launches() {
    use std::os::unix::fs::symlink;

    let root = TempDir::new().unwrap();
    let original_marker = root.path().join("original.marker");
    let replacement_marker = root.path().join("replacement.marker");
    let canonical_target = marker_executable(
        root.path(),
        "adapter-target.js",
        &original_marker,
        "original\n",
    );
    let adapter = root.path().join("codex-acp");
    symlink(&canonical_target, &adapter).unwrap();
    let resolved = AcpRuntimeProfileResolver::resolve(request(
        AcpRuntimeProfileId::Codex,
        Some(adapter),
        root.path(),
        BTreeMap::new(),
    ))
    .unwrap();

    let admitted_target = root.path().join("admitted-adapter-target.js");
    fs::rename(&canonical_target, &admitted_target).unwrap();
    marker_executable(
        root.path(),
        "adapter-target.js",
        &replacement_marker,
        "replacement\n",
    );

    for _ in 0..2 {
        let mut supervisor = RuntimeSupervisor::new();
        supervisor.launch(&resolved.launch_spec()).unwrap();
        assert_eq!(wait_for_terminal(&mut supervisor), ProcessExit::Code(0));
    }

    assert_eq!(
        fs::read_to_string(original_marker).unwrap(),
        "original\noriginal\n"
    );
    assert!(!replacement_marker.exists());
}

#[test]
fn path_discovery_ignores_relative_entries_and_canonicalizes() {
    let root = TempDir::new().unwrap();
    let bin = root.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let adapter = executable(&bin, "codex-acp");
    let path = env_path(&[PathBuf::from("relative-bin"), bin]);
    let environment = BTreeMap::from([(OsString::from("PATH"), path)]);

    let resolved = AcpRuntimeProfileResolver::resolve(request(
        AcpRuntimeProfileId::Codex,
        None,
        root.path(),
        environment,
    ))
    .unwrap();

    assert_eq!(resolved.executable(), fs::canonicalize(adapter).unwrap());
    let sanitized_path = resolved
        .environment
        .get(OsStr::new("PATH"))
        .expect("PATH remains available to script adapters");
    assert!(std::env::split_paths(sanitized_path).all(|entry| entry.is_absolute()));
}

#[test]
fn environment_is_filtered_per_profile_and_debug_is_redacted() {
    let root = TempDir::new().unwrap();
    let adapter = executable(root.path(), "claude-agent-acp");
    let secret = "highly-sensitive-secret";
    let proxy_secret = "proxy-sensitive-secret";
    let mut environment = BTreeMap::from([
        (OsString::from("HOME"), OsString::from("/safe/home")),
        (OsString::from("PATH"), OsString::from("/safe/bin")),
        (
            OsString::from("XDG_CONFIG_HOME"),
            OsString::from("/safe/xdg"),
        ),
        (OsString::from("ANTHROPIC_API_KEY"), OsString::from(secret)),
        (
            OsString::from("OPENAI_API_KEY"),
            OsString::from("wrong-provider"),
        ),
        (OsString::from("LD_PRELOAD"), OsString::from("/unsafe.so")),
        (
            OsString::from("NODE_OPTIONS"),
            OsString::from("--require=/unsafe.js"),
        ),
    ]);
    for name in PROXY_ENVIRONMENT {
        environment.insert(
            OsString::from(name),
            OsString::from(format!("http://user:{proxy_secret}@127.0.0.1:7890")),
        );
    }
    let request = request(
        AcpRuntimeProfileId::ClaudeCode,
        Some(adapter),
        root.path(),
        environment,
    );
    assert!(!format!("{request:?}").contains(secret));
    assert!(!format!("{request:?}").contains(proxy_secret));

    let resolved = AcpRuntimeProfileResolver::resolve(request).unwrap();
    let names: Vec<_> = resolved.environment_names().collect();
    assert!(names.contains(&OsStr::new("HOME")));
    assert!(names.contains(&OsStr::new("PATH")));
    assert!(names.contains(&OsStr::new("XDG_CONFIG_HOME")));
    assert!(names.contains(&OsStr::new("ANTHROPIC_API_KEY")));
    for name in PROXY_ENVIRONMENT {
        assert!(names.contains(&OsStr::new(name)));
    }
    assert!(!names.contains(&OsStr::new("OPENAI_API_KEY")));
    assert!(!names.contains(&OsStr::new("LD_PRELOAD")));
    assert!(!names.contains(&OsStr::new("NODE_OPTIONS")));
    assert!(!format!("{resolved:?}").contains(secret));
    assert!(!format!("{resolved:?}").contains(proxy_secret));
}

#[test]
fn workspace_is_canonical_and_must_be_a_directory() {
    let root = TempDir::new().unwrap();
    let adapter = executable(root.path(), "codex-acp");
    let workspace = root.path().join("workspace");
    fs::create_dir(&workspace).unwrap();
    let aliased_workspace = workspace.join("..").join("workspace");

    let resolved = AcpRuntimeProfileResolver::resolve(request(
        AcpRuntimeProfileId::Codex,
        Some(adapter.clone()),
        &aliased_workspace,
        BTreeMap::new(),
    ))
    .unwrap();
    assert_eq!(resolved.workspace(), fs::canonicalize(workspace).unwrap());

    let file = root.path().join("not-a-workspace");
    fs::write(&file, "data").unwrap();
    let result = AcpRuntimeProfileResolver::resolve(request(
        AcpRuntimeProfileId::Codex,
        Some(adapter),
        &file,
        BTreeMap::new(),
    ));
    assert!(matches!(
        result,
        Err(AcpRuntimeProfileResolveError::WorkspaceNotDirectory(_))
    ));
}

fn env_path(entries: &[PathBuf]) -> OsString {
    std::env::join_paths(entries).unwrap()
}
