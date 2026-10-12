//! Installer regression contracts, including source-open and rollback failures.

use super::*;
use crate::{digest, Component, Manifest, Payload};
use std::os::unix::fs::{symlink, MetadataExt};

struct Fixture {
    _directory: tempfile::TempDir,
    root: PathBuf,
    prefix: PathBuf,
    state: PathBuf,
    core: PathBuf,
    provider: PathBuf,
    combined: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let target = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/preview");
        fs::create_dir_all(&target).unwrap();
        let directory = tempfile::Builder::new()
            .prefix("install-test-")
            .tempdir_in(target)
            .unwrap();
        let root = directory.path().canonicalize().unwrap();
        fs::set_permissions(&root, Permissions::from_mode(0o700)).unwrap();
        let core = bundle(&root.join("core"), &["aw-core"]);
        let provider = bundle(&root.join("provider"), &["aw-provider-sec-core"]);
        let combined = bundle(&root.join("combined"), &["aw-core", "aw-provider-sec-core"]);
        Self {
            _directory: directory,
            prefix: root.join("installed"),
            state: root.join("state"),
            root,
            core,
            provider,
            combined,
        }
    }
}
fn bundle(root: &Path, names: &[&str]) -> PathBuf {
    let mut components = Vec::new();
    for name in names {
        let entries = if *name == "aw-core" {
            vec!["bin/aw".into(), "bin/aw-package".into()]
        } else {
            ["aw-provider-sec-core", "agent-sec-cli", "agent-sec-daemon"]
                .iter()
                .map(|name| format!("libexec/aw/providers/sec-core/{name}"))
                .collect()
        };
        let mut files = BTreeMap::new();
        for entry in entries {
            let path = root.join("payload").join(&entry);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, format!("fixture {entry}")).unwrap();
            files.insert(
                entry,
                Payload {
                    sha256: digest(&path).unwrap(),
                    mode: 0o755,
                },
            );
        }
        components.push(Component {
            format: 1,
            component: (*name).into(),
            version: "0.1.0-preview.1".into(),
            source_commit: "a".repeat(40),
            os: "linux".into(),
            arch: std::env::consts::ARCH.into(),
            provider_protocol: "aw-provider/v1alpha1".into(),
            requires: if *name == "aw-core" {
                BTreeMap::new()
            } else {
                BTreeMap::from([("aw-core".into(), "0.1.0-preview.1".into())])
            },
            files,
        });
    }
    fs::write(
        root.join("manifest.json"),
        serde_json::to_vec(&Manifest {
            format: 1,
            components,
        })
        .unwrap(),
    )
    .unwrap();
    root.into()
}
fn modify_manifest(bundle: &Path, update: impl FnOnce(&mut Manifest)) {
    let path = bundle.join("manifest.json");
    let mut manifest: Manifest = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    update(&mut manifest);
    fs::write(path, serde_json::to_vec(&manifest).unwrap()).unwrap();
}
#[test]
fn split_matches_combined_and_reinstall_is_refused() {
    let f = Fixture::new();
    install(&f.core, &f.prefix).unwrap();
    install(&f.provider, &f.prefix).unwrap();
    let other = f.root.join("other");
    install(&f.combined, &other).unwrap();
    assert_eq!(receipts(&f.prefix).unwrap(), receipts(&other).unwrap());
    assert!(install(&f.combined, &other)
        .unwrap_err()
        .to_string()
        .contains("already installed"));
}
#[test]
fn missing_core_rolls_back_new_prefix() {
    let f = Fixture::new();
    assert!(install(&f.provider, &f.prefix).is_err());
    assert!(!f.prefix.exists());
}
#[test]
fn provider_requires_matching_core_declaration() {
    for existing_core in [false, true] {
        for dependency in [None, Some("other")] {
            let f = Fixture::new();
            if existing_core {
                install(&f.core, &f.prefix).unwrap();
            }
            let before = receipts(&f.prefix).unwrap();
            modify_manifest(&f.provider, |m| {
                m.components[0].requires.clear();
                if let Some(version) = dependency {
                    m.components[0]
                        .requires
                        .insert("aw-core".into(), version.into());
                }
            });
            assert!(inspect(&f.provider).is_err());
            assert!(install(&f.provider, &f.prefix).is_err());
            assert_eq!(receipts(&f.prefix).unwrap(), before);
            assert!(!f.prefix.join("libexec").exists());
            assert_eq!(f.prefix.exists(), existing_core);
        }
    }
}
#[test]
fn version_and_source_mismatch_preserve_existing_core() {
    for field in ["version", "source"] {
        let f = Fixture::new();
        install(&f.core, &f.prefix).unwrap();
        modify_manifest(&f.provider, |m| {
            if field == "source" {
                m.components[0].source_commit = "b".repeat(40);
            } else {
                m.components[0]
                    .requires
                    .insert("aw-core".into(), "other".into());
            }
        });
        assert!(install(&f.provider, &f.prefix)
            .unwrap_err()
            .to_string()
            .contains("requires matching"));
        assert_eq!(receipts(&f.prefix).unwrap().len(), 1);
        assert!(!f.prefix.join("libexec").exists());
    }
}
#[test]
fn tampered_payload_is_rejected_before_creating_prefix() {
    let f = Fixture::new();
    fs::write(f.core.join("payload/bin/aw"), "tampered").unwrap();
    assert!(install(&f.core, &f.prefix).is_err());
    assert!(!f.prefix.exists());
}

