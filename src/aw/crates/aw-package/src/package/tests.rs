//! Package selection, staging and publication regressions.

use super::*;

#[test]
fn publication_failure_or_cancellation_removes_only_owned_links_and_allows_retry() {
    let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/preview");
    fs::create_dir_all(&target).unwrap();
    for interrupted in [false, true] {
        for stop in 0..4 {
            let directory = tempfile::tempdir_in(&target).unwrap();
            let output = directory.path().join("output");
            fs::create_dir(&output).unwrap();
            let artifacts: Vec<_> = [
                "core.tar.gz",
                "provider.tar.gz",
                "combined.tar.gz",
                "SHA256SUMS",
            ]
            .into_iter()
            .map(|name| {
                let path = directory.path().join(name);
                fs::write(&path, name).unwrap();
                path
            })
            .collect();
            fs::write(output.join("foreign"), "keep").unwrap();
            let cancel = AtomicBool::new(false);
            let mut count = 0;
            let result = publish_using(&artifacts, &output, &cancel, |source, target| {
                if count == stop && !interrupted {
                    return Err(std::io::Error::from_raw_os_error(libc::ENOSPC));
                }
                fs::hard_link(source, target)?;
                if count == stop {
                    cancel.store(true, Ordering::Relaxed);
                }
                count += 1;
                Ok(())
            });
            let error = result.unwrap_err();
            if interrupted {
                assert!(error.to_string().contains("interrupted"));
            } else {
                assert!(
                    matches!(error, Error::Io(error) if error.raw_os_error()==Some(libc::ENOSPC))
                );
            }
            assert_eq!(fs::read_dir(&output).unwrap().count(), 1);
            assert_eq!(fs::read(output.join("foreign")).unwrap(), b"keep");
            cancel.store(false, Ordering::Relaxed);
            let published = publish_using(&artifacts, &output, &cancel, |source, target| {
                fs::hard_link(source, target)
            })
            .unwrap();
            assert_eq!(published.len(), 4);
            for path in published {
                assert_eq!(
                    fs::read(&path).unwrap(),
                    path.file_name().unwrap().as_encoded_bytes()
                );
            }
            assert!(publish_using(
                &artifacts,
                &output,
                &cancel,
                |source, target| fs::hard_link(source, target)
            )
            .is_err());
            assert_eq!(fs::read_dir(output).unwrap().count(), 5);
        }
    }
}

#[test]
fn staging_cancellation_removes_partial_archives_and_allows_retry() {
    let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/preview");
    fs::create_dir_all(&target).unwrap();
    let directory = tempfile::tempdir_in(target).unwrap();
    let repo = directory.path().canonicalize().unwrap();
    let repo = repo.as_path();
    for (name, source) in [
        ("aw", "src/aw"),
        ("aw-package", "src/aw"),
        ("aw-provider-sec-core", "src/aw"),
        ("agent-sec-cli", "src/agent-sec-core/v2"),
        ("agent-sec-daemon", "src/agent-sec-core/v2"),
    ] {
        let binary = repo.join(source).join("target/release").join(name);
        fs::create_dir_all(binary.parent().unwrap()).unwrap();
        fs::copy("/bin/true", &binary).unwrap();
    }
    File::options()
        .write(true)
        .open(repo.join("src/aw/target/release/aw"))
        .unwrap()
        .set_len(32 * 1024 * 1024)
        .unwrap();
    for name in [
        "LICENSE",
        "NOTICE",
        "docs/user-guide/en/user-entrypoint/aw-preview.md",
        "docs/user-guide/zh/user-entrypoint/aw-preview.md",
    ] {
        let path = repo.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "fixture").unwrap();
    }
    let output = repo.join("output");
    fs::create_dir(&output).unwrap();
    let cancel = AtomicBool::new(false);
    let result = std::thread::scope(|scope| {
        let watcher = scope.spawn(|| {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                let started = fs::read_dir(&output).unwrap().any(|entry| {
                    entry
                        .unwrap()
                        .path()
                        .join("aw-core/payload/bin/aw")
                        .metadata()
                        .is_ok_and(|m| m.len() > 0)
                });
                if started {
                    cancel.store(true, Ordering::Relaxed);
                    break;
                }
                assert!(Instant::now() < deadline, "staging did not start copying");
                std::thread::sleep(Duration::from_millis(1));
            }
        });
        let result = stage(
            repo,
            "0.1.0-preview.1",
            &"a".repeat(40),
            &output,
            &cancel,
            Selection::All,
        );
        watcher.join().unwrap();
        result
    });
    assert!(result.unwrap_err().to_string().contains("interrupted"));
    assert_eq!(fs::read_dir(&output).unwrap().count(), 0);
    cancel.store(false, Ordering::Relaxed);
    let archives = stage(
        repo,
        "0.1.0-preview.1",
        &"a".repeat(40),
        &output,
        &cancel,
        Selection::All,
    )
    .unwrap();
    assert_eq!(archives.len(), 3);
    let checksums = fs::read_to_string(output.join("SHA256SUMS")).unwrap();
    for archive in archives {
        assert!(checksums.contains(&digest(&archive).unwrap()));
        tar::Archive::new(flate2::read::GzDecoder::new(File::open(archive).unwrap()))
            .unpack(&output)
            .unwrap();
    }
    let combined = output.join(format!(
        "aw-all-in-one-0.1.0-preview.1-linux-{}",
        std::env::consts::ARCH
    ));
    crate::inspect(&combined).unwrap();
    for component in crate::inspect(&combined).unwrap().components {
        for (name, payload) in component.files {
            assert_eq!(
                combined
                    .join("payload")
                    .join(name)
                    .metadata()
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o7777,
                payload.mode
            );
        }
    }
}

