//! Transient interactive activity feedback for long-running human-readable
//! commands (issue #1452).
//!
//! Repository queries, install/upgrade planning, artifact preparation, and
//! `dnf` transactions can each block for a long time with no visible output,
//! so a healthy run is indistinguishable from a hung process. This module
//! provides an injectable [`ProgressReporter`] plus an [`Activity`] guard that
//! renders feedback in one of two ways, chosen by [`feedback_mode`], without
//! ever touching the structured stdout/JSON contract:
//!
//! - **Animated** (interactive, ANSI-capable terminal): a background spinner on
//!   stderr, repainted in place.
//! - **Static** (interactive terminal that cannot animate — `TERM=dumb`, an
//!   unknown/zero width, or an animation thread that failed to spawn): a single
//!   plain stderr line per phase, no ANSI and no repaint. This keeps a feedback
//!   channel for users the animated path cannot serve, instead of going silent
//!   and reintroducing the "looks hung" problem.
//!
//! `--json`, `--quiet`, and a non-TTY stderr all resolve to **Disabled**: no
//! thread, no writes, no control sequences.
//!
//! Other design points:
//! - **Never wrap**: an animated frame is painted only when the terminal width
//!   is known and the frame fits one physical line ([`fit_line`]); otherwise the
//!   painter falls back to a one-time static line for that message, so a frame
//!   `clear_line` could not fully erase is never emitted.
//! - **No competing output**: while animated feedback is live, occasional
//!   persistent lines (repo-config deprecation, provisioning result,
//!   central-log warning) go through [`suspend_output`], which parks the painter
//!   and clears the frame so the two never interleave.
//! - **Injectable phases**: the apply loop takes a `&dyn `[`ProgressReporter`]
//!   so tests can record the exact phase messages with a fake sink.
//! - **Reliable cleanup**: the animated painter owns *every* terminal write,
//!   including the final clear it performs as its last action before publishing
//!   [`SIG_DONE`]. On Ctrl+C the scoped SIGINT handler ([`on_sigint`]) never
//!   writes itself — it only asks the painter to stop and waits for that final
//!   clear — so an in-flight frame can never overwrite the cleanup. The cursor
//!   is never hidden.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use console::{Term, measure_text_width};
use nix::libc;
use nix::sys::signal::{SaFlags, SigAction, SigHandler, SigSet, SigmaskHow, Signal, sigaction};

/// Braille spinner frames. Unicode is already used elsewhere in the CLI (see
/// `color.rs`), so no ASCII fallback is needed for the animated path (which only
/// runs on ANSI-capable terminals anyway).
const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Interval between animation frames.
const TICK: Duration = Duration::from_millis(100);

/// Granularity at which the painter polls the stop flags while waiting out a
/// [`TICK`], so shutdown (drop or Ctrl+C) is observed promptly.
const STOP_POLL: Duration = Duration::from_millis(10);

/// Upper bound on the SIGINT handler's spin-wait for the painter to finish its
/// final clear. This is only a hang-guard for a painter that is wedged or
/// starved; in the normal case the wait ends in well under a tick. It does not
/// weaken correctness: the handler never writes to the terminal, so timing out
/// cannot cause a clear to be overwritten — at worst a wedged painter leaves its
/// last frame, which no design could safely erase while that write is stuck.
const SIGINT_SPIN_LIMIT: u32 = 20_000_000;

/// The active animation's shared state, published while it is live so
/// [`suspend_output`] can coordinate with the painter from another call path.
/// At most one animation is live at a time in this CLI (never nested).
static ACTIVE: Mutex<Option<Arc<AnimationState>>> = Mutex::new(None);

/// Set by the SIGINT handler to tell the painter to stop. Global (not a field)
/// because the async-signal-safe handler cannot reach the `Arc` state.
static SIG_STOP: AtomicBool = AtomicBool::new(false);