#[test]
fn required_binaries_must_be_executable_but_data_may_be_read_only() {
    for name in [
        "bin/aw",
        "bin/aw-package",
        "libexec/aw/providers/sec-core/aw-provider-sec-core",
        "libexec/aw/providers/sec-core/agent-sec-cli",
        "libexec/aw/providers/sec-core/agent-sec-daemon",
    ] {
        let f = Fixture::new();
        modify_manifest(&f.combined, |manifest| {
            for component in &mut manifest.components {
                if let Some(payload) = component.files.get_mut(name) {
                    payload.mode = 0o644;
                }
            }
        });
        assert!(inspect(&f.combined).unwrap_err().to_string().contains(name));
        assert!(install(&f.combined, &f.prefix).is_err());
        assert!(!f.prefix.exists());
    }
    let f = Fixture::new();
    let name = "share/doc/aw/readme";
    let source = f.combined.join("payload").join(name);
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    fs::write(&source, "read-only package data").unwrap();
    modify_manifest(&f.combined, |manifest| {
        manifest.components[0].files.insert(
            name.into(),
            Payload {
                sha256: digest(&source).unwrap(),
                mode: 0o644,
            },
        );
    });
    install(&f.combined, &f.prefix).unwrap();
    assert_eq!(
        f.prefix.join(name).metadata().unwrap().permissions().mode() & 0o7777,
        0o644
    );
    uninstall(&f.prefix).unwrap();
}
#[test]
fn source_change_after_inspection_rolls_back_and_allows_retry() {
    for existing_core in [false, true] {
        let f = Fixture::new();
        let bundle = if existing_core {
            install(&f.core, &f.prefix).unwrap();
            &f.provider
        } else {
            &f.combined
        };
        let before = receipts(&f.prefix).unwrap();
        let source = bundle.join("payload/libexec/aw/providers/sec-core/aw-provider-sec-core");
        let original = fs::read(&source).unwrap();
        let result = install_using(
            bundle,
            &f.prefix,
            &AtomicBool::new(false),
            paths::lock,
            |path, output| {
                // The final Provider payload changes only after all preflight hashes pass.
                if path == source {
                    fs::write(path, "changed after inspection")?;
                }
                std::io::copy(&mut File::open(path)?, output)?;
                Ok(())
            },
        );
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("installed payload checksum mismatch"));
        assert_eq!(receipts(&f.prefix).unwrap(), before);
        assert!(!f.prefix.join("libexec").exists());
        assert_eq!(f.prefix.exists(), existing_core);
        for component in before.values() {
            for (name, metadata) in &component.files {
                assert_eq!(digest(&f.prefix.join(name)).unwrap(), metadata.sha256);
            }
        }
        fs::write(source, original).unwrap();
        install(bundle, &f.prefix).unwrap();
        assert_eq!(receipts(&f.prefix).unwrap().len(), 2);
    }
}
#[test]
fn symlink_payload_and_prefix_are_rejected() {
    let f = Fixture::new();
    symlink(&f.core, &f.prefix).unwrap();
    assert!(install(&f.core, &f.prefix).is_err());
    fs::remove_file(&f.prefix).unwrap();
    let path = f.core.join("payload/bin/aw");
    let outside = f.root.join("outside");
    fs::rename(&path, &outside).unwrap();
    symlink(outside, path).unwrap();
    assert!(install(&f.core, &f.prefix).is_err());
    assert!(!f.prefix.exists());
}
#[test]
fn writable_parent_is_rejected() {
    for mode in [0o720, 0o702] {
        let f = Fixture::new();
        fs::set_permissions(&f.root, Permissions::from_mode(mode)).unwrap();
        assert!(install(&f.core, &f.prefix).is_err());
        fs::set_permissions(&f.root, Permissions::from_mode(0o700)).unwrap();
        assert!(!f.prefix.exists());
    }
}
#[test]
fn copy_failure_rolls_back_only_new_files() {
    let f = Fixture::new();
    let result = install_using(
        &f.combined,
        &f.prefix,
        &AtomicBool::new(false),
        paths::lock,
        |_, _| Err(std::io::Error::from_raw_os_error(libc::ENOSPC).into()),
    );
    assert!(
        matches!(result,Err(crate::Error::Io(error)) if error.raw_os_error()==Some(libc::ENOSPC))
    );
    assert!(!f.prefix.exists());
}
#[test]
fn source_open_failure_preserves_core_and_allows_retry() {
    let f = Fixture::new();
    install(&f.core, &f.prefix).unwrap();
    let before = receipts(&f.prefix).unwrap();
    let result = install_using(
        &f.provider,
        &f.prefix,
        &AtomicBool::new(false),
        paths::lock,
        |_, _| Err(std::io::Error::from_raw_os_error(libc::EIO).into()),
    );
    assert!(matches!(result,Err(crate::Error::Io(error)) if error.raw_os_error()==Some(libc::EIO)));
    assert_eq!(receipts(&f.prefix).unwrap(), before);
    assert!(!f.prefix.join("libexec").exists());
    install(&f.provider, &f.prefix).unwrap();
    assert_eq!(receipts(&f.prefix).unwrap().len(), 2);
}
#[test]
fn initial_lock_creation_failure_removes_prefix_and_allows_retry() {
    let f = Fixture::new();
    let result = install_using(
        &f.combined,
        &f.prefix,
        &AtomicBool::new(false),
        |prefix| {
            assert!(prefix.is_dir());
            Err(std::io::Error::from_raw_os_error(libc::ENOSPC).into())
        },
        |_, _| panic!("copy must not run after setup fails"),
    );
    assert!(
        matches!(result, Err(crate::Error::Io(error)) if error.raw_os_error() == Some(libc::ENOSPC))
    );
    assert!(!f.prefix.exists());
    install(&f.combined, &f.prefix).unwrap();
    assert_eq!(receipts(&f.prefix).unwrap().len(), 2);
}

