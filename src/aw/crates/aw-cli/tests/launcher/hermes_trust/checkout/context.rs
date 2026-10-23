//! Native Git environment and lookup names must match the checked context.

use super::*;

/// Run each context case from the same working directory as native Git.
pub(super) fn install_command(
    fixture: &Fixture,
    profile: &std::path::Path,
    state: &str,
) -> Command {
    let mut command = command(fixture, profile, "install");
    if state.starts_with("context-ceiling-") {
        command.current_dir(profile.parent().unwrap().join("ceiling/launch"));
    }
    command
}

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
if (Path(os.environ['FAKE_ROOT']) / 'scratch-hook-started').exists():
    raise RuntimeError('scratch-index hook executed before native imports')
if os.environ.get('FAKE_PATH_BYTES') == '1' and b'\xff' not in os.environb.get(b'PATH', b''):
    raise RuntimeError('Git environment PATH bytes changed')
if 'var' in sys.argv and os.environ.get('FAKE_LEGACY_SYSTEM_QUERY') == '1':
    sys.stderr.write('unsupported Git configuration path variable\n')
    sys.exit(129)
if 'status' in sys.argv and '--porcelain' in sys.argv and 'core.fsmonitor=false' not in sys.argv:
    index = subprocess.check_output([{}, 'rev-parse', '--git-path', 'index']).decode().strip()
    (Path(os.environ['FAKE_ROOT']) / 'index-before-native').write_bytes(Path(index).read_bytes())
    if os.environ.get('FAKE_SCRATCH_HOOK') == '1':
        assert Path(index).read_bytes() == (Path(os.environ['FAKE_ROOT']) / 'original-index').read_bytes(), 'AW preflight changed the native index'
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
    fs::set_permissions(source.join(".git/hooks"), fs::Permissions::from_mode(0o700)).unwrap();
    let git = |cwd: &std::path::Path, args: &[&str]| {
        let mut command = Command::new(real_git);
        command.current_dir(cwd).env_clear().args(args);
        let output = fixture.run(command);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        if args.first() == Some(&"init") {
            fs::set_permissions(cwd.join(".git/hooks"), fs::Permissions::from_mode(0o700)).unwrap();
        }
    };
    if state.starts_with("context-env-") {
        if state.contains("-bytes-") {
            environment.push(("FAKE_PATH_BYTES", "1".into()));
        }
        let path = std::env::join_paths(
            std::iter::once(
                profile
                    .parent()
                    .unwrap()
                    .join(if state.contains("-bytes-") {
                        use std::os::unix::ffi::OsStringExt;
                        std::ffi::OsString::from_vec(vec![0xff])
                    } else {
                        "missing".into()
                    }),
            )
            .chain(std::iter::repeat_n(
                profile.parent().unwrap().to_path_buf(),
                200,
            ))
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
        )
        .unwrap();
        assert!(path.len() > 4096);
        if state.contains("-dotenv-") {
            fs::write(
                profile.join(".env"),
                json!({"PATH":path.to_str().unwrap()}).to_string(),
            )
            .unwrap();
            fs::set_permissions(profile.join(".env"), fs::Permissions::from_mode(0o600)).unwrap();
        } else {
            environment.push(("PATH", path.into()));
        }
    }
    if state == "sticky-bin" {
        let cwd = profile.parent().unwrap();
        git(cwd, &["init", "-q"]);
        fs::set_permissions(cwd.join(".git"), fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(cwd.join(".git/config"), fs::Permissions::from_mode(0o600)).unwrap();
    }
    if state.starts_with("context-ceiling-") {
        let root = profile.parent().unwrap();
        let ceiling = root.join("ceiling");
        let launch = ceiling.join("launch");
        fs::create_dir_all(&launch).unwrap();
        for directory in [&ceiling, &launch] {
            fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let repository = if state.ends_with("-safe") {
            root
        } else {
            &launch
        };
        git(repository, &["init", "-q"]);
        git(
            repository,
            &[
                "config",
                "core.fsmonitor",
                root.join("fsmonitor").to_str().unwrap(),
            ],
        );
        fs::set_permissions(repository.join(".git"), fs::Permissions::from_mode(0o700)).unwrap();
        let config = repository.join(".git/config");
        fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
        checked.push(config);
        let selected = if state.contains("-link-") {
            let link = root.join("ceiling-link");
            std::os::unix::fs::symlink(&ceiling, &link).unwrap();
            link
        } else if state.ends_with("-current") {
            launch.clone()
        } else {
            ceiling
        };
        let mut probe = Command::new(real_git);
        probe
            .current_dir(&launch)
            .env_clear()
            .env("GIT_CEILING_DIRECTORIES", &selected)
            .args(["rev-parse", "--absolute-git-dir"]);
        assert_eq!(
            fixture.run(probe).status.success(),
            !state.ends_with("-safe")
        );
        if state.contains("-dotenv-") {
            fs::write(
                profile.join(".env"),
                json!({"GIT_CEILING_DIRECTORIES":selected}).to_string(),
            )
            .unwrap();
            fs::set_permissions(profile.join(".env"), fs::Permissions::from_mode(0o600)).unwrap();
            // The inherited check still sees the ancestor; only dotenv hides it.
            git(root, &["config", "--unset", "core.fsmonitor"]);
            let include = root.join(".git/launch.conf");
            fs::write(&include, "[aw]\nfixture = keep\n").unwrap();
            fs::set_permissions(&include, fs::Permissions::from_mode(0o600)).unwrap();
            git(root, &["config", "include.path", "launch.conf"]);
            checked.push(include);
        } else {
            environment.push(("GIT_CEILING_DIRECTORIES", selected));
        }
    }
    if state.starts_with("context-cwd-") {
        let cwd = profile.parent().unwrap();
        git(cwd, &["init", "-q"]);
        fs::set_permissions(cwd.join(".git"), fs::Permissions::from_mode(0o700)).unwrap();
        let config = cwd.join(".git/config");
        if state.contains("include") {
            let include = cwd.join("launch-include.conf");
            fs::write(
                &include,
                format!("[core]\nfsmonitor = {}\n", cwd.join("fsmonitor").display()),
            )
            .unwrap();
            fs::set_permissions(&include, fs::Permissions::from_mode(0o600)).unwrap();
            git(
                cwd,
                &[
                    "config",
                    &format!("includeIf.gitdir:{}/.git.path", cwd.display()),
                    include.to_str().unwrap(),
                ],
            );
            checked.push(include);
        } else if !state.ends_with("-safe") {
            git(
                cwd,
                &[
                    "config",
                    "core.fsmonitor",
                    cwd.join("fsmonitor").to_str().unwrap(),
                ],
            );
        }
        fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
        if !state.ends_with("-safe") {
            // Prove only the launch repository activates the helper.
            git(cwd, &["status", "--porcelain", "--untracked-files=no"]);
            assert!(fixture.root.join("fsmonitor-started").exists());
            fs::remove_file(fixture.root.join("fsmonitor-started")).unwrap();
            git(source, &["status", "--porcelain", "--untracked-files=no"]);
            assert!(!fixture.root.join("fsmonitor-started").exists());
        }
        checked.push(config);
    }
    if state == "context-locator-absent" {
        let cwd = profile.parent().unwrap();
        assert!(!cwd.join(".git").exists());
        fs::set_permissions(cwd, fs::Permissions::from_mode(0o1777)).unwrap();
    } else if state.starts_with("context-locator-") {
        let cwd = profile.parent().unwrap();
        git(cwd, &["init", "-q"]);
        let metadata = cwd.join("launch-metadata");
        fs::rename(cwd.join(".git"), &metadata).unwrap();
        fs::set_permissions(&metadata, fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(metadata.join("config"), fs::Permissions::from_mode(0o600)).unwrap();
        let locator = cwd.join(".git");
        fs::write(&locator, format!("gitdir: {}\n", metadata.display())).unwrap();
        fs::set_permissions(
            &locator,
            fs::Permissions::from_mode(if state.ends_with("-gitfile") {
                0o660
            } else {
                0o600
            }),
        )
        .unwrap();
        if state.ends_with("-metadata") {
            fs::set_permissions(&metadata, fs::Permissions::from_mode(0o1777)).unwrap();
        }
        checked.extend([locator, metadata.join("config")]);
    }
    if state.starts_with("context-git-binary-") {
        let cwd = profile.parent().unwrap();
        let wrapper = cwd.join("git");
        for directory in [cwd, source] {
            let bin = directory.join("different-git");
            fs::create_dir(&bin).unwrap();
            fs::set_permissions(&bin, fs::Permissions::from_mode(0o700)).unwrap();
            let executable = bin.join("git");
            fs::copy(&wrapper, &executable).unwrap();
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
            checked.push(executable);
        }
        let path = std::env::join_paths(
            [PathBuf::from("different-git"), cwd.to_path_buf()]
                .into_iter()
                .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
        )
        .unwrap();
        if state.ends_with("-dotenv") {
            fs::write(
                profile.join(".env"),
                json!({"PATH":path.to_str().unwrap()}).to_string(),
            )
            .unwrap();
            fs::set_permissions(profile.join(".env"), fs::Permissions::from_mode(0o600)).unwrap();
        } else {
            environment.push(("PATH", path.into()));
        }
    }
    if state == "context-hook-disabled-safe" {
        git(source, &["config", "core.hooksPath", "/dev/null"]);
        fs::set_permissions(
            source.join(".git/config"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
    } else if state.starts_with("context-hook-") {
        let hooks = if state.contains("custom") {
            let path = source.parent().unwrap().join("custom-hooks");
            git(
                source,
                &["config", "core.hooksPath", path.to_str().unwrap()],
            );
            fs::set_permissions(
                source.join(".git/config"),
                fs::Permissions::from_mode(0o600),
            )
            .unwrap();
            path
        } else {
            source.join(".git/hooks")
        };
        fs::create_dir_all(&hooks).unwrap();
        fs::set_permissions(&hooks, fs::Permissions::from_mode(0o700)).unwrap();
        let hook = hooks.join("post-index-change");
        fs::write(&hook, format!("#!{}\nimport os\nfrom pathlib import Path\nindex = os.environ.get('GIT_INDEX_FILE')\nif index:\n    (Path(os.environ['FAKE_ROOT']) / 'scratch-hook-started').write_text(index)\nelif {}:\n    (Path(os.environ['FAKE_ROOT']) / 'fsmonitor-started').touch()\n", fixture.python.display(), if state.ends_with("-safe") { "False" } else { "True" })).unwrap();
        fs::set_permissions(
            &hook,
            fs::Permissions::from_mode(if state.ends_with("-safe") {
                0o700
            } else {
                0o777
            }),
        )
        .unwrap();
        fs::write(
            fixture.root.join("original-index"),
            fs::read(source.join(".git/index")).unwrap(),
        )
        .unwrap();
        let proof_index = fixture.root.join("hook-proof-index");
        let output = fixture.run({
            let mut command = Command::new(real_git);
            command
                .current_dir(source)
                .env_clear()
                .env("GIT_CONFIG_SYSTEM", "/dev/null")
                .env("FAKE_ROOT", &fixture.root)
                .env("GIT_INDEX_FILE", &proof_index)
                .args([
                    "--no-replace-objects",
                    "--no-optional-locks",
                    "-c",
                    "core.fsmonitor=false",
                    "-c",
                    "core.splitIndex=false",
                    "read-tree",
                    "HEAD",
                ]);
            command
        });
        assert!(output.status.success());
        let marker = fixture.root.join("scratch-hook-started");
        assert_eq!(
            fs::read_to_string(&marker).unwrap(),
            proof_index.to_str().unwrap()
        );
        fs::remove_file(marker).unwrap();
        fs::remove_file(proof_index).unwrap();
        // Native status refreshes this stale index even though the bytes are unchanged.
        fs::File::open(source.join("native.py"))
            .unwrap()
            .set_times(
                fs::FileTimes::new()
                    .set_modified(std::time::SystemTime::now() + Duration::from_secs(2)),
            )
            .unwrap();
        environment.push(("FAKE_SCRATCH_HOOK", "1".into()));
        checked.push(hook);
    }
    if state.starts_with("context-scope-") {
        let (key, relative, suffix) = if state.contains("global") {
            ("GIT_CONFIG_GLOBAL", "relative.conf", "")
        } else if state.contains("system") {
            ("GIT_CONFIG_SYSTEM", "relative.conf", "")
        } else if state.contains("home") {
            ("HOME", "relative-home", ".gitconfig")
        } else {
            ("XDG_CONFIG_HOME", "relative-xdg", "git/config")
        };
        let cwd = profile.parent().unwrap();
        let path = if suffix.is_empty() {
            cwd.join(relative)
        } else {
            cwd.join(relative).join(suffix)
        };
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "[core]\nfsmonitor = unchecked-cwd-helper\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o660)).unwrap();
        for directory in [source, cwd] {
            let output = fixture.run({
                let mut command = Command::new(real_git);
                command
                    .current_dir(directory)
                    .env_clear()
                    .env("GIT_CONFIG_SYSTEM", "/dev/null")
                    .env(key, relative)
                    .args(["config", "--get", "core.fsmonitor"]);
                command
            });
            assert_eq!(output.status.success(), directory == cwd);
            if directory == cwd {
                assert_eq!(output.stdout, b"unchecked-cwd-helper\n");
            }
        }
        if state.ends_with("-dotenv") {
            fs::write(profile.join(".env"), json!({key:relative}).to_string()).unwrap();
            fs::set_permissions(profile.join(".env"), fs::Permissions::from_mode(0o600)).unwrap();
        } else {
            environment.push((key, relative.into()));
        }
        checked.push(path);
    }
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

#[test]
fn hermes_git_context_rejects_relative_scopes() {
    for state in [
        "context-scope-global",
        "context-scope-system",
        "context-scope-home",
        "context-scope-xdg",
        "context-scope-global-dotenv",
        "context-scope-system-dotenv",
        "context-scope-home-dotenv",
        "context-scope-xdg-dotenv",
    ] {
        check_hermes_checkout(&[state]);
    }
    check_hermes_checkout(&["clean"]);
}

#[test]
fn hermes_git_scratch_index_disables_checkout_hooks() {
    for state in ["context-hook-default-safe", "context-hook-custom-safe"] {
        check_hermes_checkout(&[state]);
    }
}

#[test]
fn hermes_git_native_index_hook_requires_trusted_target() {
    for state in ["context-hook-default", "context-hook-custom"] {
        check_hermes_checkout(&[state]);
    }
    check_hermes_checkout(&["context-hook-disabled-safe"]);
}

#[test]
fn hermes_git_context_requires_one_executable() {
    for state in ["context-git-binary-parent", "context-git-binary-dotenv"] {
        check_hermes_checkout(&[state]);
    }
    check_hermes_checkout(&["relative-safe", "clean"]);
}

#[test]
fn hermes_git_context_checks_the_launch_repository() {
    for state in [
        "context-cwd-fsmonitor",
        "context-cwd-include",
        "context-cwd-safe",
    ] {
        check_hermes_checkout(&[state]);
    }
}

#[test]
fn hermes_git_context_protects_launch_locators() {
    for state in [
        "context-locator-gitfile",
        "context-locator-metadata",
        "context-locator-absent",
        "context-locator-safe",
    ] {
        check_hermes_checkout(&[state]);
    }
}

#[test]
fn hermes_git_context_honors_discovery_ceilings() {
    for state in [
        "context-ceiling-parent-safe",
        "context-ceiling-link-safe",
        "context-ceiling-dotenv-safe",
        "context-ceiling-current",
        "context-ceiling-nearer",
    ] {
        check_hermes_checkout(&[state]);
    }
}