/// Published by the painter after it has performed its final clear and will not
/// write again. The SIGINT handler waits on this so it never re-raises before
/// the terminal has been cleaned up.
static SIG_DONE: AtomicBool = AtomicBool::new(false);

/// Report transient phase messages during a long operation.
///
/// Production code uses [`Activity`]; tests use a recording fake to assert the
/// exact phase sequence without a TTY.
pub(crate) trait ProgressReporter {
    /// Replace the currently displayed activity message.
    fn report(&self, message: &str);

    /// Stop transient feedback before persistent command output is rendered.
    ///
    /// Recording and no-op reporters do not own terminal state, so the default
    /// implementation is intentionally empty.
    fn finish(&mut self) {}
}

/// A [`ProgressReporter`] that discards every message.
///
/// Production code always passes a live [`Activity`] (which is itself inert when
/// disabled), so this exists only for tests that exercise the apply path without
/// asserting on progress output.
#[cfg(test)]
pub(crate) struct NoopReporter;

#[cfg(test)]
impl ProgressReporter for NoopReporter {
    fn report(&self, _message: &str) {}
}

/// How a command should surface transient activity feedback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FeedbackMode {
    /// No feedback: `--json`, `--quiet`, or a non-interactive stderr.
    Disabled,
    /// In-place stderr animation on an ANSI-capable interactive terminal.
    Animated,
    /// One plain stderr line per phase when the terminal cannot animate.
    Static,
}

/// Decide the feedback mode from the output-mode flags and terminal facts.
///
/// Kept pure so the policy is unit-testable without a real TTY. Machine output
/// (`--json`), silence (`--quiet`), and a non-TTY stderr all suppress feedback;
/// an interactive terminal animates when ANSI-capable and otherwise falls back
/// to static lines rather than going silent.
pub(crate) fn feedback_mode(
    json: bool,
    quiet: bool,
    stderr_is_tty: bool,
    ansi_capable: bool,
) -> FeedbackMode {
    if json || quiet || !stderr_is_tty {
        FeedbackMode::Disabled
    } else if ansi_capable {
        FeedbackMode::Animated
    } else {
        FeedbackMode::Static
    }
}

/// Resolve [`feedback_mode`] against the live stderr terminal.
pub(crate) fn feedback_for_stderr(json: bool, quiet: bool) -> FeedbackMode {
    feedback_mode(json, quiet, stderr_is_tty(), terminal_ansi_capable())
}

/// Whether the process's stderr is an interactive terminal.
fn stderr_is_tty() -> bool {
    Term::stderr().is_term()
}

/// Whether `$TERM` denotes a terminal that can interpret ANSI control
/// sequences. A `dumb` (or empty/unset) terminal cannot, so the animated path
/// is not used there.
fn terminal_ansi_capable() -> bool {
    ansi_capable_for(std::env::var("TERM").ok().as_deref())
}

fn ansi_capable_for(term: Option<&str>) -> bool {
    matches!(term, Some(t) if !t.is_empty() && t != "dumb")
}

/// Run `f`, parking active animated feedback (if any) and clearing its frame
/// for the duration so a persistent line written by `f` cannot interleave with,
/// or be erased by, animation repaints. A no-op wrapper when no animation
/// is live (including static mode), so it is safe to route every occasional CLI
/// line through it unconditionally.
pub(crate) fn suspend_output<T>(f: impl FnOnce() -> T) -> T {
    let Some(state) = active_state() else {
        return f();
    };
    // The SIGINT handler waits for the painter. Keep it from interrupting this
    // thread while it owns the paint lock the painter needs for its final clear.
    // Locals drop in reverse order, so `_paint` unlocks before the mask restores.
    let _sigint_block = ScopedSigintBlock::block().ok();
    // Hold the paint lock for the whole of `f`: the painter also takes it each
    // tick, so it cannot repaint until `f` returns and its output is flushed.
    let _paint = state.lock_paint();
    let _ = Term::stderr().clear_line();
    f()
}

