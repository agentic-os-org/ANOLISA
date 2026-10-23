//! Profile trust is checked before native imports; checkout checks include the index.

#[path = "hermes_trust/checkout.rs"]
mod checkout;

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
    let native = profile.parent().unwrap();
    fs::create_dir(native.join("home")).unwrap();
    fs::set_permissions(native.join("home"), fs::Permissions::from_mode(0o700)).unwrap();
    let bin = native.join("bin");
    let source = native.join("source");
    fs::create_dir(&bin).unwrap();
    fs::create_dir(&source).unwrap();
    for path in [&bin, &source] {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    fs::write(
        native.join("pyvenv.cfg"),
        "include-system-site-packages = false\n",
    )
    .unwrap();
    fs::set_permissions(native.join("pyvenv.cfg"), fs::Permissions::from_mode(0o600)).unwrap();
    let python = bin.join("python3");
    std::os::unix::fs::symlink(&fixture.python, &python).unwrap();
    let site = fixture.run({
        let mut command = Command::new(&python);
        command.args(["-I", "-S", "-c", "import sys,sysconfig; from pathlib import Path; p=str(Path(sys.executable).parent.parent); print(sysconfig.get_path('purelib', vars={'base':p,'platbase':p}))"]);
        command
    });
    assert!(site.status.success());
    let metadata = PathBuf::from(String::from_utf8(site.stdout).unwrap().trim())
        .join("hermes_agent-0.dist-info");
    fs::create_dir_all(&metadata).unwrap();
    for path in metadata.ancestors().take_while(|path| *path != native) {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    fs::write(
        metadata.join("METADATA"),
        "Name: hermes-agent\nVersion: 0\n",
    )
    .unwrap();
    fs::write(
        metadata.join("direct_url.json"),
        json!({"url":format!("file://{}",source.display()), "dir_info":{"editable":true}})
            .to_string(),
    )
    .unwrap();
    fs::write(
        fixture.root.join("native-metadata"),
        metadata.to_str().unwrap(),
    )
    .unwrap();
    for entry in fs::read_dir(&metadata).unwrap() {
        fs::set_permissions(entry.unwrap().path(), fs::Permissions::from_mode(0o600)).unwrap();
    }
    let startup_site = native
        .join("lib")
        .join(
            metadata
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .file_name()
                .unwrap(),
        )
        .join("site-packages");
    fs::create_dir_all(&startup_site).unwrap();
    for path in startup_site.ancestors().take_while(|path| *path != native) {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    fs::write(
        fixture.root.join("startup-site"),
        startup_site.to_str().unwrap(),
    )
    .unwrap();
    let hermes = native.join("hermes");
    fs::write(
        &hermes,
        format!(
            r#"#!{}
# from hermes_cli.main import main
import os, sys
from pathlib import Path
root = Path(os.environ['FAKE_ROOT'])
if '--version' in sys.argv:
    import importlib.util
    if importlib.util.find_spec('hermes_cli') is not None:
        from hermes_cli.main import main
    from hermes_cli.env_loader import load_hermes_dotenv
    load_hermes_dotenv()
    import subprocess
    source = Path(__file__).parent / 'source'
    subprocess.run(['git', 'status', '--porcelain', '--untracked-files=no'], cwd=source, check=True, stdout=subprocess.DEVNULL)
    assert subprocess.check_output(['git', 'config', '--get', 'aw.fixture'], cwd=source).strip() == b'keep'
    (root / 'version-started').touch()
    print('Install directory: ' + str(Path(__file__).parent / 'source'))
else:
    (root / 'writer-started').touch()
    sys.exit(1)
"#,
            python.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&hermes, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(
        fixture.root.join("utils.py"),
        "import os\nfrom pathlib import Path\n(Path(os.environ['FAKE_ROOT']) / 'writer-started').touch()\nraise RuntimeError('fixture native writer failure')\n",
    )
    .unwrap();
    let package = fixture.root.join("hermes_cli");
    fs::create_dir(&package).unwrap();
    fs::write(package.join("__init__.py"), "").unwrap();
    fs::write(package.join("main.py"), "main = None\n").unwrap();
    fs::write(package.join("env_loader.py"), "import json, os\nfrom pathlib import Path\ndef load_hermes_dotenv(**kwargs):\n    path = Path(os.environ['HERMES_HOME']) / '.env'\n    if path.exists():\n        os.environ.update(json.loads(path.read_text()))\n").unwrap();
    let mut document = fixture.document();
    document["spec"]["agents"] = json!({"hermes":{"adapter":"hermes","argv":[hermes,"chat"]}});
    fixture.save(&document);
    (fixture, profile)
}

fn installed_hermes_fixture() -> (Fixture, PathBuf) {
    let (fixture, profile) = hermes_fixture();
    let plugin = profile.join("plugins/aw-native-hooks");
    fs::create_dir_all(&plugin).unwrap();
    let source =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../aw-adapter/native/hermes");
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
    (fixture, profile)
}

fn command(fixture: &Fixture, profile: &std::path::Path, operation: &str) -> Command {
    let mut command = fixture.command();
    command
        .current_dir(profile.parent().unwrap())
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("HOME", profile.parent().unwrap().join("home"))
        .env("PYTHONPATH", &fixture.root)
        .env("TMPDIR", profile.parent().unwrap())
        .args([operation, "--config"])
        .arg(fixture.root.join("aw.json"))
        .args(["--agent", "hermes", "--native-profile"])
        .arg(profile);
    command
}

#[test]
fn hermes_discovers_metadata_above_the_version_output_limit() {
    let (fixture, profile) = installed_hermes_fixture();
    let config = fs::read(profile.join("config.yaml")).unwrap();
    let native = profile.parent().unwrap();
    let mut source = native.to_path_buf();
    for _ in 0..12 {
        source.push("\u{754c}".repeat(60));
    }
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    fs::rename(native.join("source"), &source).unwrap();
    for path in source.ancestors().take_while(|path| *path != native) {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let site = PathBuf::from(fs::read_to_string(fixture.root.join("startup-site")).unwrap());
    let metadata = PathBuf::from(fs::read_to_string(fixture.root.join("native-metadata")).unwrap())
        .join("direct_url.json");
    let original = json!({"url":format!("file://{}",source.display()),
        "dir_info":{"editable":true}})
    .to_string();
    fs::write(&metadata, &original).unwrap();
    let probe = fixture.run({
        let mut command = Command::new(&fixture.python);
        command
            .args([
                "-I",
                "-S",
                "-c",
                "import json, sys; print(json.dumps([sys.argv[1], [sys.argv[2]]]))",
            ])
            .arg(&source)
            .arg(&site);
        command
    });
    assert!(probe.status.success());
    assert!(probe.stdout.len() > 4096);
    for args in [
        vec!["init", "-q"],
        vec![
            "-c",
            "user.name=kongche-jbw",
            "-c",
            "user.email=kongche.jbw@alibaba-inc.com",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-qm",
            "fixture",
        ],
    ] {
        let output = fixture.run({
            let mut command = Command::new("git");
            command.current_dir(&source).env_clear().args(args);
            command
        });
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fs::set_permissions(source.join(".git"), fs::Permissions::from_mode(0o700)).unwrap();
    let index = fs::read(source.join(".git/index")).ok();
    for operation in ["install", "run"] {
        let mut command = command(&fixture, &profile, operation);
        command.env("PATH", "/usr/bin:/bin");
        let output = fixture.run(command);
        assert_eq!(output.status.code(), Some(1));
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("official checkout 952c941e"),
            "{operation}: {error}"
        );
        assert!(!fixture.root.join("version-started").exists());
        assert!(!fixture.root.join("writer-started").exists());
        assert!(!fixture.root.join("state").exists());
        assert_eq!(fs::read(profile.join("config.yaml")).unwrap(), config);
        assert_eq!(fs::read(&metadata).unwrap(), original.as_bytes());
        assert_eq!(fs::read(source.join(".git/index")).ok(), index);
    }
}

#[test]
fn hermes_rejects_non_utf8_cwd_before_preflight_or_service_setup() {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};
    let (fixture, profile) = hermes_fixture();
    let cwd = fixture.root.join(OsString::from_vec(b"work-\xff".to_vec()));
    fs::create_dir(&cwd).unwrap();
    let mut command = command(&fixture, &profile, "run");
    command.current_dir(&cwd);
    let output = fixture.run(command);
    assert_eq!(output.status.code(), Some(1));
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("non-UTF-8 working directory"), "{error}");
    assert!(!error.contains("panicked"), "{error}");
    assert!(!fixture.root.join("version-started").exists());
    assert!(!fixture.root.join("state").exists());
    assert_eq!(
        fs::read(profile.join("config.yaml")).unwrap(),
        b"unknown: keep\n"
    );
    assert_eq!(fs::read_dir(&profile).unwrap().count(), 1);
}

#[test]
fn hermes_rejects_help_before_probes_or_profile_changes() {
    let (fixture, profile) = hermes_fixture();
    for flag in ["--help", "-h"] {
        for (operation, appended) in [("install", false), ("run", false), ("run", true)] {
            let mut document = fixture.document();
            let mut argv = vec![
                json!(profile.parent().unwrap().join("hermes")),
                json!("chat"),
            ];
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
fn hermes_rejects_non_executable_or_special_explicit_entrypoints() {
    use std::os::unix::fs::symlink;
    let (fixture, profile) = installed_hermes_fixture();
    let original = "plugins: {enabled: [aw-native-hooks]}\nunknown: keep\n";
    let fifo = fixture.root.join("fifo");
    let name = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
    // SAFETY: name points to a terminated path inside the fixture's owned directory.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o700) }, 0);
    let directory = fixture.root.join("directory");
    fs::create_dir(&directory).unwrap();
    let plain = profile.parent().unwrap().join("hermes");
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

#[test]
fn hermes_rejects_writable_native_paths_before_any_probe() {
    use std::os::unix::fs::symlink;
    let (fixture, profile) = hermes_fixture();
    let plugin = profile.join("plugins/aw-native-hooks");
    fs::create_dir_all(&plugin).unwrap();
    for path in [profile.join("plugins"), plugin.clone()] {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let source =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../aw-adapter/native/hermes");
    for name in ["plugin.yaml", "__init__.py"] {
        fs::copy(source.join(name), plugin.join(name)).unwrap();
        fs::set_permissions(plugin.join(name), fs::Permissions::from_mode(0o600)).unwrap();
    }
    fs::write(plugin.join(".aw-owned"), "aw-hermes-plugin/v1alpha1\n").unwrap();
    fs::set_permissions(plugin.join(".aw-owned"), fs::Permissions::from_mode(0o600)).unwrap();
    let original = "plugins: {enabled: [aw-native-hooks]}\nunknown: keep\n";
    fs::write(profile.join("config.yaml"), original).unwrap();
    let bin = profile.parent().unwrap().join("writable-bin");
    fs::create_dir(&bin).unwrap();
    let entry = bin.join("hermes");
    fs::copy(profile.parent().unwrap().join("hermes"), &entry).unwrap();
    let alias = profile.parent().unwrap().join("hermes-link");
    symlink(&entry, &alias).unwrap();
    for (file_mode, directory_mode) in [(0o775, 0o700), (0o777, 0o700), (0o700, 0o775)] {
        fs::set_permissions(&entry, fs::Permissions::from_mode(file_mode)).unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(directory_mode)).unwrap();
        for executable in [&entry, &alias] {
            let mut document = fixture.document();
            document["spec"]["agents"] =
                json!({"hermes":{"adapter":"hermes","argv":[executable,"chat"]}});
            fixture.save(&document);
            for operation in ["install", "run"] {
                let output = fixture.run(command(&fixture, &profile, operation));
                assert_eq!(output.status.code(), Some(1));
                let error = String::from_utf8_lossy(&output.stderr);
                assert!(
                    error.contains("native executable path permits modification"),
                    "{error}"
                );
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
}

#[test]
fn hermes_rejects_writable_git_helpers_before_execution() {
    let (fixture, profile) = hermes_fixture();
    let bin = profile.parent().unwrap().join("helper-bin");
    fs::create_dir(&bin).unwrap();
    let git = bin.join("git");
    fs::write(&git, format!("#!{}\nimport pathlib\npathlib.Path({}).touch()\nprint('952c941e741e922a9be8fc403c8944c6e96318bb')\n", fixture.python.display(),json!(fixture.root.join("git-started")))).unwrap();
    for (file_mode, directory_mode) in [(0o775, 0o700), (0o700, 0o775)] {
        fs::set_permissions(&git, fs::Permissions::from_mode(file_mode)).unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(directory_mode)).unwrap();
        let mut paths = vec![bin.clone()];
        paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
        let mut command = command(&fixture, &profile, "install");
        command.env("PATH", std::env::join_paths(paths).unwrap());
        let output = fixture.run(command);
        assert_eq!(output.status.code(), Some(1));
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains("permits modification"), "{error}");
        assert!(!fixture.root.join("git-started").exists());
        assert!(!fixture.root.join("version-started").exists());
        assert!(!profile.join(".aw-install.lock").exists());
        assert_eq!(
            fs::read(profile.join("config.yaml")).unwrap(),
            b"unknown: keep\n"
        );
    }
}