#[test]
fn lock_permission_failure_removes_owned_prefix_and_allows_retry() {
    let f = Fixture::new();
    let mut held = None;
    let result = install_using(
        &f.combined,
        &f.prefix,
        &AtomicBool::new(false),
        |prefix| {
            held = Some(paths::lock(prefix)?);
            // Inject an fd that rejects fchmod even as root, after creating the
            // actual owned lock. Keep that flock held throughout rollback.
            Ok(File::open("/proc/self/status")?)
        },
        |_, _| panic!("copy must not run after permission setup fails"),
    );
    assert!(
        matches!(result, Err(crate::Error::Io(error)) if error.raw_os_error() == Some(libc::EPERM))
    );
    assert!(!f.prefix.exists());
    drop(held);
    install(&f.combined, &f.prefix).unwrap();
    assert_eq!(receipts(&f.prefix).unwrap().len(), 2);
}
#[test]
fn initial_lock_contention_preserves_foreign_lock() {
    let f = Fixture::new();
    let mut held = None;
    let result = install_using(
        &f.combined,
        &f.prefix,
        &AtomicBool::new(false),
        |prefix| {
            held = Some(paths::lock(prefix)?);
            paths::lock(prefix)
        },
        |_, _| panic!("copy must not run without the lock"),
    );
    assert!(
        matches!(result, Err(crate::Error::Io(error)) if error.kind() == std::io::ErrorKind::WouldBlock)
    );
    assert_eq!(
        f.prefix.join(".aw-install.lock").metadata().unwrap().ino(),
        held.as_ref().unwrap().metadata().unwrap().ino()
    );
    assert!(paths::lock(&f.prefix).is_err());
}
#[test]
fn damaged_installed_core_rejects_provider_and_allows_retry() {
    for name in ["bin/aw", "bin/aw-package"] {
        for damage in ["bytes", "missing", "mode", "setuid"] {
            let f = Fixture::new();
            install(&f.core, &f.prefix).unwrap();
            let before = receipts(&f.prefix).unwrap();
            let receipt = f.prefix.join(".aw-packages/aw-core.json");
            let receipt_bytes = fs::read(&receipt).unwrap();
            let path = f.prefix.join(name);
            let original = fs::read(&path).unwrap();
            let permissions = path.metadata().unwrap().permissions();
            if damage == "missing" {
                fs::remove_file(&path).unwrap();
            } else if damage == "bytes" {
                fs::write(&path, "modified core").unwrap();
            } else {
                fs::set_permissions(
                    &path,
                    Permissions::from_mode(if damage == "mode" { 0o644 } else { 0o4755 }),
                )
                .unwrap();
            }
            let result = install(&f.provider, &f.prefix);
            assert!(result
                .unwrap_err()
                .to_string()
                .contains("modified or missing"));
            assert_eq!(receipts(&f.prefix).unwrap(), before);
            assert_eq!(fs::read(receipt).unwrap(), receipt_bytes);
            assert!(!f.prefix.join("libexec").exists());
            if damage == "missing" {
                assert!(!path.exists());
            } else if damage == "bytes" {
                assert_eq!(fs::read(&path).unwrap(), b"modified core");
            }
            fs::write(&path, original).unwrap();
            fs::set_permissions(path, permissions).unwrap();
            install(&f.provider, &f.prefix).unwrap();
            assert_eq!(receipts(&f.prefix).unwrap().len(), 2);
        }
    }
}
#[test]
fn rollback_failure_preserves_error_and_other_cleanup() {
    let f = Fixture::new();
    let prefix = f.prefix.clone();
    let result = install_using(
        &f.core,
        &f.prefix,
        &AtomicBool::new(false),
        paths::lock,
        |_, _| {
            fs::write(prefix.join("bin/unknown"), "keep")?;
            Err(std::io::Error::from_raw_os_error(libc::ENOSPC).into())
        },
    );
    assert!(
        matches!(result,Err(crate::Error::Io(error)) if error.raw_os_error()==Some(libc::ENOSPC))
    );
    assert!(!f.prefix.join("bin/aw").exists());
    assert!(f.prefix.join("bin/unknown").exists());
    assert!(f.prefix.join(".aw-install.lock").exists());
}
#[test]
fn competing_lock_is_never_removed() {
    let f = Fixture::new();
    install(&f.core, &f.prefix).unwrap();
    let held = paths::lock(&f.prefix).unwrap();
    let inode = held.metadata().unwrap().ino();
    assert!(
        matches!(install(&f.provider,&f.prefix),Err(crate::Error::Io(error)) if error.kind()==std::io::ErrorKind::WouldBlock)
    );
    assert_eq!(
        f.prefix.join(".aw-install.lock").metadata().unwrap().ino(),
        inode
    );
    assert!(paths::lock(&f.prefix).is_err());
}
#[test]
fn uninstall_preflights_missing_and_modified_files() {
    for missing in [false, true] {
        let f = Fixture::new();
        install(&f.combined, &f.prefix).unwrap();
        let path = f
            .prefix
            .join("libexec/aw/providers/sec-core/agent-sec-daemon");
        if missing {
            fs::remove_file(path).unwrap();
        } else {
            fs::write(path, "changed").unwrap();
        }
        assert!(uninstall(&f.prefix)
            .unwrap_err()
            .to_string()
            .contains("modified or missing"));
        assert!(f.prefix.join("bin/aw").exists());
        assert_eq!(receipts(&f.prefix).unwrap().len(), 2);
    }
}
#[test]
fn uninstall_preserves_unknown_files_and_external_data() {
    let f = Fixture::new();
    install(&f.combined, &f.prefix).unwrap();
    fs::write(f.prefix.join("note"), "keep").unwrap();
    fs::write(f.root.join("audit"), "keep").unwrap();
    uninstall(&f.prefix).unwrap();
    assert!(receipts(&f.prefix).unwrap().is_empty());
    assert!(f.prefix.join("note").exists());
    assert!(f.root.join("audit").exists());
}

