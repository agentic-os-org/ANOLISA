//! Exercise checkout inspection with real Git and native boundary markers.

use super::*;

#[path = "checkout/cases.rs"]
mod cases;
#[path = "checkout/context.rs"]
mod context;

fn check_hermes_checkout(states: &[&str]) {
    let (fixture, profile) = hermes_fixture();
    let source = profile.parent().unwrap().join("source");
    let real_git = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|path| path.join("git"))
        .find(|path| path.is_file())
        .unwrap()
        .canonicalize()
        .unwrap();
    let git = |args: &[&str]| {
        let mut command = Command::new(&real_git);
        command
            .current_dir(&source)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap())
            .env("HOME", profile.parent().unwrap().join("home"))
            .args(args);
        let output = fixture.run(command);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    };
    git(&["init", "-q"]);
    fs::write(source.join("native.py"), "original\n").unwrap();
    fs::set_permissions(source.join("native.py"), fs::Permissions::from_mode(0o600)).unwrap();
    git(&["add", "native.py"]);
    git(&[
        "-c",
        "user.name=kongche-jbw",
        "-c",
        "user.email=kongche.jbw@alibaba-inc.com",
        "-c",
        "commit.gpgsign=false",
        "commit",
        "-qm",
        "fixture",
    ]);
    let original_revision = String::from_utf8(git(&["rev-parse", "HEAD"])).unwrap();
    let original_revision = original_revision.trim();
    fs::set_permissions(source.join(".git"), fs::Permissions::from_mode(0o700)).unwrap();
    let original_git_config = fs::read(source.join(".git/config")).unwrap();
    let site = PathBuf::from(fs::read_to_string(fixture.root.join("startup-site")).unwrap());
    let pth = site.join("aw_test.pth");
    let finder = site.join("aw_test_finder.py");

    let sibling = profile.parent().unwrap().join("hermes_cli");
    let monitor = profile.parent().unwrap().join("fsmonitor");
    fs::write(
        &monitor,
        format!(
            "#!{}\nfrom pathlib import Path\nPath({}).touch()\n",
            fixture.python.display(),
            json!(fixture.root.join("fsmonitor-started"))
        ),
    )
    .unwrap();
    fs::set_permissions(&monitor, fs::Permissions::from_mode(0o700)).unwrap();
    let wrapper = context::git_wrapper(&fixture, &profile, &real_git);
    let mut paths = vec![profile.parent().unwrap().to_path_buf()];
    paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    for &state in states {
        fs::set_permissions(profile.parent().unwrap(), fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(&source, fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(source.join(".git"), fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(source.join(".git/config"), &original_git_config).unwrap();
        fs::set_permissions(
            source.join(".git/config"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        for name in ["version-started", "writer-started", "index-before-native"] {
            let marker = fixture.root.join(name);
            if marker.exists() {
                fs::remove_file(marker).unwrap();
            }
        }
        for path in [
            &pth,
            &finder,
            &source.join(".git/info/attributes"),
            &source.join(".env"),
            &source.join("sitecustomize.py"),
        ] {
            if path.exists() {
                fs::remove_file(path).unwrap();
            }
        }
        fs::set_permissions(&site, fs::Permissions::from_mode(0o700)).unwrap();
        let dotenv = profile.join(".env");
        if dotenv.exists() {
            fs::remove_file(&dotenv).unwrap();
        }
        git(&[
            "--no-replace-objects",
            "update-ref",
            "-d",
            &format!("refs/replace/{original_revision}"),
        ]);
        if sibling.exists() {
            fs::remove_dir_all(&sibling).unwrap();
        }
        let alternate = source.join("alternate");
        if alternate.exists() {
            fs::remove_dir_all(&alternate).unwrap();
        }
        let relative = source.join("relative");
        if relative.exists() {
            fs::remove_dir_all(&relative).unwrap();
        }
        let clean = (state.starts_with("config-missing-") && !state.ends_with("-unsafe"))
            || (state.starts_with("context-") && state.ends_with("-safe"))
            || matches!(
                state,
                "clean"
                    | "sticky-bin"
                    | "dotenv-safe"
                    | "fsmonitor-boolean"
                    | "stat-clean"
                    | "checkout-cache"
                    | "worktree-clean"
                    | "relative-safe"
                    | "include-missing-safe"
                    | "config-null"
            );
        for flag in ["--no-assume-unchanged", "--no-skip-worktree"] {
            git(&["update-index", flag, "native.py"]);
        }
        git(&["reset", "--hard", "-q", original_revision]);
        if matches!(state, "assume-unchanged" | "skip-worktree") {
            git(&["update-index", &format!("--{state}"), "native.py"]);
        }
        fs::write(
            source.join("native.py"),
            if matches!(
                state,
                "unstaged"
                    | "staged"
                    | "assume-unchanged"
                    | "skip-worktree"
                    | "replace-objects"
                    | "worktree-dirty"
            ) {
                "modified\n"
            } else {
                "original\n"
            },
        )
        .unwrap();
        fs::set_permissions(source.join("native.py"), fs::Permissions::from_mode(0o600)).unwrap();
        if state == "writable-root" {
            fs::set_permissions(&source, fs::Permissions::from_mode(0o775)).unwrap();
        }
        if state == "writable-file" {
            fs::set_permissions(source.join("native.py"), fs::Permissions::from_mode(0o775))
                .unwrap();
        }
        if state == "staged" {
            git(&["add", "native.py"]);
        }
        if state == "sticky-bin" {
            fs::create_dir(&sibling).unwrap();
            fs::write(
                sibling.join("__init__.py"),
                format!(
                    "from pathlib import Path\nPath({}).touch()\n",
                    json!(fixture.root.join("sibling-started"))
                ),
            )
            .unwrap();
            fs::write(sibling.join("main.py"), "main = None\n").unwrap();
            fs::set_permissions(
                profile.parent().unwrap(),
                fs::Permissions::from_mode(0o1777),
            )
            .unwrap();
        }
        if state.starts_with("checkout-sticky") {
            if state == "checkout-sticky-dotenv" {
                fs::write(source.join(".env"), "").unwrap();
                fs::set_permissions(source.join(".env"), fs::Permissions::from_mode(0o600))
                    .unwrap();
            }
            fs::set_permissions(&source, fs::Permissions::from_mode(0o1777)).unwrap();
            fs::write(&pth, format!("{}\n", source.display())).unwrap();
            fs::set_permissions(&pth, fs::Permissions::from_mode(0o600)).unwrap();
            fs::write(
                source.join("sitecustomize.py"),
                format!(
                    "from pathlib import Path\nPath({}).touch()\n",
                    json!(fixture.root.join("site-started"))
                ),
            )
            .unwrap();
        }
        let included = profile.parent().unwrap().join("monitor.config");
        if state.starts_with("include-missing-") {
            let parent = source.join("include-parent");
            fs::create_dir_all(&parent).unwrap();
            fs::set_permissions(
                &parent,
                fs::Permissions::from_mode(if state == "include-missing-safe" {
                    0o700
                } else {
                    0o1777
                }),
            )
            .unwrap();
            // Git resolves relative includes against the configuration file's directory.
            git(&["config", "include.path", "../include-parent/future.config"]);
            assert!(!parent.join("future.config").exists());
        }
        if matches!(state, "fsmonitor" | "fsmonitor-include") {
            let config = if state == "fsmonitor" {
                source.join(".git/config")
            } else {
                git(&["config", "include.path", included.to_str().unwrap()]);
                fs::set_permissions(
                    source.join(".git/config"),
                    fs::Permissions::from_mode(0o600),
                )
                .unwrap();
                included.clone()
            };
            let mut contents = fs::read(&config).unwrap_or_default();
            contents.extend_from_slice(
                format!("\n[core]\n\tfsmonitor = {}\n", monitor.display()).as_bytes(),
            );
            fs::write(&config, contents).unwrap();
            fs::set_permissions(config, fs::Permissions::from_mode(0o666)).unwrap();
        }
        if state == "fsmonitor-helper" || state == "fsmonitor-boolean" {
            git(&[
                "config",
                "core.fsmonitor",
                if state == "fsmonitor-helper" {
                    monitor.to_str().unwrap()
                } else {
                    "false"
                },
            ]);
            fs::set_permissions(
                source.join(".git/config"),
                fs::Permissions::from_mode(0o600),
            )
            .unwrap();
            fs::set_permissions(&monitor, fs::Permissions::from_mode(0o777)).unwrap();
        }
        if matches!(state, "stat-dirty" | "stat-clean") {
            let native = source.join("native.py");
            let old = std::time::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
            let stamp = |time| {
                fs::File::options()
                    .write(true)
                    .open(&native)
                    .unwrap()
                    .set_times(fs::FileTimes::new().set_modified(time))
                    .unwrap()
            };
            git(&["config", "core.trustctime", "false"]);
            git(&["config", "core.checkStat", "minimal"]);
            fs::set_permissions(
                source.join(".git/config"),
                fs::Permissions::from_mode(0o600),
            )
            .unwrap();
            stamp(old);
            git(&["update-index", "--refresh"]);
            if state == "stat-dirty" {
                fs::write(&native, "modified\n").unwrap();
                stamp(old);
            } else {
                stamp(old + Duration::from_secs(60));
            }
            assert!(git(&[
                "--no-optional-locks",
                "status",
                "--porcelain",
                "--untracked-files=no"
            ])
            .is_empty());
        }
        if state == "replace-objects" {
            git(&["add", "native.py"]);
            git(&[
                "-c",
                "user.name=kongche-jbw",
                "-c",
                "user.email=kongche.jbw@alibaba-inc.com",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-qm",
                "replacement",
            ]);
            git(&["replace", original_revision, "HEAD"]);
            git(&["reset", "--hard", "-q", original_revision]);
            fs::set_permissions(source.join("native.py"), fs::Permissions::from_mode(0o600))
                .unwrap();
            assert!(git(&["status", "--porcelain", "--untracked-files=no"]).is_empty());
        }
        if matches!(state, "site-pth" | "site-finder" | "site-directory") {
            let payload = format!(
                "import pathlib; pathlib.Path({}).touch()\n",
                json!(fixture.root.join("site-started"))
            );
            fs::write(&finder, &payload).unwrap();
            fs::set_permissions(&finder, fs::Permissions::from_mode(0o600)).unwrap();
            fs::write(
                &pth,
                if state == "site-pth" {
                    payload.as_str()
                } else {
                    "import aw_test_finder\n"
                },
            )
            .unwrap();
            fs::set_permissions(&pth, fs::Permissions::from_mode(0o600)).unwrap();
            let target = match state {
                "site-pth" => &pth,
                "site-finder" => &finder,
                _ => &site,
            };
            fs::set_permissions(target, fs::Permissions::from_mode(0o777)).unwrap();
        }
        if state == "site-sticky" {
            fs::set_permissions(&site, fs::Permissions::from_mode(0o1777)).unwrap();
        }
        let poisoned_cache = fixture.root.join("cache-path");
        if state == "checkout-cache" {
            let module = fixture.root.join("hermes_cli/env_loader.py");
            let mut command = Command::new(&fixture.python);
            command.args(["-I", "-c", r#"import importlib.util, marshal, os, struct, sys
from pathlib import Path
path, root = Path(sys.argv[1]), Path(sys.argv[2])
original = path.read_bytes()
code = compile(original + ('\nfrom pathlib import Path\nPath(' + repr(str(root / 'cache-started')) + ').touch()\n').encode(), str(path), 'exec')
cache = Path(importlib.util.cache_from_source(str(path)))
cache.parent.mkdir(exist_ok=True)
cache.parent.chmod(0o777)
cache.write_bytes(importlib.util.MAGIC_NUMBER + struct.pack('<III', 0, int(path.stat().st_mtime), len(original)) + marshal.dumps(code))
(root / 'cache-path').write_text(str(cache))
"#]).arg(module).arg(&fixture.root);
            assert!(fixture.run(command).status.success());
        }
        let cache_before = if poisoned_cache.exists() {
            Some(fs::read(fs::read_to_string(&poisoned_cache).unwrap()).unwrap())
        } else {
            None
        };
        if matches!(state, "filter-clean" | "filter-process") {
            let key = if state == "filter-clean" {
                "filter.aw.clean"
            } else {
                "filter.aw.process"
            };
            git(&["config", key, monitor.to_str().unwrap()]);
            fs::set_permissions(
                source.join(".git/config"),
                fs::Permissions::from_mode(0o600),
            )
            .unwrap();
            fs::write(source.join(".git/info/attributes"), "native.py filter=aw\n").unwrap();
            fs::set_permissions(&monitor, fs::Permissions::from_mode(0o777)).unwrap();
        }
        if state.starts_with("dotenv-") {
            let bin = profile.parent().unwrap().join("dotenv-bin");
            if !bin.exists() {
                fs::create_dir(&bin).unwrap();
            }
            let helper = bin.join("git");
            fs::write(&helper, format!("#!{}\nimport os\nfrom pathlib import Path\nPath({}).touch()\nos.execv({}, [{}] + __import__('sys').argv[1:])\n", fixture.python.display(), json!(fixture.root.join("dotenv-git-started")), json!(real_git), json!(real_git))).unwrap();
            fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();
            fs::set_permissions(&bin, fs::Permissions::from_mode(0o775)).unwrap();
            let global = profile.parent().unwrap().join("global.config");
            fs::write(
                &global,
                format!("[core]\nfsmonitor = {}\n", monitor.display()),
            )
            .unwrap();
            fs::set_permissions(&global, fs::Permissions::from_mode(0o666)).unwrap();
            let values = match state {
                "dotenv-path" => {
                    json!({"PATH":std::env::join_paths(std::iter::once(bin).chain(paths.iter().cloned())).unwrap().to_str().unwrap()})
                }
                "dotenv-global" => json!({"GIT_CONFIG_GLOBAL":global}),
                "dotenv-preload" => json!({"LD_PRELOAD":"/untrusted/library.so"}),
                "dotenv-library" => json!({"LD_LIBRARY_PATH":"/untrusted/libraries"}),
                "dotenv-audit" => json!({"LD_AUDIT":"/untrusted/audit.so"}),
                _ => {
                    json!({"PATH":std::env::join_paths(&paths).unwrap().to_str().unwrap(), "GIT_CONFIG_COUNT":"1", "GIT_CONFIG_KEY_0":"aw.fixture", "GIT_CONFIG_VALUE_0":"keep", "LD_PRELOAD":"", "LD_LIBRARY_PATH":"", "LD_AUDIT":""})
                }
            };
            fs::write(&dotenv, values.to_string()).unwrap();
            fs::set_permissions(&dotenv, fs::Permissions::from_mode(0o600)).unwrap();
        }
        if state.starts_with("worktree-") {
            fs::create_dir(&alternate).unwrap();
            fs::set_permissions(&alternate, fs::Permissions::from_mode(0o700)).unwrap();
            fs::write(alternate.join("native.py"), "original\n").unwrap();
            git(&["config", "core.worktree", alternate.to_str().unwrap()]);
        }
        if state.starts_with("relative-") {
            fs::create_dir(&relative).unwrap();
            fs::set_permissions(&relative, fs::Permissions::from_mode(0o700)).unwrap();
            if state == "relative-dirty" {
                fs::write(
                    relative.join("git"),
                    format!(
                        "#!{}\nfrom pathlib import Path\nPath({}).touch()\n",
                        fixture.python.display(),
                        json!(fixture.root.join("relative-git-started"))
                    ),
                )
                .unwrap();
                fs::set_permissions(relative.join("git"), fs::Permissions::from_mode(0o777))
                    .unwrap();
            } else {
                std::os::unix::fs::symlink(&wrapper, relative.join("git")).unwrap();
            }
        }
        if state == "source-dotenv" {
            fs::write(source.join(".env"), "HERMES_BUNDLED_PLUGINS=/untrusted\n").unwrap();
            fs::set_permissions(source.join(".env"), fs::Permissions::from_mode(0o664)).unwrap();
        }
        let mut scope_override = None;
        if state.starts_with("config-missing-") {
            let parent = source.join("scope-parent");
            fs::create_dir_all(&parent).unwrap();
            let unsafe_parent = state.ends_with("-unsafe") || state.ends_with("-disabled");
            fs::set_permissions(
                &parent,
                fs::Permissions::from_mode(if unsafe_parent { 0o1777 } else { 0o700 }),
            )
            .unwrap();
            if state.contains("-local-") {
                fs::remove_file(source.join(".git/config")).unwrap();
            } else if state.contains("-worktree-") {
                git(&["config", "extensions.worktreeConfig", "true"]);
                assert!(!source.join(".git/config.worktree").exists());
            } else if state.contains("-home-") || state.contains("-xdg-") {
                scope_override = Some((
                    if state.contains("-home-") {
                        "HOME"
                    } else {
                        "XDG_CONFIG_HOME"
                    },
                    parent,
                ));
            } else {
                scope_override = Some((
                    if state.contains("-global-") {
                        "GIT_CONFIG_GLOBAL"
                    } else {
                        "GIT_CONFIG_SYSTEM"
                    },
                    parent.join("missing.config"),
                ));
                assert!(!parent.join("missing.config").exists());
            }
            if state.contains("-local-") || state.contains("-worktree-") {
                fs::set_permissions(
                    source.join(".git"),
                    fs::Permissions::from_mode(if unsafe_parent { 0o1777 } else { 0o700 }),
                )
                .unwrap();
            }
        }
        let (overrides, checked_paths) =
            context::configure(state, &fixture, &profile, &source, &real_git);
        let original_paths: Vec<_> = checked_paths
            .iter()
            .map(|path| fs::read(path).unwrap())
            .collect();
        let source_dotenv = fs::read(source.join(".env")).ok();
        let dotenv_bytes = fs::read(&dotenv).ok();
        let git_path = |name| {
            source.join(
                String::from_utf8(git(&["rev-parse", "--git-path", name]))
                    .unwrap()
                    .trim(),
            )
        };
        let config_path = git_path("config");
        let index_path = git_path("index");
        let git_config = fs::read(&config_path).ok();
        let included_config = fs::read(&included).ok();
        let index = fs::read(&index_path).unwrap();
        let cache_parent = profile.parent().unwrap().join("cache-parent");
        if state == "cache-parent" {
            fs::create_dir(&cache_parent).unwrap();
            fs::set_permissions(&cache_parent, fs::Permissions::from_mode(0o777)).unwrap();
        }
        let mut command = context::install_command(&fixture, &profile, state);
        if state == "cache-parent" {
            command.env("TMPDIR", &cache_parent);
        }
        command
            .env("PATH", std::env::join_paths(&paths).unwrap())
            .env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", "aw.fixture")
            .env("GIT_CONFIG_VALUE_0", "keep");
        if state.starts_with("relative-") {
            command.env(
                "PATH",
                std::env::join_paths(
                    std::iter::once(PathBuf::from("relative")).chain(paths.iter().cloned()),
                )
                .unwrap(),
            );
        }
        if let Some((key, path)) = &scope_override {
            command.env(key, path);
        }
        if state == "config-missing-system-disabled" {
            command.env("GIT_CONFIG_NOSYSTEM", "1");
        }
        if state == "config-null" {
            command
                .env("GIT_CONFIG_SYSTEM", "/dev/null")
                .env("GIT_CONFIG_GLOBAL", "/dev/null");
        }
        if state == "config-system-query-unavailable" {
            command
                .env_remove("GIT_CONFIG_SYSTEM")
                .env("FAKE_LEGACY_SYSTEM_QUERY", "1");
        }
        command.envs(overrides);
        if state.starts_with("context-config-") {
            command.env_remove("HOME");
        }
        let output = fixture.run(command);
        assert!(
            !fixture.root.join("fsmonitor-started").exists(),
            "{state}: Git helper executed"
        );
        assert!(
            !fixture.root.join("site-started").exists(),
            "{state}: site startup executed"
        );
        assert_eq!(output.status.code(), Some(1));
        let error = String::from_utf8_lossy(&output.stderr);
        if state.starts_with("context-selector-") {
            assert!(
                error.contains("unset it before retrying"),
                "{state}: {error}"
            );
        }
        if state.starts_with("context-config-") && !clean {
            assert!(error.contains("external Git filters"), "{state}: {error}");
        }
        if state.starts_with("context-") && !clean {
            assert_eq!(fs::read(&index_path).unwrap(), index, "{state}");
        }
        if state == "config-system-query-unavailable" {
            assert!(
                error.contains("set GIT_CONFIG_SYSTEM to its actual path"),
                "{error}"
            );
        }
        assert_eq!(
            error.contains("no staged or unstaged tracked changes"),
            matches!(
                state,
                "unstaged" | "staged" | "replace-objects" | "stat-dirty" | "worktree-dirty"
            ),
            "{state}: {error}"
        );
        if state.starts_with("config-missing-") && state.ends_with("-unsafe")
            || matches!(
                state,
                "writable-root"
                    | "writable-file"
                    | "fsmonitor"
                    | "fsmonitor-include"
                    | "dotenv-path"
                    | "dotenv-global"
                    | "site-pth"
                    | "site-finder"
                    | "site-directory"
                    | "site-sticky"
                    | "cache-parent"
                    | "relative-dirty"
                    | "include-missing-unsafe"
                    | "checkout-sticky"
                    | "checkout-sticky-dotenv"
            )
        {
            assert!(error.contains("permits modification"), "{state}: {error}");
        }
        if state.starts_with("checkout-sticky") {
            assert!(
                error.contains("dotenv parent permits modification"),
                "{state}: {error}"
            );
        }
        if matches!(state, "dotenv-preload" | "dotenv-library" | "dotenv-audit") {
            assert!(
                error.contains("loader environment variable"),
                "{state}: {error}"
            );
        }
        if matches!(state, "filter-clean" | "filter-process") {
            assert!(error.contains("external Git filters"), "{state}: {error}");
        }
        if state == "source-dotenv" {
            assert!(error.contains(".env"), "{state}: {error}");
        }
        assert_eq!(fs::read(source.join(".env")).ok(), source_dotenv);
        assert!(!fixture.root.join("site-started").exists());
        assert!(!fixture.root.join("cache-started").exists());
        assert!(!fixture.root.join("relative-git-started").exists());
        if cache_parent.exists() {
            assert!(fs::read_dir(&cache_parent).unwrap().next().is_none());
        }
        if let Some(before) = cache_before {
            assert_eq!(
                fs::read(fs::read_to_string(&poisoned_cache).unwrap()).unwrap(),
                before
            );
        }
        if state == "fsmonitor-helper" {
            assert!(error.contains("boolean core.fsmonitor"), "{error}");
        }
        if state == "stat-clean" {
            assert_eq!(
                fs::read(fixture.root.join("index-before-native")).unwrap(),
                index
            );
        }
        if matches!(state, "assume-unchanged" | "skip-worktree") {
            assert!(
                error.contains("must not use assume-unchanged or skip-worktree"),
                "{error}"
            );
            assert_eq!(fs::read(&index_path).unwrap(), index);
        }
        assert_eq!(
            fixture.root.join("version-started").exists(),
            clean,
            "{state}: {error}"
        );
        assert_eq!(
            fixture.root.join("writer-started").exists(),
            clean,
            "{state}: {error}"
        );
        assert_eq!(
            fs::read(profile.join("config.yaml")).unwrap(),
            b"unknown: keep\n"
        );
        assert!(
            !fixture.root.join("sibling-started").exists(),
            "{state}: sibling import executed"
        );
        assert_eq!(fs::read(&config_path).ok(), git_config);
        for (path, original) in checked_paths.iter().zip(original_paths) {
            assert_eq!(
                fs::read(path).unwrap(),
                original,
                "{state}: {}",
                path.display()
            );
        }
        if let Some((key, path)) = &scope_override {
            if key.starts_with("GIT_CONFIG_") {
                assert!(!path.exists());
            }
        }
        if state.starts_with("config-missing-") && state.ends_with("-unsafe") {
            assert_eq!(fs::read(&index_path).unwrap(), index);
        }
        assert!(!source.join(".git/config.worktree").exists());
        assert_eq!(fs::read(&included).ok(), included_config);
        assert!(!fixture.root.join("dotenv-git-started").exists());
        assert_eq!(fs::read(&dotenv).ok(), dotenv_bytes);
        assert!(!profile.join("plugins").exists());
        assert!(fs::read_dir(profile.parent().unwrap())
            .unwrap()
            .all(|entry| {
                !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with("aw-hermes-")
            }));
    }
}
