//! Native Git environment and lookup names must match the checked context.

use super::*;

/// Keep actual Git semantics while supplying the pinned fixture revision.
pub(super) fn git_wrapper(
    fixture: &Fixture,
    profile: &std::path::Path,
    real_git: &std::path::Path,
) -> PathBuf {
    // Only pin reporting is faked; Git itself checks tracked content and the index.
    let wrapper = profile.parent().unwrap().join("git");
    fs::write(
        &wrapper,
        format!(
            r#"#!{}
import os, sys, subprocess
from pathlib import Path
if 'var' in sys.argv and os.environ.get('FAKE_LEGACY_SYSTEM_QUERY') == '1':
    sys.stderr.write('unsupported Git configuration path variable\n')
    sys.exit(129)
if 'status' in sys.argv and '--porcelain' in sys.argv and 'core.fsmonitor=false' not in sys.argv:
    index = subprocess.check_output([{}, 'rev-parse', '--git-path', 'index']).decode().strip()
    (Path(os.environ['FAKE_ROOT']) / 'index-before-native').write_bytes(Path(index).read_bytes())
if 'rev-parse' in sys.argv and sys.argv[-1] == 'HEAD':
    print('952c941e741e922a9be8fc403c8944c6e96318bb')
else:
    os.execv({}, [{}] + sys.argv[1:])
"#,
            fixture.python.display(),
            json!(real_git),
            json!(real_git),
            json!(real_git)
        ),
    )
    .unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o700)).unwrap();
    wrapper
}

