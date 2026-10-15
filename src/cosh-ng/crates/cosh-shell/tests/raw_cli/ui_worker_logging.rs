use std::time::Duration;

use crate::support::ui_approval_slice::{RealCoreApprovalHarness, SliceLaunch};

#[test]
fn stderr_tracing_from_a_regular_file_log_directory_forces_legacy_approval() {
    for activation in [false, true] {
        let mut launch = SliceLaunch::inline();
        launch.activation = activation;
        launch.log_directory_blocked = true;
        let mut harness = RealCoreApprovalHarness::spawn(launch);
        assert!(
            harness.stderr_logging_visible(),
            "legacy stderr-to-TTY positive control"
        );
        assert!(
            harness.request_approval_card(),
            "same request remains visible"
        );
        assert_eq!(harness.visible_approval_cards(), 1, "one current card");
        assert!(
            harness
                .worker_descendants_within(Duration::from_secs(2))
                .is_empty(),
            "stderr tracing must refuse the anchored slice"
        );
        harness
            .approve_execute_shell_second_request_from_visible_card()
            .check_inline_baseline()
            .expect("legacy approval executes once with stable Bash");
    }
}
