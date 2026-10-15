//! Worker-backed presentation of one approval card.
//!
//! This is the seam between the eligibility decision and the terminal: it spawns
//! the worker, asks it to paint the projection, publishes the frame into an
//! acquired rectangle, and releases the rectangle afterwards. It owns no approval
//! semantics — the decision, audit and handoff stay on the existing inline path,
//! which is why nothing here mutates `InlineState`.
//!
//! Two things are deliberately bounded rather than best-effort:
//!
//! - Every wait has a deadline taken from the activation policy. A worker that
//!   never completes its handshake, or never returns a frame, produces an error
//!   the caller turns into the inline path; the approval is never left waiting on
//!   a process that may be gone.
//! - The surface height comes from the same pure layout function the worker uses,
//!   and the worker's frame is then validated against that height. The worker
//!   cannot ask for more screen than the Host allocated.

use std::time::{Duration, Instant};

use crate::approval::slice::ApprovalActionKey;
use crate::raw_input::{InputRouter, InputSequence};
use crate::runtime::approval_gate::{
    ApprovalSurfaceGate, GateError, GeometryTrust, SurfaceTerminal, TtySurface,
};
use crate::runtime::approval_ui::ApprovalSliceActivation;
use crate::runtime::terminal_publisher::{PublishError, TerminalPublisher};
use crate::runtime::ui_coordinator::{
    CandidateIdentity, CoordinatorError, RecoveryMode, UiWorkerCoordinator, WorkerLifecycle,
};
use crate::ui::approval_surface::surface_rows_for;
use crate::ui::surface_frame::SurfaceRect;
use crate::ui::worker_protocol::{
    ApprovalCardView, PreparedFrame, RenderRequest, RenderView, SessionNonce, Viewport,
};

/// Poll cadence while waiting on the worker.
///
/// Short enough to notice a frame promptly, long enough that a blocked wait does
/// not spin a core. The relay loop wakes at least once a second on its own
/// maintenance interval, so [`ApprovalSurfaceSession::pump`] keeps heartbeats
/// alive between presentations without this loop running.
const POLL_INTERVAL: Duration = Duration::from_millis(2);

/// Why a worker-backed presentation could not be completed.
#[derive(Debug)]
pub(crate) enum SurfaceError {
    /// The coordinator refused a lifecycle or frame transition.
    Coordinator(CoordinatorError),
    /// The rectangle could not be acquired or released.
    Gate(GateError),
    /// The frame could not be published.
    Publish(PublishError),
    /// The worker never completed its Ready handshake within the policy bound.
    HandshakeTimeout,
    /// The worker never returned a prepared frame within the policy bound.
    RenderTimeout,
    /// The card needs more rows than the activation allows.
    TooTall {
        /// Rows the layout needs.
        rows: u16,
        /// Rows the activation permits.
        max: u16,
    },
    /// A release was requested with no live surface.
    NotLive,
    /// The worker delivered a frame while no rectangle was acquired, so there is
    /// nowhere to paint it.
    NoRectangle,
    /// The coordinator has spent its failure budget and will not recover, so the
    /// card on screen has no worker behind it.
    WorkerUnrecoverable,
    /// The acquired coordinates no longer describe the current terminal.
    GeometryInvalidated,
}

impl std::fmt::Display for SurfaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Coordinator(error) => write!(f, "worker coordinator: {error:?}"),
            Self::Gate(error) => write!(f, "surface gate: {error}"),
            Self::Publish(error) => write!(f, "frame publish: {error}"),
            Self::HandshakeTimeout => write!(f, "worker did not reach Ready in time"),
            Self::RenderTimeout => write!(f, "worker did not return a frame in time"),
            Self::TooTall { rows, max } => write!(f, "card needs {rows} rows, limit is {max}"),
            Self::NotLive => write!(f, "no live approval surface"),
            Self::NoRectangle => write!(f, "worker frame arrived with no acquired rectangle"),
            Self::WorkerUnrecoverable => write!(f, "worker exhausted its failure budget"),
            Self::GeometryInvalidated => write!(f, "approval surface geometry changed"),
        }
    }
}

impl std::error::Error for SurfaceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Gate(error) => Some(error),
            Self::Publish(error) => Some(error),
            _ => None,
        }
    }
}