/// Select one native environment or linked-worktree regression scenario.
pub(super) fn configure(
    state: &str,
    fixture: &Fixture,
    profile: &std::path::Path,
    source: &std::path::Path,
    real_git: &std::path::Path,
) -> (Vec<(&'static str, PathBuf)>, Vec<PathBuf>) {
    let mut environment = Vec::new();
    let mut checked = Vec::new();
    let git = |cwd: &std::path::Path, args: &[&str]| {
        let mut command = Command::new(real_git);
        command.current_dir(cwd).env_clear().args(args);
        let output = fixture.run(command);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    if state.starts_with("context-selector-") {
        let external = profile.parent().unwrap().join("external");
        fs::create_dir(&external).unwrap();
        git(&external, &["init", "-q"]);
        git(
            &external,
            &[
                "config",
                "core.fsmonitor",
                source.parent().unwrap().join("fsmonitor").to_str().unwrap(),
            ],
        );
        let key = if state.contains("-common") {
            "GIT_COMMON_DIR"
        } else {
            "GIT_DIR"
        };
        let path = external.join(".git");
        if state.ends_with("-dotenv") {
            fs::write(profile.join(".env"), json!({key: path}).to_string()).unwrap();
            fs::set_permissions(profile.join(".env"), fs::Permissions::from_mode(0o600)).unwrap();
        } else {
            environment.push((key, path));
        }
        checked.push(external.join(".git/config"));
    }
    if state.starts_with("context-config-") {
        let override_path = profile.parent().unwrap().join("config-override");
        fs::write(&override_path, "[aw]\nfixture = keep\n").unwrap();
        fs::set_permissions(&override_path, fs::Permissions::from_mode(0o600)).unwrap();
        checked.push(override_path.clone());
        if state.ends_with("-dotenv") {
            let filter = format!(
                "{}; cat",
                source.parent().unwrap().join("fsmonitor").display()
            );
            fs::write(
                profile.join(".env"),
                json!({"GIT_CONFIG": override_path, "GIT_CONFIG_COUNT":"2",
                    "GIT_CONFIG_KEY_0":"aw.fixture", "GIT_CONFIG_VALUE_0":"keep",
                    "GIT_CONFIG_KEY_1":"filter.fixture.clean", "GIT_CONFIG_VALUE_1":filter})
                .to_string(),
            )
            .unwrap();
            fs::set_permissions(profile.join(".env"), fs::Permissions::from_mode(0o600)).unwrap();
        } else {
            environment.push(("GIT_CONFIG", override_path));
        }
        if !state.ends_with("-safe") {
            if !state.ends_with("-dotenv") {
                git(
                    source,
                    &[
                        "config",
                        "filter.fixture.clean",
                        &format!(
                            "{}; cat",
                            source.parent().unwrap().join("fsmonitor").display()
                        ),
                    ],
                );
            }
            fs::write(
                source.join(".git/info/attributes"),
                "native.py filter=fixture\n",
            )
            .unwrap();
        }
    }
    if state.starts_with("context-path-") {
        let bin = source.parent().unwrap().join("earlier-bin");
        fs::create_dir(&bin).unwrap();
        fs::set_permissions(
            &bin,
            fs::Permissions::from_mode(
                if state.ends_with("-safe") || state.contains("nonexecutable") {
                    0o700
                } else {
                    0o1777
                },
            ),
        )
        .unwrap();
        if state.contains("nonexecutable") {
            fs::write(bin.join("git"), "not executable\n").unwrap();
            fs::set_permissions(bin.join("git"), fs::Permissions::from_mode(0o666)).unwrap();
            checked.push(bin.join("git"));
        }
        let inherited_path = std::env::var_os("PATH").unwrap();
        let paths = std::iter::once(bin)
            .chain(std::iter::once(source.parent().unwrap().to_path_buf()))
            .chain(std::env::split_paths(&inherited_path));
        environment.push(("PATH", std::env::join_paths(paths).unwrap().into()));
    }
    if state.starts_with("context-linked-") {
        let origin = source.parent().unwrap().join("origin");
        fs::rename(source, &origin).unwrap();
        git(
            &origin,
            &[
                "worktree",
                "add",
                "--detach",
                source.to_str().unwrap(),
                "HEAD",
            ],
        );
        let gitfile = source.join(".git");
        let git_dir = fs::read_to_string(&gitfile).unwrap();
        let commondir =
            PathBuf::from(git_dir.trim().strip_prefix("gitdir: ").unwrap()).join("commondir");
        for directory in [
            source,
            commondir.parent().unwrap(),
            &origin.join(".git/worktrees"),
        ] {
            fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).unwrap();
        }
        fs::set_permissions(source.join("native.py"), fs::Permissions::from_mode(0o600)).unwrap();
        for path in [&gitfile, &commondir] {
            fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        if state.ends_with("-gitfile") {
            fs::set_permissions(&gitfile, fs::Permissions::from_mode(0o660)).unwrap();
        } else if state.ends_with("-commondir") {
            fs::set_permissions(&commondir, fs::Permissions::from_mode(0o660)).unwrap();
        }
        checked.extend([gitfile, commondir]);
    }
    (environment, checked)
}

#[test]
fn hermes_git_context_rejects_external_repository_selectors() {
    for state in [
        "context-selector-dir",
        "context-selector-common",
        "context-selector-dir-dotenv",
        "context-selector-common-dotenv",
    ] {
        check_hermes_checkout(&[state]);
    }
    check_hermes_checkout(&["clean"]);
}

#[test]
fn hermes_git_context_checks_status_configuration_with_file_override() {
    for state in [
        "context-config-parent",
        "context-config-dotenv",
        "context-config-safe",
    ] {
        check_hermes_checkout(&[state]);
    }
}

#[test]
fn hermes_git_context_protects_skipped_path_names() {
    for state in [
        "context-path-absent",
        "context-path-nonexecutable",
        "context-path-safe",
    ] {
        check_hermes_checkout(&[state]);
    }
}

#[test]
fn hermes_git_context_protects_worktree_locators() {
    for state in [
        "context-linked-gitfile",
        "context-linked-commondir",
        "context-linked-safe",
    ] {
        check_hermes_checkout(&[state]);
    }
}
