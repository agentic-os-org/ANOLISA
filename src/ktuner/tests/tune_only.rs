//! End-to-end coverage for `ktuner tune --only`, driving the real binary.
//!
//! The dry-run preview is read-only, so the whole contract is testable
//! unprivileged: only the named parameters stay in `would_apply`, every other
//! in-scope recommendation names itself in `would_skip` with the operator's
//! reason (`not_selected`), and the plan still accounts for every
//! recommendation `check` reports. A name that matches nothing in the plan is
//! not an error — the run keeps its verdict, reports the name in
//! `unmatched_only`, and blocks when the selection is empty — and a run
//! without `--only` carries none of the new keys. Selecting every
//! recommendation is the whole plan, so its output is byte-identical to a run
//! with no flag at all.

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
fn dry_run_only_keeps_the_selected_parameter_and_names_the_rest() {
    // `--only` narrows the plan to the named parameter: everything else is
    // dropped, and each dropped entry says so in its own terms
    // (`not_selected`) instead of borrowing an environment reason — on a host
    // that could write them, the reason is the operator's selection alone.
    let findings = check_findings();
    let Some(target) = findings
        .first()
        .and_then(|finding| finding["param"].as_str())
        .map(str::to_string)
    else {
        // Nothing to recommend on this host: the selection has nothing to
        // narrow (covered by the blocked/unmatched tests).
        return;
    };
    let plan = ktuner(&["tune", "--dry-run", "--only", &target]);
    assert!(
        matches!(plan.status.code(), Some(0 | 1)),
        "tune --dry-run --only must exit 0 (plan) or 1 (every entry blocked): {}",
        String::from_utf8_lossy(&plan.stderr)
    );
    let preview = json(&plan, "tune --dry-run --only");
    assert!(
        preview.get("unmatched_only").is_none(),
        "a name that matched must not be reported as inert: {preview}"
    );
    let would_apply = preview["would_apply"]
        .as_array()
        .unwrap_or_else(|| panic!("tune --dry-run must carry would_apply: {preview}"));
    let would_skip = preview["would_skip"]
        .as_array()
        .unwrap_or_else(|| panic!("tune --dry-run must name what it leaves out: {preview}"));

    assert!(
        would_apply
            .iter()
            .all(|rec| rec["param"] == target.as_str()),
        "only the selected parameter may stay in the plan: {preview}"
    );
    for entry in would_skip {
        if entry["param"] == target.as_str() {
            let reason = entry["reason"].as_str().unwrap_or_default();
            assert!(
                matches!(reason, "unwritable" | "runtime_dangerous"),
                "the selected parameter keeps its environment reason: {preview}"
            );
        } else {
            assert_eq!(
                entry["reason"],
                serde_json::json!("not_selected"),
                "every other in-scope entry is dropped by the selection: {preview}"
            );
        }
    }

    // The two lists still partition check's findings: a selection moves
    // entries between them, it never makes one disappear.
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
            let total = preview["blocked_not_selected"].as_u64().unwrap_or(0)
                + preview["blocked_unwritable"].as_u64().unwrap_or(0)
                + preview["blocked_runtime_dangerous"].as_u64().unwrap_or(0);
            assert_eq!(
                total,
                preview["recommendations"]
                    .as_u64()
                    .unwrap_or_else(|| panic!(
                        "a blocked shape must carry the recommendation count: {preview}"
                    )),
                "the blocked counts must partition the recommendations: {preview}"
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
        let plan = ktuner(&["tune", "--dry-run", "--only", &alias]);
        let preview = json(&plan, "tune --dry-run --only <slash alias>");
        let would_apply = preview["would_apply"].as_array().expect("would_apply");
        assert!(
            would_apply
                .iter()
                .all(|rec| rec["param"] == target.as_str()),
            "the slash spelling of {target} must select the same parameter: {preview}"
        );
    }
}

#[test]
fn selecting_every_recommendation_is_the_whole_plan() {
    // The compatibility half of the contract, measured directly: a selection
    // that names every in-scope recommendation keeps the plan intact, so its
    // document must equal a run with no flag at all — byte for byte.
    let findings = check_findings();
    let mut arguments = vec!["tune", "--dry-run"];
    for finding in &findings {
        let param = finding["param"]
            .as_str()
            .unwrap_or_else(|| panic!("entry without a param: {finding}"));
        arguments.push("--only");
        arguments.push(param);
    }
    let selecting_all = ktuner(&arguments);
    let baseline = ktuner(&["tune", "--dry-run"]);
    assert_eq!(
        selecting_all.status.code(),
        baseline.status.code(),
        "selecting every recommendation must keep the verdict: {}",
        String::from_utf8_lossy(&selecting_all.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&selecting_all.stdout),
        String::from_utf8_lossy(&baseline.stdout),
        "selecting every recommendation must keep the document byte-identical"
    );
    assert_eq!(
        String::from_utf8_lossy(&selecting_all.stderr),
        String::from_utf8_lossy(&baseline.stderr)
    );
}