/// One worker-backed approval presentation, from spawn to release.
///
/// The session owns its terminal through the gate, so no other writer can
/// interleave with a surface transaction.
pub(crate) struct ApprovalSurfaceSession<T: SurfaceTerminal> {
    activation: ApprovalSliceActivation,
    action_key: ApprovalActionKey,
    coordinator: UiWorkerCoordinator,
    gate: ApprovalSurfaceGate<T>,
    publisher: TerminalPublisher,
    router: InputRouter,
    sequence: InputSequence,
    live: bool,
}

/// A session writing to the real outer tty.
pub(crate) type TtyApprovalSurfaceSession = ApprovalSurfaceSession<TtySurface>;

impl TtyApprovalSurfaceSession {
    #[cfg(test)]
    pub(crate) fn idle_closed_tty_for_test(
        activation: ApprovalSliceActivation,
        action_key: ApprovalActionKey,
    ) -> Self {
        let nonce = [1; 16];
        let coordinator = UiWorkerCoordinator::new(
            CandidateIdentity {
                binary_path: activation.worker_path.clone(),
                candidate_version: env!("CARGO_PKG_VERSION").into(),
            },
            nonce,
            1,
            activation.coordinator_policy(),
        )
        .expect("valid idle coordinator fixture");
        let publisher = TerminalPublisher::new(
            Viewport {
                columns: activation.columns,
                rows: activation.max_surface_rows,
            },
            activation.limits,
        );
        let router = InputRouter::new(nonce, 1, 0, 0);
        let sequence = router.input_sequence();
        Self {
            activation,
            action_key,
            coordinator,
            gate: ApprovalSurfaceGate::claimed_closed_tty_for_test(SurfaceRect {
                anchor_row: 18,
                columns: 40,
                height: 12,
            }),
            publisher,
            router,
            sequence,
            live: true,
        }
    }

    /// Spawns the worker against the real terminal.
    ///
    /// # Errors
    /// Returns [`SurfaceError`] when the worker cannot be spawned or does not
    /// complete its handshake within the activation's bound.
    pub(crate) fn start_tty(
        activation: ApprovalSliceActivation,
        action_key: ApprovalActionKey,
    ) -> Result<Self, SurfaceError> {
        Self::start(activation, action_key, TtySurface::stdout())
    }
}

impl<T: SurfaceTerminal> ApprovalSurfaceSession<T> {
    /// Spawns the worker and waits for its Ready handshake.
    ///
    /// # Errors
    /// Returns [`SurfaceError::Coordinator`] when the worker cannot be spawned or
    /// the coordinator rejects a transition, or [`SurfaceError::HandshakeTimeout`]
    /// when Ready does not arrive within `activation.handshake_timeout`.
    pub(crate) fn start(
        activation: ApprovalSliceActivation,
        action_key: ApprovalActionKey,
        term: T,
    ) -> Result<Self, SurfaceError> {
        let nonce: SessionNonce = *uuid::Uuid::new_v4().as_bytes();
        let candidate = CandidateIdentity {
            binary_path: activation.worker_path.clone(),
            // Same-build admission: the worker must report the version it was
            // compiled from, and the coordinator rejects any other value.
            candidate_version: env!("CARGO_PKG_VERSION").to_owned(),
        };
        let policy = activation.coordinator_policy();
        let coordinator = UiWorkerCoordinator::new(candidate, nonce, 1, policy)
            .map_err(SurfaceError::Coordinator)?;
        let publisher = TerminalPublisher::new(
            Viewport {
                columns: activation.columns,
                rows: activation.max_surface_rows,
            },
            activation.limits,
        );
        let router = InputRouter::new(nonce, 1, 0, 0);
        let sequence = router.input_sequence();
        let mut session = Self {
            activation,
            action_key,
            coordinator,
            gate: ApprovalSurfaceGate::new(term),
            publisher,
            router,
            sequence,
            live: false,
        };
        let now = Instant::now();
        session
            .coordinator
            .start(now)
            .map_err(SurfaceError::Coordinator)?;
        session.wait_for_ready(now)?;
        Ok(session)
    }

    /// Frozen business identity of the approval this session presents.
    pub(crate) const fn action_key(&self) -> &ApprovalActionKey {
        &self.action_key
    }

