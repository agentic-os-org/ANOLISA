//! A library import that writes the value already live changed nothing, so it
//! must not strip the classification the recording rule stamped on the ledger
//! entry: the record still describes the value the rules wrote.
#![cfg(target_os = "linux")]

use ktuner_engine::{rules::Recommendation, tuner, Category};
use std::fs;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const LEDGER: &str = "/var/lib/ktuner/rollback.json";
const PARAM: &str = "vm.swappiness";
const LIVE: &str = "/proc/sys/vm/swappiness";

/// A rule's recommendation: the category is the one `tune`/`fix` record with
/// every entry they write.
fn rec(param: &str, value: &str) -> Recommendation {
    Recommendation {
        param: param.into(),
        current_value: "stale gathered value".into(),
        recommended_value: value.into(),
        writable: true,
        category: Category::Performance,
        ..Default::default()
    }
}

fn ledger() -> serde_json::Value {
    serde_json::from_str(&fs::read_to_string(LEDGER).unwrap()).unwrap()
}

fn category_of(data: &serde_json::Value) -> serde_json::Value {
    data["entries"][PARAM]["category"].clone()
}

fn body() {
    fs::create_dir_all("/var/lib/ktuner").unwrap();

    // A rule wrote the knob: the ledger classifies the entry with the rule's
    // own category, which is the only record of it — a parameter name does not
    // say which rule wrote the value.
    fs::write(LIVE, "60").unwrap();
    let _ = fs::remove_file(LEDGER);
    tuner::apply_one(&rec(PARAM, "1")).unwrap();
    let data = ledger();
    assert_eq!(data["entries"][PARAM]["applied"], "1");
    assert_eq!(
        category_of(&data),
        "performance",
        "a rule write records the rule's category: {data}"
    );

    // An import of the value that is already live: the kernel takes the write
    // and changes nothing, so the record still describes the value the rules
    // wrote.
    tuner::apply_import(PARAM, "1", None).unwrap();
    let data = ledger();
    assert_eq!(data["entries"][PARAM]["previous"], "60");
    assert_eq!(data["entries"][PARAM]["applied"], "1");
    assert_eq!(
        category_of(&data),
        "performance",
        "a write that changes nothing must not strip the rule's classification: {data}"
    );

    // The category selection still owns the entry: rolling that category back
    // restores the write the rule made.
    let category = tuner::rollback_categories(&["mem".to_string()], &[]).unwrap();
    assert!(
        category.unclassified.is_empty(),
        "a rule-tuned knob is not unclassified: {:?}",
        category.unclassified
    );
    assert_eq!(category.outcome.restored(), 1);
    assert_eq!(
        fs::read_to_string(LIVE).unwrap(),
        "60",
        "the rule's write must come back with its own category"
    );

    // Control: an import that MOVES the value does take the classification
    // away — the live value is then the imported file's, not a rule's.
    fs::write(LIVE, "60").unwrap();
    let _ = fs::remove_file(LEDGER);
    tuner::apply_one(&rec(PARAM, "1")).unwrap();
    tuner::apply_import(PARAM, "10", None).unwrap();
    let data = ledger();
    assert_eq!(data["entries"][PARAM]["applied"], "10");
    assert_eq!(
        category_of(&data),
        serde_json::Value::Null,
        "an import that changes the value leaves no rule classification: {data}"
    );
    tuner::rollback_quiet().unwrap();
}

#[test]
#[ignore = "requires root and private mount namespaces; only fixture files are written"]
fn a_no_op_import_keeps_the_rule_category() {
    if std::env::var_os("KTUNER_IMPORT_CATEGORY_CHILD").is_some() {
        body();
        return;
    }
    assert_eq!(unsafe { libc::geteuid() }, 0);
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "ktuner-import-category-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir_all(dir.join("varlib")).unwrap();
    fs::create_dir_all(dir.join("etc/sysctl.d")).unwrap();
    fs::write(dir.join("swappiness"), "60").unwrap();
    let before = fs::read_to_string(LIVE).unwrap();
    let out = Command::new("unshare").args(["--mount", "--propagation", "private", "sh", "-ec",
        "mount --bind \"$1/varlib\" /var/lib; mount --bind \"$1/etc\" /etc; mount --bind \"$1/swappiness\" /proc/sys/vm/swappiness; KTUNER_IMPORT_CATEGORY_CHILD=1 exec \"$2\" --exact a_no_op_import_keeps_the_rule_category --ignored --nocapture",
        "import-category-test"]).arg(&dir).arg(std::env::current_exe().unwrap()).output().unwrap();
    // The live host knob is untouched: everything happened on fixture files.
    assert_eq!(fs::read_to_string(LIVE).unwrap(), before);
    let _ = fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
