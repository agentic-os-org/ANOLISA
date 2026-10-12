//! CLI contract for the repeatable `ktuner rollback <param>…` positionals: the
//! added arguments must leave the existing invocations and their error bodies
//! exactly as they were, and combined with `--list` they narrow the read-only
//! preview to the entries they name. The restoration itself is covered
//! by the engine's private-fixture tests; this file deliberately never runs a
//! rollback that could reach a real ledger.
use std::process::{Command, Output};

fn ktuner(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ktuner"))
        .args(arguments)
        .output()
        .expect("run ktuner")
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
fn rollback_multi_param_cli_contract() {
    // A caller who cannot reach the ledger is answered before anything runs,
    // and a batch asks the question a single parameter asked: the root gate
    // precedes the batch, so every arity answers identical bytes.
    if unsafe { libc::geteuid() } != 0 {
        for arguments in [
            &["rollback"][..],
            &["rollback", "--list"][..],
            &["rollback", "--list", "vm.swappiness"][..],
            &["rollback", "--list", "vm.swappiness", "net.core.somaxconn"][..],
            &["rollback", "vm.swappiness"][..],
            &["rollback", "vm.swappiness", "net.core.somaxconn"][..],
            &[
                "rollback",
                "vm.swappiness",
                "net.core.somaxconn",
                "vm.vfs_cache_pressure",
            ][..],
        ] {
            let out = ktuner(arguments);
            assert_eq!(out.status.code(), Some(2), "{arguments:?}");
            assert!(out.stdout.is_empty(), "{arguments:?}");
            assert_eq!(
                String::from_utf8_lossy(&out.stderr),
                error_body("rollback requires root (sudo ktuner rollback)"),
                "{arguments:?} must answer the same root-refusal bytes as before \
                 the batch existed"
            );
        }
    }
}