/// Emit a single plain stderr line (no ANSI, no repaint) — the static-feedback
/// primitive, safe on terminals that cannot interpret control sequences.
fn emit_static_line(message: &str) {
    let _ = Term::stderr().write_line(message);
}

/// A transient activity guard.
///
/// Construct with [`Activity::start`]; the mode decides whether it animates,
/// prints static lines, or does nothing. Dropping it tears down the animated
/// worker (join the thread, restore SIGINT); the other modes need no cleanup.
pub(crate) struct Activity {
    inner: ActivityInner,
}

enum ActivityInner {
    Disabled,
    Static(StaticHint),
    Animated(AnimatedFeedback),
}

impl Activity {
    /// Start feedback for `message` in the given [`FeedbackMode`].
    ///
    /// An `Animated` request whose worker thread cannot be spawned degrades to
    /// `Static` — a non-critical progress hint must never crash the command, and
    /// falling back keeps a feedback channel rather than going silent.
    pub(crate) fn start(mode: FeedbackMode, message: &str) -> Self {
        let inner = match mode {
            FeedbackMode::Disabled => ActivityInner::Disabled,
            FeedbackMode::Static => ActivityInner::Static(StaticHint::new(message)),
            FeedbackMode::Animated => match AnimatedFeedback::start(message) {
                Some(animation) => ActivityInner::Animated(animation),
                None => ActivityInner::Static(StaticHint::new(message)),
            },
        };
        Self { inner }
    }

    /// Replace the displayed message. A no-op when disabled.
    pub(crate) fn set_message(&self, message: &str) {
        match &self.inner {
            ActivityInner::Disabled => {}
            ActivityInner::Static(hint) => hint.report(message),
            ActivityInner::Animated(animation) => {
                // Avoid pausing this thread in the handler while it owns the
                // message lock the painter may need before its final clear.
                let _sigint_block = ScopedSigintBlock::block().ok();
                set_shared_message(&animation.state.message, message);
            }
        }
    }

    /// Stop feedback and clear any animated frame.
    ///
    /// Keeping the guard usable after this call lets command pipelines finish
    /// it through a borrowed [`ProgressReporter`] before printing their final
    /// result. Dropping the guard later is then a no-op.
    pub(crate) fn finish(&mut self) {
        self.inner = ActivityInner::Disabled;
    }
}

impl ProgressReporter for Activity {
    fn report(&self, message: &str) {
        self.set_message(message);
    }

    fn finish(&mut self) {
        Activity::finish(self);
    }
}

/// Static feedback: one plain line per distinct message, deduplicated so an
/// unchanged phase is not reprinted.
struct StaticHint {
    last: Mutex<Option<String>>,
}

impl StaticHint {
    fn new(message: &str) -> Self {
        emit_static_line(message);
        Self {
            last: Mutex::new(Some(message.to_string())),
        }
    }

    fn report(&self, message: &str) {
        let mut last = self
            .last
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if last.as_deref() != Some(message) {
            emit_static_line(message);
            *last = Some(message.to_string());
        }
    }
}

/// Animated feedback: a background worker repaints frames on stderr.
struct AnimatedFeedback {
    state: Arc<AnimationState>,
    handle: Option<JoinHandle<()>>,
    /// SIGINT disposition to restore on drop; `None` when the scoped handler
    /// could not be installed.
    prev_sigint: Option<SigAction>,
}

