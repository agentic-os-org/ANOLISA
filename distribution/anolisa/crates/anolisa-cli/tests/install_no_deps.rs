//! Exercise dependency skipping and success notices through the compiled CLI.

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Output;

use anolisa_core::state::ObjectKind;
use anolisa_core::state_store::StateStore;
use anolisa_platform::fs_layout::FsLayout;
use flate2::{Compression, write::GzEncoder};
use sha2::{Digest, Sha256};

mod common;

const NOTICE: &str = "Runtime dependency checks and installation were skipped (--no-deps). Ensure required dependencies are available before use.";

struct Fixture {
    root: tempfile::TempDir,
    layout: FsLayout,
    mode: &'static str,
}

impl Fixture {
    fn new(mode: &'static str, components: &[(&str, &str)]) -> Self {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let layout = if mode == "system" {
            FsLayout::system(Some(root.path().join("system")))
        } else {
            FsLayout::user_with_overrides(
                home,
                Some(root.path().join("data")),
                Some(root.path().join("config")),
                Some(root.path().join("state")),
                Some(root.path().join("cache")),
                Some(root.path().join("runtime")),
            )
        };
        let repo = root.path().join("repo/v1");
        let bin = root.path().join("bin");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        // Native tools cannot mutate the host, even if a regression reaches them.
        for (name, marker, code) in [
            ("no-deps-probe", "dependency-probed", 1),
            ("rpm", "rpm-called", 1),
            ("dpkg-query", "package-manager-called", 97),
            ("yum", "package-manager-called", 97),
            ("dnf", "package-manager-called", 97),
            ("apt-get", "package-manager-called", 97),
            ("brew", "package-manager-called", 97),
        ] {
            let path = bin.join(name);
            std::fs::write(
                &path,
                format!(
                    "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexit {code}\n",
                    root.path().join(marker).display()
                ),
            )
            .unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let os = std::env::consts::OS;
        let arch = std::env::consts::ARCH;
        let mut components_index = String::from("schema_version = 2\n");
        let mut index =
            String::from("schema_version = 1\nchannel = \"stable\"\npublisher = \"test\"\n");
        for (name, extra) in components {
            let manifest = format!(
                r#"[component]
name = "{name}"
version = "1.0.0"
{extra}
[component.layout]
modes = ["user", "system"]
[[component.layout.files]]
source = "bin/{name}"
target = "{{bindir}}/{name}"
mode = "0755"
type = "executable"
[[component.dependencies]]
name = "no-deps-probe"
kind = "language-runtime"
probe = "no-deps-probe --version"
[[component.dependencies]]
name = "future-kernel"
kind = "platform-capability"
min_kernel = "9999.0"
"#
            );
            let version_dir = repo.join(name).join("1.0.0");
            std::fs::create_dir_all(&version_dir).unwrap();
            std::fs::write(version_dir.join("meta.toml"), &manifest).unwrap();
            let mut archive = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::default()));
            for (path, bytes) in [
                (".anolisa/component.toml".to_string(), manifest.as_bytes()),
                (format!("bin/{name}"), b"#!/bin/sh\nexit 0\n".as_slice()),
            ] {
                let mut header = tar::Header::new_gnu();
                header.set_size(bytes.len() as u64);
                header.set_mode(0o755);
                header.set_cksum();
                archive.append_data(&mut header, path, bytes).unwrap();
            }
            let artifact = archive.into_inner().unwrap().finish().unwrap();
            std::fs::write(version_dir.join("artifact.tar.gz"), &artifact).unwrap();
            let sha = format!("{:x}", Sha256::digest(&artifact));
            index.push_str(&format!(
                r#"
[[entries]]
component = "{name}"
version = "1.0.0"
channel = "stable"
artifact_type = "tar_gz"
backend = "raw"
url = "{name}/1.0.0/artifact.tar.gz"
os = "{os}"
arch = "{arch}"
install_modes = ["user", "system"]
sha256 = "{sha}"
"#
            ));
            components_index.push_str(&format!(
                r#"
[[components]]
name = "{name}"
targets = [{{ os = "{os}", arch = "{arch}" }}]
[[components.backends]]
kind = "raw"
package = "{name}"
[[components.backends]]
kind = "rpm"
package = "{name}"
"#
            ));
        }
        std::fs::write(repo.join("index.toml"), index).unwrap();
        std::fs::write(repo.join("components-v2.toml"), components_index).unwrap();
        std::fs::create_dir_all(&layout.etc_dir).unwrap();
        std::fs::write(
            layout.etc_dir.join("repo.toml"),
            format!(
                "schema_version = 1\ndefault_backend = \"raw\"\n[backends.raw]\nbase_url = \"file://{}\"\n[backends.rpm]\nbase_url = \"file://{}\"\ngpgcheck = false\n",
                repo.display(),
                repo.display()
            ),
        )
        .unwrap();
        Self { root, layout, mode }
    }

    fn run(&self, extra: &[&str]) -> Output {
        let root = self.root.path();
        let prefix = root.join("system");
        let mut args = vec!["--no-color", "--install-mode", self.mode, "--prefix"];
        args.push(prefix.to_str().unwrap());
        args.extend_from_slice(extra);
        let path = std::env::join_paths(
            [root.join("bin")]
                .into_iter()
                .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
        )
        .unwrap();
        common::run_with_path_env(
            &args,
            &[
                ("HOME", &root.join("home")),
                ("XDG_DATA_HOME", &root.join("data")),
                ("XDG_CONFIG_HOME", &root.join("config")),
                ("XDG_STATE_HOME", &root.join("state")),
                ("XDG_CACHE_HOME", &root.join("cache")),
                ("XDG_RUNTIME_DIR", &root.join("runtime")),
                ("PATH", &PathBuf::from(path)),
            ],
        )
    }

    fn assert_installed(&self, name: &str) {
        assert!(self.layout.bin_dir.join(name).is_file());
        let store = StateStore::load_for_layout(
            &self.layout.state_dir.join("installed.toml"),
            anolisa_platform::privilege::effective_uid(),
            &self.layout,
        )
        .unwrap();
        assert!(store.find(ObjectKind::Component, name).is_some());
    }
}

