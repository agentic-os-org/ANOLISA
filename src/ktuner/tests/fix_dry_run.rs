//! End-to-end coverage for `ktuner fix <param> --dry-run`, driving the real
//! binary unprivileged.
//!
//! The preview is read-only, so the whole contract is testable without root:
//! it carries the `tune --dry-run` keys scoped to one parameter, writes
//! nothing, and a parameter the plain command would refuse answers with the
//! plain command's own error — same stderr JSON, same exit code — instead of
//! a preview that could disagree with the command it previews.

use std::process::{Command, Output};

fn ktuner(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ktuner"))
        .args(arguments)
        .output()
        .expect("run ktuner")
}

fn stdout_json(output: &Output, what: &str) -> serde_json::Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "{what} must print JSON on stdout: {e}\nstderr: {}",
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn error_json(output: &Output, what: &str) -> serde_json::Value {
    assert!(
        output.stdout.is_empty(),
        "{what} must not print on stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    serde_json::from_slice(&output.stderr).unwrap_or_else(|e| {
        panic!(
            "{what} must report its error as JSON on stderr: {e}\nstderr: {}",
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

/// An applicable finding from `check` — writable here and not skipped —
/// or None on a host with nothing to recommend.
fn applicable_finding() -> Option<serde_json::Value> {
    let check = ktuner(&["check"]);
    assert!(
        matches!(check.status.code(), Some(0 | 1)),
        "ktuner check must exit 0 (nothing to advise) or 1 (recommendations): {}",
        String::from_utf8_lossy(&check.stderr)
    );
    stdout_json(&check, "ktuner check")["recommendations"]
        .as_array()?
        .iter()
        .find(|rec| rec["writable"] == serde_json::json!(true) && rec.get("skip_reason").is_none())
        .cloned()
}

#[test]
fn fix_dry_run_previews_the_single_write_in_the_tune_shape() {
    // The one case that prints a preview body: an in-plan recommendation this
    // environment can write. The keys must match `tune --dry-run` so one
    // parser reads both previews, `would_apply` must carry exactly this
    // parameter in check's own entry shape, `would_skip` is empty (a refusal
    // takes the command's error channel instead), and the run exits 0 without
    // touching the rollback ledger or the persisted file.
    let Some(finding) = applicable_finding() else {
        // Nothing applicable on this host: only the refusal paths are
        // exercised (covered by the other tests).
        return;
    };
    let param = finding["param"].as_str().expect("param").to_string();

    // Snapshot before the preview: no ledger byte may change (or appear), and
    // a 0600 ledger left by a root run reads as None here on both sides.
    let ledger = std::path::Path::new("/var/lib/ktuner/rollback.json");
    let persisted = std::path::Path::new("/etc/sysctl.d/99-ktuner.conf");
    let ledger_before = std::fs::read(ledger).ok();
    let persisted_before = std::fs::read(persisted).ok();

    let plan = ktuner(&["fix", &param, "--dry-run"]);
    assert_eq!(
        plan.status.code(),
        Some(0),
        "a planned preview exits 0: {}",
        String::from_utf8_lossy(&plan.stderr)
    );
    assert!(
        plan.stderr.is_empty(),
        "the preview is not an error: {}",
        String::from_utf8_lossy(&plan.stderr)
    );
    let preview = stdout_json(&plan, "ktuner fix <param> --dry-run");

    let keys: Vec<&str> = preview
        .as_object()
        .expect("ktuner fix --dry-run prints an object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        vec!["blocked", "dry_run", "status", "would_apply", "would_skip"],
        "the preview carries the tune plan's keys, scoped to one parameter: {preview}"
    );
    assert_eq!(preview["dry_run"], serde_json::json!(true));
    assert_eq!(preview["status"], serde_json::json!("planned"));
    assert_eq!(preview["blocked"], serde_json::json!(0));
    assert_eq!(preview["would_skip"], serde_json::json!([]));

    let would_apply = preview["would_apply"]
        .as_array()
        .unwrap_or_else(|| panic!("the preview must carry would_apply: {preview}"));
    assert_eq!(
        would_apply.len(),
        1,
        "the scope is this one parameter: {preview}"
    );
    // The entry is check's own object: same param, same recommendation, and
    // the same key set as the diagnosis it is reconciled against.
    for field in ["param", "current", "recommended", "reason", "confidence"] {
        assert_eq!(
            would_apply[0][field], finding[field],
            "would_apply must carry check's own {field}: {preview}"
        );
    }
    let mut entry_keys: Vec<&str> = would_apply[0]
        .as_object()
        .expect("a recommendation object")
        .keys()
        .map(String::as_str)
        .collect();
    let mut finding_keys: Vec<&str> = finding
        .as_object()
        .expect("a recommendation object")
        .keys()
        .map(String::as_str)
        .collect();
    entry_keys.sort_unstable();
    finding_keys.sort_unstable();
    assert_eq!(
        entry_keys, finding_keys,
        "the preview entry and the diagnosis carry one shape: {preview}"
    );

    // Read-only end to end: neither the ledger nor the persisted sysctl.d
    // file gained a byte (neither may even have been created).
    assert_eq!(
        ledger_before,
        std::fs::read(ledger).ok(),
        "the preview must not touch the rollback ledger"
    );
    assert_eq!(
        persisted_before,
        std::fs::read(persisted).ok(),
        "the preview must not touch the persisted file"
    );
}

#[test]
fn fix_dry_run_accepts_the_spellings_plain_fix_does() {
    // The preview goes through the same lookup as `fix` and `why`, so the
    // slash/dot aliases address the same recommendation and the entry names
    // its canonical spelling. Sysfs identities are filesystem names and have
    // no alias form, so only dotted sysctl parameters are exercised here.
    let Some(finding) = applicable_finding() else {
        return;
    };
    let param = finding["param"].as_str().expect("param").to_string();
    if !param.contains('.') || param.contains('/') {
        return;
    }
    let alias = param.replace('.', "/");
    let plan = ktuner(&["fix", &alias, "--dry-run"]);
    assert_eq!(
        plan.status.code(),
        Some(0),
        "the alias of {param} must resolve: {}",
        String::from_utf8_lossy(&plan.stderr)
    );
    let preview = stdout_json(&plan, "ktuner fix <slash alias> --dry-run");
    assert_eq!(
        preview["would_apply"][0]["param"],
        serde_json::json!(param),
        "the preview names the canonical parameter: {preview}"
    );
}

#[test]
fn fix_dry_run_refuses_off_plan_parameters_like_plain_fix() {
    // A parameter outside the plan — already at its recommended value, an
    // unknown name, or a deny-listed parameter (no built-in rule ever
    // recommends one, so it never reaches the plan) — is not a preview: the
    // flag answers with the plain command's error, byte for byte. A preview
    // that invented its own verdict would make
    // `ktuner fix X --dry-run && ktuner fix X` a lying pre-check.
    let is_root = unsafe { libc::geteuid() } == 0;
    for param in [
        "no_such_ktuner_parameter",
        "kernel.core_pattern",
        "kernel/modules_disabled",
    ] {
        let dry = ktuner(&["fix", param, "--dry-run"]);
        assert_eq!(
            dry.status.code(),
            Some(2),
            "{param}: {}",
            String::from_utf8_lossy(&dry.stderr)
        );
        assert_eq!(
            error_json(&dry, "ktuner fix <param> --dry-run"),
            serde_json::json!({
                "error": format!("parameter not found or already optimal: {param}")
            }),
            "the preview refuses in the plain command's own terms: {param}"
        );
        if is_root {
            // Root's plain command refuses the same input identically (and
            // writes nothing: the parameter is not in the plan). Non-root
            // cannot compare here — its plain run stops at the root gate —
            // so the byte comparison is asserted where it exists.
            let plain = ktuner(&["fix", param]);
            assert_eq!(plain.status.code(), dry.status.code(), "{param}");
            assert_eq!(plain.stdout, dry.stdout, "{param}");
            assert_eq!(
                plain.stderr, dry.stderr,
                "the preview must refuse byte-for-byte like the command it previews: {param}"
            );
        }
    }
}

#[test]
fn plain_fix_keeps_its_root_gate_bytes() {
    // The non-dry-run contract this change must not move: as a non-root
    // caller the gate fires before anything else — nothing on stdout, exactly
    // the documented stderr JSON, exit 2 — for a planned parameter and an
    // absent one alike, so no classification can run before it.
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("skipping: running as root, where plain `fix` would write");
        return;
    }
    for param in ["vm.swappiness", "no_such_ktuner_parameter"] {
        let out = ktuner(&["fix", param]);
        assert_eq!(
            out.status.code(),
            Some(2),
            "{param}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            out.stdout.is_empty(),
            "{param} must not print on stdout: {}",
            String::from_utf8_lossy(&out.stdout)
        );
        assert_eq!(
            String::from_utf8_lossy(&out.stderr),
            format!("{{\n  \"error\": \"fix requires root (sudo ktuner fix {param})\"\n}}\n"),
            "{param}: the root gate's bytes must not move"
        );
    }
}
