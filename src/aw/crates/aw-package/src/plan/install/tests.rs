//! Combined capacity uses real verified bytes so additive installs remain manageable.

use super::*;
use serde_json::json;
use std::{
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::PathBuf,
    sync::atomic::AtomicBool,
};

mod capacity;

fn bundle(root: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let bundle = root.join(name);
    let relative = format!("bin/{name}");
    let payload = bundle.join("payload").join(&relative);
    fs::create_dir_all(payload.parent().unwrap()).unwrap();
    fs::write(&payload, bytes).unwrap();
    fs::set_permissions(&payload, fs::Permissions::from_mode(0o755)).unwrap();
    let description = json!({
        "schema_version":1,"package_id":format!("example.{name}"),"version":"1.0.0",
        "os":"linux","arch":std::env::consts::ARCH,
        "files":{relative:{"sha256":crate::digest(&payload).unwrap(),"mode":493}},
        "dependencies":[]
    });
    fs::write(
        bundle.join("package.json"),
        serde_json::to_vec(&description).unwrap(),
    )
    .unwrap();
    bundle
}

fn snapshot(root: &Path) -> BTreeMap<PathBuf, (u32, u64, Vec<u8>)> {
    let mut result = BTreeMap::new();
    for entry in fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        let metadata = fs::symlink_metadata(&path).unwrap();
        let bytes = if metadata.is_file() {
            fs::read(&path).unwrap()
        } else {
            Vec::new()
        };
        result.insert(path.clone(), (metadata.mode(), metadata.ino(), bytes));
        if metadata.is_dir() {
            result.extend(snapshot(&path));
        }
    }
    result
}

#[test]
fn combined_prefix_byte_limit_blocks_append_without_preventing_later_removal() {
    let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/package-capacity");
    fs::create_dir_all(&target).unwrap();
    let temporary = tempfile::tempdir_in(target.canonicalize().unwrap()).unwrap();
    let root = temporary.path();
    let paths = DatabasePaths::user(Some(root), None).unwrap();
    let prefix = root.join("installed");
    let base = bundle(root, "base", b"base");
    let mant = bundle(root, "mant", b"mant");
    assert_eq!(open_bundle::inspect(&base).unwrap().payload_bytes, 4);
    assert_eq!(open_bundle::inspect(&mant).unwrap().payload_bytes, 4);
    let first = plan_with_limit(&paths, &base, &prefix, 7).unwrap();
    assert!(first.ready(), "{:?}", first.problems);
    crate::plan::execute(&paths, &first, &AtomicBool::new(false)).unwrap();

    let before = snapshot(root);
    let blocked = plan_with_limit(&paths, &mant, &prefix, 7).unwrap();
    assert!(!blocked.ready());
    assert!(blocked
        .problems
        .iter()
        .any(|problem| problem.code == "verification_limit"));
    assert!(crate::plan::execute(&paths, &blocked, &AtomicBool::new(false)).is_err());
    assert_eq!(before, snapshot(root));
    assert!(!prefix.join("bin/mant").exists());

    let exact = plan_with_limit(&paths, &mant, &prefix, 8).unwrap();
    assert!(exact.ready(), "{:?}", exact.problems);
    assert_eq!(before, snapshot(root));
    crate::plan::execute(&paths, &exact, &AtomicBool::new(false)).unwrap();
    let removal = crate::plan::uninstall(&paths, &prefix, &["example.mant".into()]).unwrap();
    assert!(removal.ready(), "{:?}", removal.problems);
    crate::plan::execute(&paths, &removal, &AtomicBool::new(false)).unwrap();
    assert!(prefix.join("bin/base").is_file());
    assert!(!prefix.join("bin/mant").exists());
    assert!(crate::plan::uninstall(&paths, &prefix, &[])
        .unwrap()
        .ready());
}
