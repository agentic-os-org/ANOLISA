//! Glue between the eligibility decision and the two inline presentation calls.
//!
//! Kept out of `approval/panel.rs` so the inline renderer stays readable and so
//! every slice-specific failure has one place to become a documented return to
//! the inline path. Nothing here mutates approval business state: the decision,
//! journal, audit and handoff all stay on the path that already exists.

use std::io::Write;

use crate::approval::slice::approval_slice_eligibility;
use crate::approval::surface_session::TtyApprovalSurfaceSession;
use crate::runtime::approval_gate::{
    encode_viewport_scrollout, GeometryTrust, SurfaceTerminal, TtySurface,
};
use crate::runtime::prelude::*;

/// Presents the first pending approval through the worker, if the slice allows it.
///
/// Returns `true` after a worker frame or its inline replacement is fully drawn.
/// Ineligible requests return `false` for the caller to render inline.
pub(crate) fn present_via_worker_slice<W: Write>(
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<bool> {
    present(state, output, false).inspect_err(|_| retire_slice_after_io_failure(state))
}

/// Repaints a card that is already on a live slice surface.
///
/// A focus change or a details toggle arrives through
/// `redraw_current_approval_request`, which would otherwise withdraw the
/// rectangle first. Withdrawing and re-acquiring scrolls `height` rows of the
/// user's visible history into scrollback on every arrow key, which the inline
/// path never did, so the redraw keeps the rectangle and paints over it.
pub(crate) fn repaint_worker_slice<W: Write>(
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<bool> {
    if !slice_surface_is_live(state) {
        return Ok(false);
    }
    present(state, output, true).inspect_err(|_| retire_slice_after_io_failure(state))
}

/// Relinquishes a live slice surface after the terminal changed.
///
/// The question panel already redraws on a width change from the same observer
/// pass; this is the slice equivalent. A *stable* viewport keeps the rectangle and
/// does nothing here, so an ordinary focus change never scrolls. Any observed
/// change invalidates the recorded coordinates — including one that ends back at
/// the original size, because the rows they name have still had content scrolled
/// and reflowed through them.
///
/// An unreadable viewport is treated the same way rather than as "no change": a
/// caller that is about to write cannot prove the old region is still theirs.
pub(crate) fn repaint_worker_slice_if_viewport_changed<W: Write>(
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    let resized = state
        .approval_slice
        .capture_representation_reset
        .take_resize();
    let trust = {
        if let Some(session) = state.approval_slice.session.as_mut() {
            if !session.is_live() {
                return Ok(());
            }
            if resized {
                GeometryTrust::Changed
            } else {
                session.geometry_trust()
            }
        } else if state.approval_slice.disabled
            && (state.approval_slice.geometry_required
                || state.approval_slice.activation.is_some()
                || state.approval_slice.inline_geometry.is_some())
            && state.approvals.active_panel_id.is_some()
        {
            match current_viewport() {
                Ok(size) if !resized && state.approval_slice.inline_geometry == Some(size) => {
                    GeometryTrust::Trusted
                }
                Ok(_) => GeometryTrust::Changed,
                Err(_) => GeometryTrust::Unknown,
            }
        } else {
            return Ok(());
        }
    };
    match trust {
        GeometryTrust::Trusted => Ok(()),
        GeometryTrust::Changed | GeometryTrust::Unknown => {
            relinquish_untrusted_and_render_inline(state, output)
        }
    }
}

/// Gives up an untrusted surface without erasing it, then draws the same approval
/// inline on a region that provably holds nothing else.
///
/// Nothing is erased at the recorded rectangle: after a reflow those rows may be
/// the user's output, and destroying it to tidy up a card is the worse failure. The
/// old viewport is pushed into scrollback instead, which keeps the invalidated card
/// readable as history while leaving exactly one card — the new inline one — in the
/// visible viewport. The worker is retired and the slice stays disabled, so no later
/// pass tries the worker again for this approval.
///
/// Approval business facts are untouched: the request, its run, provider ids,
/// Pending status, focus and expansion all survive, and only the presentation claim
/// is cleared so the inline path will draw.
///
/// Crate-visible for tests: signalling the representation reset is the link between
/// a rebuilt card and the retired capture generation, and it has to be assertable
/// without a real tty.
pub(crate) fn relinquish_untrusted_and_render_inline<W: Write>(
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    let mut scrolled_viewport = None;
    invalidate_representation(state);
    let scroll_result = if let Some(session) = state.approval_slice.session.as_mut() {
        let result = if state.approval_slice.fresh_region_required {
            if let Err(error) = output.flush() {
                retire_slice_after_io_failure(state);
                return Err(error);
            }
            current_viewport().and_then(|size| {
                scrolled_viewport = Some(size);
                session
                    .push_viewport_into_scrollback()
                    .map_err(std::io::Error::other)
            })
        } else {
            Ok(())
        };
        session.relinquish_untrusted();
        result
    } else {
        Ok(())
    };
    state.approval_slice.session = None;
    state.approval_slice.disabled = true;
    clear_panel_claim(state);
    scroll_result?;
    output.flush()?;
    let fresh_viewport = if state.approval_slice.geometry_required {
        let size = current_viewport()?;
        if scrolled_viewport.is_some_and(|before| before != size) {
            return Err(std::io::Error::other("viewport changed during scrollout"));
        }
        if state.approval_slice.fresh_region_required && scrolled_viewport.is_none() {
            output.write_all(&encode_viewport_scrollout(size.0))?;
            output.flush()?;
            if current_viewport()? != size {
                return Err(std::io::Error::other("viewport changed during scrollout"));
            }
        }
        Some(size)
    } else {
        None
    };
    if !state
        .approvals
        .requests
        .iter()
        .any(|request| request.status == ApprovalRequestStatus::Pending)
    {
        output.flush()?;
        state.approval_slice.fresh_region_required = false;
        return Ok(());
    }
    state.approval_slice.fresh_region_required = false;
    let publication = (|| {
        let notice = state
            .i18n()
            .t(crate::MessageId::ApprovalPresentationInvalidatedNotice);
        writeln!(output, "{notice}")?;
        state.approval_slice.inline_geometry = fresh_viewport;
        crate::approval::panel::render_current_approval_request(state, output)?;
        // Capture can resume only after the replacement has been fully flushed.
        if fresh_viewport.is_none() {
            output.flush()?;
        }
        Ok(())
    })();
    if let Err(error) = publication {
        if state.approval_slice.geometry_required {
            state.approval_slice.fresh_region_required = true;
            invalidate_representation(state);
        }
        return Err(error);
    }
    state.approval_slice.fresh_region_required = false;
    Ok(())
}

#[cfg(test)]
thread_local! {
    static TEST_VIEWPORT: std::cell::Cell<Option<(u16, u16)>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(crate) fn set_test_viewport(size: Option<(u16, u16)>) -> Option<(u16, u16)> {
    TEST_VIEWPORT.with(|viewport| viewport.replace(size))
}

pub(crate) fn current_viewport() -> std::io::Result<(u16, u16)> {
    #[cfg(test)]
    if let Some(size) = TEST_VIEWPORT.with(std::cell::Cell::get) {
        return Ok(size);
    }
    let size = TtySurface::stdout().winsize()?;
    if size.ws_row == 0 || size.ws_col < 20 {
        return Err(std::io::Error::other(
            "terminal cannot contain an approval card",
        ));
    }
    Ok((size.ws_row, size.ws_col))
}

pub(crate) fn invalidate_representation(state: &mut InlineState) {
    state.approval_slice.fresh_region_required |= state.approval_slice.inline_geometry.is_some()
        || state
            .approval_slice
            .session
            .as_ref()
            .is_some_and(|session| session.region_needs_repair(None));
    state.approval_slice.geometry_required |= state.approval_slice.activation.is_some()
        || slice_surface_is_live(state)
        || state.approval_slice.inline_geometry.is_some();
    if let Some((id, action)) = state.approval_slice.observed_focus.take() {
        if crate::approval::panel::approval_is_pending(state, &id) {
            state.approvals.focus.insert(id, action);
        }
    }
    state.approval_slice.capture_representation_reset.signal();
    state.approval_slice.invalidated_card_events_before = state
        .approval_slice
        .invalidated_card_events_before
        .max(state.approval_slice.observer_event_end);
    clear_panel_claim(state);
}

/// Whether a slice surface is currently painted.
pub(crate) fn slice_surface_is_live(state: &InlineState) -> bool {
    state
        .approval_slice
        .session
        .as_ref()
        .is_some_and(|session| session.is_live())
}

fn present<W: Write>(
    state: &mut InlineState,
    output: &mut W,
    force: bool,
) -> std::io::Result<bool> {
    if state.approval_slice.disabled || state.approval_slice.activation.is_none() {
        if slice_surface_is_live(state) {
            relinquish_untrusted_and_render_inline(state, output)?;
            return Ok(true);
        }
        return Ok(false);
    }
    // The same selection rule the inline path uses, so the slice can never pick a
    // different request than the one the inline card would have shown.
    let Some(index) = state
        .approvals
        .requests
        .iter()
        .position(|request| request.status == ApprovalRequestStatus::Pending)
    else {
        // Nothing left to present, so this is the end of the interaction rather
        // than a redraw: retire the worker here instead of on every withdraw,
        // which would otherwise pay a shutdown wait per focus change.
        retire_idle_session(state, output)?;
        return Ok(false);
    };
    // Cloned so the eligibility check and the session can both borrow `state`
    // mutably without holding a reference into `state.approvals.requests`.
    let request = state.approvals.requests[index].clone();

    let activation = state
        .approval_slice
        .activation
        .clone()
        .expect("activation was checked above");
    let revision = state.approval_slice.revision + 1;
    let eligible = match approval_slice_eligibility(Some(&activation), state, &request, revision) {
        Ok(eligible) => eligible,
        Err(_) if slice_surface_is_live(state) => {
            relinquish_untrusted_and_render_inline(state, output)?;
            return Ok(true);
        }
        Err(_) => return Ok(false),
    };
    state.approval_slice.geometry_required = true;
    let already_presented = !force
        && state.approvals.active_panel_id.as_deref() == Some(request.id.as_str())
        && slice_surface_is_live(state);
    if already_presented {
        return Ok(true);
    }
    state.approval_slice.revision = revision;

    // The inline path clears the agent spinner before drawing a card. The slice
    // returns from `render_current_approval_request` before reaching that clear,
    // and a published frame parks the cursor on the sentinel row, which is inside
    // the rectangle, so a spinner left marked visible would later be cleared over
    // the card's bottom border with no repaint until the next keypress.
    if let Some(active_run) = state.agent_run.active.as_mut() {
        active_run.status_animation.clear(output)?;
    }

    // The gate writes straight to the tty with no userspace buffer, while the
    // inline path writes through a line-buffered stdout. Flushing first is what
    // keeps already-produced presentation bytes ahead of the surface bytes.
    output.flush()?;

    if state.approval_slice.session.is_none() {
        match TtyApprovalSurfaceSession::start_tty(activation, eligible.action_key.clone()) {
            Ok(session) => state.approval_slice.session = Some(Box::new(session)),
            Err(error) => {
                // One line at the choke point, carrying the origin: the shell owns
                // the terminal, so the log file is the only durable evidence.
                tracing::warn!(
                    error = %error,
                    worker = %eligible.action_key.approval_id,
                    "approval slice worker unavailable, staying on the inline path"
                );
                relinquish_untrusted_and_render_inline(state, output)?;
                return Ok(true);
            }
        }
    }
    let session = state
        .approval_slice
        .session
        .as_mut()
        .expect("session was just installed");
    if let Err(error) = session.present(eligible.card) {
        state.approval_slice.fresh_region_required |= session.region_needs_repair(Some(&error));
        tracing::warn!(
            error = %error,
            approval = %request.id,
            "approval slice presentation failed, staying on the inline path"
        );
        relinquish_untrusted_and_render_inline(state, output)?;
        return Ok(true);
    }

    state.approvals.active_panel_id = Some(request.id.clone());
    state.approval_slice.fresh_region_required = false;
    // The inline erase path is height-driven; the gate owns the rectangle, so the
    // height stays zero and `clear_active_approval_panel` takes the slice branch.
    state.approvals.active_panel_height = 0;
    Ok(true)
}

/// Drops the claim that a card is already on screen.
///
/// `render_current_approval_request` returns early when the active panel id
/// matches the pending request, which is right while a surface is painted and
/// wrong the moment one has been erased, so every path that erases without
/// repainting has to release the claim.
fn clear_panel_claim(state: &mut InlineState) {
    state.approvals.active_panel_id = None;
    state.approvals.active_panel_height = 0;
    state.approval_slice.inline_geometry = None;
}

/// Releases a live slice surface, erasing the rectangle so following output
/// continues where the card was.
///
/// Returns `true` when a slice surface was released and the caller must not run
/// the inline cursor-relative erase, which would move the cursor by a height the
/// slice never recorded.
pub(crate) fn release_worker_slice<W: Write>(
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<bool> {
    let Some(session) = state.approval_slice.session.as_mut() else {
        return Ok(false);
    };
    if !session.is_live() {
        return Ok(false);
    }
    if let Err(error) = output.flush() {
        retire_slice_after_io_failure(state);
        return Err(error);
    }
    if let Err(error) = session.withdraw() {
        tracing::warn!(error = %error, "approval slice withdraw failed");
        retire_slice_after_io_failure(state);
        return Err(std::io::Error::other(error));
    }
    // The session survives: a focus change or a details toggle redraws the same
    // approval, and reusing the worker keeps that a repaint instead of a spawn.
    clear_panel_claim(state);
    Ok(true)
}

fn retire_slice_after_io_failure(state: &mut InlineState) {
    invalidate_representation(state);
    if let Some(session) = state.approval_slice.session.as_mut() {
        session.relinquish_untrusted();
    }
    state.approval_slice.session = None;
    state.approval_slice.disabled = true;
}

/// Shuts the worker down once no approval is left to present.
fn retire_idle_session<W: Write>(state: &mut InlineState, output: &mut W) -> std::io::Result<()> {
    let Some(session) = state.approval_slice.session.as_mut() else {
        return Ok(());
    };
    if let Err(error) = output.flush() {
        retire_slice_after_io_failure(state);
        return Err(error);
    }
    if session.geometry_trust() == GeometryTrust::Trusted {
        if let Err(error) = session.release(false) {
            tracing::warn!(error = %error, "approval slice release failed");
            retire_slice_after_io_failure(state);
            return Err(std::io::Error::other(error));
        }
    } else {
        // Withdrawing would erase at an anchor that may now name the user's
        // output, so the rectangle is given up untouched.
        session.relinquish_untrusted();
    }
    state.approval_slice.session = None;
    Ok(())
}

/// Services the worker while a card is on screen, and falls back to inline if it
/// stops answering.
///
/// Called from the relay observer, which runs on every loop iteration, so a dead
/// or hung worker is noticed within its heartbeat timeout instead of leaving a
/// card on screen that nothing is rendering.
pub(crate) fn pump_worker_slice<W: Write>(
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    let Some(session) = state.approval_slice.session.as_mut() else {
        return Ok(());
    };
    // A pump can commit a rebuilt worker's frame straight to the tty, so anything
    // the line-buffered inline writer is still holding has to land first or it
    // overprints the frame at the sentinel row.
    if let Err(error) = output.flush() {
        retire_slice_after_io_failure(state);
        return Err(error);
    }
    let error = match session.pump() {
        Ok(()) => return Ok(()),
        Err(error) => error,
    };
    let approval = session.action_key().approval_id.clone();
    // Flush before the gate erases, so buffered inline bytes cannot land after
    // the erasure and reappear inside the freed rectangle.
    if let Err(error) = output.flush() {
        retire_slice_after_io_failure(state);
        return Err(error);
    }
    tracing::warn!(
        error = %error,
        approval = %approval,
        "approval slice worker failed while its card was on screen; re-rendering inline"
    );
    relinquish_untrusted_and_render_inline(state, output)
}
