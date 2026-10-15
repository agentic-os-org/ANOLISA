//! Unit tests for the Host-owned input router publish/consume contract.

use super::{
    CaptureAuthority, CaptureError, CardAction, HostInputAction, InputRouter, PublishGuard,
    PublishedCaptureToken,
};
use crate::ui::worker_protocol::{FrameGeometry, FrameKey};

const NONCE: [u8; 16] = [7u8; 16];
const WORKER_GENERATION: u64 = 3;
const INPUT_EPOCH: u64 = 5;
const PROMPT_EVIDENCE_EPOCH: u64 = 9;
const FRAME_ID: u64 = 11;
const CAPTURE_ID: u64 = 17;

fn geometry() -> FrameGeometry {
    FrameGeometry {
        columns: 80,
        rows: 24,
        cursor_x: 0,
        cursor_y: 0,
    }
}

fn fresh_router() -> InputRouter {
    InputRouter::new(NONCE, WORKER_GENERATION, INPUT_EPOCH, PROMPT_EVIDENCE_EPOCH)
}

/// Drives the real publish path to a `Published` state and returns both the
/// router and the exact token a matching Host action must echo.
fn published_router(publish_seq: u64) -> (InputRouter, PublishedCaptureToken) {
    let mut router = fresh_router();
    let key = FrameKey {
        worker_generation: WORKER_GENERATION,
        frame_id: FRAME_ID,
        capture_id: Some(CAPTURE_ID),
    };
    let guard = router.begin_publish(key, publish_seq);
    let token = router
        .commit_publish(guard, publish_seq, geometry())
        .expect("a frame carrying a capture id must publish a token");
    (router, token)
}

fn approve(token: PublishedCaptureToken, input_seq: u64) -> HostInputAction {
    HostInputAction {
        token,
        input_seq,
        action: CardAction::Approve,
    }
}

#[test]
fn rejects_input_seq_not_strictly_after_publish_seq() {
    let (mut router, token) = published_router(42);
    // Bytes sampled at the publish cutover are stale and must be refused.
    // Guarded by the `input_seq <= publish_seq` check in `consume_once`.
    assert_eq!(
        router.consume_once(approve(token, 42)),
        Err(CaptureError::StaleInput)
    );
    // The rejection above did not consume the token, so strictly-newer bytes
    // are still accepted.
    let (mut router, token) = published_router(42);
    assert_eq!(
        router.consume_once(approve(token, 43)),
        Ok(CardAction::Approve)
    );
}

#[test]
fn positive_control_consumes_exactly_once() {
    let (mut router, token) = published_router(100);
    assert_eq!(
        router.consume_once(approve(token, 101)),
        Ok(CardAction::Approve)
    );
    assert_eq!(router.authority(), None);
    assert_eq!(router.route_tracker().snapshot().authority, None);
    // A replayed duplicate permanently rejects: guarded by the `consumed` flag.
    assert_eq!(
        router.consume_once(approve(token, 101)),
        Err(CaptureError::AlreadyConsumed)
    );
}

#[test]
fn rejects_worker_generation_mutation() {
    let (mut router, mut token) = published_router(100);
    token.worker_generation = token.worker_generation.wrapping_add(1);
    assert_eq!(
        router.consume_once(approve(token, 101)),
        Err(CaptureError::IdentityMismatch)
    );
}

#[test]
fn rejects_input_epoch_mutation() {
    let (mut router, mut token) = published_router(100);
    token.input_epoch = token.input_epoch.wrapping_add(1);
    assert_eq!(
        router.consume_once(approve(token, 101)),
        Err(CaptureError::IdentityMismatch)
    );
}

#[test]
fn rejects_prompt_evidence_epoch_mutation() {
    let (mut router, mut token) = published_router(100);
    token.prompt_evidence_epoch = token.prompt_evidence_epoch.wrapping_add(1);
    assert_eq!(
        router.consume_once(approve(token, 101)),
        Err(CaptureError::IdentityMismatch)
    );
}

#[test]
fn rejects_session_nonce_mutation() {
    let (mut router, mut token) = published_router(100);
    token.session_nonce[0] ^= 0xff;
    assert_eq!(
        router.consume_once(approve(token, 101)),
        Err(CaptureError::IdentityMismatch)
    );
}

