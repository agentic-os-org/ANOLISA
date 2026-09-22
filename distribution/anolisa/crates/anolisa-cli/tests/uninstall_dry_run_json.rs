//! Subprocess wire-contract coverage for `uninstall --dry-run --json`.
//!
//! Plain uninstall runs the planner pipeline: for an absent component the
//! dry-run reports the same refusal as a real run would (an error envelope,
//! `NOT_INSTALLED`, exit 2), so previews never disagree with reality. The
//! name itself is validated first — state and the component index are the
//! identity authorities (issue #2630), so `NOT_INSTALLED` means "a supported
//! component with nothing installed", as
//! `unknown_name_is_rejected_while_supported_name_reports_not_installed`
//! pins. `--purge` keeps the legacy plan view: a unified `data.dry_run`,
//! plan fields flat under `data`. These tests drive the compiled binary and
//! assert the full envelope, which in-crate unit tests cannot cover.

use std::path::Path;
use std::process::Output;

mod common;

/// Run the CLI and parse its stdout as a JSON envelope, asserting `expected`
/// as the exit code.
fn run_json(arguments: &[&str], expected: i32) -> serde_json::Value {
    let output: Output = common::run(arguments);
    assert_eq!(
        Some(expected),
        output.status.code(),
        "unexpected exit code; stderr: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "stdout must be a JSON envelope: {error}; stdout: {}",
            String::from_utf8_lossy(&output.stdout),
        )
    })
}

/// The #1471 contract every generic plan-view dry-run must satisfy: a unified
/// `data.dry_run`, a genuinely empty `phases` for an absent target, and plan
/// fields kept flat under `data` (never nested behind a `plan` key).
fn assert_plan_dry_run_contract(data: &serde_json::Value) {
    assert_eq!(
        data.get("dry_run"),
        Some(&serde_json::Value::Bool(true)),
        "data.dry_run must be true across the plan view: {data}",
    );
    assert_eq!(
        data.get("phases"),
        Some(&serde_json::Value::Array(Vec::new())),
        "absent-component phases must be empty: {data}",
    );
    assert!(
        data.get("plan").is_none(),
        "plan fields must stay flat under data, not nested under 'plan': {data}",
    );
}