    /// Whether a rectangle is currently acquired.
    pub(crate) const fn is_live(&self) -> bool {
        self.live
    }

    pub(crate) fn region_needs_repair(&self, error: Option<&SurfaceError>) -> bool {
        self.gate.rect().is_some()
            || matches!(error, Some(SurfaceError::Gate(GateError::Write { accepted, .. })) if *accepted > 0)
    }

    /// Acquires the rectangle and publishes one card into it.
    ///
    /// The rectangle is acquired before the render request, so a frame can only
    /// ever be painted into rows the Host already proved it owns. If publishing
    /// fails, the coordinator is told, and the caller must release.
    ///
    /// # Errors
    /// Returns [`SurfaceError::TooTall`] when the layout needs more rows than the
    /// activation allows, [`SurfaceError::Gate`] when the rectangle cannot be
    /// acquired, [`SurfaceError::RenderTimeout`] when no frame arrives, or
    /// [`SurfaceError::Publish`] / [`SurfaceError::Coordinator`] when the frame
    /// cannot be committed.
    pub(crate) fn present(&mut self, card: ApprovalCardView) -> Result<SurfaceRect, SurfaceError> {
        let view = RenderView::ApprovalCard(card);
        let rows = surface_rows_for(&view, self.activation.columns);
        if rows > self.activation.max_surface_rows {
            return Err(SurfaceError::TooTall {
                rows,
                max: self.activation.max_surface_rows,
            });
        }
        // Reuse a rectangle that already fits. Acquiring again would scroll
        // another `height` rows of the user's history into scrollback, and a
        // focus change redraws the same card at the same height, so an arrow key
        // would otherwise eat part of the screen every time it is pressed.
        let reusable = self.gate.rect().is_some() && self.rectangle_still_owns_the_bottom(rows)?;
        let rect = if reusable {
            self.gate.rect().expect("reuse implies a live rectangle")
        } else {
            if self.gate.rect().is_some() {
                if self.geometry_trust() != GeometryTrust::Trusted {
                    return Err(SurfaceError::GeometryInvalidated);
                }
                self.gate.withdraw().map_err(SurfaceError::Gate)?;
            }
            self.gate.acquire(rows).map_err(SurfaceError::Gate)?
        };
        let request = RenderRequest {
            capture_id: None,
            viewport: Viewport {
                columns: rect.columns,
                rows: rect.height,
            },
            render_view: view,
        };
        let now = Instant::now();
        self.coordinator
            .request_render(request, now)
            .map_err(SurfaceError::Coordinator)?;
        let frame = self.await_frame(now)?;
        self.commit(frame, rect).map(|()| rect)
    }

    /// Services heartbeats and pending writes while the card is on screen.
    ///
    /// Called from the relay loop, which iterates at least once per second, so a
    /// worker that stops answering is noticed within its heartbeat timeout
    /// instead of being assumed alive.
    ///
    /// A tick can also hand back a frame: after a crash or a hang the coordinator
    /// reaps the child, advances the generation and re-issues the render it was
    /// already holding, so the rebuilt worker repaints the same card. Dropping
    /// that frame would strand the coordinator waiting on a publish that never
    /// comes and burn the rest of the failure budget, so it is committed into the
    /// rectangle that is still acquired.
    ///
    /// # Errors
    /// Returns [`SurfaceError::Coordinator`] when the coordinator observes a
    /// failure it cannot recover from, [`SurfaceError::NoRectangle`] when a frame
    /// arrives with nothing to paint into, or [`SurfaceError::Publish`] when the
    /// rebuilt frame does not fit the acquired rectangle. In every case the caller
    /// must release and fall back to the inline card.
    pub(crate) fn pump(&mut self) -> Result<(), SurfaceError> {
        let Some(frame) = self
            .coordinator
            .tick(Instant::now())
            .map_err(SurfaceError::Coordinator)?
        else {
            // Once the failure budget is spent the coordinator reports no error
            // and simply stops doing anything, so a card would sit on screen with
            // no worker behind it and no fallback. Treat that as the failure it
            // is; `Rebuilding` is progress and must not be reported.
            if matches!(self.coordinator.lifecycle(), WorkerLifecycle::Unavailable)
                || self.coordinator.recovery_mode() != RecoveryMode::Normal
            {
                return Err(SurfaceError::WorkerUnrecoverable);
            }
            return Ok(());
        };
        let Some(rect) = self.gate.rect() else {
            // Refuse it explicitly rather than dropping it. No publish has begun,
            // so the frame is still only prepared and there is no publishing state
            // to roll back; `publish_failed` applies to a frame that reached
            // `begin_publishing` and would report `InvalidState` here. The caller
            // discards the whole session on this error, so the coordinator's
            // prepared frame goes away with it.
            return Err(SurfaceError::NoRectangle);
        };
        self.commit(frame, rect)
    }

