//! Worker-first shutdown preserves SDK transports and still reclaims descendants.

use super::*;

#[test]
fn worker_shutdown_keeps_native_children_alive_until_the_worker_closes_them() {
    for leader_signals in [false, true] {
        for interrupt in [false, true] {
            let directory = Directory::new();
            let mut command = grace_command(&directory, "worker", 1000, "");
            if leader_signals {
                command
                    .environment
                    .insert("AW_FOREGROUND_WORKER".into(), "1".into());
            }
            if interrupt {
                command
                    .environment
                    .insert("AW_FOREGROUND_INT".into(), "1".into());
            }
            let (result, _) = run_grace_case(&directory, &command);
            assert_eq!(
                result,
                if leader_signals {
                    "code=Some(37) signal=None"
                } else {
                    "code=Some(42) signal=None"
                }
            );
            assert_eq!(
                directory.0.join("cleanup-complete").exists(),
                leader_signals
            );
            assert!(directory.process("sdk").state().is_none());
            directory.assert_reaped();
        }
    }
}

#[test]
fn worker_shutdown_still_bounds_and_reaps_a_stubborn_group() {
    for mode in ["repeat", "failure-first", "failure-after"] {
        let directory = Directory::new();
        let mut command = grace_command(&directory, "grace-stubborn", 800, mode);
        command
            .environment
            .insert("AW_FOREGROUND_WORKER".into(), "1".into());
        let (result, elapsed) = run_grace_case(&directory, &command);
        assert_eq!(result, "code=None signal=Some(9)");
        let minimum = if mode == "failure-first" { 2000 } else { 800 };
        assert!(
            (minimum..3500).contains(&elapsed),
            "{mode}: elapsed {elapsed}"
        );
        directory.assert_descendant_stopped();
    }
}
