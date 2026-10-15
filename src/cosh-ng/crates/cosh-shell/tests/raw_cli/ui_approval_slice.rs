use std::time::Duration;

use crate::support::ui_approval_slice::{
    real_cosh_core_path, test_worker_profile, write_malformed_worker,
    write_script_core_for_mutation, CoreChoice, OracleError, RealCoreApprovalHarness, ServerScript,
    SliceLaunch, WorkerFault,
};

/// §14-3 product-behaviour RED: with the debug slice activated, an eligible
/// high-risk approval must be rendered by a separate `cosh-ui-worker` process.
/// This fails while nothing spawns the worker, and it fails on behaviour
/// (0 live workers) rather than on a missing symbol.
#[test]
fn eligible_approval_spawns_worker_process() {
    let core = real_cosh_core_path();
    assert!(core.exists(), "real cosh-core must be built: {core:?}");
    let mut harness = RealCoreApprovalHarness::spawn(SliceLaunch::activated());
    assert!(
        harness.request_approval_card(),
        "approval card must be visible before asserting who rendered it"
    );
    let workers = harness.worker_descendants_within(Duration::from_secs(5));
    assert_eq!(
        workers.len(),
        1,
        "worker_process_count: expected exactly 1, got {} ({workers:?})",
        workers.len()
    );
    harness.deny_and_exit();
}

/// The product closed loop with the slice activated: a real provider request is
/// approved on a card a separate worker process painted, the approved command
/// runs exactly once, an ordinary Bash command still works, a second request
/// arrives, the Bash facts are unchanged and no worker survives the session.
#[test]
fn eligible_approval_worker_closed_loop() {
    let core = real_cosh_core_path();
    assert!(core.exists(), "real cosh-core must be built: {core:?}");
    let mut harness = RealCoreApprovalHarness::spawn(SliceLaunch::activated());
    assert!(
        harness.request_approval_card(),
        "worker-rendered card must be visible before approving it"
    );
    let workers = harness.worker_descendants_within(Duration::from_secs(5));
    assert_eq!(
        workers.len(),
        1,
        "worker_process_count: expected exactly 1, got {} ({workers:?})",
        workers.len()
    );
    let observation = harness.approve_execute_shell_second_request_from_visible_card();
    observation
        .check_inline_baseline()
        .expect("worker-rendered closed loop");
}

/// Deny path: Esc on a worker-rendered card must leave the command unexecuted,
/// reap the worker and keep Bash intact.
///
/// The audit comparison runs against the inline launch on purpose. How many
/// resolutions a denial records is a fact about the existing product path, not
/// something the slice gets to decide, and asserting a literal count here would
/// stay green while the two paths diverged.
#[test]
fn denied_worker_card_matches_inline_and_executes_nothing() {
    let core = real_cosh_core_path();
    assert!(core.exists(), "real cosh-core must be built: {core:?}");

    let mut inline = RealCoreApprovalHarness::spawn(SliceLaunch::inline());
    assert!(
        inline.request_approval_card(),
        "inline card must be visible before denying it"
    );
    let inline_observation = inline.deny_and_observe();
    let inline_resolved = inline_observation.audit_event_count("approval.resolved");
    let inline_started = inline_observation.audit_event_count("tool.execution.started");
    inline_observation
        .check_denied_slice()
        .expect("inline deny path must stay safe");

    let mut worker = RealCoreApprovalHarness::spawn(SliceLaunch::activated());
    assert!(
        worker.request_approval_card(),
        "worker-rendered card must be visible before denying it"
    );
    let workers = worker.worker_descendants_within(Duration::from_secs(5));
    assert_eq!(
        workers.len(),
        1,
        "expected exactly 1 worker, got {workers:?}"
    );
    let worker_observation = worker.deny_and_observe();
    assert_eq!(
        worker_observation.audit_event_count("approval.resolved"),
        inline_resolved,
        "the slice must not change how many resolutions a denial records"
    );
    assert_eq!(
        worker_observation.audit_event_count("tool.execution.started"),
        inline_started,
        "the slice must not change whether a denial executes anything"
    );
    worker_observation
        .check_denied_slice()
        .expect("worker deny path must stay safe");
}