#[test]
fn uninstall_backup_cleanup_failure_reports_a_committed_removal() {
    let f = Fixture::new();
    install(&f.combined, &f.prefix).unwrap();
    let installed = receipts(&f.prefix).unwrap();
    let mut backup = None;
    uninstall_using(
        &f.prefix,
        &AtomicBool::new(false),
        |path| fs::remove_file(path),
        |directory| {
            // Cleanup may partially succeed before encountering a filesystem error.
            fs::remove_file(directory.join("0"))?;
            backup = Some(directory.to_path_buf());
            Err(std::io::Error::from_raw_os_error(libc::EACCES))
        },
    )
    .unwrap();
    assert!(receipts(&f.prefix).unwrap().is_empty());
    for component in installed.values() {
        assert!(component
            .files
            .keys()
            .all(|name| !f.prefix.join(name).exists()));
    }
    let backup = backup.unwrap();
    assert!(backup.exists());
    assert!(!fs::read_dir(&backup)
        .unwrap()
        .collect::<Vec<_>>()
        .is_empty());
    fs::remove_dir_all(&backup).unwrap();
    assert!(!backup.exists());
}

#[test]
fn uninstall_deletion_failure_restores_files_and_allows_retry() {
    for interrupted in [false, true] {
        for name in [
            "libexec/aw/providers/sec-core/agent-sec-cli",
            ".aw-packages/aw-provider-sec-core.json",
        ] {
            let f = Fixture::new();
            install(&f.combined, &f.prefix).unwrap();
            let before = receipts(&f.prefix).unwrap();
            let cancel = AtomicBool::new(false);
            let result = uninstall_using(
                &f.prefix,
                &cancel,
                |path| {
                    if path == f.prefix.join(name) && !interrupted {
                        Err(std::io::Error::from_raw_os_error(libc::EACCES))
                    } else {
                        fs::remove_file(path)?;
                        if path == f.prefix.join(name) {
                            cancel.store(true, Ordering::Relaxed);
                        }
                        Ok(())
                    }
                },
                |path| fs::remove_dir_all(path),
            );
            let error = result.unwrap_err();
            if interrupted {
                assert!(error.to_string().contains("interrupted"));
            } else {
                assert!(
                    matches!(error, crate::Error::Io(error) if error.kind() == std::io::ErrorKind::PermissionDenied)
                );
            }
            assert_eq!(receipts(&f.prefix).unwrap(), before);
            verified_files(&f.prefix, &before).unwrap();
            assert!(!fs::read_dir(&f.prefix).unwrap().any(|entry| entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("uninstall-")));
            uninstall(&f.prefix).unwrap();
            assert!(receipts(&f.prefix).unwrap().is_empty());
            for component in before.values() {
                for name in component.files.keys() {
                    assert!(!f.prefix.join(name).exists());
                }
            }
        }
    }
}
#[test]
fn exclusive_private_write_rejects_existing_files_and_symlinks() {
    let f = Fixture::new();
    let path = f.root.join("config");
    paths::write_private(&path, b"{}").unwrap();
    assert_eq!(path.metadata().unwrap().permissions().mode() & 0o777, 0o600);
    assert!(paths::write_private(&path, b"overwrite").is_err());
    let link = f.root.join("link");
    symlink(&path, &link).unwrap();
    assert!(paths::write_private(&link, b"overwrite").is_err());
    assert_eq!(fs::read(path).unwrap(), b"{}");
}