    /// viewport could not be read.
    pub(crate) fn geometry_trust(&mut self) -> GeometryTrust {
        self.gate.observe_geometry()
    }

    /// Scrolls the current viewport into scrollback so a replacement card can be
    /// drawn on a region that provably holds nothing else.
    ///
    /// # Errors
    /// Returns [`SurfaceError::Gate`] when the viewport could not be read or the
    /// scroll-out was not fully accepted.
    pub(crate) fn push_viewport_into_scrollback(&mut self) -> Result<(), SurfaceError> {
        self.gate
            .push_viewport_into_scrollback()
            .map_err(SurfaceError::Gate)
    }

    /// Gives up the surface without erasing anything, then shuts the worker down.
    ///
    /// This is the only correct teardown once geometry or write ownership is
    /// untrusted. [`Self::abandon`] erases at the recorded rectangle, which after a
    /// reflow may be the user's output rather than the card, so an untrusted path
    /// must not reach it. Clearing the rectangle first is what makes the later
    /// `Drop` a no-op instead of a stale `CUP`/`EL2`.
    pub(crate) fn relinquish_untrusted(&mut self) {
        self.live = false;
        self.gate.relinquish_without_erasing();
        self.shutdown();
    }

    /// Releases the rectangle but keeps the worker alive for the next
    /// presentation of the same approval.
    ///
    /// A focus change or a details toggle withdraws and repaints; paying a
    /// shutdown wait and a fresh spawn for each one would make arrow keys
    /// visibly slow, and the worker is still the same trusted candidate.
    ///
    /// # Errors
    /// Returns [`SurfaceError::Gate`] when the terminal refused the erase, or
    /// [`SurfaceError::NotLive`] when no rectangle is acquired.
    pub(crate) fn withdraw(&mut self) -> Result<(), SurfaceError> {
        if !self.live {
            return Err(SurfaceError::NotLive);
        }
        self.gate.withdraw().map_err(SurfaceError::Gate)?;
        self.live = false;
        Ok(())
    }

    /// Releases the surface and shuts the worker down, waiting for it to be
    /// reaped.
    ///
    /// `sealed` keeps the last published content readable as scrollback, which is
    /// what a decision receipt wants; otherwise the rectangle is erased. Shutdown
    /// is attempted even when no surface is live, because a spawned worker must
    /// not outlive the interaction that spawned it.
    ///
    /// # Errors
    /// Returns [`SurfaceError::Gate`] when the terminal refused the release
    /// bytes. Shutdown failures are not reported: the caller is already leaving,
    /// and the coordinator kills the child when the graceful path exceeds its
    /// bound.
    pub(crate) fn release(&mut self, sealed: bool) -> Result<(), SurfaceError> {
        if self.live {
            let result = if sealed {
                self.gate.seal_to_scrollback()
            } else {
                self.gate.withdraw()
            };
            self.live = false;
            result.map_err(SurfaceError::Gate)?;
        }
        self.shutdown();
        Ok(())
    }

    /// Best-effort cleanup after a failure: erase any acquired rectangle, then
    /// shut the worker down.
    ///
    /// Erasure errors are not reported because the caller is already falling back
    /// to the inline path, and a terminal that refuses the erase will also refuse
    /// the fallback; the inline renderer's own error path owns that case.
    pub(crate) fn abandon(&mut self) {
        self.live = false;
        if self.gate.rect().is_some() {
            let _ = self.gate.withdraw();
        }
        self.shutdown();
    }