#[test]
fn copy_and_compression_stop_after_cancellation() {
    let cancel = AtomicBool::new(false);
    let mut copy = Cancellable {
        inner: Vec::new(),
        cancel: &cancel,
    };
    copy.write_all(b"before").unwrap();
    let mut archive = tar::Builder::new(Cancellable {
        inner: GzEncoder::new(Vec::new(), Compression::default()),
        cancel: &cancel,
    });
    cancel.store(true, Ordering::Relaxed);
    assert!(std::io::copy(&mut &b"after"[..], &mut copy)
        .unwrap_err()
        .to_string()
        .contains("interrupted"));
    assert_eq!(copy.inner, b"before");
    assert!(archive
        .finish()
        .unwrap_err()
        .to_string()
        .contains("interrupted"));
}

#[test]
fn version_cannot_inject_paths_or_release_names() {
    for value in ["0.1.0-preview.1", "10.20.0-preview.12"] {
        assert!(super::valid_version(value));
    }
    for value in [
        "0.1-preview.1",
        "0.1.0",
        "../preview.1",
        "0.1.0-preview.",
        "0.1.0-preview.1/../../x",
    ] {
        assert!(!super::valid_version(value));
    }
}

#[test]
fn core_selection_builds_and_stages_without_any_provider_tree() {
    let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/preview");
    fs::create_dir_all(&target).unwrap();
    let directory = tempfile::tempdir_in(target).unwrap();
    let repo = directory.path().canonicalize().unwrap();
    assert_eq!(
        Selection::Core.builds(&repo),
        vec![(repo.join("src/aw"), vec!["aw-package", "aw-service"])]
    );
    for name in ["aw", "aw-package"] {
        let path = repo.join("src/aw/target/release").join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::copy("/bin/true", path).unwrap();
    }
    for name in [
        "LICENSE",
        "NOTICE",
        "docs/user-guide/en/user-entrypoint/aw-preview.md",
        "docs/user-guide/zh/user-entrypoint/aw-preview.md",
    ] {
        let path = repo.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "fixture").unwrap();
    }
    let output = repo.join("output");
    fs::create_dir(&output).unwrap();
    let archives = stage(
        &repo,
        "0.1.0-preview.1",
        &"a".repeat(40),
        &output,
        &AtomicBool::new(false),
        Selection::Core,
    )
    .unwrap();
    assert_eq!(archives.len(), 1);
    assert!(archives[0]
        .file_name()
        .unwrap()
        .to_string_lossy()
        .starts_with("aw-core-"));
    tar::Archive::new(flate2::read::GzDecoder::new(
        File::open(&archives[0]).unwrap(),
    ))
    .unpack(&output)
    .unwrap();
    let unpacked = output.join(format!(
        "aw-core-0.1.0-preview.1-linux-{}",
        std::env::consts::ARCH
    ));
    let manifest = crate::inspect(&unpacked).unwrap();
    assert_eq!(manifest.components.len(), 1);
    assert!(manifest.components[0].requires.is_empty());
    assert!(!unpacked.join("payload/libexec").exists());
    assert!(!repo.join("src/agent-sec-core").exists());
}

#[test]
fn sec_core_selection_stages_only_extension_with_matching_core_requirement() {
    let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/preview");
    fs::create_dir_all(&target).unwrap();
    let directory = tempfile::tempdir_in(target).unwrap();
    let repo = directory.path().canonicalize().unwrap();
    for (name, source) in [
        ("aw-package", "src/aw"),
        ("aw-provider-sec-core", "src/aw"),
        ("agent-sec-cli", "src/agent-sec-core/v2"),
        ("agent-sec-daemon", "src/agent-sec-core/v2"),
    ] {
        let path = repo.join(source).join("target/release").join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::copy("/bin/true", path).unwrap();
    }
    for name in [
        "LICENSE",
        "NOTICE",
        "docs/user-guide/en/user-entrypoint/aw-preview.md",
        "docs/user-guide/zh/user-entrypoint/aw-preview.md",
    ] {
        let path = repo.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "fixture").unwrap();
    }
    let output = repo.join("output");
    fs::create_dir(&output).unwrap();
    let archives = stage(
        &repo,
        "0.1.0-preview.1",
        &"a".repeat(40),
        &output,
        &AtomicBool::new(false),
        Selection::SecCore,
    )
    .unwrap();
    assert_eq!(archives.len(), 1);
    tar::Archive::new(flate2::read::GzDecoder::new(
        File::open(&archives[0]).unwrap(),
    ))
    .unpack(&output)
    .unwrap();
    let package = output.join(format!(
        "aw-provider-sec-core-0.1.0-preview.1-linux-{}",
        std::env::consts::ARCH
    ));
    let manifest = crate::inspect(&package).unwrap();
    assert_eq!(manifest.components.len(), 1);
    assert_eq!(
        manifest.components[0].requires,
        BTreeMap::from([("aw-core".into(), "0.1.0-preview.1".into())])
    );
    assert!(!package.join("payload/bin/aw").exists());
    assert!(!repo.join("src/aw/target/release/aw").exists());
}
