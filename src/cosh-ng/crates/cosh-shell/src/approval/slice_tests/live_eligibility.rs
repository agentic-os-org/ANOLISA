use super::*;

struct ViewportReset(Option<(u16, u16)>);

impl Drop for ViewportReset {
    fn drop(&mut self) {
        crate::approval::slice_present::set_test_viewport(self.0);
    }
}

struct FailedPublication {
    reset: CaptureRepresentationReset,
    invalidated_before_flush: bool,
    bytes: Vec<u8>,
}

impl std::io::Write for FailedPublication {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.invalidated_before_flush = self.reset.consume();
        Err(std::io::Error::other(
            "eligibility replacement flush failed",
        ))
    }
}

#[test]
fn live_slice_eligibility_changes_retire_before_republishing_the_same_approval() {
    let _restore = ViewportReset(crate::approval::slice_present::set_test_viewport(Some((
        32, 80,
    ))));
    let (mut stable, _receiver) = live_idle_closed_tty_state();
    let stable_revision = stable.approval_slice.revision;
    assert!(
        crate::approval::slice_present::present_via_worker_slice(&mut stable, &mut Vec::new())
            .unwrap()
    );
    assert!(crate::approval::slice_present::slice_surface_is_live(
        &stable
    ));
    assert!(!stable.approval_slice.capture_representation_reset.consume());
    assert_eq!(stable.approval_slice.revision, stable_revision);
    let (activation, mut first_draw, _request, _receiver) = eligible();
    first_draw.approval_slice.activation = Some(activation);
    first_draw.approvals.requests[0].risk = "low";
    assert!(!crate::approval::slice_present::present_via_worker_slice(
        &mut first_draw,
        &mut Vec::new()
    )
    .unwrap());
    assert!(!first_draw
        .approval_slice
        .capture_representation_reset
        .consume());
    assert!(!first_draw.approval_slice.geometry_required);
    let (_activation, mut legacy, _request, _receiver) = eligible();
    assert!(legacy.approval_slice.activation.is_none());
    relinquish_untrusted_and_render_inline(&mut legacy, &mut Vec::new()).unwrap();
    assert!(legacy.approvals.active_panel_id.is_some());
    assert!(legacy.approval_slice.inline_geometry.is_none());
    assert!(!legacy.approval_slice.geometry_required);
    assert_startup_failure_preserves_the_shell_viewport();

    let mut failures = Vec::new();
    for path in ["present", "repaint"] {
        for change in [
            "second-pending",
            "second-denied",
            "low-risk",
            "hook-kind",
            "activation-removed",
            "disabled",
        ] {
            crate::approval::slice_present::set_test_viewport(Some((32, 80)));
            let (mut state, _receiver) = live_idle_closed_tty_state();
            state
                .approvals
                .focus
                .insert(APPROVAL_ID.into(), ApprovalPanelAction::Deny);
            let old = approval_card_view(
                1,
                &state,
                &state.approvals.requests[0],
                ApprovalPanelAction::Deny,
                false,
            )
            .unwrap();
            assert_eq!(
                old.actions,
                [
                    ApprovalActionView::Approve,
                    ApprovalActionView::Deny,
                    ApprovalActionView::Details
                ]
            );
            let mut buffer =
                ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(0, 0, 40, 24));
            crate::ui::approval_surface::render_approval_view(
                &crate::ui::worker_protocol::RenderView::ApprovalCard(old),
                &mut buffer,
            );
            let visible: String = buffer.content().iter().map(|cell| cell.symbol()).collect();
            assert!(visible.contains("> [ Deny ]"), "{visible}");
            let expected_set = match change {
                "second-pending" | "second-denied" => {
                    let mut second = eligible_request();
                    second.id = "req-2".into();
                    second.request_id = Some("ctrl-2".into());
                    second.tool_use_id = Some("toolu-2".into());
                    if change == "second-denied" {
                        second.status = ApprovalRequestStatus::Denied;
                    }
                    state.approvals.requests.push(second);
                    ApprovalActionSet::TurnConsentHighRisk
                }
                "low-risk" => {
                    state.approvals.requests[0].risk = "low";
                    ApprovalActionSet::Standard
                }
                "hook-kind" => {
                    state.approvals.requests[0].subject = "HOOK: fixture".into();
                    ApprovalActionSet::Hook
                }
                "activation-removed" => {
                    state.approval_slice.activation = None;
                    ApprovalActionSet::StandardHighRisk
                }
                "disabled" => {
                    state.approval_slice.disabled = true;
                    ApprovalActionSet::StandardHighRisk
                }
                _ => unreachable!(),
            };
            assert!(
                matches!(crate::runtime::controller::pending_card_capture(&state), Some(RawInputCapture::Approval { ref id, action_set }) if id == APPROVAL_ID && action_set == expected_set)
            );
            let mut output = FailedPublication {
                reset: state.approval_slice.capture_representation_reset.clone(),
                invalidated_before_flush: false,
                bytes: Vec::new(),
            };
            let result = if path == "present" {
                crate::approval::slice_present::present_via_worker_slice(&mut state, &mut output)
            } else {
                crate::approval::slice_present::repaint_worker_slice(&mut state, &mut output)
            };
            if !matches!(result, Err(ref error) if error.to_string() == "eligibility replacement flush failed")
                || !output.invalidated_before_flush
                || state.approval_slice.session.is_some()
                || state.approvals.active_panel_id.is_some()
            {
                failures.push(format!("{path}/{change}: result={result:?}, invalidated_before_flush={}, live={}, claim={:?}", output.invalidated_before_flush, crate::approval::slice_present::slice_surface_is_live(&state), state.approvals.active_panel_id));
                continue;
            }
            assert_live_io_failure_retires_claim(&mut state, ApprovalRequestStatus::Pending);
            assert_eq!(
                state.approvals.focus.get(APPROVAL_ID),
                Some(&ApprovalPanelAction::Deny)
            );
            let input_mode = install_approval_capture();
            let old_generation = live_capture_generation(&input_mode);
            assert!(retire_capture_for_representation(
                &mut input_mode.lock().unwrap()
            ));
            if change == "activation-removed" {
                crate::approval::slice_present::set_test_viewport(Some((32, 39)));
            }
            let mut inline = Vec::new();
            if path == "present" && change == "activation-removed" {
                crate::approval::panel::render_current_approval_request(&mut state, &mut inline)
                    .unwrap();
            } else {
                relinquish_untrusted_and_render_inline(&mut state, &mut inline).unwrap();
            }
            if change == "activation-removed" {
                assert!(inline.starts_with(b"\x18\x1b[32;1H"), "{path}: retry must establish a fresh viewport before the replacement card: {inline:?}");
            }
            let rendered = String::from_utf8(inline).unwrap();
            if change == "activation-removed" {
                assert_eq!(
                    state.approval_slice.inline_geometry,
                    Some((32, 39)),
                    "{path}: retry after failed flush must retain managed geometry"
                );
                let height = state.approvals.active_panel_height;
                assert!(height > 2 && height < 32);
                let start = rendered.lines().count().checked_sub(height).unwrap();
                for line in rendered.lines().skip(start) {
                    assert!(
                        crate::ui::agent_render::display_width(line.trim_end_matches('\r')) <= 39,
                        "{line:?}"
                    );
                }
                assert!(state.approval_slice.capture_representation_reset.consume());
                crate::approval::slice_present::set_test_viewport(Some((48, 47)));
                let mut resized = Vec::new();
                crate::approval::slice_present::repaint_worker_slice_if_viewport_changed(
                    &mut state,
                    &mut resized,
                )
                .unwrap();
                assert_eq!(state.approval_slice.inline_geometry, Some((48, 47)));
                assert!(resized.starts_with(b"\x18\x1b[48;1H"));
                assert!(state.approval_slice.capture_representation_reset.consume());
            }
            assert!(
                rendered.contains(
                    I18n::new(state.language).t(MessageId::ApprovalPresentationInvalidatedNotice)
                ),
                "{rendered}"
            );
            if expected_set == ApprovalActionSet::TurnConsentHighRisk {
                assert!(rendered.contains("Allow all this turn"), "{rendered}");
                assert_eq!(
                    expected_set.action_at(1),
                    Some(ApprovalPanelAction::ApproveTurn)
                );
                assert_ne!(
                    expected_set.action_index(ApprovalPanelAction::Deny),
                    Some(1)
                );
            }
            let capture = crate::runtime::controller::pending_card_capture(&state).unwrap();
            update_input_mode(&input_mode, &RawObserverAction::CaptureInput(capture), None);
            assert_ne!(live_capture_generation(&input_mode), old_generation);
            assert!(state.approvals.journal.is_empty());
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
    assert_tracked_inline_stays_bound_without_activation();
}

fn assert_startup_failure_preserves_the_shell_viewport() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().unwrap();
    let unavailable = directory.path().join("unavailable-worker");
    let unexecutable = directory.path().join("unexecutable-worker");
    std::fs::write(&unexecutable, b"not an executable").unwrap();
    std::fs::set_permissions(&unexecutable, std::fs::Permissions::from_mode(0o600)).unwrap();
    for worker_path in [unavailable, unexecutable] {
        let (mut activation, mut state, request, _receiver) = eligible();
        activation.worker_path = worker_path;
        activation.failure_limit = 1;
        activation.handshake_timeout = Duration::from_millis(20);
        state.approval_slice.activation = Some(activation);
        let mut output = Vec::new();
        assert!(
            crate::approval::slice_present::present_via_worker_slice(&mut state, &mut output)
                .unwrap()
        );
        assert!(!output.contains(&0x18), "a worker that never acquired a surface must not scroll out the shell viewport: {output:?}");
        let rendered = String::from_utf8(output).unwrap();
        for label in ["Allow once", "Deny", "Details", "slice_effect"] {
            assert!(rendered.contains(label), "{rendered}");
        }
        assert!(state.approval_slice.session.is_none());
        assert!(state.approval_slice.disabled);
        assert_eq!(state.approval_slice.inline_geometry, Some((32, 80)));
        assert_eq!(state.approvals.active_panel_id, Some(request.id));
        assert_eq!(
            state.approvals.requests[0].status,
            ApprovalRequestStatus::Pending
        );
        assert!(state.approvals.journal.is_empty());
    }
}