/// Recovery budget for a faulted worker: the heartbeat timeout is three seconds
/// and the coordinator then reaps, rebuilds, re-handshakes and re-renders.
const RECOVERY_TIMEOUT: Duration = Duration::from_secs(30);

/// A worker killed while its card is on screen must be reaped, and the approval
/// must still complete exactly once.
///
/// Removing the frame handling in `ApprovalSurfaceSession::pump` turns this red:
/// the coordinator would rebuild and re-render, the rebuilt frame would be
/// dropped, and the coordinator would then burn the rest of its failure budget
/// waiting for a publish that never comes.
#[test]
fn killed_worker_is_reaped_and_the_approval_still_completes_once() {
    let core = real_cosh_core_path();
    assert!(core.exists(), "real cosh-core must be built: {core:?}");
    let mut harness = RealCoreApprovalHarness::spawn(SliceLaunch::activated());
    assert!(
        harness.request_approval_card(),
        "worker card must be visible before faulting it"
    );
    let victim = harness.fault_worker(WorkerFault::Kill);
    assert!(
        harness.wait_for_pid_gone(victim, RECOVERY_TIMEOUT),
        "the Host must reap killed worker {victim}, not leave it as a zombie"
    );
    assert!(
        harness
            .wait_for_rebuilt_worker(victim, RECOVERY_TIMEOUT)
            .is_some(),
        "a rebuilt worker must take over the card, got {:?}",
        harness.live_worker_pids()
    );
    let observation = harness.approve_execute_shell_second_request_from_visible_card();
    observation
        .check_inline_baseline()
        .expect("a killed worker must not change the product facts");
}

/// A hung worker is a different failure from a dead one: nothing exits, so only
/// the heartbeat timeout can notice. The stopped child must then be killed and
/// reaped, and the approval must still complete exactly once.
#[test]
fn hung_worker_times_out_is_reaped_and_the_approval_still_completes_once() {
    let core = real_cosh_core_path();
    assert!(core.exists(), "real cosh-core must be built: {core:?}");
    let mut harness = RealCoreApprovalHarness::spawn(SliceLaunch::activated());
    assert!(
        harness.request_approval_card(),
        "worker card must be visible before hanging it"
    );
    let victim = harness.fault_worker(WorkerFault::Hang);
    assert!(
        harness.wait_for_pid_gone(victim, RECOVERY_TIMEOUT),
        "a SIGSTOPped worker cannot answer a heartbeat, so the Host must kill and reap {victim}"
    );
    assert!(
        harness
            .wait_for_rebuilt_worker(victim, RECOVERY_TIMEOUT)
            .is_some(),
        "a rebuilt worker must take over the card, got {:?}",
        harness.live_worker_pids()
    );
    let observation = harness.approve_execute_shell_second_request_from_visible_card();
    observation
        .check_inline_baseline()
        .expect("a hung worker must not change the product facts");
}

/// A worker that handshakes correctly and then breaks the protocol must be
/// refused rather than painted, and the refusal must cost the user nothing: the
/// card still appears inline, still resolves exactly once, and no worker
/// survives.
///
/// The handshake is valid on purpose. A candidate that failed the handshake
/// would only re-test version rejection, which the coordinator suite already
/// covers; what is untested is a frame the Host cannot decode arriving from a
/// worker that already proved it could speak v2.
#[test]
fn malformed_worker_frame_is_refused_and_the_card_falls_back_inline() {
    let core = real_cosh_core_path();
    assert!(core.exists(), "real cosh-core must be built: {core:?}");
    let profile = test_worker_profile(&write_malformed_worker());
    let mut harness = RealCoreApprovalHarness::spawn(SliceLaunch {
        worker_profile: Some(profile.path().to_owned()),
        worker_binary: Some(profile.path().join("cosh-ui-worker")),
        ..SliceLaunch::activated()
    });
    assert!(
        harness.request_approval_card(),
        "a malformed worker must not cost the user the approval card"
    );
    assert!(
        harness.wait_for_no_workers(RECOVERY_TIMEOUT),
        "a refused worker must not be left running: {:?}",
        harness.live_worker_pids()
    );
    let observation = harness.approve_execute_shell_second_request_from_visible_card();
    let proof: serde_json::Value = serde_json::from_slice(
        &std::fs::read(profile.path().join("malformed-proof.json"))
            .expect("candidate reached Render and sent malformed frame"),
    )
    .expect("malformed candidate proof");
    assert_eq!(
        proof,
        serde_json::json!({"protocol_version": 2, "candidate_version": env!("CARGO_PKG_VERSION"), "render_received": true, "wire_bytes": 68})
    );
    assert!(
        observation
            .diagnostics
            .contains("approval slice presentation failed"),
        "malformed candidate must fail presentation after handshake: {}",
        observation.diagnostics
    );
    assert!(
        observation
            .transcript
            .contains("The approval display above is no longer current"),
        "a failed worker replacement must announce the current card: {}",
        observation.transcript
    );
    observation
        .check_inline_baseline()
        .expect("malformed worker must fall back to the inline card");
}

