// Owner: shell_host (interactive sentinel, issue #2025). Classifies the
// interactive-wait state of an agent handoff command running on the
// foreground TTY from kernel-level signals only: termios bits sampled on
// the PTY master (S1), the alternate-screen flag accumulated by the OSC
// parser (S2), and the foreground process group's blocking point read
// from /proc on Linux (S3). Static command profiles are wording priors
// only and never gate the classification (spec D2/D3).
//
// Every sampling step is fail-quiet: any error collapses to "no hint",
// so the worst failure mode is the pre-#2025 status quo.
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::i18n::{I18n, MessageId};
use crate::shell_host::HintCardRenderer;
use crate::types::CommandOrigin;

use super::super::osc::OscParser;

/// Output-quiet threshold before the sentinel starts sampling (spec D4).
pub(crate) const SENTINEL_QUIET: Duration = Duration::from_secs(2);
/// Minimum interval between two samples while quiet (spec D4).
pub(crate) const SENTINEL_SAMPLE_INTERVAL: Duration = Duration::from_secs(1);

/// Kernel-signal snapshot of the foreground terminal state. Plain data so
/// the classification decision table is unit-testable without a PTY.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct InteractiveSnapshot {
    pub(crate) echo_off: bool,
    pub(crate) icanon: bool,
    /// tcgetpgrp(master) differs from the shell's own process group.
    pub(crate) fg_is_foreign: bool,
    /// A foreground-group member is blocked in a tty/stdin read (Linux).
    pub(crate) blocked_tty_read: bool,
    /// comm of the blocked foreground process, wording prior only.
    pub(crate) fg_comm: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InteractiveHintKind {
    Password,
    Fullscreen { comm: Option<String> },
    Pager { comm: Option<String> },
    RawInteractive { comm: Option<String> },
    StdinWait,
}

/// Decision table (spec design.md, single source of truth). First match
/// wins; unlisted combinations fall through to `None` so a blind spot can
/// only suppress a hint, never fabricate one.
pub(crate) fn classify_interactive_state(
    snapshot: &InteractiveSnapshot,
    alt_screen_active: bool,
    static_prior: Option<&'static str>,
) -> Option<InteractiveHintKind> {
    let comm = || snapshot.fg_comm.clone();
    // Row 1: echo off with icanon retained is the password-read
    // fingerprint (read -s / getpass / sudo family); no other probed
    // class keeps icanon while dropping echo.
    if snapshot.echo_off && snapshot.icanon {
        return Some(InteractiveHintKind::Password);
    }
    // Row 2: an application owning the alternate screen is a fullscreen
    // TUI regardless of its read pattern (vi blocks in select, not read).
    if alt_screen_active {
        return Some(InteractiveHintKind::Fullscreen { comm: comm() });
    }
    if snapshot.echo_off && !snapshot.icanon {
        // Row 3: raw mode + blocked tty read matches the pager family
        // (less/more block in a /dev/tty read between keys).
        if snapshot.blocked_tty_read {
            return Some(InteractiveHintKind::Pager { comm: comm() });
        }
        // Row 4: raw mode without a blocking read (select/poll loops:
        // top, REPLs, TUIs that skipped smcup). The static prior only
        // sharpens the wording via comm-independent copy upstream.
        let _ = static_prior;
        return Some(InteractiveHintKind::RawInteractive { comm: comm() });
    }
    // Row 5: cooked mode but blocked reading stdin/tty — confirmation
    // prompts (rm -i) and bare stdin consumers (cat, wc).
    if !snapshot.echo_off && snapshot.icanon && snapshot.blocked_tty_read {
        return Some(InteractiveHintKind::StdinWait);
    }
    // Row 6: everything else (sleep, cpu-bound work, sampler errors).
    None
}

/// Sampling throttle: tracks output quiescence and the last sample time.
/// The caller feeds it output activity and asks whether to sample now.
#[derive(Debug)]
pub(crate) struct SentinelThrottle {
    last_output_at: Instant,
    last_sample_at: Option<Instant>,
}

impl SentinelThrottle {
    pub(crate) fn new() -> Self {
        Self {
            last_output_at: Instant::now(),
            last_sample_at: None,
        }
    }

    pub(crate) fn note_output(&mut self) {
        self.last_output_at = Instant::now();
        self.last_sample_at = None;
    }

    pub(crate) fn should_sample(&mut self, now: Instant) -> bool {
        if now.duration_since(self.last_output_at) < SENTINEL_QUIET {
            return false;
        }
        if let Some(last) = self.last_sample_at {
            if now.duration_since(last) < SENTINEL_SAMPLE_INTERVAL {
                return false;
            }
        }
        self.last_sample_at = Some(now);
        true
    }
}

