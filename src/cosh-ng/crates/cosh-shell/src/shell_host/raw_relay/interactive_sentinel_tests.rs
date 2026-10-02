use super::*;

use super::super::super::osc::{VisibleTailTracker, VISIBLE_TAIL_MAX_CHARS};

/// Test shim over the bounded tracker (#2196 R7): every input is fed
/// twice — whole-buffer and byte-at-a-time — so each assertion also pins
/// cross-chunk escape-state and UTF-8 carry invariance.
fn prompt_tail_from_display(display: &[u8]) -> String {
    let mut whole = VisibleTailTracker::default();
    whole.feed(display);
    let mut split = VisibleTailTracker::default();
    for byte in display {
        split.feed(std::slice::from_ref(byte));
    }
    let tail = whole.tail();
    assert_eq!(
        tail,
        split.tail(),
        "chunk-split feed must agree: {display:?}"
    );
    tail
}

fn snap(echo_off: bool, icanon: bool, blocked: bool) -> InteractiveSnapshot {
    InteractiveSnapshot {
        echo_off,
        icanon,
        fg_is_foreign: true,
        blocked_tty_read: blocked,
        fg_comm: Some("probe".to_string()),
    }
}

#[test]
fn decision_table_rows_match_probe_matrix_anchors() {
    // Row 1: read -s / getpass / sudo fingerprint.
    assert_eq!(
        classify_interactive_state(&snap(true, true, true), false, None),
        Some(InteractiveHintKind::Password)
    );
    // Row 2: alt-screen wins for any snapshot (vi, less with smcup).
    assert!(matches!(
        classify_interactive_state(&snap(true, false, false), true, None),
        Some(InteractiveHintKind::Fullscreen { .. })
    ));
    // Row 3: less/more between keys.
    assert!(matches!(
        classify_interactive_state(&snap(true, false, true), false, None),
        Some(InteractiveHintKind::Pager { .. })
    ));
    // Row 4: top / python repl (select loop, no blocking read).
    assert!(matches!(
        classify_interactive_state(&snap(true, false, false), false, None),
        Some(InteractiveHintKind::RawInteractive { .. })
    ));
    // Row 5: rm -i / cat.
    assert_eq!(
        classify_interactive_state(&snap(false, true, true), false, None),
        Some(InteractiveHintKind::StdinWait)
    );
    // Row 6: sleep (cooked, no blocked read) and busy loop.
    assert_eq!(
        classify_interactive_state(&snap(false, true, false), false, None),
        None
    );
    assert_eq!(
        classify_interactive_state(&snap(false, false, false), false, None),
        None
    );
}

#[test]
fn sampler_error_snapshot_classifies_to_none() {
    assert_eq!(
        classify_interactive_state(&InteractiveSnapshot::default(), false, None),
        None
    );
}

#[test]
fn missing_comm_keeps_classification() {
    let mut snapshot = snap(true, false, true);
    snapshot.fg_comm = None;
    assert_eq!(
        classify_interactive_state(&snapshot, false, None),
        Some(InteractiveHintKind::Pager { comm: None })
    );
}

#[test]
fn static_prior_never_gates_classification() {
    assert_eq!(
        classify_interactive_state(&snap(false, true, false), false, Some("needs a tty")),
        None
    );
    assert_eq!(
        classify_interactive_state(&snap(true, true, false), false, Some("needs a tty")),
        Some(InteractiveHintKind::Password)
    );
}

#[test]
fn throttle_requires_quiet_window_and_interval() {
    let mut throttle = SentinelThrottle::new();
    let start = Instant::now();
    assert!(!throttle.should_sample(start));
    let quiet = start + SENTINEL_QUIET + Duration::from_millis(10);
    assert!(throttle.should_sample(quiet));
    assert!(!throttle.should_sample(quiet + Duration::from_millis(100)));
    assert!(throttle.should_sample(quiet + SENTINEL_SAMPLE_INTERVAL + Duration::from_millis(10)));
    throttle.note_output();
    assert!(!throttle.should_sample(Instant::now()));
}