#[test]
fn concurrent_first_install_keeps_one_complete_receipt_set() {
    let f = Fixture::new();
    let barrier = std::sync::Barrier::new(2);
    let outcomes = std::thread::scope(|scope| {
        let install_once = || {
            barrier.wait();
            install(&f.combined, &f.prefix)
        };
        let other = scope.spawn(install_once);
        let first = install_once();
        (first, other.join().unwrap())
    });
    assert!(outcomes.0.is_ok() || outcomes.1.is_ok(), "{outcomes:?}");
    assert!(!(outcomes.0.is_ok() && outcomes.1.is_ok()));
    assert_eq!(receipts(&f.prefix).unwrap().len(), 2);
    for component in receipts(&f.prefix).unwrap().values() {
        for (name, metadata) in &component.files {
            assert_eq!(digest(&f.prefix.join(name)).unwrap(), metadata.sha256);
        }
    }
}

#[test]
fn concurrent_observer_refuses_uninitialized_prefix_before_locking() {
    let f = Fixture::new();
    fs::create_dir(&f.prefix).unwrap();
    fs::set_permissions(&f.prefix, Permissions::from_mode(0o755)).unwrap();
    let held = paths::lock(&f.prefix).unwrap();
    let inode = held.metadata().unwrap().ino();
    // Reproduce a creator paused between mkdir and the first receipt. An observer
    // must reject the uninitialized prefix without entering lock contention.
    let error = install(&f.combined, &f.prefix).unwrap_err();
    assert!(matches!(error, crate::Error::Invalid(_)), "{error}");
    assert_eq!(
        f.prefix.join(".aw-install.lock").metadata().unwrap().ino(),
        inode
    );
    drop(held);
    assert!(receipts(&f.prefix).unwrap().is_empty());
}

