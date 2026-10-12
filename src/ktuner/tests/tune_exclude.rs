//! End-to-end coverage for `ktuner tune --exclude`, driving the real binary.
//!
//! The dry-run preview is read-only, so the whole contract is testable
//! unprivileged: an excluded parameter leaves `would_apply`, names itself in
//! `would_skip` with the operator's reason (`excluded`), and the plan still
//! accounts for every recommendation `check` reports. A name that matches
//! nothing in the plan is not an error — the run keeps its verdict and
//! reports the name in `unmatched_exclude` — and a run without `--exclude`
//! carries none of the new keys.

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

fn check_findings() -> Vec<serde_json::Value> {
    let check = ktuner(&["check"]);
    assert!(
        matches!(check.status.code(), Some(0 | 1)),
        "ktuner check must exit 0 (nothing to advise) or 1 (recommendations): {}",
        String::from_utf8_lossy(&check.stderr)
    );
    json(&check, "ktuner check")["recommendations"]
        .as_array()
        .expect("check must print a recommendations array")
        .clone()
}

fn sorted_params(entries: &[serde_json::Value]) -> Vec<String> {
    let mut params: Vec<String> = entries
        .iter()
        .map(|entry| {
            entry["param"]
                .as_str()
                .unwrap_or_else(|| panic!("entry without a param: {entry}"))
                .to_string()
        })
        .collect();
    params.sort_unstable();
    params
}

#[test]
fn dry_run_excludes_a_parameter_and_names_it() {
    // `--exclude` drops the named parameter from the plan: it must leave
    // `would_apply` (nothing about it may be written) and appear in
    // `would_skip` with the operator's own reason, so an agent reconciling
    // the preview with `check` — which keeps reporting the parameter — sees
    // why it is missing. The exclusion is the operator's instruction, so it
    // is the reason reported whether or not the environment could have taken
    // the write.
    let findings = check_findings();
    let Some((target, plan)) = findings.first().and_then(|finding| {
        let target = finding["param"].as_str()?.to_string();
        let plan = ktuner(&["tune", "--dry-run", "--exclude", &target]);
        Some((target, plan))
    }) else {
        // Nothing to recommend on this host: the exclusion has nothing to
        // drop, so only the report key is exercised (covered by the
        // unmatched-name test).
        return;
    };
    assert!(
        matches!(plan.status.code(), Some(0 | 1)),
        "tune --dry-run --exclude must exit 0 (plan) or 1 (every entry blocked): {}",
        String::from_utf8_lossy(&plan.stderr)
    );
    let preview = json(&plan, "tune --dry-run --exclude");
    let would_apply = preview["would_apply"]
        .as_array()
        .unwrap_or_else(|| panic!("tune --dry-run must carry would_apply: {preview}"));
    let would_skip = preview["would_skip"]
        .as_array()
        .unwrap_or_else(|| panic!("tune --dry-run must name what it leaves out: {preview}"));

    assert!(
        !would_apply.iter().any(|rec| rec["param"] == target),
        "an excluded parameter must not stay in the plan: {preview}"
    );
    let entry = would_skip
        .iter()
        .find(|entry| entry["param"] == target)
        .unwrap_or_else(|| panic!("the excluded {target} must be named in would_skip: {preview}"));
    assert_eq!(
        entry["reason"],
        serde_json::json!("excluded"),
        "the operator's exclusion is the reason the plan drops the entry: {preview}"
    );

    // The two lists still partition check's findings: an exclusion moves an
    // entry between them, it never makes it disappear.
    let mut previewed = sorted_params(would_apply);
    previewed.extend(sorted_params(would_skip));
    previewed.sort_unstable();
    assert_eq!(
        previewed,
        sorted_params(&findings),
        "the preview must account for every check finding exactly once: {preview}"
    );
    assert_eq!(
        would_skip.len() as u64,
        preview["blocked"].as_u64().unwrap_or_else(|| panic!(
            "the preview must carry the skip count it used to report: {preview}"
        )),
        "blocked stays the would_skip length: {preview}"
    );
    match preview["status"].as_str() {
        Some("planned") => {
            assert!(
                !would_apply.is_empty(),
                "a planned shape without applicable entries: {preview}"
            );
            assert_eq!(plan.status.code(), Some(0), "a plan exits 0: {preview}");
        }
        Some("blocked") => {
            assert!(
                would_apply.is_empty(),
                "a blocked shape without applicable entries: {preview}"
            );
            assert!(
                preview["blocked_excluded"].as_u64().unwrap_or(0) >= 1,
                "the blocked shape must count the excluded entries: {preview}"
            );
            assert_eq!(
                plan.status.code(),
                Some(1),
                "every entry blocked exits 1 like check: {preview}"
            );
        }
        other => panic!("unexpected tune --dry-run status {other:?}: {preview}"),
    }

    // The same parameter is addressed by its alias spelling, exactly like
    // `fix`/`why` do. Sysfs identities are filesystem names and have no alias
    // form, so only sysctl parameters (dotted names) are exercised here.
    if target.contains('.') && !target.contains('/') {
        let alias = target.replace('.', "/");
        let plan = ktuner(&["tune", "--dry-run", "--exclude", &alias]);
        let preview = json(&plan, "tune --dry-run --exclude <slash alias>");
        let would_apply = preview["would_apply"].as_array().expect("would_apply");
        assert!(
            !would_apply.iter().any(|rec| rec["param"] == target),
            "the slash spelling of {target} must exclude the same parameter: {preview}"
        );
        let would_skip = preview["would_skip"].as_array().expect("would_skip");
        assert!(
            would_skip.iter().any(|entry| {
                entry["param"] == target && entry["reason"] == serde_json::json!("excluded")
            }),
            "the slash spelling must name {target} as excluded: {preview}"
        );
    }
}

