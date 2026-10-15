//! End-to-end coverage for `ktuner check --only` / `check --exclude`,
//! driving the real binary.
//!
//! `check` is read-only, so the whole contract is testable unprivileged: the
//! report keeps only the named recommendations (or everything but them), a
//! name that addresses nothing in scope is echoed in the flag's own key
//! instead of failing the run, and a run without the flags keeps the exact
//! key set it always had. The findings themselves are read from this host's
//! own `check`, so nothing here depends on a particular system: on a host
//! with nothing to recommend, the selection has nothing to narrow and the
//! empty-report rules apply.

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

fn params(entries: &[serde_json::Value]) -> Vec<String> {
    entries
        .iter()
        .map(|entry| {
            entry["param"]
                .as_str()
                .unwrap_or_else(|| panic!("entry without a param: {entry}"))
                .to_string()
        })
        .collect()
}

fn first_param(entries: &[serde_json::Value]) -> Option<String> {
    entries.first().map(|entry| {
        entry["param"]
            .as_str()
            .unwrap_or_else(|| panic!("entry without a param: {entry}"))
            .to_string()
    })
}

/// The keys a `check` body has always carried: the selection may add its own
/// inert-name key, never remove one of these.
const BASELINE_KEYS: [&str; 9] = [
    "counts",
    "environment",
    "predicted_score",
    "recommendations",
    "score",
    "services",
    "system",
    "total_checked",
    "workload",
];

