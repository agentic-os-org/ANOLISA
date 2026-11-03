//! End-to-end coverage for `ktuner tune --dry-run`, driving the real binary.
//!
//! The preview is read-only, so it runs unprivileged. It must account for
//! every recommendation `check` reports on the same host: the entries a real
//! run would write (`would_apply`) and the ones the environment filters out
//! (`would_skip`, with the reason a real run would drop them), so a script can
//! tell a partial plan from a complete one and see which parameters it is
//! missing.

use std::process::{Command, Output};

fn ktuner(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ktuner"))
        .args(arguments)
        .output()
        .expect("run ktuner")
}

fn json(output: &Output, what: &str) -> serde_json::Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "{what} must print JSON on stdout: {e}\nstderr: {}",
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

#[test]
fn dry_run_accounts_for_every_recommendation_check_reports() {
    // `tune --dry-run` is the read-only preview of the plan (README "preview,
    // no changes"; the user guide lists it as Root: No, "Previews changes,
    // writes nothing"). It has to show the whole plan — `would_apply` for the
    // entries a real run would write here, `would_skip` for the ones this
    // environment filters out (unwritable, or dangerous to change at runtime)
    // — so the missing parameters are visible instead of only counted.
    let check = ktuner(&["check"]);
    assert!(
        matches!(check.status.code(), Some(0 | 1)),
        "ktuner check must exit 0 (nothing to advise) or 1 (recommendations): {}",
        String::from_utf8_lossy(&check.stderr)
    );
    let evaluation = json(&check, "ktuner check");
    let findings = evaluation["recommendations"]
        .as_array()
        .expect("check must print a recommendations array");

    let plan = ktuner(&["tune", "--dry-run"]);
    // The preview keeps the short-circuit exit convention: 1 means every
    // recommendation here is blocked (e.g. read-only /proc/sys in a container),
    // 0 means at least one is applicable.
    assert!(
        matches!(plan.status.code(), Some(0 | 1)),
        "tune --dry-run must exit 0 (plan) or 1 (every entry blocked): {}",
        String::from_utf8_lossy(&plan.stderr)
    );
    let preview = json(&plan, "tune --dry-run");
    let would_apply = preview["would_apply"]
        .as_array()
        .unwrap_or_else(|| panic!("tune --dry-run must carry would_apply: {preview}"));
    let would_skip = preview["would_skip"]
        .as_array()
        .unwrap_or_else(|| panic!("tune --dry-run must list what it leaves out: {preview}"));

    // Everything in the plan is applicable: check published the same
    // `writable` flag, and an unwritable entry can only be skipped.
    for rec in would_apply {
        assert_eq!(
            rec["writable"],
            serde_json::json!(true),
            "would_apply took an unwritable parameter: {rec}"
        );
    }
    // Every skipped entry names a parameter check reported and a reason that
    // agrees with check's `writable` flag: an unwritable parameter is skipped
    // as unwritable, a writable one only for being runtime-dangerous.
    for entry in would_skip {
        let param = entry["param"]
            .as_str()
            .unwrap_or_else(|| panic!("would_skip entry without a param: {entry}"));
        let reason = entry["reason"]
            .as_str()
            .unwrap_or_else(|| panic!("would_skip entry without a reason: {entry}"));
        let finding = findings
            .iter()
            .find(|r| r["param"] == serde_json::json!(param))
            .unwrap_or_else(|| {
                panic!("would_skip lists {param}, which check did not report: {preview}")
            });
        match reason {
            "unwritable" => assert_eq!(
                finding["writable"],
                serde_json::json!(false),
                "{param} is writable, so it must not be skipped as unwritable: {preview}"
            ),
            "runtime_dangerous" => assert_eq!(
                finding["writable"],
                serde_json::json!(true),
                "{param} is unwritable, which wins over runtime-dangerous: {preview}"
            ),
            other => panic!("unknown skip reason {other} for {param}: {preview}"),
        }
    }
    // The two lists partition check's findings: the preview drops nothing and
    // lists nothing twice.
    let mut previewed: Vec<&str> = Vec::with_capacity(findings.len());
    previewed.extend(
        would_apply
            .iter()
            .map(|r| r["param"].as_str().expect("would_apply param")),
    );
    previewed.extend(
        would_skip
            .iter()
            .map(|e| e["param"].as_str().expect("would_skip param")),
    );
    previewed.sort_unstable();
    let mut reported: Vec<&str> = findings
        .iter()
        .map(|r| r["param"].as_str().expect("check param"))
        .collect();
    reported.sort_unstable();
    assert_eq!(
        previewed, reported,
        "the preview must account for every check finding exactly once: {preview}"
    );
    // The counts stay consistent with the lists on every shape: the planned
    // shape reports `blocked`, the short-circuit shape its two partitions.
    match preview["status"].as_str() {
        Some("planned") => assert_eq!(
            preview["blocked"].as_u64(),
            Some(would_skip.len() as u64),
            "blocked must equal the would_skip length: {preview}"
        ),
        Some("blocked") => {
            assert!(
                would_apply.is_empty(),
                "blocked shape without applicable entries: {preview}"
            );
            assert_eq!(
                preview["blocked_unwritable"].as_u64().unwrap_or(0)
                    + preview["blocked_runtime_dangerous"].as_u64().unwrap_or(0),
                would_skip.len() as u64,
                "the short-circuit partitions must add up to would_skip: {preview}"
            );
        }
        Some("optimal") => assert!(
            would_apply.is_empty() && would_skip.is_empty(),
            "optimal host with a non-empty preview: {preview}"
        ),
        other => panic!("unexpected tune --dry-run status {other:?}: {preview}"),
    }
}
