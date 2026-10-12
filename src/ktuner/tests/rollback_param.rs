//! CLI contract for the optional `ktuner rollback <param>` positional: the new
//! argument must leave the existing invocations and their error bodies exactly
//! as they were, and combined with `--list` it narrows the read-only preview to
//! the entries it names. The actual restoration is covered by the
//! engine's private-fixture tests; this file deliberately never runs a rollback
//! that could reach a real ledger.
use std::process::{Command, Output};

fn ktuner(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ktuner"))
        .args(arguments)
        .output()
        .expect("run ktuner")
}

/// The exact bytes a non-root caller got before the positional existed: the
/// README's stderr JSON, pretty-printed like every other body the binary emits.
fn root_refusal() -> String {
    format!(
        "{}\n",
        serde_json::to_string_pretty(&serde_json::json!({
            "error": "rollback requires root (sudo ktuner rollback)"
        }))
        .unwrap()
    )
}

#[test]
fn rollback_param_cli_contract() {
    // A caller who cannot reach the ledger is answered before anything runs.
    // On a root host these commands would touch the real ledger, so this branch
    // only asserts the message bytes when the root gate is what answers.
    if unsafe { libc::geteuid() } != 0 {
        for arguments in [
            &["rollback"][..],
            &["rollback", "--list"][..],
            &["rollback", "--list", "vm.swappiness"][..],
            &["rollback", "vm.swappiness"][..],
        ] {
            let out = ktuner(arguments);
            assert_eq!(out.status.code(), Some(2), "{arguments:?}");
            assert!(out.stdout.is_empty(), "{arguments:?}");
            assert_eq!(
                String::from_utf8_lossy(&out.stderr),
                root_refusal(),
                "{arguments:?} must answer the same root-refusal bytes as before \
                 the positional existed"
            );
        }
    }
}