#[test]
fn an_unmatched_only_name_blocks_the_plan_and_is_reported() {
    // A name that matches no recommendation in the plan is not an error (the
    // same command is scripted across hosts whose plans differ), but it
    // selects nothing: the plan has no applicable entry, so the run answers
    // `blocked` and exits 1 like any other plan with nothing to apply, while
    // the name — in the spelling given — is echoed so the caller can tell its
    // selection was empty. On a host with nothing to recommend at all, the
    // plan is `optimal` instead and the echo follows the empty-plan rule.
    let out = ktuner(&["tune", "--dry-run", "--only", "no_such_ktuner_parameter"]);
    assert!(
        matches!(out.status.code(), Some(0 | 1)),
        "tune --dry-run --only must exit 0 (nothing to recommend) or 1 (everything blocked): {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let preview = json(&out, "tune --dry-run --only <absent>");
    assert_eq!(
        preview["unmatched_only"],
        serde_json::json!(["no_such_ktuner_parameter"]),
        "a selection that matched nothing must be reported: {preview}"
    );
    assert!(
        preview["would_apply"].as_array().is_some_and(Vec::is_empty),
        "an unmatched selection cannot leave anything applicable: {preview}"
    );
    for entry in preview["would_skip"].as_array().expect("would_skip") {
        assert_eq!(
            entry["reason"],
            serde_json::json!("not_selected"),
            "every in-scope entry is outside the selection: {preview}"
        );
    }
    match preview["status"].as_str() {
        Some("optimal") => {
            assert_eq!(out.status.code(), Some(0), "nothing to recommend exits 0");
            assert_eq!(preview["applied"], serde_json::json!(0));
        }
        Some("blocked") => {
            assert_eq!(
                out.status.code(),
                Some(1),
                "a blocked plan exits 1: {preview}"
            );
            assert_eq!(
                preview["blocked_not_selected"], preview["recommendations"],
                "with nothing selected the count is the whole plan: {preview}"
            );
            assert_eq!(
                preview["blocked"], preview["recommendations"],
                "blocked stays the would_skip length: {preview}"
            );
        }
        other => panic!("unexpected tune --dry-run status {other:?}: {preview}"),
    }
}

#[test]
fn only_and_exclude_together_are_a_usage_error() {
    // The two flags are opposite directions of one instruction; a run that
    // carried both would have to guess which one wins. Refusing is an
    // argument error (stderr JSON, exit 2), and it is decided before the root
    // gate: without --dry-run a non-root caller must read the usage error,
    // not "tune requires root".
    for arguments in [
        vec![
            "tune",
            "--dry-run",
            "--only",
            "vm.swappiness",
            "--exclude",
            "vm.dirty_ratio",
        ],
        vec![
            "tune",
            "--only",
            "vm.swappiness",
            "--exclude",
            "vm.dirty_ratio",
        ],
    ] {
        let out = ktuner(&arguments);
        assert!(
            out.stdout.is_empty(),
            "a usage error must not print a body on stdout: {}",
            String::from_utf8_lossy(&out.stdout)
        );
        assert_eq!(
            out.status.code(),
            Some(2),
            "a usage error exits 2 like the parser's own: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let error: serde_json::Value = serde_json::from_slice(&out.stderr).unwrap_or_else(|e| {
            panic!(
                "a usage error must print JSON on stderr: {e}\nstderr: {}",
                String::from_utf8_lossy(&out.stderr)
            )
        });
        assert_eq!(
            error["error"],
            serde_json::json!("tune takes either --only or --exclude, not both"),
            "the usage error must name the conflict"
        );
    }
}

#[test]
fn a_run_without_only_carries_none_of_the_new_keys() {
    // The compatibility half of the contract: without `--only` the output is
    // the shape it always had — no `not_selected` reason, no `unmatched_only`
    // and no `blocked_not_selected` on the short-circuit shapes — so an
    // existing consumer sees a byte-identical document. `--exclude` may add
    // its own keys; none of them may be an only key.
    for arguments in [
        vec!["tune", "--dry-run"],
        vec!["tune", "--dry-run", "--conservative"],
        vec!["tune", "--dry-run", "--category", "net"],
        vec!["tune", "--dry-run", "--exclude", "no_such_ktuner_parameter"],
    ] {
        let out = ktuner(&arguments);
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
            !keys.contains(&"unmatched_only") && !keys.contains(&"blocked_not_selected"),
            "a run without --only must not grow the new keys: {body}"
        );
        for entry in body["would_skip"].as_array().expect("would_skip") {
            assert_ne!(
                entry["reason"],
                serde_json::json!("not_selected"),
                "an unselected reason cannot appear without --only: {body}"
            );
        }
    }
}
