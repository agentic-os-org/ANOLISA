//! CLI contract for `ktuner why <param>…`: a batch answers one document from
//! one diagnosis — `count` and one entry per parameter, in the order given and
//! deduplicated by the lookup's own alias policy — while a single parameter
//! keeps the flat body it always had, byte for byte. A parameter the batch
//! cannot answer refuses the whole command (no partial document). The command
//! is read-only and needs no root, so the tests run wherever the parameters
//! they name exist on a Linux host.
use std::process::{Command, Output};

fn ktuner(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ktuner"))
        .args(arguments)
        .output()
        .expect("run read-only ktuner command")
}

/// The exact bytes the binary emits for an error: the README's stderr JSON,
/// pretty-printed like every other body it prints.
fn error_body(message: &str) -> String {
    format!(
        "{}\n",
        serde_json::to_string_pretty(&serde_json::json!({ "error": message })).unwrap()
    )
}

#[test]
fn why_batch_answers_one_document_for_every_parameter() {
    // `kernel.ostype` is a read-only kernel string on every Linux host, so it
    // answers the fallback (optimal) branch; `vm.swappiness` answers whichever
    // branch the host's rules decide, so its entry is only checked for shape.
    // The batch document carries count + params and the exit code aggregates:
    // 0 when every entry is optimal, 1 when at least one is a recommendation.
    let Ok(_) = std::fs::metadata("/proc/sys/vm/swappiness") else {
        eprintln!("skip: vm.swappiness is unavailable on this host");
        return;
    };
    let out = ktuner(&["why", "kernel.ostype", "vm.swappiness"]);
    assert!(
        matches!(out.status.code(), Some(0 | 1)),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.stderr.is_empty());
    let body: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("the batch prints JSON on stdout");
    let entries = body["params"].as_array().expect("params is an array");
    assert_eq!(body["count"], serde_json::json!(2));
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0]["param"], serde_json::json!("kernel.ostype"));
    assert_eq!(entries[0]["status"], serde_json::json!("optimal"));
    assert_eq!(entries[1]["param"], serde_json::json!("vm.swappiness"));
    // The flat single-parameter body must not leak into the batch shape.
    for key in ["param", "current", "status"] {
        assert!(
            body.get(key).is_none(),
            "a batch must not carry the single-parameter key {key}: {body}"
        );
    }
}

#[test]
fn why_batch_dedupes_the_aliases_of_one_parameter() {
    let Ok(current) = std::fs::read_to_string("/proc/sys/kernel/ostype") else {
        eprintln!("skip: kernel.ostype is unavailable or unreadable on this host");
        return;
    };
    let out = ktuner(&["why", "kernel.ostype", "kernel/ostype", "KERNEL.OSTYPE"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.stderr.is_empty());
    let body: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(body["count"], serde_json::json!(1));
    assert_eq!(
        body["params"][0],
        serde_json::json!({
            "param": "kernel.ostype",
            "current": current.trim(),
            "status": "optimal",
        })
    );
}

#[test]
fn why_single_parameter_keeps_the_flat_body() {
    // The byte-level anchor for the single-parameter contract: one positional
    // answers the same pretty document with the same trailing newline as
    // before the batch existed.
    let Ok(current) = std::fs::read_to_string("/proc/sys/kernel/ostype") else {
        eprintln!("skip: kernel.ostype is unavailable or unreadable on this host");
        return;
    };
    let out = ktuner(&["why", "kernel.ostype"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stderr.is_empty());
    let expected = serde_json::json!({
        "param": "kernel.ostype",
        "current": current.trim(),
        "status": "optimal",
    });
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!(
            "{}\n",
            serde_json::to_string_pretty(&expected).expect("render the expected body")
        )
    );
}

#[test]
fn why_batch_refuses_the_whole_command_on_a_missing_parameter() {
    // Mixed batch: the first parameter answers, the second does not — the
    // command answers the single-parameter error naming the miss, with
    // nothing on stdout, so a caller never reads a partial batch.
    let out = ktuner(&["why", "kernel.ostype", "this.is.not.a.parameter"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        out.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        error_body("parameter not found: this.is.not.a.parameter")
    );
}

#[test]
fn why_without_a_parameter_keeps_the_usage_error() {
    // The parser still refuses a bare `why` (the positional is required), and
    // the usage error keeps the README's stderr JSON contract.
    let out = ktuner(&["why"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    let error: serde_json::Value =
        serde_json::from_slice(&out.stderr).expect("usage errors are stderr JSON");
    assert!(error["error"].as_str().is_some_and(|text| !text.is_empty()));
}
