//! Small real bundles exercise aggregate admission and removal at shared journal limits.

use super::*;

fn bundle_files(root: &Path, name: &str, names: &[&str]) -> PathBuf {
    let directory = root.join(format!("bundle-{name}"));
    let mut files = BTreeMap::new();
    for name in names {
        let file = directory.join("payload").join(name);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, b"x").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
        files.insert(
            name.to_string(),
            crate::Payload {
                sha256: crate::digest(&file).unwrap(),
                mode: 0o644,
            },
        );
    }
    let value = json!({
        "schema_version":1,"package_id":format!("example.{name}"),"version":"1.0.0",
        "os":"linux","arch":std::env::consts::ARCH,"files":files,"dependencies":[]
    });
    fs::write(
        directory.join("package.json"),
        serde_json::to_vec(&value).unwrap(),
    )
    .unwrap();
    directory
}

fn root() -> tempfile::TempDir {
    let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/package-action-capacity");
    fs::create_dir_all(&target).unwrap();
    let directory = tempfile::tempdir_in(target.canonicalize().unwrap()).unwrap();
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
    directory
}

fn install_limit(
    paths: &DatabasePaths,
    bundle: &Path,
    prefix: &Path,
    limits: Limits,
) -> PackagePlan {
    plan_with_limits(
        paths,
        bundle,
        prefix,
        open_bundle::MAX_PAYLOAD_BYTES,
        limits,
    )
    .unwrap()
}

#[test]
fn aggregate_journal_admission_keeps_whole_prefix_removal_possible() {
    let temporary = root();
    let root = temporary.path();
    let paths = DatabasePaths::user(Some(root), None).unwrap();
    let prefix = root.join("installed");
    for name in ["base", "mant", "other"] {
        let names = (0..3)
            .map(|index| format!("share/{name}/{index}"))
            .collect::<Vec<_>>();
        let files = names.iter().map(String::as_str).collect::<Vec<_>>();
        let bundle = bundle_files(root, name, &files);
        let plan = crate::plan::install(&paths, &bundle, &prefix).unwrap();
        assert!(plan.ready(), "{:?}", plan.problems);
        crate::plan::execute(&paths, &plan, &AtomicBool::new(false)).unwrap();
    }
    // This valid existing database models a prefix admitted before the aggregate bound.
    let limits = Limits {
        actions: 8,
        directories: 32,
    };
    let extra = bundle_files(root, "extra", &["share/extra/tool"]);
    let before = snapshot(root);
    let addition = install_limit(&paths, &extra, &prefix, limits);
    assert!(addition
        .problems
        .iter()
        .any(|problem| problem.code == "installation_limit"));
    let whole = crate::plan::uninstall::plan_with_limits(&paths, &prefix, &[], limits).unwrap();
    assert!(whole
        .problems
        .iter()
        .any(|problem| problem.code == "transaction_limit"));
    for blocked in [&addition, &whole] {
        assert!(!blocked.ready());
        assert!(crate::plan::execute(&paths, blocked, &AtomicBool::new(false)).is_err());
    }
    assert_eq!(before, snapshot(root));

    let shrink = crate::plan::uninstall::plan_with_limits(
        &paths,
        &prefix,
        &["example.other".into()],
        limits,
    )
    .unwrap();
    assert!(shrink.ready(), "{:?}", shrink.problems);
    crate::plan::execute(&paths, &shrink, &AtomicBool::new(false)).unwrap();
    let two = bundle_files(root, "two", &["share/two/a", "share/two/b"]);
    let before = snapshot(root);
    let blocked = install_limit(&paths, &two, &prefix, limits);
    assert!(blocked
        .problems
        .iter()
        .any(|problem| problem.code == "installation_limit"));
    assert_eq!(before, snapshot(root));
    let exact = install_limit(&paths, &extra, &prefix, limits);
    assert!(exact.ready(), "{:?}", exact.problems);
    assert_eq!(before, snapshot(root));
    crate::plan::execute(&paths, &exact, &AtomicBool::new(false)).unwrap();
    let removal = crate::plan::uninstall::plan_with_limits(&paths, &prefix, &[], limits).unwrap();
    assert!(removal.ready(), "{:?}", removal.problems);
    crate::plan::execute(&paths, &removal, &AtomicBool::new(false)).unwrap();
    assert!(crate::database::Inventory::read(&paths, Some(&prefix))
        .unwrap()
        .installations[0]
        .packages
        .is_empty());
}

#[test]
fn locator_and_new_parent_directories_count_toward_transaction_capacity() {
    let temporary = root();
    let root = temporary.path();
    let paths = DatabasePaths::user(Some(root), None).unwrap();
    let prefix = root.join("installed");
    let batch = bundle_files(root, "batch", &["share/a", "share/b", "share/c", "share/d"]);
    let deep = bundle_files(root, "deep", &["share/one/two/three/tool"]);
    let before = snapshot(root);
    let blocked = install_limit(
        &paths,
        &batch,
        &prefix,
        Limits {
            actions: 4,
            directories: 32,
        },
    );
    assert!(blocked
        .problems
        .iter()
        .any(|problem| problem.code == "transaction_limit"
            && problem.message.contains("5 journal actions")));
    let blocked = install_limit(
        &paths,
        &deep,
        &prefix,
        Limits {
            actions: 8,
            directories: 4,
        },
    );
    assert!(blocked
        .problems
        .iter()
        .any(|problem| problem.code == "transaction_limit"
            && problem.message.contains("5 journaled directories")));
    assert_eq!(before, snapshot(root));
    let exact = install_limit(
        &paths,
        &deep,
        &prefix,
        Limits {
            actions: 8,
            directories: 5,
        },
    );
    assert!(exact.ready(), "{:?}", exact.problems);
    assert_eq!(before, snapshot(root));
    crate::plan::execute(&paths, &exact, &AtomicBool::new(false)).unwrap();
    assert!(prefix.join("share/one/two/three/tool").is_file());
}