#[test]
fn timeout_eligibility_requires_blocked_read_evidence() {
    // Eligible: blocked-tty-read kinds (#2161 harm classes).
    assert!(timeout_eligible(&InteractiveHintKind::Password));
    assert!(timeout_eligible(&InteractiveHintKind::StdinWait));
    assert!(timeout_eligible(&InteractiveHintKind::Pager { comm: None }));
    // Exempt: alt-screen TUIs (D10) and evidence-free select loops.
    assert!(!timeout_eligible(&InteractiveHintKind::Fullscreen {
        comm: None
    }));
    assert!(!timeout_eligible(&InteractiveHintKind::RawInteractive {
        comm: None
    }));
}

#[test]
fn hint_card_covers_presentation_contract() {
    let i18n = crate::i18n::I18n::new(crate::config::Language::EnUs);
    // The layout gate forbids shell_host tests from reaching into the UI
    // layer, so this fake stands in for the injected panel renderer; the
    // real NoticePanel framing is asserted in ui/agent_render tests and
    // the production closure lives in the runtime bootstrap.
    let renderer = crate::shell_host::HintCardRenderer::new(|title, body| {
        let mut lines = vec![format!("[{title}]")];
        lines.extend(body);
        lines
    });
    // Fullscreen: in-screen insert exempt (D10) => nothing rendered.
    assert_eq!(
        hint_card(
            &InteractiveHintKind::Fullscreen { comm: None },
            "anything",
            &i18n,
            120,
            &renderer
        ),
        None
    );
    // Prompt tail present: card shows the tail and redraws it after.
    let card = hint_card(
        &InteractiveHintKind::StdinWait,
        "Confirm? (y/n):",
        &i18n,
        120,
        &renderer,
    )
    .expect("card rendered");
    assert!(card.contains("Command is waiting for input"), "{card}");
    assert!(
        card.matches("Confirm? (y/n):").count() >= 2,
        "tail must appear in the card body and in the redraw: {card}"
    );
    assert!(card.contains("Auto-interrupts after 120s"), "{card}");
    assert!(card.trim_end().ends_with("Confirm? (y/n):"), "{card}");
    // The renderer owns the framing: sentinel lines reach it unframed and
    // come back joined with raw-mode `\r\n` endings.
    assert!(card.starts_with("\r\n["), "{card}");
    // Timeout disabled: no forecast line.
    let card = hint_card(
        &InteractiveHintKind::StdinWait,
        "Confirm? (y/n):",
        &i18n,
        0,
        &renderer,
    )
    .expect("card rendered");
    assert!(!card.contains("Auto-interrupts"), "{card}");
    // Empty tail (e.g. read -s without prompt text): kind body only,
    // no redraw suffix.
    let card = hint_card(&InteractiveHintKind::Password, "", &i18n, 120, &renderer)
        .expect("card rendered");
    assert!(card.contains("password/hidden input"), "{card}");
    assert!(card.ends_with("\r\n"), "{card}");
    // A renderer that yields nothing (e.g. plain sink filtered it away)
    // collapses to no card at all, never a bare frame.
    let empty = crate::shell_host::HintCardRenderer::new(|_, _| Vec::new());
    assert_eq!(
        hint_card(&InteractiveHintKind::Password, "", &i18n, 120, &empty),
        None
    );
}