#[test]
fn rejects_frame_id_mutation() {
    let (mut router, mut token) = published_router(100);
    token.frame_id = token.frame_id.wrapping_add(1);
    assert_eq!(
        router.consume_once(approve(token, 101)),
        Err(CaptureError::IdentityMismatch)
    );
}

#[test]
fn rejects_capture_id_mutation() {
    let (mut router, mut token) = published_router(100);
    token.capture_id = token.capture_id.wrapping_add(1);
    assert_eq!(
        router.consume_once(approve(token, 101)),
        Err(CaptureError::IdentityMismatch)
    );
}

#[test]
fn commit_without_capture_id_publishes_nothing() {
    let mut router = fresh_router();
    let key = FrameKey {
        worker_generation: WORKER_GENERATION,
        frame_id: FRAME_ID,
        capture_id: None,
    };
    let guard = router.begin_publish(key, 0);
    // A frame with no capture id owns no input, so no token is minted.
    assert!(router.commit_publish(guard, 0, geometry()).is_none());
    assert_eq!(router.authority(), Some(CaptureAuthority::Legacy));
}

#[test]
fn publish_revokes_capture_and_grants_no_owner_until_commit() {
    let mut router = fresh_router();
    assert_eq!(router.authority(), Some(CaptureAuthority::Legacy));
    let key = FrameKey {
        worker_generation: WORKER_GENERATION,
        frame_id: FRAME_ID,
        capture_id: Some(CAPTURE_ID),
    };
    let guard = router.begin_publish(key, 0);
    // Entering publish revokes capture; mid-publish bytes belong to no owner.
    assert_eq!(router.authority(), None);
    let token = router
        .commit_publish(guard, 7, geometry())
        .expect("capture id present");
    assert_eq!(router.authority(), Some(CaptureAuthority::Published(token)));
    assert_eq!(router.published_geometry(), Some(geometry()));
}

#[test]
fn abort_publish_never_publishes() {
    let mut router = fresh_router();
    let key = FrameKey {
        worker_generation: WORKER_GENERATION,
        frame_id: FRAME_ID,
        capture_id: Some(CAPTURE_ID),
    };
    let guard = router.begin_publish(key, 0);
    router.abort_publish(guard);
    // Abort returns input to the legacy owner and mints no token.
    assert_eq!(router.authority(), Some(CaptureAuthority::Legacy));
    let orphan = PublishedCaptureToken {
        session_nonce: NONCE,
        worker_generation: WORKER_GENERATION,
        input_epoch: INPUT_EPOCH,
        prompt_evidence_epoch: PROMPT_EVIDENCE_EPOCH,
        frame_id: FRAME_ID,
        capture_id: CAPTURE_ID,
        publish_seq: 0,
    };
    assert_eq!(
        router.consume_once(approve(orphan, 1)),
        Err(CaptureError::NoPublishedCapture)
    );
}

#[test]
fn legacy_state_has_no_published_capture() {
    let mut router = fresh_router();
    let orphan = PublishedCaptureToken {
        session_nonce: NONCE,
        worker_generation: WORKER_GENERATION,
        input_epoch: INPUT_EPOCH,
        prompt_evidence_epoch: PROMPT_EVIDENCE_EPOCH,
        frame_id: FRAME_ID,
        capture_id: CAPTURE_ID,
        publish_seq: 0,
    };
    assert_eq!(
        router.consume_once(approve(orphan, 1)),
        Err(CaptureError::NoPublishedCapture)
    );
}

#[test]
fn input_sequence_hands_out_strictly_increasing_shared_values() {
    let router = fresh_router();
    let seq = router.input_sequence();
    let first = seq.next();
    let second = seq.next();
    assert!(second > first);
    assert_eq!(seq.current(), second);
    // A clone observes the same counter: the Host owns a single sequence.
    assert_eq!(router.input_sequence().current(), second);
}

