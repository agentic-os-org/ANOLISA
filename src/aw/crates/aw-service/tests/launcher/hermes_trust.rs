//! Profile trust is checked before native imports; checkout checks include the index.

use super::*;
use std::{os::unix::fs::PermissionsExt, path::PathBuf, process::Command};

fn hermes_fixture() -> (Fixture, PathBuf) {
    let mut fixture = Fixture::new();
    let profile = fixture.native_profile();
    fs::write(profile.join("config.yaml"), "unknown: keep\n").unwrap();
    fs::set_permissions(
        profile.join("config.yaml"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let hermes = fixture.root.join("hermes");
    fs::write(
        &hermes,
        format!(
            r#"#!{}
# from hermes_cli.main import main
import os, sys
from pathlib import Path
root = Path(os.environ['FAKE_ROOT'])
if '--version' in sys.argv:
    (root / 'version-started').touch()
    print('Install directory: ' + str(root))
else:
    (root / 'writer-started').touch()
    sys.exit(1)
"#,
            fixture.python.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&hermes, fs::Permissions::from_mode(0o700)).unwrap();
    let mut document = fixture.document();
    document["spec"]["agents"] = json!({"hermes":{"adapter":"hermes","argv":[hermes,"chat"]}});
    fixture.save(&document);
    (fixture, profile)
}

fn command(fixture: &Fixture, profile: &std::path::Path, operation: &str) -> Command {
    let mut command = fixture.command();
    command
        .args([operation, "--config"])
        .arg(fixture.root.join("aw.json"))
        .args(["--agent", "hermes", "--native-profile"])
        .arg(profile);
    command
}

#[test]
fn hermes_rejects_help_before_probes_or_profile_changes() {
    let (fixture, profile) = hermes_fixture();
    for flag in ["--help", "-h"] {
        for (operation, appended) in [("install", false), ("run", false), ("run", true)] {
            let mut document = fixture.document();
            let mut argv = vec![json!(fixture.root.join("hermes")), json!("chat")];
            if !appended {
                argv.push(json!(flag));
            }
            document["spec"]["agents"] = json!({"hermes":{"adapter":"hermes","argv":argv}});
            fixture.save(&document);
            let mut command = command(&fixture, &profile, operation);
            if appended {
                command.args(["--", flag]);
            }
            let output = fixture.run(command);
            assert_eq!(output.status.code(), Some(1));
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                stderr.contains("run hermes chat --help directly"),
                "{stderr}"
            );
            assert!(!fixture.root.join("version-started").exists());
            assert!(!fixture.root.join("writer-started").exists());
            assert_eq!(
                fs::read(profile.join("config.yaml")).unwrap(),
                b"unknown: keep\n"
            );
            assert_eq!(fs::read_dir(&profile).unwrap().count(), 1);
        }
    }
}

#[test]
fn hermes_rejects_writable_dotenv_before_run_or_install_probes() {
    let (fixture, profile) = hermes_fixture();
    let dotenv = profile.join(".env");
    fs::write(&dotenv, "PATH=/untrusted\n").unwrap();
    for mode in [0o664, 0o666] {
        fs::set_permissions(&dotenv, fs::Permissions::from_mode(mode)).unwrap();
        for operation in ["run", "install"] {
            let output = fixture.run(command(&fixture, &profile, operation));
            assert_eq!(output.status.code(), Some(1));
            assert!(String::from_utf8_lossy(&output.stderr).contains("Hermes profile .env"));
            assert!(!fixture.root.join("version-started").exists());
            assert!(!fixture.root.join("writer-started").exists());
            assert_eq!(
                fs::read(profile.join("config.yaml")).unwrap(),
                b"unknown: keep\n"
            );
            assert_eq!(fs::read(&dotenv).unwrap(), b"PATH=/untrusted\n");
            assert_eq!(fs::read_dir(&profile).unwrap().count(), 2);
        }
    }
}

#[test]
fn hermes_rejects_tracked_worktree_and_index_changes() {
    let (fixture, profile) = hermes_fixture();
    let real_git = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|path| path.join("git"))
        .find(|path| path.is_file())
        .unwrap()
        .canonicalize()
        .unwrap();
    let git = |args: &[&str]| {
        let mut command = Command::new(&real_git);
        command
            .current_dir(&fixture.root)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap())
            .env("HOME", fixture.root.join("home"))
            .args(args);
        let output = fixture.run(command);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init", "-q"]);
    fs::write(fixture.root.join("native.py"), "original\n").unwrap();
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
    // Only pin reporting is faked; Git itself checks tracked content and the index.
    let wrapper = fixture.root.join("git");
    fs::write(
        &wrapper,
        format!(
            r#"#!{}
import os, sys
if 'rev-parse' in sys.argv:
    print('952c941e741e922a9be8fc403c8944c6e96318bb')
else:
    os.execv({}, [{}] + sys.argv[1:])
"#,
            fixture.python.display(),
            json!(real_git),
            json!(real_git)
        ),
    )
    .unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o700)).unwrap();
    let mut paths = vec![fixture.root.clone()];
    paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    for state in ["unstaged", "staged", "clean"] {
        fs::write(
            fixture.root.join("native.py"),
            if state == "clean" {
                "original\n"
            } else {
                "modified\n"
            },
        )
        .unwrap();
        if state != "unstaged" {
            git(&["add", "native.py"]);
        }
        let mut command = command(&fixture, &profile, "install");
        command.env("PATH", std::env::join_paths(&paths).unwrap());
        let output = fixture.run(command);
        assert_eq!(output.status.code(), Some(1));
        let error = String::from_utf8_lossy(&output.stderr);
        assert_eq!(
            error.contains("no staged or unstaged tracked changes"),
            state != "clean",
            "{state}: {error}"
        );
        assert!(fixture.root.join("version-started").exists());
        assert_eq!(
            fixture.root.join("writer-started").exists(),
            state == "clean"
        );
        assert_eq!(
            fs::read(profile.join("config.yaml")).unwrap(),
            b"unknown: keep\n"
        );
        assert!(!profile.join("plugins").exists());
    }
}