#[test]
fn prompt_tail_extraction_strips_escapes_and_controls() {
    let display = b"line one\n\x1b[1;32mConfirm?\x1b[0m (y/n): \x07";
    assert_eq!(prompt_tail_from_display(display), "Confirm? (y/n): ");
    assert_eq!(prompt_tail_from_display(b""), "");
    let osc = b"step\n\x1b]0;title\x07Grant it? ";
    assert_eq!(prompt_tail_from_display(osc), "Grant it? ");
    // #2179: `echo "prompt"` + `read` leaves the final line empty; the
    // extractor walks back to the last non-blank line so the card still
    // echoes and replays the program's own question.
    assert_eq!(
        prompt_tail_from_display(b"=== step ===\nType y or n:\n"),
        "Type y or n:"
    );
    assert_eq!(
        prompt_tail_from_display(b"Type y or n:\n\x1b[0m \n"),
        "Type y or n:"
    );
    assert_eq!(prompt_tail_from_display(b"\n\n\n"), "");

    // #2196 review R4 matrix: stripping is a single stateful pass before
    // line anchoring, so string-family payloads spanning LF never surface
    // as fake prompt lines. Rows: introducer kind x terminator x trailing
    // visible text (PM/SOS share the same introducer branch as APC).
    assert_eq!(
        prompt_tail_from_display(b"\x1b]0;hidden\nFAKE\x07"),
        "",
        "OSC payload spanning LF, BEL-terminated: nothing visible"
    );
    assert_eq!(
        prompt_tail_from_display(b"\x1b]0;hidden\nFAKE\x07Type y or n: "),
        "Type y or n: ",
        "text after the BEL terminator is the real prompt"
    );
    assert_eq!(
        prompt_tail_from_display(b"\x1b]0;hidden\nmore\x1b\\Grant it? "),
        "Grant it? ",
        "OSC payload spanning LF, ST-terminated"
    );
    assert_eq!(
        prompt_tail_from_display(b"echo done\n\x1bPdcs\npayload\x1b\\prompt: "),
        "prompt: ",
        "DCS payload spanning LF, ST-terminated"
    );
    assert_eq!(
        prompt_tail_from_display(b"\x1b_apc\nrest\x1b\\ok: "),
        "ok: ",
        "APC payload spanning LF, ST-terminated"
    );
    assert_eq!(
        prompt_tail_from_display(b"\x1b^pm\nrest\x1b\\"),
        "",
        "PM payload spanning LF: fully hidden"
    );
    // #2196 review R5: BEL terminates OSC only; APC/DCS/SOS/PM run until
    // ST, so a BEL inside the payload must not release the rest as text.
    assert_eq!(
        prompt_tail_from_display(b"\x1b_hidden\x07FAKE\x1b\\\n"),
        "",
        "APC containing BEL, ST-terminated, trailing LF: nothing visible"
    );
    assert_eq!(
        prompt_tail_from_display(b"\x1bPhidden\x07still-hidden\x1b\\Type y or n: \n"),
        "Type y or n: ",
        "DCS containing BEL: only text after ST is the prompt"
    );
    assert_eq!(
        prompt_tail_from_display(b"before\n\x1b]0;unterminated\nFAKE"),
        "before",
        "unterminated payload is consumed to the window end (fail-safe)"
    );
    // Multi-byte chars survive the byte-at-a-time feed (UTF-8 carry).
    assert_eq!(
        prompt_tail_from_display("回答完毕\n请输入密码: ".as_bytes()),
        "请输入密码: "
    );

    // #2196 R7 bounds: a handoff that logs heavily before blocking must
    // not require rescanning its output — the tracker is incremental and
    // every retained line is capped.
    let mut heavy = VisibleTailTracker::default();
    for index in 0..100_000 {
        heavy.feed(format!("log line {index} with padding\n").as_bytes());
    }
    heavy.feed(b"\x1b[1mType y or n: \x1b[0m");
    assert_eq!(heavy.tail(), "Type y or n: ");
    // #2196 R8: an over-long line loses tail candidacy outright — keeping
    // only its trailing window would hand redaction a suffix with the
    // sensitive field-name prefix cut off (`password=` + 600 chars), so
    // the sanitized secret would reach the card verbatim.
    let mut secret_line = VisibleTailTracker::default();
    let mut assignment = b"password=".to_vec();
    assignment.extend(std::iter::repeat_n(b'a', 600));
    secret_line.feed(&assignment);
    assert_eq!(
        secret_line.tail(),
        "",
        "an over-long unfinished line must not surface any suffix"
    );
    let (redacted, _) = crate::evidence::redact_sensitive_text(&secret_line.tail());
    assert!(!redacted.contains('a'), "nothing to redact, nothing leaked");
    // R9: completing the over-long line is a barrier — the tail must not
    // fall back to output older than it (`Status` here), it stays empty
    // while the command blocks silently after the newline.
    let mut barrier = VisibleTailTracker::default();
    barrier.feed(b"Status\n");
    barrier.feed(&assignment);
    barrier.feed(b"\n");
    assert_eq!(
        barrier.tail(),
        "",
        "an over-long line must not fall back to earlier output"
    );
    // After the barrier, a normal prompt line recovers.
    barrier.feed(b"Type y or n: ");
    assert_eq!(barrier.tail(), "Type y or n: ");
    secret_line.feed(b"\nType y or n: ");
    assert_eq!(secret_line.tail(), "Type y or n: ");

    // #2196 review: the tail is anchored on the active command's display
    // window, so a previous command's output is never replayed as the
    // prompt (a bare `read` with no output yields no tail at all).
    let mut parser = OscParser::new(
        "sentinel-prompt-window".to_string(),
        std::env::temp_dir().join("cosh-sentinel-prompt-window"),
        "test-marker-token".to_string(),
    );
    parser
        .feed(b"stale previous output\n")
        .expect("feed stale output");
    assert_eq!(
        parser.active_command_visible_tail(),
        "",
        "no active command means no window, even with session history"
    );
    parser
        .feed(b"\x1b]1337;COSH;{\"event\":\"preexec\",\"token\":\"test-marker-token\",\"session_id\":\"sentinel-prompt-window\",\"command\":\"read -s secret\",\"cwd\":\"/tmp\"}\x07")
        .expect("feed preexec");
    assert_eq!(
        parser.active_command_visible_tail(),
        "",
        "a silent command must not inherit the previous command's output"
    );
    parser.feed(b"Type y or n:\n").expect("feed prompt");
    assert_eq!(parser.active_command_visible_tail(), "Type y or n:");
}