/// Samples S1 (termios) and S3 (foreground pgrp blocking point) for the
/// given PTY master fd. Fail-quiet: every error path yields the default
/// snapshot, which classifies to `None`.
pub(crate) fn sample_interactive_state(master_fd: i32, shell_pgid: i32) -> InteractiveSnapshot {
    let mut snapshot = InteractiveSnapshot::default();
    let Ok(termios) =
        nix::sys::termios::tcgetattr(unsafe { std::os::fd::BorrowedFd::borrow_raw(master_fd) })
    else {
        return snapshot;
    };
    use nix::sys::termios::LocalFlags;
    snapshot.echo_off = !termios.local_flags.contains(LocalFlags::ECHO);
    snapshot.icanon = termios.local_flags.contains(LocalFlags::ICANON);

    let fg = unsafe { nix::libc::tcgetpgrp(master_fd) };
    if fg > 0 && fg != shell_pgid {
        snapshot.fg_is_foreign = true;
        #[cfg(target_os = "linux")]
        linux::fill_foreground_block_state(fg, &mut snapshot);
    }
    snapshot
}

/// One-shot inline hint card (spec Q6, user-decided 2026-08-04): a
/// NoticePanel-family card (#2179: same framing, width contract, and
/// plain fallback as every other panel, produced by the injected
/// [`HintCardRenderer`]) appended to the terminal stream once per
/// episode/kind, followed by a redraw of the program's own prompt tail so
/// the user's typing target visually returns to the bottom (continuity).
/// Fullscreen (alt-screen) episodes render nothing: any in-screen insert
/// would corrupt the fullscreen layout (D10 in-screen exemption).
pub(crate) fn hint_card(
    kind: &InteractiveHintKind,
    prompt_tail: &str,
    i18n: &I18n,
    input_wait_timeout_secs: u64,
    renderer: &HintCardRenderer,
) -> Option<String> {
    if matches!(kind, InteractiveHintKind::Fullscreen { .. }) {
        return None;
    }
    let kind_body = match kind {
        InteractiveHintKind::Password => i18n.t(MessageId::ShellInputWaitHintPasswordBody),
        InteractiveHintKind::Pager { .. } => i18n.t(MessageId::ShellInputWaitHintPagerBody),
        InteractiveHintKind::RawInteractive { .. } => {
            i18n.t(MessageId::ShellInputWaitHintRawInteractiveBody)
        }
        InteractiveHintKind::StdinWait => i18n.t(MessageId::ShellInputWaitHintStdinWaitBody),
        InteractiveHintKind::Fullscreen { .. } => unreachable!(),
    };
    // The prompt tail is untrusted subprocess output: it is already
    // control-stripped by the caller; redact secrets before re-displaying
    // it inside the card (the redraw below replays the same safe text).
    let (safe_tail, _) = crate::evidence::redact_sensitive_text(prompt_tail.trim_end());
    let safe_tail = safe_tail.trim().to_string();

    let mut body: Vec<String> = Vec::new();
    if safe_tail.is_empty() {
        body.push(kind_body.to_string());
    } else {
        body.push(safe_tail.clone());
        body.push(kind_body.to_string());
    }
    body.push(
        i18n.t(MessageId::ShellInputWaitHintGuidanceBody)
            .to_string(),
    );
    if input_wait_timeout_secs > 0 && timeout_eligible(kind) {
        body.push(i18n.format(
            MessageId::ShellInputWaitHintTimeoutForecastBody,
            &[("seconds", &input_wait_timeout_secs.to_string())],
        ));
    }
    let lines = renderer.render(i18n.t(MessageId::ShellInputWaitHintTitle), body);
    if lines.is_empty() {
        return None;
    }

    let mut rendered = format!("\r\n{}\r\n", lines.join("\r\n"));
    if !safe_tail.is_empty() {
        // A+ continuity redraw: re-present the prompt tail below the card
        // so the echo of the user's answer lands right after it.
        rendered.push_str(&safe_tail);
        rendered.push(' ');
    }
    Some(rendered)
}

/// Timeout eligibility for the #2161 input-wait interrupt: only kinds
/// backed by kernel-level blocked-tty-read evidence count towards the
/// `shell.input_wait_timeout_secs` timer. Fullscreen (alt-screen) is
/// exempt by decision D10; RawInteractive has no blocking read (select
/// loops such as top/REPLs), so interrupting it would act without
/// waiting evidence (fail-safe direction: never eligible).
pub(crate) fn timeout_eligible(kind: &InteractiveHintKind) -> bool {
    matches!(
        kind,
        InteractiveHintKind::Password
            | InteractiveHintKind::Pager { .. }
            | InteractiveHintKind::StdinWait
    )
}