impl AnimatedFeedback {
    /// Spawn the painter, or `None` if the worker thread cannot be created.
    fn start(message: &str) -> Option<Self> {
        // Block before installing the handler and spawning. The painter inherits
        // this mask, closing the window where it could handle SIGINT itself and
        // wait for its own `SIG_DONE`. Failure degrades to static feedback.
        let sigint_block = ScopedSigintBlock::block().ok()?;

        // Fresh interrupt-handshake state for this animation.
        SIG_STOP.store(false, Ordering::Release);
        SIG_DONE.store(false, Ordering::Release);

        let state = Arc::new(AnimationState {
            message: Mutex::new(message.to_string()),
            stop: AtomicBool::new(false),
            paint: Mutex::new(()),
        });
        set_active(Some(Arc::clone(&state)));
        let prev_sigint = install_sigint_cleanup();

        let worker_state = Arc::clone(&state);
        match thread::Builder::new()
            .name("anolisa-activity".to_string())
            .spawn(move || run_animation(&worker_state))
        {
            Ok(handle) => {
                // The worker now exists with SIGINT blocked, so a pending signal
                // can safely run the handler when the parent mask is restored.
                drop(sigint_block);
                Some(Self {
                    state,
                    handle: Some(handle),
                    prev_sigint,
                })
            }
            Err(_) => {
                // Restore the prior disposition before unblocking so a pending
                // signal cannot enter our handler without a painter to finish.
                restore_sigint(prev_sigint);
                set_active(None);
                drop(sigint_block);
                None
            }
        }
    }
}

impl Drop for AnimatedFeedback {
    fn drop(&mut self) {
        self.state.stop.store(true, Ordering::Release);
        if let Some(handle) = self.handle.take() {
            // A worker panic must not poison the drop path; the defensive clear
            // below still runs.
            let _ = handle.join();
        }
        // The painter clears as its last act; this is a belt-and-suspenders
        // clear in case the worker panicked before it could.
        let _ = Term::stderr().clear_line();
        restore_sigint(self.prev_sigint.take());
        set_active(None);
    }
}

/// Shared animation state driven by the worker thread and updated by
/// [`Activity::set_message`].
struct AnimationState {
    message: Mutex<String>,
    stop: AtomicBool,
    /// Serializes terminal repaints against [`suspend_output`].
    paint: Mutex<()>,
}

impl AnimationState {
    fn lock_paint(&self) -> MutexGuard<'_, ()> {
        self.paint
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Whether the painter should stop, for either a normal drop
    /// (`stop`) or an interrupt ([`SIG_STOP`]).
    fn should_stop(&self) -> bool {
        self.stop.load(Ordering::Acquire) || SIG_STOP.load(Ordering::Acquire)
    }
}

/// Worker loop: repaint the animation until a stop flag is set, then perform the
/// one and only final clear.
///
/// The painter is the *sole* terminal writer, which is what lets the SIGINT
/// handler stay write-free: the last thing written here is always the final
/// clear, after which [`SIG_DONE`] is published and the thread exits.
fn run_animation(state: &AnimationState) {
    let term = Term::stderr();
    let mut frame = 0usize;
    let mut render_state = RenderState::default();

    while !state.should_stop() {
        {
            let _paint = state.lock_paint();
            if !state.should_stop() {
                let message = read_shared_message(&state.message);
                match spinner_line(&term, frame, &message) {
                    Some(line) => {
                        // Best-effort paint: a transient stderr write error must
                        // not abort the whole command.
                        let _ = term.clear_line();
                        let _ = term.write_str(&line);
                        let _ = term.flush();
                        render_state.record_animated();
                    }
                    None => {
                        // Width unknown/degenerate: fall back to a one-time
                        // static line so the user still sees activity rather
                        // than a frozen-looking terminal (issue #1452 P2).
                        let (clear_frame, emit_line) = render_state.enter_static(&message);
                        if clear_frame {
                            let _ = term.clear_line();
                        }
                        if emit_line {
                            let _ = term.write_line(&message);
                        }
                    }
                }
            }
        }
        sleep_until_stop(state);
        frame = frame.wrapping_add(1);
    }

    // Final clear is the painter's last write; the SIGINT handler relies on this
    // (it never writes the terminal itself).
    {
        let _paint = state.lock_paint();
        let _ = term.clear_line();
        let _ = term.flush();
    }
    SIG_DONE.store(true, Ordering::Release);
}