/// A temp prefix + system mode isolates the run from real state; a name that
/// is index-supported but not installed falls through to the absent-plan
/// branch.
fn absent_uninstall_args<'a>(prefix: &'a str, extra: &[&'a str]) -> Vec<&'a str> {
    let mut args = vec![
        "--json",
        "--dry-run",
        "--install-mode",
        "system",
        "--prefix",
        prefix,
        "uninstall",
    ];
    args.extend_from_slice(extra);
    args.push("definitely-missing");
    args
}

fn seed_local_repo(prefix: &Path) {
    seed_local_repo_with_index(prefix, &["definitely-missing"]);
}

/// Seed `repo.toml` plus a generation-2 component index publishing
/// `components`, so identity resolution can validate targets against a local
/// authority.
fn seed_local_repo_with_index(prefix: &Path, components: &[&str]) {
    let repo_v1 = prefix.join("repo/v1");
    std::fs::create_dir_all(&repo_v1).expect("local repo");
    let mut index = String::from("schema_version = 2\n");
    for component in components {
        index.push_str(&format!(
            "\n[[components]]\nname = \"{component}\"\ntargets = [{{ os = \"{os}\", arch = \"{arch}\" }}]\n",
            os = std::env::consts::OS,
            arch = std::env::consts::ARCH,
        ));
    }
    std::fs::write(repo_v1.join("components-v2.toml"), index).expect("component index");
    let etc = prefix.join("etc/anolisa");
    std::fs::create_dir_all(&etc).expect("config dir");
    std::fs::write(
        etc.join("repo.toml"),
        format!(
            "schema_version = 1\ndefault_backend = \"raw\"\n\n[backends.raw]\nbase_url = \"file://{}\"\n",
            repo_v1.display()
        ),
    )
    .expect("repo config");
}

/// A dry-run of an absent component must report the same refusal a real run
/// would — an error envelope with the actionable "not installed" reason —
/// never a hollow successful preview.
#[test]
fn uninstall_dry_run_json_absent_component_reports_not_installed() {
    let tmp = tempfile::tempdir().expect("tempdir");
    seed_local_repo(tmp.path());
    let prefix = tmp.path().to_str().expect("utf-8 prefix");
    let value = run_json(&absent_uninstall_args(prefix, &[]), 2);

    assert_eq!(
        value.get("ok"),
        Some(&serde_json::Value::Bool(false)),
        "an absent component refuses on dry-run exactly like a real run: {value}",
    );
    let error = value.get("error").expect("envelope must carry error");
    assert_eq!(
        error.get("code").and_then(|v| v.as_str()),
        Some("NOT_INSTALLED"),
        "an absent target is its own code, distinct from a malformed one: {value}",
    );
    assert!(
        error
            .get("reason")
            .and_then(|v| v.as_str())
            .is_some_and(|reason| reason.contains("not installed")),
        "the reason must say not installed: {value}",
    );
}

/// The issue #2630 identity contract on the public wire: a name the index
/// knows but state lacks is `NOT_INSTALLED`, a name the index rejects is
/// `INVALID_ARGUMENT`, and without any index an unseen name cannot be
/// validated at all (`EXECUTION_FAILED`, exit 1). Callers may therefore read
/// `NOT_INSTALLED` as "the name was valid, it just isn't installed".
#[test]
fn unknown_name_is_rejected_while_supported_name_reports_not_installed() {
    let tmp = tempfile::tempdir().expect("tempdir");
    seed_local_repo_with_index(tmp.path(), &["cosh"]);
    let prefix = tmp.path().to_str().expect("utf-8 prefix");

    let uninstall = |component: &str, expected: i32| {
        run_json(
            &[
                "--json",
                "--dry-run",
                "--install-mode",
                "system",
                "--prefix",
                prefix,
                "uninstall",
                component,
            ],
            expected,
        )
    };
    let code = |value: &serde_json::Value| {
        value
            .get("error")
            .and_then(|error| error.get("code"))
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string()
    };

    // "cosh" is in the index but not installed; "coshh" is in no index at all.
    assert_eq!(code(&uninstall("cosh", 2)), "NOT_INSTALLED");
    let unknown = uninstall("coshh", 2);
    assert_eq!(code(&unknown), "INVALID_ARGUMENT");
    assert!(
        unknown
            .get("error")
            .and_then(|error| error.get("reason"))
            .and_then(|v| v.as_str())
            .is_some_and(|reason| reason.contains("unsupported component 'coshh'")),
        "the unknown name must be rejected as unsupported: {unknown}",
    );

    // Without an index nothing can validate a new name: explicit failure
    // instead of a synthesized not_installed. The repo config points at a
    // repository that publishes no component index, keeping the run hermetic.
    let bare = tempfile::tempdir().expect("tempdir");
    let etc = bare.path().join("etc/anolisa");
    std::fs::create_dir_all(&etc).expect("config dir");
    std::fs::write(
        etc.join("repo.toml"),
        format!(
            "schema_version = 1\ndefault_backend = \"raw\"\n\n[backends.raw]\nbase_url = \"file://{}\"\n",
            bare.path().join("no-such-repo/v1").display()
        ),
    )
    .expect("repo config");
    let bare_prefix = bare.path().to_str().expect("utf-8 prefix");
    let unvalidated = run_json(
        &[
            "--json",
            "--dry-run",
            "--install-mode",
            "system",
            "--prefix",
            bare_prefix,
            "uninstall",
            "coshh",
        ],
        1,
    );
    assert_eq!(code(&unvalidated), "EXECUTION_FAILED");
    assert!(
        unvalidated
            .get("error")
            .and_then(|error| error.get("reason"))
            .and_then(|v| v.as_str())
            .is_some_and(|reason| reason.contains("component index is unavailable")),
        "an unvalidatable name must name the missing index: {unvalidated}",
    );
}

#[test]
fn uninstall_purge_dry_run_json_uses_same_contract() {
    let tmp = tempfile::tempdir().expect("tempdir");
    seed_local_repo(tmp.path());
    let prefix = tmp.path().to_str().expect("utf-8 prefix");
    let value = run_json(&absent_uninstall_args(prefix, &["--purge"]), 0);

    let data = value.get("data").expect("envelope must carry data");
    assert_eq!(
        data.get("operation").and_then(|v| v.as_str()),
        Some("purge"),
        "purge keeps the generic plan view: {data}",
    );
    assert_plan_dry_run_contract(data);
}