/// A guard that a caller could still be holding after its publish already
/// resolved (the pre-fix `PublishGuard` was `Copy`). Reconstructed here via the
/// public fields to exercise the resolve-exactly-once state gate.
fn stale_guard() -> PublishGuard {
    PublishGuard {
        frame_id: FRAME_ID,
        capture_id: Some(CAPTURE_ID),
        input_barrier: 0,
    }
}

fn capture_key() -> FrameKey {
    FrameKey {
        worker_generation: WORKER_GENERATION,
        frame_id: FRAME_ID,
        capture_id: Some(CAPTURE_ID),
    }
}

#[test]
fn begin_abort_then_commit_mints_no_token() {
    // begin -> abort -> commit must not mint a token. The abort takes the
    // router out of `Publishing`, so a stale guard fed to `commit_publish`
    // finds no in-flight publish and returns None.
    // Named production mutation: removing the `matches!(self.state,
    // PublishState::Publishing)` guard in `commit_publish` reddens this
    // (it is exactly the pre-fix behavior).
    let mut router = fresh_router();
    let guard = router.begin_publish(capture_key(), 0);
    router.abort_publish(guard);
    assert!(router
        .commit_publish(stale_guard(), 5, geometry())
        .is_none());
    // No capture was published; input stays with the legacy owner.
    assert_eq!(router.authority(), Some(CaptureAuthority::Legacy));
}

#[test]
fn commit_after_commit_mints_no_token() {
    // Double-commit: the second commit finds the router already `Published`,
    // not `Publishing`, so it mints nothing and leaves the first capture live.
    // Named production mutation: removing the `commit_publish` `Publishing`
    // state gate reddens this (the second commit would re-mint a token).
    let mut router = fresh_router();
    let guard = router.begin_publish(capture_key(), 0);
    let token = router
        .commit_publish(guard, 5, geometry())
        .expect("first commit publishes a token");
    assert!(router
        .commit_publish(stale_guard(), 9, geometry())
        .is_none());
    // The originally published capture is untouched by the stale second commit.
    assert_eq!(router.authority(), Some(CaptureAuthority::Published(token)));
}

#[test]
fn abort_after_commit_keeps_published_capture() {
    // abort after commit must be rejected: a stale abort must not revoke the
    // live published capture back to legacy.
    // Named production mutation: removing the `matches!(self.state,
    // PublishState::Publishing)` guard in `abort_publish` reddens this (the
    // abort would drop the router to `Legacy` and destroy the capture).
    let mut router = fresh_router();
    let guard = router.begin_publish(capture_key(), 0);
    let token = router
        .commit_publish(guard, 5, geometry())
        .expect("commit publishes a token");
    router.abort_publish(stale_guard());
    // The published capture survives the stale abort and still consumes once.
    assert_eq!(router.authority(), Some(CaptureAuthority::Published(token)));
    assert_eq!(
        router.consume_once(approve(token, 6)),
        Ok(CardAction::Approve)
    );
}

#[test]
fn route_tracker_advances_at_publish_boundaries() {
    // RED mutations: omit any begin/commit route transition, or reuse one epoch.
    let mut router = fresh_router();
    let tracker = router.route_tracker();
    let legacy = tracker.snapshot();
    assert_eq!(legacy.authority, Some(CaptureAuthority::Legacy));

    let guard = router.begin_publish(capture_key(), 4);
    let publishing = tracker.snapshot();
    assert!(publishing.epoch > legacy.epoch);
    assert_eq!(publishing.authority, None);

    let token = router
        .commit_publish(guard, 5, geometry())
        .expect("commit publishes a token");
    let published = tracker.snapshot();
    assert!(published.epoch > publishing.epoch);
    assert_eq!(
        published.authority,
        Some(CaptureAuthority::Published(token))
    );
}

#[test]
fn route_tracker_advances_when_publish_aborts() {
    // RED mutation: leave readers on the Publishing epoch after abort.
    let mut router = fresh_router();
    let tracker = router.route_tracker();
    let guard = router.begin_publish(capture_key(), 4);
    let publishing = tracker.snapshot();

    router.abort_publish(guard);

    let aborted = tracker.snapshot();
    assert!(aborted.epoch > publishing.epoch);
    assert_eq!(aborted.authority, Some(CaptureAuthority::Legacy));
}