/// Restores the calling thread's prior signal mask when it leaves scope.
struct ScopedSigintBlock {
    previous: SigSet,
}

impl ScopedSigintBlock {
    fn block() -> nix::Result<Self> {
        let mut sigint = SigSet::empty();
        sigint.add(Signal::SIGINT);
        let previous = sigint.thread_swap_mask(SigmaskHow::SIG_BLOCK)?;
        Ok(Self { previous })
    }
}

impl Drop for ScopedSigintBlock {
    fn drop(&mut self) {
        let _ = self.previous.thread_set_mask();
    }
}

/// Tracks transitions between in-place frames and persistent fallback lines.
#[derive(Default)]
struct RenderState {
    animated_visible: bool,
    static_shown: Option<String>,
}

impl RenderState {
    fn record_animated(&mut self) {
        self.animated_visible = true;
        // Feedback from before a resumed animated period must not suppress a
        // later transition back to the persistent fallback.
        self.static_shown = None;
    }

    /// Returns whether to clear an animated frame and emit the static message.
    fn enter_static(&mut self, message: &str) -> (bool, bool) {
        let clear_frame = std::mem::take(&mut self.animated_visible);
        let emit_line = self.static_shown.as_deref() != Some(message);
        if emit_line {
            self.static_shown = Some(message.to_string());
        }
        (clear_frame, emit_line)
    }
}

/// Sleep out one [`TICK`], waking early (within [`STOP_POLL`]) when a stop flag
/// is set so shutdown latency stays low.
fn sleep_until_stop(state: &AnimationState) {
    let mut elapsed = Duration::ZERO;
    while elapsed < TICK {
        if state.should_stop() {
            return;
        }
        let step = STOP_POLL.min(TICK - elapsed);
        thread::sleep(step);
        elapsed += step;
    }
}

/// Fit `line` to the terminal width, or `None` when it cannot be shown on a
/// single physical line (width unknown, zero, or too narrow). Delegates the
/// pure decision to [`fit_line`] so it is testable without a terminal.
fn fit_to_width(term: &Term, line: &str) -> Option<String> {
    fit_line(term.size_checked().map(|(_rows, cols)| cols), line)
}

fn spinner_line(term: &Term, frame: usize, message: &str) -> Option<String> {
    fit_to_width(term, &format!("{} {message}", FRAMES[frame % FRAMES.len()]))
}

/// Pure width-fitting: `Some(truncated)` when `cols` is known and greater than
/// one (leaving one spare column), else `None`. Returning `None` for an unknown
/// or degenerate width lets the painter fall back to a static line rather than
/// risk a wrapped frame that `clear_line` could only partially erase.
fn fit_line(cols: Option<u16>, line: &str) -> Option<String> {
    match cols {
        Some(cols) if cols > 1 => {
            let max = cols as usize - 1;
            if measure_text_width(line) <= max {
                Some(line.to_string())
            } else {
                Some(truncate_to_width(line, max))
            }
        }
        _ => None,
    }
}

/// Take the longest prefix of `s` whose display width does not exceed `max`.
fn truncate_to_width(s: &str, max: usize) -> String {
    let mut out = String::new();
    let mut width = 0usize;
    for ch in s.chars() {
        let w = measure_text_width(&ch.to_string());
        if width + w > max {
            break;
        }
        out.push(ch);
        width += w;
    }
    out
}

// ── global active-animation registry ─────────────────────────────────────────

fn active_state() -> Option<Arc<AnimationState>> {
    match ACTIVE.lock() {
        Ok(guard) => guard.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    }
}

fn set_active(state: Option<Arc<AnimationState>>) {
    match ACTIVE.lock() {
        Ok(mut guard) => *guard = state,
        Err(poisoned) => *poisoned.into_inner() = state,
    }
}