#[test]
fn prompt_tail_strips_string_family_and_nf_variants() {
    // Terminator semantics are owned by the delegated
    // `evidence::clean_terminal_control_sequences` (#2196 review R5):
    // ST always terminates, BEL terminates OSC only.
    assert_eq!(
        prompt_tail_from_display(b"\x1bPpayload\x1b\\Continue? "),
        "Continue? "
    );
    assert_eq!(
        prompt_tail_from_display(b"\x1b_meta\x07Proceed? "),
        "",
        "BEL does not end APC; the whole remainder stays hidden"
    );
    // Charset selection `ESC ( B` drops the final byte too.
    assert_eq!(prompt_tail_from_display(b"\x1b(BAnswer: "), "Answer: ");
    // ECMA-48 two-byte escapes consume their final byte (#2196 review R6:
    // terminfo smkx ends in `ESC =`, so a pager starting under a handoff
    // must not surface `=` as the prompt).
    assert_eq!(prompt_tail_from_display(b"\x1b=ok\x1b"), "ok");
    assert_eq!(
        prompt_tail_from_display(b"\x1b[?1h\x1b="),
        "",
        "keypad-mode init alone leaves no visible prompt"
    );
    // An orphan trailing ESC stays silent.
    assert_eq!(prompt_tail_from_display(b"ok\x1b"), "ok");
    // Unterminated string sequences swallow the tail (fail-quiet:
    // an empty tail suppresses the redraw, never shows raw bytes).
    assert_eq!(prompt_tail_from_display(b"\x1b]0;title"), "");
}

#[cfg(target_os = "linux")]
#[test]
fn linux_probe_requires_tty_evidence_for_blocked_reads() {
    // read(2) on both supported arches parses the fd argument.
    assert_eq!(linux::blocked_read_fd("0 0x0 0x7ffd 0x2000"), Some(0));
    assert_eq!(linux::blocked_read_fd("63 0x3 0x7ffd 0x2000"), Some(3));
    // Non-read syscalls and running processes never count.
    assert_eq!(linux::blocked_read_fd("7 0x0"), None);
    assert_eq!(linux::blocked_read_fd("running"), None);
    // Only terminal device targets qualify as tty evidence.
    assert!(linux::tty_path("/dev/pts/3"));
    assert!(linux::tty_path("/dev/tty1"));
    assert!(linux::tty_path("/dev/console"));
    assert!(!linux::tty_path("pipe:[12345]"));
    assert!(!linux::tty_path("/tmp/file"));
    assert!(!linux::tty_path("socket:[999]"));
}

