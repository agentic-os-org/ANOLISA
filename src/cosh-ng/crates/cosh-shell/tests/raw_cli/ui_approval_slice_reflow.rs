use std::time::Duration;

use crate::support::ui_approval_slice::{
    real_cosh_core_path, RealCoreApprovalHarness, SliceLaunch, WorkerFault,
};

#[test]
fn nonempty_long_line_survives_worker_card_row_shrink_and_width_reflow() {
    assert!(
        real_cosh_core_path().exists(),
        "real cosh-core must be built"
    );
    for fault in [None, Some(WorkerFault::Kill)] {
        let mut launch = SliceLaunch::activated();
        launch.cols = 80;
        launch.rows = 30;
        let mut harness = RealCoreApprovalHarness::spawn(launch);
        assert!(harness.seed_reflow_long_line(), "long output must be ready");
        assert!(
            harness.request_approval_card(),
            "worker card must be visible"
        );
        assert_eq!(
            harness
                .worker_descendants_within(Duration::from_secs(5))
                .len(),
            1,
            "the old card must be worker rendered: {}",
            harness.current_diagnostics()
        );
        let victim = fault.map(|fault| harness.fault_worker(fault));
        assert!(
            harness.resize_and_expect_one_inline_card(20, 47, Duration::from_secs(20)),
            "real row shrink and width reflow must leave one visible card and no worker"
        );
        if let Some(victim) = victim {
            assert!(
                harness.wait_for_pid_gone(victim, Duration::from_secs(10)),
                "faulted worker must be reaped"
            );
        }
        harness
            .approve_execute_shell_second_request_from_visible_card()
            .check_inline_baseline()
            .expect("resized approval must execute once");
    }
}
