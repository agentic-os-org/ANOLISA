//! End-to-end coverage for a repeatable `ktuner check/tune --category`,
//! driving the real binary.
//!
//! `--category` accepts several values and keeps their union: the entries are
//! exactly the union of the single-value runs by param, repeating the aliases
//! of one category changes nothing, and a value outside the known set is
//! rejected before the system is read (stderr JSON, exit 2) instead of being
//! silently ignored. A single value keeps the filter and exit codes it always
//! had.

use std::collections::BTreeSet;
use std::process::{Command, Output};

fn ktuner(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ktuner"))
        .args(arguments)
        .output()
        .expect("run ktuner")
}

fn check(arguments: &[&str]) -> (Option<i32>, serde_json::Value) {
    let mut args = vec!["check"];
    args.extend_from_slice(arguments);
    let out = ktuner(&args);
    assert!(
        matches!(out.status.code(), Some(0 | 1)),
        "ktuner {args:?} must exit 0 (nothing to advise) or 1 (recommendations): {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let body = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("ktuner {args:?} must print JSON on stdout: {e}"));
    (out.status.code(), body)
}

/// The params `check` reports for a view, as a set: the comparison is on the
/// entries themselves, so output order is not part of the contract.
fn check_params(view: &[&str]) -> BTreeSet<String> {
    let (_, body) = check(view);
    body["recommendations"]
        .as_array()
        .expect("check must print a recommendations array")
        .iter()
        .map(|entry| {
            entry["param"]
                .as_str()
                .unwrap_or_else(|| panic!("entry without a param: {entry}"))
                .to_string()
        })
        .collect()
}

/// Every param a `tune --dry-run` view accounts for: the entries it would
/// apply plus the ones it skips, which together are the filtered plan.
fn tune_params(view: &[&str]) -> BTreeSet<String> {
    let mut args = vec!["tune", "--dry-run"];
    args.extend_from_slice(view);
    let out = ktuner(&args);
    assert!(
        matches!(out.status.code(), Some(0 | 1)),
        "ktuner {args:?} must exit 0 (plan) or 1 (every entry blocked): {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let body: serde_json::Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("ktuner {args:?} must print JSON on stdout: {e}"));
    let mut params = BTreeSet::new();
    for key in ["would_apply", "would_skip"] {
        for entry in body[key].as_array().into_iter().flatten() {
            params.insert(
                entry["param"]
                    .as_str()
                    .unwrap_or_else(|| panic!("tune entry without a param: {entry}"))
                    .to_string(),
            );
        }
    }
    params
}

#[test]
fn repeated_categories_keep_the_union_of_the_single_runs() {
    // `--category net --category mem` is one run whose view is the union of
    // the two single-category views — every entry each value keeps, and
    // nothing else. This is what lets an agent ask for "network and memory"
    // in one tune instead of merging two scoped runs itself.
    let net = check_params(&["--category", "net"]);
    let mem = check_params(&["--category", "mem"]);
    let union = check_params(&["--category", "net", "--category", "mem"]);
    let expected: BTreeSet<String> = net.union(&mem).cloned().collect();
    assert_eq!(union, expected);
    if expected.is_empty() {
        eprintln!("skip: this host has no net/mem recommendations");
        return;
    }
    // The union is still a filter: nothing outside the bare check view.
    let all = check_params(&[]);
    let extra: Vec<&String> = union.difference(&all).collect();
    assert!(
        extra.is_empty(),
        "union must not add entries outside check: {extra:?}"
    );
}

#[test]
fn repeated_aliases_of_one_category_count_once() {
    // net/network/网络 name one category: repeating them (in any combination)
    // must keep exactly the single-value result, each entry once.
    let single = check_params(&["--category", "net"]);
    for view in [
        &["--category", "net", "--category", "net"][..],
        &[
            "--category",
            "net",
            "--category",
            "network",
            "--category",
            "网络",
        ][..],
        &["--category", "NET", "--category", "Net"][..],
    ] {
        assert_eq!(check_params(view), single, "view {view:?}");
    }
}

#[test]
fn a_single_category_keeps_its_existing_filter_and_exit_codes() {
    // The pre-existing single-value contract: a view is a subset of the bare
    // check, each category keeps its own membership, and the exit code stays
    // 1 exactly when the view still has recommendations.
    let (_, bare) = check(&[]);
    let all: BTreeSet<String> = bare["recommendations"]
        .as_array()
        .expect("recommendations array")
        .iter()
        .filter_map(|entry| entry["param"].as_str().map(str::to_string))
        .collect();
    for (category, membership) in [
        ("net", "net. prefix or conntrack"),
        ("mem", "param_subcategory memory"),
        ("security", "Category::Security"),
    ] {
        let (code, body) = check(&["--category", category]);
        let recs = body["recommendations"].as_array().expect("array");
        let params: BTreeSet<String> = recs
            .iter()
            .filter_map(|entry| entry["param"].as_str().map(str::to_string))
            .collect();
        assert!(
            params.is_subset(&all),
            "{category} must stay in check's view"
        );
        assert_eq!(code, Some(if params.is_empty() { 0 } else { 1 }));
        for entry in recs {
            let param = entry["param"].as_str().expect("param");
            match category {
                "net" => assert!(
                    param.starts_with("net.") || param.contains("conntrack"),
                    "{param} is not a network param"
                ),
                "mem" => assert_eq!(
                    entry["subcategory"], "memory",
                    "{param} is not a memory param"
                ),
                "security" => assert_eq!(
                    entry["category"], "security",
                    "{param} is not a security param"
                ),
                other => panic!("unhandled category {other} ({membership})"),
            }
        }
    }
}

#[test]
fn an_unknown_category_is_rejected_before_the_system_is_read() {
    // Every value is validated before the system read (the position the
    // single value had), so a typo anywhere in the list fails the command
    // with the documented error and no stdout — never a plan missing one of
    // the named categories. The invalid value is named even when it follows
    // known ones: validation does not stop at the first accepted value.
    let net = check(&["--category", "net"]);
    assert!(matches!(net.0, Some(0 | 1)), "net must be a valid category");

    let out = ktuner(&["check", "--category", "net", "--category", "no_such"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        out.stdout.is_empty(),
        "an invalid category must not print a plan: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    let body: serde_json::Value =
        serde_json::from_slice(&out.stderr).unwrap_or_else(|e| panic!("stderr must be JSON: {e}"));
    let error = body["error"].as_str().expect("error string");
    assert!(
        error.contains("no_such"),
        "the error must name the rejected value, got: {error}"
    );
}

#[test]
fn tune_dry_run_plans_the_union_it_would_apply() {
    // `tune` reads the same view: the union of two categories is what the
    // plan accounts for (would_apply plus would_skip), so an agent can drive
    // one scoped run per category pair instead of one per category.
    let net = tune_params(&["--category", "net"]);
    let mem = tune_params(&["--category", "mem"]);
    let union = tune_params(&["--category", "net", "--category", "mem"]);
    let expected: BTreeSet<String> = net.union(&mem).cloned().collect();
    assert_eq!(union, expected);
    if expected.is_empty() {
        eprintln!("skip: this host has no net/mem recommendations");
    }
}
