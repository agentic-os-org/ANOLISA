//! Persistent registration must retain managed command files throughout native probes.

use super::{Fixture, Process, TIMEOUT};
use aw_package::database::{
    DatabasePaths, Installation, InstalledDatabase, Inventory, Manager, Registration as Registered,
    RegistrationPointer, Scope,
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    process::Command,
};

struct Registration {
    fixture: Fixture,
    _native: tempfile::TempDir,
    core: PathBuf,
    agent: PathBuf,
    profile: PathBuf,
    state: PathBuf,
}

impl Registration {
    fn new(bare: bool) -> Self {
        let fixture = Fixture::new();
        // Lease regressions start from committed packages; first-install staging has
        // separate package acceptance. Native probes still need trusted path ancestors.
        let native = tempfile::Builder::new()
            .prefix("aw-registration-")
            .tempdir_in("/tmp")
            .unwrap();
        fs::set_permissions(native.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let profile = native.path().join("profile");
        fs::create_dir(&profile).unwrap();
        fs::set_permissions(&profile, fs::Permissions::from_mode(0o700)).unwrap();
        let core = native.path().join("aw-core");
        let agent = native.path().join("agent");
        let state = fixture.root.join("package-state");
        let paths = DatabasePaths::user(Some(&state), None).unwrap();
        let core_bundle = fixture.root.join("bundle-core");
        let executable = core_bundle.join("payload/bin/aw");
        fs::create_dir_all(executable.parent().unwrap()).unwrap();
        fs::copy(env!("CARGO_BIN_EXE_aw"), &executable).unwrap();
        package(&core_bundle, "aw-core", &["bin/aw"]);
        let core_record = seed(&paths, &core_bundle, &core, "registration-core");

        let bundle = fixture.root.join("bundle-agent");
        fs::create_dir_all(bundle.join("payload/bin")).unwrap();
        fs::write(
            bundle.join("payload/bin/hermes"),
            format!(
                "#!{}\n# from hermes_cli.main import main\n",
                agent.join("bin/python-fixture").display()
            ),
        )
        .unwrap();
        // This interpreter fixture stops at the real Adapter's first metadata probe. It
        // exercises CLI lease lifetime and cancellation, not native Hermes acceptance.
        fs::write(
            bundle.join("payload/bin/python-fixture"),
            format!(
                r#"#!{}
import os, time
from pathlib import Path
root = Path(os.environ['FAKE_ROOT'])
(root / 'probe-ready').write_text(str(os.getpid()))
time.sleep(30)
"#,
                fixture.python.display()
            ),
        )
        .unwrap();
        package(
            &bundle,
            "example.agent",
            &["bin/hermes", "bin/python-fixture"],
        );
        let agent_record = seed(&paths, &bundle, &agent, "registration-agent");
        fs::create_dir_all(paths.root.join("transactions")).unwrap();
        for directory in [
            &state,
            &state.join("aw"),
            &paths.root,
            &paths.root.join("transactions"),
        ] {
            fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let lock = paths.root.join("lock");
        fs::write(&lock, []).unwrap();
        fs::set_permissions(lock, fs::Permissions::from_mode(0o644)).unwrap();
        fs::write(
            paths.installed(),
            serde_json::to_vec_pretty(&InstalledDatabase {
                schema_version: 1,
                generation: 2,
                installations: vec![core_record, agent_record],
            })
            .unwrap(),
        )
        .unwrap();
        fs::set_permissions(paths.installed(), fs::Permissions::from_mode(0o644)).unwrap();
        let inventory = Inventory::read(&paths, None)
            .expect("registration fixture must contain valid committed package records");
        let report = inventory.doctor().unwrap();
        assert!(report.healthy(), "{:?}", report.issues);
        assert_eq!(report.files_checked, 3);
        drop(inventory);
        fs::write(
            agent.join("pyvenv.cfg"),
            "include-system-site-packages = false\n",
        )
        .unwrap();
        fs::set_permissions(agent.join("pyvenv.cfg"), fs::Permissions::from_mode(0o600)).unwrap();
        fs::write(profile.join("config.yaml"), "unknown: keep\n").unwrap();
        fs::set_permissions(
            profile.join("config.yaml"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        let mut document = fixture.document();
        let program = if bare {
            PathBuf::from("hermes")
        } else {
            agent.join("bin/hermes")
        };
        document["spec"]["agents"] = json!({"hermes":{"adapter":"hermes","argv":[program,"chat"]}});
        fixture.save(&document);
        Self {
            fixture,
            _native: native,
            core,
            agent,
            profile,
            state,
        }
    }

    fn command(&self, legacy: bool) -> Command {
        let mut command = Command::new(self.core.join("bin/aw"));
        let mut path = vec![self.agent.join("bin")];
        path.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        command
            .current_dir(self.profile.parent().unwrap())
            .env_clear()
            .env("PATH", std::env::join_paths(path).unwrap())
            .env("HOME", self.fixture.root.join("home"))
            .env("XDG_STATE_HOME", &self.state)
            .env("TMPDIR", self.profile.parent().unwrap())
            .env("FAKE_ROOT", &self.fixture.root);
        if legacy {
            command.arg("install");
        } else {
            command.args(["agent", "connect"]);
        }
        command
            .arg("--config")
            .arg(self.fixture.root.join("aw.json"))
            .args(["--agent", "hermes", "--native-profile"])
            .arg(&self.profile);
        command
    }

    fn remove(&self, prefix: &Path, expected: i32) -> Value {
        let mut command = self.fixture.command();
        command
            .env("XDG_STATE_HOME", &self.state)
            .args(["uninstall", "--yes", "--json", "--prefix"])
            .arg(prefix);
        let output = self.fixture.run(command);
        assert_eq!(
            output.status.code(),
            Some(expected),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stderr.is_empty());
        serde_json::from_slice(&output.stdout).unwrap()
    }
}

fn seed(
    paths: &DatabasePaths,
    bundle: &Path,
    prefix: &Path,
    installation_id: &str,
) -> Installation {
    let packages = aw_package::open_bundle::inspect(bundle).unwrap().packages;
    for package in &packages {
        for (name, payload) in &package.files {
            let destination = prefix.join(name);
            fs::create_dir_all(destination.parent().unwrap()).unwrap();
            fs::copy(bundle.join("payload").join(name), &destination).unwrap();
            fs::set_permissions(destination, fs::Permissions::from_mode(payload.mode)).unwrap();
        }
    }
    fs::create_dir_all(prefix.join(".aw-packages")).unwrap();
    for directory in [
        prefix.to_owned(),
        prefix.join("bin"),
        prefix.join(".aw-packages"),
    ] {
        fs::set_permissions(directory, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let lock = prefix.join(".aw-install.lock");
    fs::write(&lock, []).unwrap();
    fs::set_permissions(lock, fs::Permissions::from_mode(0o644)).unwrap();
    let pointer = RegistrationPointer::new(paths, installation_id.into()).unwrap();
    let marker = prefix.join(".aw-packages/aw-core.json");
    // Exact removal-plan encoding keeps the retained locator unchanged during uninstall.
    fs::write(&marker, serde_json::to_vec_pretty(&pointer).unwrap()).unwrap();
    fs::set_permissions(marker, fs::Permissions::from_mode(0o644)).unwrap();
    Installation {
        installation_id: installation_id.into(),
        prefix: prefix.into(),
        owner_uid: fs::metadata(prefix).unwrap().uid(),
        scope: Scope::User,
        manager: Manager::Raw,
        registration: Registered::Registered,
        packages,
    }
}

fn package(bundle: &Path, id: &str, names: &[&str]) {
    let mut files = BTreeMap::new();
    for name in names {
        let file = bundle.join("payload").join(name);
        fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).unwrap();
        files.insert(
            name,
            json!({"sha256":aw_package::digest(&file).unwrap(),"mode":0o755}),
        );
    }
    fs::write(
        bundle.join("package.json"),
        serde_json::to_vec(&json!({
            "schema_version":1,"package_id":id,"version":"1.2.3","os":"linux",
            "arch":std::env::consts::ARCH,"files":files,"dependencies":[]
        }))
        .unwrap(),
    )
    .unwrap();
}

fn exclusive(prefix: &Path) -> File {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(prefix.join(".aw-install.lock"))
        .unwrap();
    // SAFETY: the returned owned descriptor retains this nonblocking lock for the test.
    assert_eq!(
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    file
}

#[test]
fn persistent_registration_protects_aw_and_agent_packages_until_cancelled() {
    for legacy in [false, true] {
        let registration = Registration::new(legacy);
        let mut process = Process::spawn(&registration.fixture, registration.command(legacy));
        process
            .wait_until("persistent registration metadata probe", TIMEOUT, |_| {
                Ok((
                    registration.fixture.root.join("probe-ready").is_file(),
                    "waiting for managed Agent probe".into(),
                ))
            })
            .unwrap();
        for prefix in [&registration.core, &registration.agent] {
            let blocked = registration.remove(prefix, 1);
            assert_eq!(blocked["error"]["code"], "package_in_use");
        }
        assert!(registration.core.join("bin/aw").is_file());
        assert!(registration.agent.join("bin/hermes").is_file());
        process.signal(libc::SIGTERM);
        let output = process.finish();
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(output.status.signal(), Some(libc::SIGTERM));
        assert!(!registration.profile.join("plugins").exists());
        assert_eq!(
            fs::read(registration.profile.join("config.yaml")).unwrap(),
            b"unknown: keep\n"
        );
        registration.remove(&registration.agent, 0);
        registration.remove(&registration.core, 0);
        assert!(!registration.agent.join("bin/hermes").exists());
        assert!(!registration.core.join("bin/aw").exists());
    }
}

#[test]
fn an_existing_package_writer_blocks_persistent_registration_before_native_execution() {
    for legacy in [false, true] {
        let registration = Registration::new(legacy);
        for prefix in [&registration.core, &registration.agent] {
            let lock = exclusive(prefix);
            let output = registration.fixture.run(registration.command(legacy));
            assert_eq!(output.status.code(), Some(1));
            assert!(String::from_utf8_lossy(&output.stderr)
                .contains("installed package files are being changed"));
            assert!(!registration.fixture.root.join("probe-ready").exists());
            assert!(!registration.profile.join("plugins").exists());
            drop(lock);
        }
    }
}
