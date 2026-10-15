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
    uninstall(&split).unwrap();
    uninstall(&all).unwrap();
    assert!(receipts(&split).unwrap().is_empty() && receipts(&all).unwrap().is_empty());
}