#[test]
fn cancellation_rolls_back_fresh_and_split_installations() {
    for existing_core in [false, true] {
        for last_file in [false, true] {
            let f = Fixture::new();
            let bundle = if existing_core {
                install(&f.core, &f.prefix).unwrap();
                &f.provider
            } else {
                &f.combined
            };
            let before = receipts(&f.prefix).unwrap();
            let cancel = AtomicBool::new(false);
            let result = install_using(bundle, &f.prefix, &cancel, paths::lock, |path, output| {
                std::io::copy(&mut File::open(path)?, output)?;
                if !last_file || path.ends_with("aw-provider-sec-core") {
                    cancel.store(true, Ordering::Relaxed);
                }
                Ok(())
            });
            assert!(result.unwrap_err().to_string().contains("interrupted"));
            assert_eq!(receipts(&f.prefix).unwrap(), before);
            assert_eq!(f.prefix.exists(), existing_core);
            assert!(!f.prefix.join("libexec").exists());
            verified_files(&f.prefix, &before).unwrap();
            install(bundle, &f.prefix).unwrap();
            assert_eq!(receipts(&f.prefix).unwrap().len(), 2);
        }
    }
}

#[test]
fn already_cancelled_install_does_not_create_prefix() {
    let f = Fixture::new();
    assert!(
        crate::install_cancellable(&f.combined, &f.prefix, &AtomicBool::new(true))
            .unwrap_err()
            .to_string()
            .contains("interrupted")
    );
    assert!(!f.prefix.exists());
}

mod configuration;
mod standalone;