#[test]
fn only_keeps_the_selected_recommendation_and_the_system_fields() {
    // `--only` narrows the report to the named recommendation. `score` and
    // `total_checked` describe the system, so they stay; `counts` and the
    // exit code describe what is reported, so they follow the narrowed list.
    let findings = check_findings();
    let Some(target) = first_param(&findings) else {
        // Nothing to recommend on this host: the selection has nothing to
        // narrow (the empty-report rules are covered by the unmatched tests).
        return;
    };
    let out = ktuner(&["check", "--only", &target]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "the narrowed report still has a recommendation: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let body = json(&out, "check --only");
    let recs = body["recommendations"]
        .as_array()
        .unwrap_or_else(|| panic!("check must carry recommendations: {body}"));
    assert_eq!(
        recs.len(),
        1,
        "only the selected recommendation stays: {body}"
    );
    assert_eq!(recs[0]["param"], serde_json::json!(target));
    assert!(
        body.get("unmatched_only").is_none(),
        "a name that matched must not be reported as inert: {body}"
    );

    let baseline = json(&ktuner(&["check"]), "check");
    assert_eq!(
        body["score"], baseline["score"],
        "score describes the system"
    );
    assert_eq!(
        body["total_checked"], baseline["total_checked"],
        "total_checked describes the system"
    );
    assert_eq!(body["system"], baseline["system"]);
    assert_eq!(body["environment"], baseline["environment"]);
    assert_eq!(body["workload"], baseline["workload"]);
    assert_eq!(body["services"], baseline["services"]);

    let counts = &body["counts"];
    let rec = &recs[0];
    assert_eq!(
        counts["high_confidence"].as_u64(),
        Some((rec["confidence"] == serde_json::json!("high")) as u64),
        "the confidence count covers the narrowed report: {body}"
    );
    assert_eq!(
        counts["performance"].as_u64(),
        Some((rec["category"] == serde_json::json!("performance")) as u64),
        "the performance count covers the narrowed report: {body}"
    );
    assert_eq!(
        counts["security"].as_u64(),
        Some((rec["category"] == serde_json::json!("security")) as u64),
        "the security count covers the narrowed report: {body}"
    );
    assert_eq!(
        counts["writable"].as_u64(),
        Some(u64::from(rec["writable"].as_bool().unwrap_or(false))),
        "the writable count covers the narrowed report: {body}"
    );
    let score = body["score"].as_u64().expect("score");
    let predicted = body["predicted_score"].as_u64().expect("predicted_score");
    assert!(
        predicted >= score,
        "applying a recommendation cannot score worse than doing nothing: {body}"
    );
}

#[test]
fn only_and_exclude_take_multiple_names_in_report_order() {
    // Both flags are repeatable, and the report keeps check's own order
    // rather than the order the names were given in.
    let findings = check_findings();
    if findings.len() < 2 {
        return;
    }
    let first = params(&findings[..1]).remove(0);
    let second = params(&findings[1..2]).remove(0);

    let selected = ktuner(&["check", "--only", &first, "--only", &second]);
    assert_eq!(selected.status.code(), Some(1));
    let body = json(&selected, "check --only <two>");
    assert_eq!(
        params(body["recommendations"].as_array().expect("recommendations")),
        vec![first.clone(), second.clone()],
        "the selected recommendations stay in report order: {body}"
    );

    let excluded = ktuner(&["check", "--exclude", &first, "--exclude", &second]);
    let body = json(&excluded, "check --exclude <two>");
    let mut expected = params(&findings);
    expected.retain(|param| param != &first && param != &second);
    assert_eq!(
        params(body["recommendations"].as_array().expect("recommendations")),
        expected,
        "the named recommendations are the ones removed, the rest stay in order: {body}"
    );
    assert_eq!(
        excluded.status.code(),
        Some(if expected.is_empty() { 0 } else { 1 }),
        "the exit code follows the report that is left: {body}"
    );
}

#[test]
fn a_slash_spelling_selects_the_same_recommendation() {
    // Names are resolved exactly as `fix`/`why` resolve them, so the dotted
    // and slash spellings must produce the same document. Sysfs identities
    // are filesystem names and have no alias form, so only sysctl
    // recommendations are exercised here.
    let findings = check_findings();
    let Some(target) = params(&findings)
        .into_iter()
        .find(|param| param.contains('.') && !param.contains('/'))
    else {
        return;
    };
    let dotted = ktuner(&["check", "--only", &target]);
    let slash = target.replace('.', "/");
    let slashed = ktuner(&["check", "--only", &slash]);
    assert_eq!(
        dotted.status.code(),
        Some(1),
        "the dotted spelling must select the recommendation: {}",
        String::from_utf8_lossy(&dotted.stderr)
    );
    assert_eq!(
        params(
            json(&dotted, "check --only")["recommendations"]
                .as_array()
                .expect("recommendations")
        ),
        vec![target.clone()],
        "the dotted spelling selects exactly its recommendation"
    );
    assert_eq!(
        String::from_utf8_lossy(&dotted.stdout),
        String::from_utf8_lossy(&slashed.stdout),
        "the slash spelling of {target} must select the same recommendation"
    );
    assert_eq!(dotted.status.code(), slashed.status.code());
    assert_eq!(
        String::from_utf8_lossy(&dotted.stderr),
        String::from_utf8_lossy(&slashed.stderr)
    );
}

#[test]
fn category_and_conservative_narrow_the_selection_before_it_applies() {
    // The selection applies after the filters, so a recommendation that
    // survives both is addressed the same with or without them — the
    // documents are byte-identical — and a name the filters removed is
    // reported as inert, in the spelling given.
    let net = json(&ktuner(&["check", "--category", "net"]), "check --category");
    if let Some(target) = first_param(net["recommendations"].as_array().expect("recommendations")) {
        let composed = ktuner(&["check", "--category", "net", "--only", &target]);
        let plain = ktuner(&["check", "--only", &target]);
        assert_eq!(
            String::from_utf8_lossy(&composed.stdout),
            String::from_utf8_lossy(&plain.stdout),
            "a recommendation inside the category is selected the same either way"
        );
        assert_eq!(composed.status.code(), plain.status.code());
    }

    let findings = check_findings();
    let Some(non_net) = params(&findings)
        .into_iter()
        .find(|param| !param.starts_with("net."))
    else {
        return;
    };
    let out = ktuner(&["check", "--category", "net", "--only", &non_net]);
    let body = json(&out, "check --category net --only <outside>");
    assert_eq!(
        body["unmatched_only"],
        serde_json::json!([non_net]),
        "a name the category filter removed is inert here: {body}"
    );

    let conservative = json(
        &ktuner(&["check", "--conservative"]),
        "check --conservative",
    );
    if let Some(target) = first_param(
        conservative["recommendations"]
            .as_array()
            .expect("recommendations"),
    ) {
        let composed = ktuner(&["check", "--conservative", "--only", &target]);
        let plain = ktuner(&["check", "--only", &target]);
        assert_eq!(
            String::from_utf8_lossy(&composed.stdout),
            String::from_utf8_lossy(&plain.stdout),
            "a high-confidence recommendation is selected the same either way"
        );
        assert_eq!(composed.status.code(), plain.status.code());
    }

    let Some(not_high) = findings.iter().find_map(|finding| {
        (finding["confidence"] != serde_json::json!("high"))
            .then(|| finding["param"].as_str().map(str::to_string))
            .flatten()
    }) else {
        return;
    };
    let out = ktuner(&["check", "--conservative", "--only", &not_high]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "nothing is reported when the filter removed the name's recommendation: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let body = json(&out, "check --conservative --only <filtered out>");
    assert!(
        body["recommendations"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "the filtered recommendation cannot be reported: {body}"
    );
    assert_eq!(
        body["unmatched_only"],
        serde_json::json!([not_high]),
        "the report is computed against the post-filter list: {body}"
    );
}

#[test]
fn an_unmatched_name_is_echoed_and_never_an_error() {
    // A name that addresses nothing in scope is not an error (the same
    // command is scripted across hosts whose findings differ), but it must be
    // visible so the caller can tell an inert instruction from a silent one.
    // The echo keeps the spelling given and the order given.
    let out = ktuner(&["check", "--only", "No_Such_Ktuner_Param"]);
    assert!(
        out.stderr.is_empty(),
        "an inert selection is not an error: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.status.code(), Some(0), "nothing is reported");
    let body = json(&out, "check --only <absent>");
    assert!(
        body["recommendations"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "a selection that matched nothing reports nothing: {body}"
    );
    assert_eq!(
        body["unmatched_only"],
        serde_json::json!(["No_Such_Ktuner_Param"]),
        "the spelling given is echoed: {body}"
    );

    let out = ktuner(&[
        "check",
        "--exclude",
        "No_Such_Ktuner_Param",
        "--exclude",
        "also_missing",
    ]);
    assert!(
        out.stderr.is_empty(),
        "an inert exclusion is not an error: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let body = json(&out, "check --exclude <absent>");
    assert_eq!(
        body["unmatched_exclude"],
        serde_json::json!(["No_Such_Ktuner_Param", "also_missing"]),
        "the names are echoed in the order given: {body}"
    );
    let findings = check_findings();
    assert_eq!(
        params(body["recommendations"].as_array().expect("recommendations")).len(),
        findings.len(),
        "an exclusion that removed nothing keeps the whole report: {body}"
    );
    assert_eq!(
        out.status.code(),
        Some(if findings.is_empty() { 0 } else { 1 })
    );
}

#[test]
fn excluding_every_recommendation_is_an_empty_report() {
    // `check` reports findings, it does not carry out a plan: a selection
    // that leaves nothing to report answers with an empty list and exit 0,
    // exactly as `--category` does when a category has nothing to advise —
    // there is no blocked state and no new count key in the diagnosis.
    let findings = check_findings();
    let mut arguments = vec!["check"];
    for finding in &findings {
        arguments.push("--exclude");
        arguments.push(finding["param"].as_str().expect("param"));
    }
    let out = ktuner(&arguments);
    assert_eq!(out.status.code(), Some(0), "an empty report exits 0");
    let body = json(&out, "check --exclude <every>");
    assert!(
        body["recommendations"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "nothing is reported: {body}"
    );
    assert_eq!(body["counts"]["performance"], serde_json::json!(0));
    assert_eq!(body["counts"]["security"], serde_json::json!(0));
    assert_eq!(body["counts"]["high_confidence"], serde_json::json!(0));
    assert_eq!(body["counts"]["writable"], serde_json::json!(0));
    for category in ["network", "memory", "io", "cpu"] {
        assert_eq!(body["counts"][category], serde_json::json!(0));
    }
    assert!(
        body.get("unmatched_exclude").is_none(),
        "every name matched, so there is nothing inert to report: {body}"
    );
    let baseline = json(&ktuner(&["check"]), "check");
    assert_eq!(body["score"], baseline["score"]);
    assert_eq!(body["total_checked"], baseline["total_checked"]);
}

/// The per-category counts are what each `--category` run would keep, so a
/// caller can tell from one report where the findings sit without running the
/// filter once per category: `network`, `memory`, `io` and `cpu` count the
/// performance findings of their category and `security` the security ones,
/// and the counts follow a narrowed report like the other counts do.
#[test]
fn category_counts_match_what_each_category_keeps() {
    let full = json(&ktuner(&["check"]), "check");
    for (key, category) in [
        ("network", "net"),
        ("memory", "mem"),
        ("io", "io"),
        ("cpu", "cpu"),
        ("security", "security"),
    ] {
        let narrowed = json(
            &ktuner(&["check", "--category", category]),
            "check --category",
        );
        let kept = narrowed["recommendations"]
            .as_array()
            .unwrap_or_else(|| panic!("check must carry recommendations: {narrowed}"))
            .len() as u64;
        assert_eq!(
            full["counts"][key].as_u64(),
            Some(kept),
            "counts.{key} must be what --category {category} keeps: {full}"
        );
        assert_eq!(
            narrowed["counts"][key].as_u64(),
            Some(kept),
            "a narrowed report counts what it kept: {narrowed}"
        );
        for other in ["network", "memory", "io", "cpu", "security"] {
            if other != key {
                assert_eq!(
                    narrowed["counts"][other],
                    serde_json::json!(0),
                    "--category {category} keeps nothing of {other}: {narrowed}"
                );
            }
        }
    }
}

#[test]
fn a_run_without_the_new_flags_keeps_its_key_set() {
    // The compatibility half of the contract: without `--only`/`--exclude`
    // the document keeps the exact keys it always had, so an existing
    // consumer reads the same object it read before.
    for arguments in [
        vec!["check"],
        vec!["check", "--category", "net"],
        vec!["check", "--conservative"],
    ] {
        let out = ktuner(&arguments);
        assert!(
            matches!(out.status.code(), Some(0 | 1)),
            "check must exit 0 (nothing to advise) or 1 (recommendations): {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let body = json(&out, "check");
        let mut keys: Vec<&str> = body
            .as_object()
            .unwrap_or_else(|| panic!("check prints an object: {body}"))
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys, BASELINE_KEYS,
            "a run without the new flags must keep its exact key set: {body}"
        );
    }
}

#[test]
fn selecting_every_recommendation_is_the_whole_report() {
    // The compatibility contract, measured directly: a selection that names
    // every recommendation keeps the report intact, so its output must equal
    // a run with no flag at all — byte for byte.
    let findings = check_findings();
    let mut arguments = vec!["check"];
    for finding in &findings {
        arguments.push("--only");
        arguments.push(finding["param"].as_str().expect("param"));
    }
    let selecting_all = ktuner(&arguments);
    let baseline = ktuner(&["check"]);
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
fn only_and_exclude_together_are_a_usage_error() {
    // The two flags are opposite directions of one instruction; a run that
    // carried both would have to guess which one wins. Refusing is an
    // argument error (stderr JSON, exit 2), and it is decided before the
    // system is read: with a category that is itself invalid the usage error
    // must still win, so no host state can change the answer.
    for arguments in [
        vec![
            "check",
            "--only",
            "vm.swappiness",
            "--exclude",
            "vm.dirty_ratio",
        ],
        vec![
            "check",
            "--only",
            "vm.swappiness",
            "--exclude",
            "vm.dirty_ratio",
            "--category",
            "no_such_category",
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
            serde_json::json!("check takes either --only or --exclude, not both"),
            "the usage error must name the conflict"
        );
    }
}