fn assert_output(output: &Output, code: i32, notices: usize) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(code),
        "{stderr}\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(stderr.matches(NOTICE).count(), notices, "{stderr}");
    assert!(!String::from_utf8_lossy(&output.stdout).contains(NOTICE));
}

#[test]
fn no_deps_skips_probes_and_installs_in_user_scope() {
    let default = Fixture::new("user", &[("example", "")]);
    let output = default.run(&["--json", "install", "example"]);
    assert_output(&output, 1, 0);
    assert!(default.root.path().join("dependency-probed").exists());
    assert!(!default.layout.bin_dir.join("example").exists());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        json["error"]["reason"]
            .as_str()
            .unwrap()
            .contains("future-kernel")
    );

    let skipped = Fixture::new("user", &[("example", "")]);
    let output = skipped.run(&[
        "--json",
        "install",
        "example",
        "--no-deps",
        "--version",
        "1.0.0",
    ]);
    assert_output(&output, 0, 1);
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["data"]["action"], "installed");
    assert!(
        !json["data"]["plan"]
            .as_array()
            .unwrap()
            .iter()
            .any(|step| step == "provision runtime dependencies")
    );
    skipped.assert_installed("example");
    assert!(!skipped.root.path().join("dependency-probed").exists());
    assert!(!skipped.root.path().join("package-manager-called").exists());
    assert_output(
        &skipped.run(&["--json", "install", "example", "--no-deps"]),
        0,
        0,
    );
}

#[test]
fn no_deps_notice_respects_human_json_and_quiet_output() {
    for flags in [
        vec![],
        vec!["--json"],
        vec!["--quiet"],
        vec!["--json", "--quiet"],
    ] {
        let fixture = Fixture::new("user", &[("example", "")]);
        let mut args = flags.clone();
        args.extend(["install", "example", "--no-deps"]);
        let output = fixture.run(&args);
        assert_output(&output, 0, usize::from(!flags.contains(&"--quiet")));
        if flags.contains(&"--json") {
            let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(json["ok"], true);
        } else if flags.contains(&"--quiet") {
            assert!(output.stdout.is_empty());
            assert!(output.stderr.is_empty());
        } else {
            assert!(String::from_utf8_lossy(&output.stdout).contains("installed example 1.0.0"));
        }
    }
}

#[test]
fn no_deps_preview_omits_provisioning_without_installing_or_notifying() {
    let fixture = Fixture::new("user", &[("example", "")]);
    for json in [false, true] {
        for no_deps in [false, true] {
            let mut args = vec!["--dry-run", "install", "example"];
            if json {
                args.push("--json");
            }
            if no_deps {
                args.push("--no-deps");
            }
            let output = fixture.run(&args);
            assert_output(&output, 0, 0);
            assert_eq!(
                String::from_utf8_lossy(&output.stdout).contains("provision runtime dependencies"),
                !no_deps
            );
            if json {
                let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
                assert_eq!(value["data"]["action"], "planned");
            }
        }
    }
    assert!(!fixture.layout.bin_dir.join("example").exists());
    assert!(!fixture.layout.state_dir.join("installed.toml").exists());
    assert!(!fixture.root.path().join("dependency-probed").exists());
}