#[test]
fn hermes_rejects_non_executable_or_special_explicit_entrypoints() {
    use std::os::unix::fs::symlink;
    let (fixture, profile) = hermes_fixture();
    let plugin = profile.join("plugins/aw-native-hooks");
    fs::create_dir_all(&plugin).unwrap();
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../adapters/hermes");
    for name in ["plugin.yaml", "__init__.py"] {
        fs::copy(source.join(name), plugin.join(name)).unwrap();
        fs::set_permissions(plugin.join(name), fs::Permissions::from_mode(0o600)).unwrap();
    }
    fs::write(plugin.join(".aw-owned"), "aw-hermes-plugin/v1alpha1\n").unwrap();
    fs::set_permissions(plugin.join(".aw-owned"), fs::Permissions::from_mode(0o600)).unwrap();
    for path in [profile.join("plugins"), plugin] {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let original = "plugins: {enabled: [aw-native-hooks]}\nunknown: keep\n";
    fs::write(profile.join("config.yaml"), original).unwrap();
    let fifo = fixture.root.join("fifo");
    let name = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
    // SAFETY: name points to a terminated path inside the fixture's owned directory.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o700) }, 0);
    let directory = fixture.root.join("directory");
    fs::create_dir(&directory).unwrap();
    let plain = fixture.root.join("hermes");
    fs::set_permissions(&plain, fs::Permissions::from_mode(0o600)).unwrap();
    let link = fixture.root.join("fifo-link");
    symlink(&fifo, &link).unwrap();
    for executable in [&fifo, &directory, &plain, &link] {
        let mut document = fixture.document();
        document["spec"]["agents"] =
            json!({"hermes":{"adapter":"hermes","argv":[executable,"chat"]}});
        fixture.save(&document);
        for operation in ["run", "install"] {
            let started = Instant::now();
            let output = fixture.run(command(&fixture, &profile, operation));
            assert_eq!(output.status.code(), Some(1));
            assert!(String::from_utf8_lossy(&output.stderr).contains("executable regular file"));
            assert!(started.elapsed() < Duration::from_secs(3));
            assert!(!fixture.root.join("version-started").exists());
            assert!(!fixture.root.join("writer-started").exists());
            assert!(!profile.join(".aw-install.lock").exists());
            assert_eq!(
                fs::read(profile.join("config.yaml")).unwrap(),
                original.as_bytes()
            );
        }
    }
}