/// Read the shared message, recovering from a poisoned lock rather than
/// panicking (a worker/main panic must not take down the reporter).
fn read_shared_message(message: &Mutex<String>) -> String {
    match message.lock() {
        Ok(guard) => guard.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    }
}

/// Update the shared message, recovering from a poisoned lock.
fn set_shared_message(message: &Mutex<String>, next: &str) {
    match message.lock() {
        Ok(mut guard) => *guard = next.to_string(),
        Err(poisoned) => *poisoned.into_inner() = next.to_string(),
    }
}

// ── scoped SIGINT cleanup ─────────────────────────────────────────────────────

/// Async-signal-safe SIGINT handler.
///
/// It never writes the terminal itself. Instead it asks the painter to stop
/// ([`SIG_STOP`]) and waits for the painter's own final clear to complete
/// ([`SIG_DONE`]) before restoring the default disposition and re-raising. Since
/// the painter is the sole writer and its last write is a clear, an in-flight
/// frame can never overwrite the cleanup. The spin is bounded only as a
/// hang-guard for a wedged painter; timing out is still write-free and therefore
/// cannot corrupt output.
///
/// Only atomics, `signal`, and `raise` are used — all async-signal-safe.
extern "C" fn on_sigint(_sig: libc::c_int) {
    SIG_STOP.store(true, Ordering::Release);
    let mut spins = 0u32;
    while !SIG_DONE.load(Ordering::Acquire) && spins < SIGINT_SPIN_LIMIT {
        std::hint::spin_loop();
        spins += 1;
    }
    // SAFETY: both calls are async-signal-safe and act only on the process's own
    // SIGINT disposition.
    unsafe {
        libc::signal(libc::SIGINT, libc::SIG_DFL);
        libc::raise(libc::SIGINT);
    }
}

/// Install the scoped SIGINT cleanup handler, returning the previous
/// disposition to restore later. Best-effort: a failure yields `None` and the
/// animation simply falls back to "leftover frame on Ctrl+C".
fn install_sigint_cleanup() -> Option<SigAction> {
    let action = SigAction::new(
        SigHandler::Handler(on_sigint),
        SaFlags::empty(),
        SigSet::empty(),
    );
    // SAFETY: `on_sigint` is async-signal-safe (see its docs); installing a
    // SIGINT handler has no memory-safety obligations beyond that.
    unsafe { sigaction(Signal::SIGINT, &action) }.ok()
}