#[test]
fn an_unmatched_exclude_name_is_reported_without_changing_the_run() {
    // A name that matches no recommendation in the plan is not an error: the
    // same command is scripted across hosts whose plans differ, so a name
    // that drops nothing must not fail the run. It must be visible, though —
    // otherwise the caller cannot tell that its exclusion was inert. The key
    // carries the name in the spelling that was given.
    let baseline = ktuner(&["tune", "--dry-run"]);
    assert!(
        matches!(baseline.status.code(), Some(0 | 1)),
        "tune --dry-run must exit 0 (plan) or 1 (every entry blocked): {}",
        String::from_utf8_lossy(&baseline.stderr)
    );
    let base = json(&baseline, "tune --dry-run");

    let out = ktuner(&["tune", "--dry-run", "--exclude", "no_such_ktuner_parameter"]);
    assert_eq!(
        out.status.code(),
        baseline.status.code(),
        "an inert exclusion must not change the run's verdict: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let mut report = json(&out, "tune --dry-run --exclude <absent>");
    assert_eq!(
        report["unmatched_exclude"],
        serde_json::json!(["no_such_ktuner_parameter"]),
        "an exclusion that matched nothing must be reported: {report}"
    );
    assert!(
        report
            .as_object_mut()
            .expect("tune --dry-run prints an object")
            .remove("unmatched_exclude")
            .is_some(),
        "the report key must be the exclusion's own addition: {report}"
    );
    assert_eq!(
        report, base,
        "an inert exclusion must leave every other field identical: {report}"
    );
}

#[test]
fn a_run_without_exclude_carries_none_of_the_new_keys() {
    // The compatibility half of the contract: without `--exclude` the output
    // is the shape it always had — no `excluded` reason, no
    // `unmatched_exclude`, and no `blocked_excluded` on the short-circuit
    // shapes — so an existing consumer sees a byte-identical document.
    let out = ktuner(&["tune", "--dry-run"]);
    assert!(
        matches!(out.status.code(), Some(0 | 1)),
        "tune --dry-run must exit 0 (plan) or 1 (every entry blocked): {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let body = json(&out, "tune --dry-run");
    let keys: Vec<&str> = body
        .as_object()
        .expect("tune --dry-run prints an object")
        .keys()
        .map(String::as_str)
        .collect();
    assert!(
        !keys.contains(&"unmatched_exclude") && !keys.contains(&"blocked_excluded"),
        "a run without --exclude must not grow the new keys: {body}"
    );
    match body["status"].as_str() {
        Some("planned") => assert_eq!(
            keys,
            vec!["blocked", "dry_run", "status", "would_apply", "would_skip"],
            "the planned shape is unchanged: {body}"
        ),
        Some("blocked") => assert_eq!(
            keys,
            vec![
                "applied",
                "blocked",
                "blocked_runtime_dangerous",
                "blocked_unwritable",
                "dry_run",
                "recommendations",
                "status",
                "would_apply",
                "would_skip",
            ],
            "the blocked shape is unchanged: {body}"
        ),
        Some("optimal") => assert_eq!(
            keys,
            vec![
                "applied",
                "blocked",
                "dry_run",
                "status",
                "would_apply",
                "would_skip",
            ],
            "the optimal shape is unchanged: {body}"
        ),
        other => panic!("unexpected tune --dry-run status {other:?}: {body}"),
    }
    // Every skip reason on a run without --exclude is still an environment
    // reason: the operator's reason cannot appear on its own.
    for entry in body["would_skip"].as_array().expect("would_skip") {
        assert_ne!(
            entry["reason"],
            serde_json::json!("excluded"),
            "an environment-only run must not report an operator's exclusion: {body}"
        );
    }
}