/// A focus change must repaint in place.
///
/// This is the case that catches the expensive way to redraw: withdrawing the
/// rectangle and acquiring it again scrolls the surface height off the top of the
/// visible screen, so two arrow keys would destroy roughly a third of what the
/// user can see. The inline path was scroll-neutral, so the slice has to be too.
#[test]
fn focus_change_repaints_in_place_without_scrolling_history_away() {
    let core = real_cosh_core_path();
    assert!(core.exists(), "real cosh-core must be built: {core:?}");
    let mut harness = RealCoreApprovalHarness::spawn(SliceLaunch::activated());
    assert!(
        harness.request_approval_card(),
        "worker card must be visible before moving focus"
    );
    assert_eq!(
        harness
            .worker_descendants_within(Duration::from_secs(5))
            .len(),
        1,
        "the card must be worker-rendered for this to test the slice"
    );
    let (before, after) = harness.focus_change_round_trip(Duration::from_secs(15));
    assert_eq!(
        before, after,
        "a focus round trip must leave the screen exactly as it was"
    );
    let observation = harness.approve_execute_shell_second_request_from_visible_card();
    observation
        .check_inline_baseline()
        .expect("repainting must not change the product facts");
}

#[test]
fn focus_change_repaints_inline_card_in_place() {
    let core = real_cosh_core_path();
    assert!(core.exists(), "real cosh-core must be built: {core:?}");
    let mut harness = RealCoreApprovalHarness::spawn(SliceLaunch::inline());
    assert!(
        harness.request_approval_card(),
        "inline card must be visible before moving focus"
    );
    let (before, after) = harness.focus_change_round_trip(Duration::from_secs(15));
    assert_eq!(
        before, after,
        "an inline focus round trip must leave the screen exactly as it was"
    );
}

#[test]
fn a_row_resize_relinquishes_the_worker_surface_leaving_one_card() {
    let core = real_cosh_core_path();
    assert!(core.exists(), "real cosh-core must be built: {core:?}");
    let mut harness = RealCoreApprovalHarness::spawn(SliceLaunch::activated());
    assert!(
        harness.request_approval_card(),
        "worker card must be visible before resizing"
    );
    assert_eq!(
        harness
            .worker_descendants_within(Duration::from_secs(5))
            .len(),
        1,
        "the card must be worker-rendered for this to test the slice"
    );
    // A resize invalidates the recorded rectangle. The rows it names may now hold
    // the user's output, so nothing may be erased there: the surface is relinquished
    // untouched, the old viewport goes into scrollback, and the same approval is
    // redrawn inline as the only visible card.
    assert!(
        harness.resize_rows_and_expect_one_inline_card(30, Duration::from_secs(20)),
        "a row resize must leave exactly one visible card and no worker"
    );
    let observation = harness.approve_execute_shell_second_request_from_visible_card();
    observation
        .check_inline_baseline()
        .expect("a resize fallback must not change the product facts");
}

#[test]
fn a_same_size_resize_notification_relinquishes_the_worker_surface() {
    let mut harness = RealCoreApprovalHarness::spawn(SliceLaunch::activated());
    assert!(harness.request_approval_card());
    assert_eq!(
        harness
            .worker_descendants_within(Duration::from_secs(5))
            .len(),
        1
    );
    assert!(harness.resize_rows_and_expect_one_inline_card(24, Duration::from_secs(20)));
    harness
        .approve_execute_shell_second_request_from_visible_card()
        .check_inline_baseline()
        .expect("a same-size notification must rebuild and resolve once");
}