/// Shared input-wait episode clock between the relay loop (producer: the
/// sentinel sampler) and the runtime controller (consumer: the #2161
/// input-wait timeout). Stores the monotonic-millis (see [`Self::now_ms`])
/// when the current timeout-eligible episode began; 0 means "not
/// waiting". The producer clears it on output activity, ineligible/None
/// classifications, and handoff exit, so the consumer's duration read is
/// always the length of the *current uninterrupted* wait (re-entry
/// restarts the clock).
#[derive(Debug, Clone, Default)]
pub(crate) struct InputWaitStatus {
    waiting_since_ms: Arc<AtomicU64>,
    /// Test-only pinned clock (0 = use the real monotonic clock).
    /// Shared across clones like the episode stamp so producer/consumer
    /// handles observe the same simulated time; absent from release
    /// builds so the production layout stays a single atomic.
    #[cfg(test)]
    test_clock_ms: Arc<AtomicU64>,
}

impl InputWaitStatus {
    /// Milliseconds on a process-local monotonic clock (#2168 review
    /// P1-1): an `Instant` anchor makes the interrupt timer immune to
    /// NTP/VM/container wall-clock jumps, which with `SystemTime` could
    /// fire a false SIGINT (forward jump) or never fire (backward jump).
    /// Only differences of this clock are ever used; the constant offset
    /// keeps values strictly positive (0 stays "not waiting") and leaves
    /// backdating headroom early in the process lifetime.
    ///
    /// Bounds: the `1 << 32` offset is a ~49.7-day base, and elapsed
    /// milliseconds only reach the remaining u64 range after ~584 million
    /// years of process uptime — consumers may treat this value as
    /// overflow-free for any conceivable timeout configuration.
    fn now_ms() -> u64 {
        use std::sync::OnceLock;
        static EPOCH: OnceLock<Instant> = OnceLock::new();
        let epoch = *EPOCH.get_or_init(Instant::now);
        Instant::now().duration_since(epoch).as_millis() as u64 + (1 << 32)
    }

    /// Clock reading feeding the production chain. Tests may pin it via
    /// [`Self::set_test_clock_ms`] to drive deterministic forward/backward
    /// jump scenarios through `mark_waiting`/`waiting_for`; the real path
    /// stays the lock-free `Instant` read above (#2176 review P2).
    fn clock_ms(&self) -> u64 {
        #[cfg(test)]
        {
            let pinned = self.test_clock_ms.load(Ordering::Acquire);
            if pinned != 0 {
                return pinned;
            }
        }
        Self::now_ms()
    }

