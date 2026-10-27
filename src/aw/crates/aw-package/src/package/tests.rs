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
        ("aw-provider-sec-core", "providers/agent-sec-core"),
        ("aw-provider-tokenless", "providers/tokenless"),
        ("tokenless", "src/tokenless"),
        ("rtk", "src/aw"),
        ("agent-sec-cli", "src/agent-sec-core/v2"),
        ("agent-sec-daemon", "src/agent-sec-core/v2"),
        ("agent-sec-daemon-health", "src/agent-sec-core/v2"),
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
        "src/agent-sec-core/packaging/systemd/agent-sec-core-v2.service.in",
        "src/agent-sec-core/packaging/systemd/skillsec.json",
        "LICENSE",
        "NOTICE",
        "src/tokenless/third_party/rtk/LICENSE",
        "src/tokenless/third_party/rtk/.anolisa-rtk-commit",
        "src/tokenless/crates/tokenless-stats/src/home.rs",
        "src/tokenless/crates/tokenless-stats/src/path_policy.rs",
        "src/tokenless/crates/tokenless-stats/src/rtk_outcome.rs",
        "src/tokenless/scripts/setup-rtk.sh",
        "src/tokenless/third_party/patches/rtk-tokenless-stats.patch",
        "src/tokenless/third_party/patches/rtk-pytest-error-report.patch",
        "docs/user-guide/en/aw/installation.md",
        "docs/user-guide/zh/aw/installation.md",
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
            Some(&repo.join("src/tokenless/third_party/rtk")),
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
        Some(&repo.join("src/tokenless/third_party/rtk")),
    )
    .unwrap();
    assert_eq!(archives.len(), 4);
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
    assert!(combined.join("aw").is_file());
    assert!(!combined.join("aw-package").exists());
    assert!(!combined.join("payload/bin/aw-package").exists());
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
fn every_archive_builds_the_unified_cli() {
    let repo = Path::new("/fixture/repo");
    for selection in [
        Selection::Core,
        Selection::SecCore,
        Selection::Tokenless,
        Selection::All,
    ] {
        assert_eq!(
            selection.builds(repo)[0],
            (repo.join("src/aw"), vec!["aw-cli"])
        );
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
        vec![(repo.join("src/aw"), vec!["aw-cli"])]
    );
    let path = repo.join("src/aw/target/release/aw");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::copy("/bin/true", path).unwrap();
    for name in ["LICENSE", "NOTICE"] {
        fs::write(repo.join(name), "fixture").unwrap();
    }
    for language in ["en", "zh"] {
        let name = format!("docs/user-guide/{language}/aw/installation.md");
        let path = repo.join(&name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../..")
                .join(name),
            path,
        )
        .unwrap();
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
        None,
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
    assert!(unpacked.join("aw").is_file());
    assert_eq!(
        digest(&unpacked.join("aw")).unwrap(),
        digest(&unpacked.join("payload/bin/aw")).unwrap()
    );
    assert!(!unpacked.join("aw-package").exists());
    assert!(!unpacked.join("payload/bin/aw-package").exists());
    assert!(!unpacked.join("payload/libexec").exists());
    assert!(!repo.join("src/agent-sec-core").exists());
    assert!(!repo.join("src/tokenless").exists());
    for (name, counterpart, locale) in [
        ("README.md", "README_zh.md", ""),
        ("README_zh.md", "README.md", "/zh"),
    ] {
        let readme = fs::read_to_string(unpacked.join(name)).unwrap();
        assert!(readme.contains(&format!("]({counterpart})")));
        assert!(readme.contains("aw config init"));
        assert!(readme.contains("./$AW_BUNDLE/aw\" install --bundle"));
        assert!(!readme.contains("aw-package\" install"));
        assert!(!readme.contains("/docs/user-guide/user-entrypoint/"));
        for route in [
            "aw/configuration",
            "aw/tokenless",
            "aw/security",
            "aw/operations",
        ] {
            assert!(readme.contains(&format!(
                "](https://agentic-os.sh{locale}/docs/user-guide/{route}/)"
            )));
        }
        assert!(readme.contains(&format!(
            "](https://agentic-os.sh{locale}/docs/contribute/aw/building/)"
        )));
        for target in readme.split("](").skip(1) {
            let target = target.split(')').next().unwrap();
            assert!(
                target.starts_with("https://") || unpacked.join(target).is_file(),
                "packaged {name} has an unavailable link: {target}"
            );
        }
    }
}

#[test]
fn sec_core_selection_stages_only_extension_with_matching_core_requirement() {
    let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/preview");
    fs::create_dir_all(&target).unwrap();
    let directory = tempfile::tempdir_in(target).unwrap();
    let repo = directory.path().canonicalize().unwrap();
    for (name, source) in [
        ("aw", "src/aw"),
        ("aw-provider-sec-core", "providers/agent-sec-core"),
        ("agent-sec-cli", "src/agent-sec-core/v2"),
        ("agent-sec-daemon", "src/agent-sec-core/v2"),
        ("agent-sec-daemon-health", "src/agent-sec-core/v2"),
    ] {
        let path = repo.join(source).join("target/release").join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::copy("/bin/true", path).unwrap();
    }
    for name in [
        "src/agent-sec-core/packaging/systemd/agent-sec-core-v2.service.in",
        "src/agent-sec-core/packaging/systemd/skillsec.json",
        "LICENSE",
        "NOTICE",
        "docs/user-guide/en/aw/installation.md",
        "docs/user-guide/zh/aw/installation.md",
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
        None,
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
    assert!(package.join("aw").is_file());
    assert!(!package.join("aw-package").exists());
    assert!(!package.join("payload/bin/aw-package").exists());
    let component = &manifest.components[0];
    for name in [
        "libexec/aw/providers/sec-core/agent-sec-daemon-health",
        "share/aw/providers/sec-core/agent-sec-core-v2.service.in",
        "share/aw/providers/sec-core/skillsec.json",
    ] {
        let metadata = &component.files[name];
        assert_eq!(
            metadata.sha256,
            crate::digest(&package.join("payload").join(name)).unwrap()
        );
        assert_eq!(
            metadata.mode,
            if name.starts_with("libexec/") {
                0o755
            } else {
                0o644
            }
        );
    }
    assert!(!package.join("payload/etc").exists());
    assert!(!package.join("payload/usr/lib/systemd").exists());
}

#[test]
fn tokenless_extension_stages_without_sec_core_or_native_plugins() {
    let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/preview");
    fs::create_dir_all(&target).unwrap();
    let directory = tempfile::tempdir_in(target).unwrap();
    let repo = directory.path().canonicalize().unwrap();
    for (name, source) in [
        ("aw", "src/aw"),
        ("aw-provider-tokenless", "providers/tokenless"),
        ("tokenless", "src/tokenless"),
        ("rtk", "src/aw"),
    ] {
        let path = repo.join(source).join("target/release").join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::copy("/bin/true", path).unwrap();
    }
    for name in [
        "LICENSE",
        "NOTICE",
        "src/tokenless/third_party/rtk/LICENSE",
        "src/tokenless/third_party/rtk/.anolisa-rtk-commit",
        "src/tokenless/crates/tokenless-stats/src/home.rs",
        "src/tokenless/crates/tokenless-stats/src/path_policy.rs",
        "src/tokenless/crates/tokenless-stats/src/rtk_outcome.rs",
        "src/tokenless/scripts/setup-rtk.sh",
        "src/tokenless/third_party/patches/rtk-tokenless-stats.patch",
        "src/tokenless/third_party/patches/rtk-pytest-error-report.patch",
        "docs/user-guide/en/aw/installation.md",
        "docs/user-guide/zh/aw/installation.md",
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
        Selection::Tokenless,
        Some(&repo.join("src/tokenless/third_party/rtk")),
    )
    .unwrap();
    assert_eq!(archives.len(), 1);
    tar::Archive::new(flate2::read::GzDecoder::new(
        File::open(&archives[0]).unwrap(),
    ))
    .unpack(&output)
    .unwrap();
    let package = output.join(format!(
        "aw-provider-tokenless-0.1.0-preview.1-linux-{}",
        std::env::consts::ARCH
    ));
    let manifest = crate::inspect(&package).unwrap();
    assert_eq!(manifest.components.len(), 1);
    assert_eq!(manifest.components[0].component, "aw-provider-tokenless");
    let provenance_path = "share/doc/aw-provider-tokenless/rtk/provenance.json";
    let provenance: serde_json::Value =
        serde_json::from_slice(&fs::read(package.join("payload").join(provenance_path)).unwrap())
            .unwrap();
    assert_eq!(provenance["anolisa_source_commit"], "a".repeat(40));
    assert!(provenance["shared_sources_reason"]
        .as_str()
        .unwrap()
        .contains("relative path"));
    for module in ["home.rs", "path_policy.rs", "rtk_outcome.rs"] {
        let origin = format!("src/tokenless/crates/tokenless-stats/src/{module}");
        let input = provenance["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|input| input["source"] == origin)
            .unwrap();
        assert_eq!(input["sha256"], digest(&repo.join(&origin)).unwrap());
        assert_eq!(input["payload"], format!("tokenless-stats/{module}"));
    }
    assert_eq!(manifest.components[0].files[provenance_path].mode, 0o644);
    assert_eq!(
        manifest.components[0].requires,
        BTreeMap::from([("aw-core".into(), "0.1.0-preview.1".into())])
    );
    assert!(!repo.join("src/agent-sec-core").exists());
    assert!(!package.join("payload/bin/aw").exists());
    assert!(package.join("aw").is_file());
    assert!(!package.join("aw-package").exists());
    assert!(!package.join("payload/bin/aw-package").exists());
}

#[test]
fn rtk_preparation_uses_fresh_owned_source_and_preserves_cached_edits() {
    let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/preview");
    fs::create_dir_all(&target).unwrap();
    let directory = tempfile::tempdir_in(target.canonicalize().unwrap()).unwrap();
    let repo = directory.path();
    let cached = repo.join("src/tokenless/third_party/rtk");
    fs::create_dir_all(&cached).unwrap();
    fs::write(cached.join("local-edit"), "preserve cached source").unwrap();
    fs::write(repo.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
    let modules = repo.join("src/tokenless/crates/tokenless-stats/src");
    fs::create_dir_all(&modules).unwrap();
    fs::write(modules.join("home.rs"), "trusted home module").unwrap();
    fs::write(modules.join("path_policy.rs"), "shared state path module").unwrap();
    fs::write(modules.join("rtk_outcome.rs"), "call-scoped receipt module").unwrap();
    let source = rtk::prepare_using(repo, &AtomicBool::new(false), |root| {
        let destination = root.join("third_party/rtk");
        assert!(!destination.exists());
        fs::create_dir_all(destination.join("src/core"))?;
        // Match the relative module includes in the official RTK patch.
        for name in ["home.rs", "path_policy.rs", "rtk_outcome.rs"] {
            let relative = destination
                .join("src/core/../../../../crates/tokenless-stats/src")
                .join(name);
            assert_eq!(digest(&relative)?, digest(&modules.join(name))?);
        }
        fs::write(destination.join("source-marker"), "pinned")?;
        Ok(())
    })
    .unwrap();
    let owned = source.path().to_path_buf();
    assert!(
        !owned.starts_with(repo),
        "upstream source must escape the AW workspace"
    );
    assert_eq!(
        fs::read_to_string(owned.join("third_party/rtk/source-marker")).unwrap(),
        "pinned"
    );
    assert!(!owned.join("third_party/rtk/local-edit").exists());
    assert_eq!(
        fs::read_to_string(cached.join("local-edit")).unwrap(),
        "preserve cached source"
    );
    drop(source);
    assert!(!owned.exists());
    assert_eq!(
        fs::read_to_string(cached.join("local-edit")).unwrap(),
        "preserve cached source"
    );
}