#[test]
fn a_rebuilt_card_keeps_deny_focused_for_the_next_enter() {
    let mut harness = RealCoreApprovalHarness::spawn(SliceLaunch::activated());
    assert!(harness.request_approval_card());
    assert!(harness.select_deny_and_wait());
    assert!(harness.resize_rows_and_expect_one_inline_card(30, Duration::from_secs(20)));
    let observation = harness.submit_with_enter_and_observe_noneffect();
    assert!(
        observation.audit.iter().any(|line| {
            serde_json::from_str::<serde_json::Value>(line).is_ok_and(|event| {
                event["event_type"] == "approval.resolved" && event["data"]["decision"] == "denied"
            })
        }),
        "fresh Enter must record Denied, not merely avoid executing"
    );
    observation
        .check_denied_slice()
        .expect("the visible Deny focus must make a fresh Enter deny without effects");
}

#[test]
fn narrowing_below_the_surface_width_leaves_a_visible_inline_card() {
    let core = real_cosh_core_path();
    assert!(core.exists(), "real cosh-core must be built: {core:?}");
    let mut harness = RealCoreApprovalHarness::spawn(SliceLaunch::activated());
    assert!(
        harness.request_approval_card(),
        "worker card must be visible before narrowing"
    );
    assert_eq!(
        harness
            .worker_descendants_within(Duration::from_secs(5))
            .len(),
        1,
        "the card must be worker-rendered for this to test the slice"
    );
    // One column short of the 40-column surface. The gate must refuse to
    // re-acquire, the slice must disable itself, and the inline card must take
    // over in the same pass rather than leaving the approval erased.
    assert!(
        harness.narrow_below_surface_and_card_is_visible(39, Duration::from_secs(20)),
        "narrowing below the surface width must leave a visible inline card"
    );
    let (before, after) = harness.focus_change_round_trip(Duration::from_secs(20));
    assert_eq!(
        before, after,
        "a narrow inline card must repaint without scrolling"
    );
    let observation = harness.approve_execute_shell_second_request_from_visible_card();
    observation
        .check_inline_baseline()
        .expect("falling back inline must not change the product facts");
}

#[test]
fn real_core_inline_approval_slice_baseline() {
    let core = real_cosh_core_path();
    assert!(core.exists(), "real cosh-core must be built: {core:?}");
    let mut harness = RealCoreApprovalHarness::spawn(SliceLaunch::inline());
    let observation = harness.run_approve_execute_shell_second_request();
    observation
        .check_inline_baseline()
        .expect("AB-00 inline baseline");
}

#[test]
fn ab06_oracle_rejects_script_core() {
    let script = write_script_core_for_mutation();
    let err = RealCoreApprovalHarness::spawn(SliceLaunch {
        core: CoreChoice::ScriptForMutation(script),
        ..SliceLaunch::inline()
    })
    .run_approve_execute_shell_second_request()
    .check_inline_baseline()
    .unwrap_err();
    assert!(matches!(err, OracleError::CoreNotElf), "{err:?}");
}

/// `recommend` is the MVP display-only policy (`agent/governance.rs:52` strips
/// execution when `policy.recommend_only`), so a high-risk tool request never
/// reaches a card and the terminal-state guarantee denies it. Verified
/// 2026-10-09: audit shows `approval.requested` then `approval.dropped` with
/// `reason_code=batch_drain` and no card on screen. The oracle must report that
/// as a missing card rather than certifying the run.
#[test]
fn ab06_oracle_rejects_display_only_mode() {
    let err = RealCoreApprovalHarness::spawn(SliceLaunch {
        approval_mode: "recommend",
        ..SliceLaunch::inline()
    })
    .run_approve_execute_shell_second_request()
    .check_inline_baseline()
    .unwrap_err();
    assert!(matches!(err, OracleError::ApprovalCardMissing), "{err:?}");
}

#[test]
fn ab06_oracle_rejects_missing_host_result() {
    let err = RealCoreApprovalHarness::spawn(SliceLaunch {
        server_script: ServerScript::OmitHostResult,
        ..SliceLaunch::inline()
    })
    .run_approve_execute_shell_second_request()
    .check_inline_baseline()
    .unwrap_err();
    assert!(matches!(err, OracleError::HostResultMissing), "{err:?}");
}