    /// Whether an acquired rectangle is still the bottom of the terminal.
    ///
    /// The anchor row is derived from the viewport height at acquisition time, so a
    /// resize while a card is live leaves the rectangle naming rows that are no
    /// longer at the bottom. Painting there puts the new card in the wrong place
    /// and leaves the pre-resize copy behind, which the later `withdraw` cannot
    /// erase because it only clears the rows it is told about. Re-reading also
    /// restores the width and height refusals `acquire` makes, which reusing a
    /// rectangle would otherwise skip.
    fn rectangle_still_owns_the_bottom(&mut self, rows: u16) -> Result<bool, SurfaceError> {
        self.gate
            .rectangle_still_owns_the_bottom(rows, self.activation.columns)
            .map_err(SurfaceError::Gate)
    }

    fn shutdown(&mut self) {
        let now = Instant::now();
        if self
            .coordinator
            .begin_shutdown("approval interaction ended".to_owned(), now)
            .is_err()
        {
            return;
        }
        let deadline = now
            .checked_add(self.activation.shutdown_timeout)
            .unwrap_or(now);
        while Instant::now() < deadline {
            if self.coordinator.tick(Instant::now()).is_err() || self.coordinator.is_closed() {
                return;
            }
            // With no child left there is nothing to reap, and `tick_shutdown`
            // can only reach `closed` at the deadline, so waiting would stall the
            // controller for the whole bound after an ordinary fault.
            if !self.coordinator.has_worker() {
                return;
            }
            std::thread::sleep(POLL_INTERVAL);
        }
    }

    fn commit(&mut self, frame: PreparedFrame, rect: SurfaceRect) -> Result<(), SurfaceError> {
        if self.geometry_trust() != GeometryTrust::Trusted {
            return Err(SurfaceError::GeometryInvalidated);
        }
        let key = frame.frame_key();
        let barrier = self.router.input_sequence().current();
        self.coordinator
            .begin_publishing(key, barrier)
            .map_err(SurfaceError::Coordinator)?;
        let receipt = match self.publisher.publish_surface(
            self.gate.term_mut(),
            frame,
            rect,
            &mut self.router,
            &self.sequence,
        ) {
            Ok(receipt) => receipt,
            Err(error) => {
                let _ = self.coordinator.publish_failed(key);
                return Err(SurfaceError::Publish(error));
            }
        };
        self.coordinator
            .publish_succeeded(key, receipt.publish_seq, receipt.geometry, Instant::now())
            .map_err(SurfaceError::Coordinator)?;
        self.live = true;
        Ok(())
    }

    fn wait_for_ready(&mut self, start: Instant) -> Result<(), SurfaceError> {
        let deadline = start
            .checked_add(self.activation.handshake_timeout)
            .unwrap_or(start);
        while Instant::now() < deadline {
            self.coordinator
                .tick(Instant::now())
                .map_err(SurfaceError::Coordinator)?;
            if matches!(self.coordinator.lifecycle(), WorkerLifecycle::Ready(_)) {
                return Ok(());
            }
            std::thread::sleep(POLL_INTERVAL);
        }
        Err(SurfaceError::HandshakeTimeout)
    }

    fn await_frame(&mut self, start: Instant) -> Result<PreparedFrame, SurfaceError> {
        let deadline = start
            .checked_add(self.activation.render_timeout)
            .unwrap_or(start);
        while Instant::now() < deadline {
            if let Some(frame) = self
                .coordinator
                .tick(Instant::now())
                .map_err(SurfaceError::Coordinator)?
            {
                return Ok(frame);
            }
            std::thread::sleep(POLL_INTERVAL);
        }
        Err(SurfaceError::RenderTimeout)
    }
}

impl<T: SurfaceTerminal> Drop for ApprovalSurfaceSession<T> {
    fn drop(&mut self) {
        // A spawned worker must never outlive the session that spawned it, even
        // on an early return or a panic in the caller. The gate is dropped with
        // the session, so only the child needs cleaning up here.
        if !self.coordinator.is_closed() {
            self.shutdown();
        }
        // Frame publishing hides the caret and nothing else in the shell shows it
        // again, so a session dropped by a panic or an early return would
        // otherwise hand the user a terminal with no cursor. Best effort: the
        // terminal may already be gone, and there is nobody left to tell.
        if self.gate.rect().is_some() {
            let _ = self.gate.write_presentation(b"\x1b[0m\x1b[?25h");
        }
    }
}

#[cfg(test)]
mod tests;