    /// Marks the start of an eligible wait episode; keeps the original
    /// start if one is already running.
    pub(crate) fn mark_waiting(&self) {
        let _ = self.waiting_since_ms.compare_exchange(
            0,
            self.clock_ms(),
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }

    pub(crate) fn clear(&self) {
        self.waiting_since_ms.store(0, Ordering::Release);
    }

    /// How long the current eligible episode has lasted, if any.
    pub(crate) fn waiting_for(&self) -> Option<Duration> {
        let since = self.waiting_since_ms.load(Ordering::Acquire);
        if since == 0 {
            return None;
        }
        Some(Duration::from_millis(Self::elapsed_ms(
            self.clock_ms(),
            since,
        )))
    }

    /// Jump-defensive elapsed arithmetic: a start stamp ahead of `now`
    /// (impossible on the monotonic clock; the backward-jump failure
    /// mode of the former wall-clock implementation) clamps to zero
    /// rather than underflowing into an instant timeout.
    fn elapsed_ms(now_ms: u64, since_ms: u64) -> u64 {
        now_ms.saturating_sub(since_ms)
    }

    #[cfg(test)]
    pub(crate) fn backdate_for_test(&self, age: Duration) {
        self.waiting_since_ms.store(
            self.clock_ms().saturating_sub(age.as_millis() as u64),
            Ordering::Release,
        );
    }

    /// Pins the episode clock for tests (0 restores the real clock).
    #[cfg(test)]
    pub(crate) fn set_test_clock_ms(&self, ms: u64) {
        self.test_clock_ms.store(ms, Ordering::Release);
    }
}

/// #2025 interactive sentinel: while an agent-approved handoff command is
/// running on the foreground TTY and its output has gone quiet, sample the
/// kernel-level interactive signals and append a one-shot hint card per
/// episode/kind (Q6: card + prompt-tail redraw; fullscreen episodes render
/// nothing). User-typed commands and user-driven send_to_shell handoffs
/// never reach the sampler; every step is fail-quiet.
#[allow(clippy::too_many_arguments)]
pub(super) fn emit_interactive_hint_if_waiting<W: std::io::Write>(
    master_fd: i32,
    shell_pgid: i32,
    parser: &OscParser,
    output: &mut W,
    throttle: &mut SentinelThrottle,
    shown: &mut Option<InteractiveHintKind>,
    input_wait_status: &InputWaitStatus,
    hint_i18n: &I18n,
    input_wait_timeout_secs: u64,
    hint_card_renderer: Option<&HintCardRenderer>,
) -> std::io::Result<()> {
    let is_agent_handoff = matches!(
        parser.active_command_origin(),
        Some(CommandOrigin::AgentHandoff | CommandOrigin::ProviderTool)
    );
    if !is_agent_handoff {
        *shown = None;
        input_wait_status.clear();
        return Ok(());
    }
    if !throttle.should_sample(Instant::now()) {
        return Ok(());
    }
    let snapshot = sample_interactive_state(master_fd, shell_pgid);
    let Some(kind) = classify_interactive_state(&snapshot, parser.alt_screen_active(), None) else {
        // Signals may flap between keypresses; keep the episode's shown
        // kind so the hint is not re-printed when the state re-appears.
        // The timeout clock is conservative the other way: no current
        // evidence => restart it (#2161 fail-safe).
        input_wait_status.clear();
        return Ok(());
    };
    // #2161: only blocked-tty-read kinds accrue timeout; anything else
    // (fullscreen per D10, select loops) resets the episode clock.
    if timeout_eligible(&kind) {
        input_wait_status.mark_waiting();
    } else {
        input_wait_status.clear();
    }
    let changed = shown
        .as_ref()
        .map(std::mem::discriminant)
        .is_none_or(|seen| seen != std::mem::discriminant(&kind));
    if changed {
        // #2179 fail-quiet: no injected panel renderer means no card, but
        // the episode kind is still recorded so re-entry stays deduped.
        if let Some(renderer) = hint_card_renderer {
            // Prompt tail: the parser's incrementally maintained, bounded
            // visible tail (#2196 reviews R1/R4/R5/R7 — window anchored at
            // preexec, one escape-stripping authority with state carried
            // across chunks, retained lines capped, O(1) per sample; the
            // last non-blank visible line keeps the #2179 echo+read
            // semantics).
            let prompt_tail = parser.active_command_visible_tail();
            if let Some(card) = hint_card(
                &kind,
                &prompt_tail,
                hint_i18n,
                input_wait_timeout_secs,
                renderer,
            ) {
                output.write_all(card.as_bytes())?;
                output.flush()?;
            }
        }
        *shown = Some(kind);
    }
    Ok(())
}

#[cfg(target_os = "linux")]
mod linux {
    use super::InteractiveSnapshot;

    /// wchan symbols that are tty-read specific; kernel drift only causes
    /// missed hints (fail-quiet direction), never false ones. Generic wait
    /// symbols (e.g. `wait_woken`) are NOT listed: they also cover pipe and
    /// socket reads, so they must be confirmed by fd evidence instead.
    const TTY_READ_WCHANS: [&str; 2] = ["n_tty_read", "tty_read"];
    /// read(2) syscall numbers: x86_64 and aarch64 respectively.
    const READ_SYSCALL_NRS: [&str; 2] = ["0", "63"];
    /// Cost bound per sample (#2168 review): stop probing group members
    /// after this many, keeping the worst case fail-quiet, not slow.
    const MAX_PGRP_PROBES: usize = 32;
    #[cfg(test)]
    pub(super) const MAX_PGRP_PROBES_FOR_TEST: usize = MAX_PGRP_PROBES;