/// Restore a previously saved SIGINT disposition, if one was captured.
fn restore_sigint(prev: Option<SigAction>) {
    if let Some(prev) = prev {
        // SAFETY: `prev` was produced by a prior successful `sigaction` call.
        let _ = unsafe { sigaction(Signal::SIGINT, &prev) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// Recording reporter used by command tests to assert the exact phase
    /// sequence without a TTY or timing on animated frames.
    struct RecordingReporter {
        messages: RefCell<Vec<String>>,
    }

    #[test]
    fn feedback_mode_animates_on_ansi_tty() {
        assert_eq!(
            feedback_mode(false, false, true, true),
            FeedbackMode::Animated
        );
    }

    #[test]
    fn feedback_mode_static_on_non_ansi_tty() {
        // Interactive but cannot interpret ANSI (e.g. TERM=dumb): still give a
        // static hint rather than going silent.
        assert_eq!(
            feedback_mode(false, false, true, false),
            FeedbackMode::Static
        );
    }

    #[test]
    fn feedback_mode_disabled_for_json_quiet_and_non_tty() {
        assert_eq!(
            feedback_mode(true, false, true, true),
            FeedbackMode::Disabled
        );
        assert_eq!(
            feedback_mode(false, true, true, true),
            FeedbackMode::Disabled
        );
        assert_eq!(
            feedback_mode(false, false, false, true),
            FeedbackMode::Disabled
        );
    }

    #[test]
    fn ansi_capability_rejects_dumb_and_empty_and_unset() {
        assert!(ansi_capable_for(Some("xterm-256color")));
        assert!(ansi_capable_for(Some("screen")));
        assert!(!ansi_capable_for(Some("dumb")));
        assert!(!ansi_capable_for(Some("")));
        assert!(!ansi_capable_for(None));
    }

    /// Width fitting truncates an over-long frame, keeps a fitting one, and
    /// refuses to paint (returns `None`) when the width is unknown or degenerate
    /// — the branch that hands off to the static fallback.
    #[test]
    fn fit_line_bounds_or_skips() {
        assert_eq!(fit_line(Some(80), "short"), Some("short".to_string()));
        // A 10-column terminal leaves a 9-column budget.
        assert_eq!(
            fit_line(Some(10), "Checking for updates..."),
            Some("Checking ".to_string())
        );
        assert_eq!(fit_line(None, "Checking for updates..."), None);
        assert_eq!(fit_line(Some(0), "x"), None);
        assert_eq!(fit_line(Some(1), "x"), None);
    }

    #[test]
    fn truncate_to_width_bounds_display_width() {
        assert_eq!(truncate_to_width("hello world", 5), "hello");
        assert_eq!(truncate_to_width("hi", 5), "hi");
        assert_eq!(truncate_to_width("anything", 0), "");
    }

    #[test]
    fn render_state_clears_and_reemits_across_mode_transitions() {
        let mut state = RenderState::default();

        state.record_animated();
        assert_eq!(state.enter_static("Checking for updates..."), (true, true));
        assert_eq!(
            state.enter_static("Checking for updates..."),
            (false, false)
        );

        state.record_animated();
        assert_eq!(state.enter_static("Checking for updates..."), (true, true));
    }

    #[test]
    fn scoped_sigint_block_is_inherited_and_restored() {
        let previous = SigSet::thread_get_mask().expect("read initial signal mask");
        let block = ScopedSigintBlock::block().expect("block SIGINT");
        assert!(
            SigSet::thread_get_mask()
                .expect("read blocked signal mask")
                .contains(Signal::SIGINT)
        );

        let child_blocked = thread::spawn(|| {
            SigSet::thread_get_mask()
                .expect("read child signal mask")
                .contains(Signal::SIGINT)
        })
        .join()
        .expect("join signal mask probe");
        assert!(child_blocked);

        drop(block);
        assert_eq!(
            SigSet::thread_get_mask().expect("read restored signal mask"),
            previous
        );
    }

    /// A disabled activity does nothing: it never registers as the active
    /// animation, and `report` is a no-op.
    #[test]
    fn disabled_activity_is_inert() {
        let activity = Activity::start(FeedbackMode::Disabled, "Checking for updates...");
        assert!(matches!(activity.inner, ActivityInner::Disabled));
        assert!(active_state().is_none());
        activity.report("still inert");
    }

    /// With no active animation, `suspend_output` is a transparent
    /// passthrough that runs the closure exactly once and returns its value.
    #[test]
    fn suspend_output_without_animation_is_passthrough() {
        let ran = RefCell::new(0);
        let value = suspend_output(|| {
            *ran.borrow_mut() += 1;
            42
        });
        assert_eq!(value, 42);
        assert_eq!(*ran.borrow(), 1);
    }

    #[test]
    fn noop_reporter_discards_messages() {
        let reporter = NoopReporter;
        reporter.report("ignored");
    }

    #[test]
    fn recording_reporter_preserves_order() {
        let reporter = RecordingReporter {
            messages: RefCell::new(Vec::new()),
        };
        reporter.report("first");
        reporter.report("second");
        assert_eq!(reporter.messages.borrow().as_slice(), ["first", "second"]);
    }

    impl ProgressReporter for RecordingReporter {
        fn report(&self, message: &str) {
            self.messages.borrow_mut().push(message.to_string());
        }
    }
}