#[test]
fn no_deps_batch_notifies_once_only_when_members_were_installed() {
    for flags in [
        vec![],
        vec!["--json"],
        vec!["--quiet"],
        vec!["--json", "--quiet"],
        vec!["--dry-run"],
    ] {
        for corrupt in [0, 1, 2] {
            if flags.contains(&"--dry-run") && corrupt > 0 {
                continue;
            }
            let fixture = Fixture::new("user", &[("example-a", ""), ("example-b", "")]);
            for name in ["example-a", "example-b"].iter().take(corrupt) {
                std::fs::write(
                    fixture
                        .root
                        .path()
                        .join(format!("repo/v1/{name}/1.0.0/artifact.tar.gz")),
                    "tampered",
                )
                .unwrap();
            }
            let mut args = flags.clone();
            args.extend(["install", "--all", "--no-deps"]);
            let output = fixture.run(&args);
            let preview = flags.contains(&"--dry-run");
            let notice = usize::from(corrupt < 2 && !preview && !flags.contains(&"--quiet"));
            assert_output(&output, if corrupt == 0 { 0 } else { 1 }, notice);
            if flags.contains(&"--json") {
                let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
                assert_eq!(json["data"]["installed"], 2 - corrupt);
                assert_eq!(json["data"]["failed"], corrupt);
            }
            if corrupt < 2 && !preview {
                fixture.assert_installed("example-b");
            }
            if corrupt == 0 && !preview {
                assert_output(&fixture.run(&args), 0, 0);
            }
            assert!(!fixture.root.path().join("dependency-probed").exists());
            assert!(!fixture.root.path().join("package-manager-called").exists());
        }
    }
}

#[test]
fn no_deps_keeps_integrity_and_conflict_checks() {
    let corrupt = Fixture::new("user", &[("example", "")]);
    std::fs::write(
        corrupt
            .root
            .path()
            .join("repo/v1/example/1.0.0/artifact.tar.gz"),
        "tampered",
    )
    .unwrap();
    assert_output(&corrupt.run(&["install", "example", "--no-deps"]), 1, 0);
    assert!(!corrupt.layout.bin_dir.join("example").exists());

    let fixture = Fixture::new(
        "user",
        &[
            ("example-a", ""),
            ("example-b", "conflicts = [\"example-a\"]"),
        ],
    );
    assert_output(&fixture.run(&["install", "example-a", "--no-deps"]), 0, 1);
    for dry_run in [false, true] {
        let mut args = vec!["--json", "install", "example-b", "--no-deps"];
        if dry_run {
            args.push("--dry-run");
        }
        let output = fixture.run(&args);
        assert_output(&output, 2, 0);
        let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(
            json["error"]["reason"]
                .as_str()
                .unwrap()
                .contains("conflict")
        );
    }
    assert!(!fixture.layout.bin_dir.join("example-b").exists());
}

#[test]
fn no_deps_rejects_explicit_and_default_rpm_before_native_queries() {
    for explicit in [true, false] {
        let fixture = Fixture::new("system", &[("example", "")]);
        if !explicit {
            let path = fixture.layout.etc_dir.join("repo.toml");
            let config = std::fs::read_to_string(&path).unwrap();
            std::fs::write(
                path,
                config.replace("default_backend = \"raw\"", "default_backend = \"rpm\""),
            )
            .unwrap();
        }
        for target in ["example", "--all"] {
            let mut args = vec!["--json", "--dry-run", "install", target, "--no-deps"];
            if explicit {
                args.extend(["--backend", "rpm"]);
            }
            let output = fixture.run(&args);
            assert_output(&output, if target == "--all" { 1 } else { 2 }, 0);
            let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            if target == "example" {
                assert_eq!(json["error"]["code"], "INVALID_ARGUMENT");
            }
            assert!(
                String::from_utf8_lossy(&output.stdout)
                    .contains("only supported by the raw backend")
            );
            assert!(!fixture.root.path().join("rpm-called").exists());
            assert!(!fixture.root.path().join("package-manager-called").exists());
        }
    }
}