    pub(super) fn fill_foreground_block_state(fg_pgid: i32, snapshot: &mut InteractiveSnapshot) {
        // #2168 review P1-2: probe the group leader first — the common
        // single-process foreground group (read -p, sudo, pagers) then
        // resolves without scanning the whole process table.
        //
        // Assumption scope: when the leader is not the blocked reader
        // (a descendant is), this shortcut simply misses and the walk
        // below degrades to the pre-shortcut full-table scan — the
        // shortcut is budgeted separately (#2176 review P2), so all
        // MAX_PGRP_PROBES fallback slots stay available to non-leader
        // members and every member base could reach is still reached.
        // Enumeration order stays /proc-dependent exactly as before;
        // evidence is per-group (one blocked member suffices), so order
        // can only affect which member supplies the wording-only
        // `fg_comm`, never the verdict.
        let leader_in_group = process_pgid(fg_pgid) == Some(fg_pgid);
        if leader_in_group && probe_member(fg_pgid, snapshot) {
            return;
        }
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return;
        };
        let members = entries.flatten().filter_map(|entry| {
            let pid = entry.file_name().to_str()?.parse::<i32>().ok()?;
            if pid == fg_pgid && leader_in_group {
                return None;
            }
            (process_pgid(pid) == Some(fg_pgid)).then_some(pid)
        });
        scan_members(members, |pid| probe_member(pid, snapshot));
    }

    /// Walks fallback group members with the full probe budget: up to
    /// MAX_PGRP_PROBES members are probed, stopping early on the first
    /// evidence hit. Extracted so the budget contract (a blocked member
    /// in the last slot is still probed; the bound stays fail-quiet)
    /// is testable without a live /proc (#2176 review P2).
    pub(super) fn scan_members(
        members: impl Iterator<Item = i32>,
        mut probe: impl FnMut(i32) -> bool,
    ) -> bool {
        for pid in members.take(MAX_PGRP_PROBES) {
            if probe(pid) {
                return true;
            }
        }
        false
    }

    /// Probes one foreground-group member; returns true once
    /// blocked-tty-read evidence is found (evidence is per-group: one
    /// blocked member is enough, the caller stops scanning).
    ///
    /// `fg_comm` is a wording prior only (see the field docs on
    /// [`InteractiveSnapshot`]): it merely sharpens hint copy and never
    /// carries identity semantics — which member it names may depend on
    /// probe order, and that is acceptable by design.
    fn probe_member(pid: i32, snapshot: &mut InteractiveSnapshot) -> bool {
        let wchan = read_proc(pid, "wchan").unwrap_or_default();
        let syscall = read_proc(pid, "syscall").unwrap_or_default();
        // #2168 review: a blocked read(2) only counts when the fd it
        // waits on resolves to the tty — `sleep 130 | cat` blocks in a
        // pipe read on fd 0 and must never accrue input-wait timeout.
        let blocked_read = blocked_read_fd(&syscall)
            .map(|fd| fd_is_tty(pid, fd))
            .unwrap_or(false);
        if TTY_READ_WCHANS.contains(&wchan.trim()) || blocked_read {
            snapshot.blocked_tty_read = true;
            snapshot.fg_comm = member_comm(pid).or_else(|| snapshot.fg_comm.take());
            return true;
        }
        if snapshot.fg_comm.is_none() {
            snapshot.fg_comm = member_comm(pid);
        }
        false
    }

    fn member_comm(pid: i32) -> Option<String> {
        read_proc(pid, "comm")
            .map(|comm| comm.trim().to_string())
            .filter(|comm| !comm.is_empty())
    }

    /// Parses `/proc/<pid>/syscall`: the fd argument of a blocked read(2),
    /// or `None` when the process is not in a read syscall.
    pub(super) fn blocked_read_fd(syscall: &str) -> Option<u64> {
        let mut fields = syscall.split_whitespace();
        let nr = fields.next()?;
        if !READ_SYSCALL_NRS.contains(&nr) {
            return None;
        }
        let fd_arg = fields.next()?;
        u64::from_str_radix(fd_arg.trim_start_matches("0x"), 16).ok()
    }

    fn fd_is_tty(pid: i32, fd: u64) -> bool {
        std::fs::read_link(format!("/proc/{pid}/fd/{fd}"))
            .ok()
            .map(|target| tty_path(&target.to_string_lossy()))
            .unwrap_or(false)
    }

    /// Whether an fd symlink target names a terminal device.
    pub(super) fn tty_path(target: &str) -> bool {
        target.starts_with("/dev/pts/")
            || target.starts_with("/dev/tty")
            || target == "/dev/console"
    }

    fn process_pgid(pid: i32) -> Option<i32> {
        let stat = read_proc(pid, "stat")?;
        // pgrp is field 5; comm may contain spaces, split after ')'.
        let rest = stat.rsplit_once(')')?.1;
        rest.split_whitespace().nth(2)?.parse().ok()
    }

    fn read_proc(pid: i32, name: &str) -> Option<String> {
        std::fs::read_to_string(format!("/proc/{pid}/{name}")).ok()
    }
}

#[cfg(test)]
#[path = "interactive_sentinel_tests.rs"]
mod tests;
