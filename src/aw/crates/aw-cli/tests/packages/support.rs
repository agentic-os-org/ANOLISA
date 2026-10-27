//! Shared package fixtures and bounded process execution; no terminal driver.

use aw_package::Payload;
use serde_json::json;
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Command, Output},
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

pub(super) struct Fixture {
    _directory: tempfile::TempDir,
    pub(super) root: PathBuf,
    pub(super) bundle: PathBuf,
    pub(super) prefix: PathBuf,
    pub(super) config: PathBuf,
    pub(super) state: PathBuf,
}

impl Fixture {
    pub(super) fn new() -> Self {
        let target =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/package-fixtures");
        fs::create_dir_all(&target).unwrap();
        let directory = tempfile::tempdir_in(target).unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let root = directory.path().canonicalize().unwrap();
        let bundle = root.join("bundle");
        let mut files: BTreeMap<String, Payload> = BTreeMap::new();
        let name = "bin/aw";
        let path = bundle.join("payload").join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let copied = invoke(
            Command::new("/bin/cp")
                .arg("--")
                .arg(env!("CARGO_BIN_EXE_aw"))
                .arg(&path)
                .current_dir(path.parent().unwrap()),
        );
        assert!(
            copied.status.success(),
            "{}",
            String::from_utf8_lossy(&copied.stderr)
        );
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        files.insert(
            name.into(),
            Payload {
                sha256: aw_package::digest(&path).unwrap(),
                mode: 0o755,
            },
        );
        fs::write(
            bundle.join("package.json"),
            serde_json::to_vec(&json!({
                "schema_version": 1,
                "package_id": "aw-core",
                "version": "1.0.0",
                "os": "linux",
                "arch": std::env::consts::ARCH,
                "files": files,
                "dependencies": []
            }))
            .unwrap(),
        )
        .unwrap();
        Self {
            prefix: root.join("prefix"),
            config: root.join("aw.yaml"),
            state: root.join("state"),
            _directory: directory,
            root,
            bundle,
        }
    }

    pub(super) fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_aw"));
        command.current_dir(&self.root);
        command.env("XDG_STATE_HOME", self.root.join("package-state"));
        command
    }

    pub(super) fn install(&self) -> Command {
        let mut command = self.command();
        command
            .args(["install", "--prefix"])
            .arg(&self.prefix)
            .arg("--bundle")
            .arg(&self.bundle);
        command
    }

    pub(super) fn init(&self) -> Command {
        let mut command = self.command();
        command
            .args(["config", "init", "--prefix"])
            .arg(&self.prefix)
            .arg("--config")
            .arg(&self.config)
            .arg("--state-dir")
            .arg(&self.state)
            .args(["--qoder", "/bin/true"]);
        command
    }

    pub(super) fn edit(&self, action: &str, arguments: &[&str]) -> Command {
        let mut command = self.command();
        command
            .args(["config", action, "--config"])
            .arg(&self.config)
            .args(arguments);
        command
    }
}

pub(super) fn success(output: Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    String::from_utf8(output.stdout).unwrap()
}

pub(super) fn invoke(command: &Command) -> aw_exec::Output {
    let mut environment: BTreeMap<_, _> = std::env::vars_os().collect();
    for (name, value) in command.get_envs() {
        if let Some(value) = value {
            environment.insert(name.to_owned(), value.to_owned());
        } else {
            environment.remove(name);
        }
    }
    let started = Instant::now();
    let name = std::thread::current();
    let test = name.name().unwrap_or("unnamed");
    let arguments: Vec<_> = command.get_args().collect();
    eprintln!(
        "package-test start: test={test:?} program={:?} args={arguments:?} cwd={:?} deadline=30s",
        command.get_program(),
        command.get_current_dir(),
    );
    let result = aw_exec::run(
        &aw_exec::CommandSpec {
            program: command.get_program().into(),
            args: command.get_args().map(Into::into).collect(),
            cwd: command.get_current_dir().unwrap().into(),
            environment: environment.into_iter().collect(),
        },
        b"",
        aw_exec::Limits {
            input_bytes: 0,
            stdout_bytes: 65536,
            stderr_bytes: 65536,
        },
        started + Duration::from_secs(30),
        &AtomicBool::new(false),
    );
    eprintln!(
        "package-test finish: test={test:?} elapsed={:?}",
        started.elapsed()
    );
    result.unwrap_or_else(|error| {
        panic!(
            "package command failed: program={:?} args={arguments:?} cwd={:?} elapsed={:?}: {error:?}",
            command.get_program(),
            command.get_current_dir(),
            started.elapsed(),
        )
    })
}