fn assert_tracked_inline_stays_bound_without_activation() {
    crate::approval::slice_present::set_test_viewport(Some((32, 39)));
    let (_activation, mut state, request, _receiver) = eligible();
    state.approval_slice.disabled = true;
    state.approval_slice.inline_geometry = Some((32, 39));
    state.approvals.active_panel_id = Some(request.id.clone());
    state.approvals.active_panel_height = 8;
    assert!(state.approval_slice.activation.is_none());
    let mut output = Vec::new();
    relinquish_untrusted_and_render_inline(&mut state, &mut output).unwrap();
    assert_eq!(state.approval_slice.inline_geometry, Some((32, 39)));
    assert!(output.starts_with(b"\x18\x1b[32;1H"));
    let height = state.approvals.active_panel_height;
    assert!(height > 2 && height < 32);
    let text = String::from_utf8(output).unwrap();
    let start = text.lines().count().checked_sub(height).unwrap();
    for line in text.lines().skip(start) {
        assert!(
            crate::ui::agent_render::display_width(line.trim_end_matches('\r')) <= 39,
            "{line:?}"
        );
    }
    assert!(state.approval_slice.capture_representation_reset.consume());
    crate::approval::slice_present::set_test_viewport(Some((48, 47)));
    let mut resized = Vec::new();
    crate::approval::slice_present::repaint_worker_slice_if_viewport_changed(
        &mut state,
        &mut resized,
    )
    .unwrap();
    assert_eq!(state.approval_slice.inline_geometry, Some((48, 47)));
    assert!(resized.starts_with(b"\x18\x1b[48;1H"));
    assert!(state.approval_slice.capture_representation_reset.consume());
    struct FailedErase;
    impl std::io::Write for FailedErase {
        fn write(&mut self, _bytes: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("tracked erase failed"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let error = crate::approval::panel::clear_active_approval_panel(&mut state, &mut FailedErase)
        .unwrap_err();
    assert_eq!(error.to_string(), "tracked erase failed");
    assert!(state.approval_slice.capture_representation_reset.consume());
    assert!(state.approvals.active_panel_id.is_none());
    assert!(state.approval_slice.inline_geometry.is_none());
    assert_eq!(
        state.approvals.requests[0].status,
        ApprovalRequestStatus::Pending
    );
    assert!(state.approvals.journal.is_empty());
    assert!(state.approval_slice.fresh_region_required);
    state.approvals.requests[0].status = ApprovalRequestStatus::Denied;
    let mut no_pending = Vec::new();
    crate::approval::panel::render_current_approval_request(&mut state, &mut no_pending).unwrap();
    assert!(no_pending.starts_with(b"\x18\x1b[48;1H"));
    assert!(!state.approval_slice.fresh_region_required);
    assert!(state.approvals.active_panel_id.is_none());
    state.approvals.requests[0].status = ApprovalRequestStatus::Pending;
    let mut next = Vec::new();
    crate::approval::panel::render_current_approval_request(&mut state, &mut next).unwrap();
    assert!(!next.contains(&0x18));
    assert!(state.approvals.active_panel_id.is_some());
    assert!(!state.approval_slice.fresh_region_required);
}
