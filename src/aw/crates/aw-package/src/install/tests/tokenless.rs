//! Split and combined Tokenless installation provenance.

use super::*;

#[test]
fn tokenless_split_and_all_in_one_have_identical_receipts_and_payloads() {
    let f = Fixture::new();
    let tokenless = bundle(&f.root.join("tokenless"), &["aw-provider-tokenless"]);
    let combined = bundle(
        &f.root.join("all"),
        &["aw-core", "aw-provider-sec-core", "aw-provider-tokenless"],
    );
    let split = f.root.join("split");
    let all = f.root.join("all-prefix");
    assert!(install(&tokenless, &split).is_err());
    install(&f.core, &split).unwrap();
    install(&f.provider, &split).unwrap();
    install(&tokenless, &split).unwrap();
    install(&combined, &all).unwrap();
    assert_eq!(receipts(&split).unwrap(), receipts(&all).unwrap());
    for component in receipts(&all).unwrap().values() {
        for path in component.files.keys() {
            assert_eq!(
                fs::read(split.join(path)).unwrap(),
                fs::read(all.join(path)).unwrap()
            );
        }
    }
    // Uninstall cannot consume a developer-owned RTK or external state.
    let external = f.root.join("user-bin/rtk");
    fs::create_dir_all(external.parent().unwrap()).unwrap();
    fs::write(&external, "user RTK").unwrap();
    fs::create_dir(&f.state).unwrap();
    fs::write(f.state.join("stats.db"), "user statistics").unwrap();
    uninstall(&split).unwrap();
    uninstall(&all).unwrap();
    assert!(receipts(&split).unwrap().is_empty() && receipts(&all).unwrap().is_empty());
    assert!(!split.join("libexec/aw/providers/tokenless/rtk").exists());
    assert_eq!(fs::read_to_string(external).unwrap(), "user RTK");
    assert_eq!(
        fs::read_to_string(f.state.join("stats.db")).unwrap(),
        "user statistics"
    );
}

#[test]
fn tokenless_package_requires_its_rtk_and_preserves_core_on_failure() {
    let f = Fixture::new();
    let tokenless = bundle(&f.root.join("tokenless"), &["aw-provider-tokenless"]);
    install(&f.core, &f.prefix).unwrap();
    let before = receipts(&f.prefix).unwrap();
    let path = tokenless.join("manifest.json");
    let mut manifest: Manifest = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    manifest.components[0]
        .files
        .remove("libexec/aw/providers/tokenless/rtk");
    fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    assert!(install(&tokenless, &f.prefix)
        .unwrap_err()
        .to_string()
        .contains("incomplete component payload"));
    assert_eq!(receipts(&f.prefix).unwrap(), before);
    assert!(!f.prefix.join("libexec").exists());
}