/// Repeated invalidation must leave the approval resolvable exactly once.
///
/// Each resize invalidates the presentation, rebuilds the same approval inline and
/// retires the capture generation the previous card was presented under. What a
/// product test can pin deterministically is the outcome the user depends on: after
/// any number of invalidations exactly one card is visible, no worker survives, the
/// rebuilt card still takes focus keys, and one Enter resolves the approval exactly
/// once — the command runs once, the audit records one resolution, and the Bash
/// facts are unchanged.
///
/// The focus round trip after the first fallback is what makes this discriminating
/// rather than merely descriptive: a cutover that wedged the mode — parking it in
/// `Hold`, or draining it the way `abandon_active_capture` does, where a later
/// `CaptureInput` for the same capture is ignored — would leave the rebuilt card
/// deaf to arrow keys and fail here.
///
/// What this layer cannot pin is the *rejection* of a keystroke read against a
/// retired generation. Whether a given byte is stamped before or after the cutover
/// is decided by when the reader thread returns from its blocking read, which the
/// test does not control: an Enter written after the fallback has completed is
/// stamped with the new generation and is correctly accepted. An earlier version of
/// this case asserted that rejection and failed identically before and after the
/// cutover existed, so it was never evidence of anything. The rejection half is
/// proven where the read can be held open across the cutover — see
/// `shell_host::implementation::raw_relay::tests` controlled-read fixture.
#[test]
fn repeated_resize_fallback_leaves_the_approval_resolvable_exactly_once() {
    let core = real_cosh_core_path();
    assert!(core.exists(), "real cosh-core must be built: {core:?}");
    let mut harness = RealCoreApprovalHarness::spawn(SliceLaunch::activated());
    assert!(
        harness.request_approval_card(),
        "worker card must be visible before resizing"
    );
    assert_eq!(
        harness
            .worker_descendants_within(Duration::from_secs(5))
            .len(),
        1,
        "the card must be worker-rendered for this to test the slice"
    );

    let (before, after) = harness.focus_change_round_trip(Duration::from_secs(20));
    assert_eq!(
        before, after,
        "the worker card must settle a focus round trip before it is invalidated"
    );

    assert!(
        harness.resize_rows_and_expect_one_inline_card(30, Duration::from_secs(20)),
        "the first row resize must leave exactly one visible inline card and no worker"
    );

    let (rebuilt, resettled) = harness.focus_change_round_trip(Duration::from_secs(20));
    assert_eq!(
        rebuilt, resettled,
        "the rebuilt inline card must still accept focus keys and settle"
    );

    assert!(
        harness.resize_rows_and_expect_one_inline_card(26, Duration::from_secs(20)),
        "shrinking rows after fallback must rebuild exactly one visible card"
    );

    let observation = harness.approve_execute_shell_second_request_from_visible_card();
    observation
        .check_inline_baseline()
        .expect("a twice-rebuilt card must still approve exactly once");
}

/// A worker fault and a resize must not compound into a lost or duplicated card.
///
/// Both invalidate the presentation independently: the kill through the pump, the
/// resize through the geometry check. Crossing them is what shows the fallback is
/// idempotent, because the second invalidation lands while the first is still being
/// handled and the approval must survive both — one visible card, no worker left,
/// and exactly one resolution.
#[test]
fn a_worker_kill_crossed_with_a_resize_still_resolves_the_approval_once() {
    let core = real_cosh_core_path();
    assert!(core.exists(), "real cosh-core must be built: {core:?}");
    let mut harness = RealCoreApprovalHarness::spawn(SliceLaunch::activated());
    assert!(
        harness.request_approval_card(),
        "worker card must be visible before it is faulted"
    );
    let victim = harness.fault_worker(WorkerFault::Kill);

    assert!(
        harness.resize_rows_and_expect_one_inline_card(30, Duration::from_secs(20)),
        "a kill crossed with a resize must still leave exactly one visible card"
    );
    assert!(
        harness.wait_for_pid_gone(victim, Duration::from_secs(10)),
        "the killed worker must be reaped"
    );

    let observation = harness.approve_execute_shell_second_request_from_visible_card();
    observation
        .check_inline_baseline()
        .expect("a fault crossed with a resize must still approve exactly once");
}