#[cfg(target_os = "linux")]
#[test]
fn linux_fallback_scan_keeps_full_probe_budget() {
    // #2176 review P2: a missed leader shortcut must not consume
    // fallback slots — a blocked reader sitting in the *last* budget
    // slot (the 32nd non-leader member in /proc order) is still probed
    // and still produces the verdict, exactly as the pre-shortcut scan
    // did.
    let budget = linux::MAX_PGRP_PROBES_FOR_TEST;
    let blocked_pid = budget as i32; // last member inside the budget
    let mut probes = Vec::new();
    let hit = linux::scan_members(1..=blocked_pid, |pid| {
        probes.push(pid);
        pid == blocked_pid
    });
    assert!(hit, "evidence in the last budget slot must be found");
    assert_eq!(probes.len(), budget, "every budget slot must be probed");

    // The bound itself stays fail-quiet: a member beyond the budget is
    // never probed, and the scan reports no evidence.
    let mut probed_beyond = false;
    let over_budget = blocked_pid + 1;
    let hit = linux::scan_members(1..=over_budget, |pid| {
        probed_beyond |= pid == over_budget;
        pid == over_budget
    });
    assert!(!hit);
    assert!(!probed_beyond, "budget bound must cap the scan");

    // Early evidence short-circuits the walk.
    let mut count = 0;
    assert!(linux::scan_members(1..=blocked_pid, |pid| {
        count += 1;
        pid == 3
    }));
    assert_eq!(count, 3);
}

#[test]
fn input_wait_status_marks_clears_and_restarts() {
    let status = InputWaitStatus::default();
    assert_eq!(status.waiting_for(), None);
    status.mark_waiting();
    let first = status.waiting_for().expect("episode running");
    // A second mark keeps the original episode start.
    status.backdate_for_test(Duration::from_secs(30));
    status.mark_waiting();
    let aged = status.waiting_for().expect("episode still running");
    assert!(aged >= Duration::from_secs(30), "{aged:?}");
    assert!(aged >= first);
    // Clearing resets; a new mark restarts from zero.
    status.clear();
    assert_eq!(status.waiting_for(), None);
    status.mark_waiting();
    let restarted = status.waiting_for().expect("restarted episode");
    assert!(restarted < Duration::from_secs(30), "{restarted:?}");
}

#[test]
fn input_wait_clock_is_jump_defensive() {
    // Drive wall-clock jump scenarios through the production call chain
    // (`mark_waiting` -> `waiting_for`) on a pinned episode clock
    // (#2176 review P2): the consumer-visible duration must reflect
    // exactly the episode-clock delta, never a jump artifact.
    let base = 1u64 << 32;
    let status = InputWaitStatus::default();

    // Forward jump shape: a timeout may only fire after the configured
    // wait truly elapsed on the episode clock. With the former
    // `SystemTime` chain a wall jump inflated this reading and fired a
    // false SIGINT seconds into a legitimate wait.
    status.set_test_clock_ms(base);
    status.mark_waiting();
    assert_eq!(status.waiting_for(), Some(Duration::ZERO));
    status.set_test_clock_ms(base + 120_000);
    assert_eq!(status.waiting_for(), Some(Duration::from_millis(120_000)));

    // Backward jump artifact (start stamp ahead of "now"): re-mark at a
    // late reading, then pull the clock back — the duration clamps to
    // zero instead of underflowing into a giant elapsed value; the
    // episode keeps running and resumes counting past the stamp.
    status.clear();
    status.set_test_clock_ms(base + 120_000);
    status.mark_waiting();
    status.set_test_clock_ms(base + 115_000);
    assert_eq!(status.waiting_for(), Some(Duration::ZERO));
    status.set_test_clock_ms(base + 180_000);
    assert_eq!(status.waiting_for(), Some(Duration::from_millis(60_000)));

    // A producer clear/re-mark under the pinned clock restarts the
    // episode from the current reading, not from any stale stamp.
    status.clear();
    status.mark_waiting();
    assert_eq!(status.waiting_for(), Some(Duration::ZERO));

    // Clock-source discriminator: the real readings are anchored to a
    // process-local `Instant` epoch, so `now_ms - offset` is process
    // uptime — orders of magnitude below UNIX-epoch milliseconds
    // (~1.7e12 in 2026). Reverting `now_ms` to a `SystemTime`-derived
    // value trips this bound immediately.
    let uptime_ms = InputWaitStatus::now_ms() - base;
    assert!(
        uptime_ms < 1_000_000_000_000, // ~31.7 years of process uptime
        "now_ms looks wall-clock derived: {uptime_ms}ms past offset"
    );
    // Monotonic readings are strictly non-decreasing across calls, and
    // the offset keeps every reading past the "not waiting" sentinel
    // with backdating headroom.
    let a = InputWaitStatus::now_ms();
    let b = InputWaitStatus::now_ms();
    assert!(b >= a, "monotonic clock went backwards: {a} -> {b}");
    assert!(a >= base);
}
