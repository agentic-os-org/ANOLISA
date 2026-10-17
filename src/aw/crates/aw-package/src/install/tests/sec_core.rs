//! Independently managed backend ownership across AW package lifecycle.

use super::*;

#[test]
fn sec_core_install_and_removal_preserve_external_backend_ownership() {
    let f = Fixture::new();
    let unit = f
        .root
        .join("system/etc/systemd/system/agent-sec-core.service");
    let config = f.root.join("system/etc/agent-sec/skillsec.json");
    for path in [&unit, &config] {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "externally owned service/config").unwrap();
    }
    let before: Vec<_> = [&unit, &config]
        .iter()
        .map(|path| (fs::metadata(path).unwrap().ino(), fs::read(path).unwrap()))
        .collect();
    install(&f.combined, &f.prefix).unwrap();
    let helper = f
        .prefix
        .join("libexec/aw/providers/sec-core/agent-sec-daemon-health");
    assert!(helper.is_file());
    assert!(receipts(&f.prefix).unwrap()["aw-provider-sec-core"]
        .files
        .contains_key("libexec/aw/providers/sec-core/agent-sec-daemon-health"));
    uninstall(&f.prefix).unwrap();
    assert!(!helper.exists());
    for (path, (inode, bytes)) in [&unit, &config].iter().zip(before) {
        assert_eq!(fs::metadata(path).unwrap().ino(), inode);
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
}
