//! Shared package fixtures and bounded process execution; no terminal driver.

use aw_package::{Component, Manifest, Payload};
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
        let target = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/preview");
        fs::create_dir_all(&target).unwrap();
        let directory = tempfile::tempdir_in(target).unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let root = directory.path().canonicalize().unwrap();
        let bundle = root.join("bundle");
        let mut files = BTreeMap::new();
        let name = "bin/aw";
        let path = bundle.join("payload").join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::copy(env!("CARGO_BIN_EXE_aw"), &path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        files.insert(
            name.into(),
            Payload {
                sha256: aw_package::digest(&path).unwrap(),
                mode: 0o755,
            },
        );
        fs::write(
            bundle.join("manifest.json"),
            serde_json::to_vec(&Manifest {
                format: 1,
                components: vec![Component {
                    format: 1,
                    component: "aw-core".into(),
                    version: "0.1.0-preview.1".into(),
                    source_commit: "a".repeat(40),
                    os: "linux".into(),
                    arch: std::env::consts::ARCH.into(),
                    provider_protocol: "aw-provider/v1alpha1".into(),
                    requires: BTreeMap::new(),
                    files,
                }],
            })
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
    aw_exec::run(
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
        Instant::now() + Duration::from_secs(30),
        &AtomicBool::new(false),
    )
    .unwrap()
}
