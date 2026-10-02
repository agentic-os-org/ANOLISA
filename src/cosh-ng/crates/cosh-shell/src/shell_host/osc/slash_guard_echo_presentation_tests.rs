use super::super::super::prompt_presentation::PromptPresentation;
use super::*;

fn parser_for_test(name: &str) -> OscParser {
    let dir = std::env::temp_dir().join(format!(
        "cosh-slash-guard-presentation-{name}-{}",
        std::process::id()
    ));
    OscParser::new(name.to_string(), dir, "test-marker-token".to_string())
}

fn feed_prompt_ready(parser: &mut OscParser) {
    parser
        .feed(
            b"\x1b]1337;COSH;{\"event\":\"precmd\",\"token\":\"test-marker-token\",\"status\":0,\"cwd\":\"/tmp\",\"prompt_ready\":true}\x07",
        )
        .expect("feed prompt ready");
}

#[test]
fn reconstructed_prompt_publishes_virtual_presentation_start() {
    let mut parser = parser_for_test("reconstructed-prompt");
    parser.pending_slash_guard_echo = Some(PendingSlashGuardEcho::new_with_prompt(
        b"\x1b[?2004hprompt$ \xe4\xbd\xa0\x08\x08\x1b[K/cancel\r\x1b[K\r",
        b"\x1b[?2004hprompt$ ",
    ));
    let redraw = format!(
        "\r\x1b[K\rprompt$ {}\r\n",
        std::str::from_utf8(GUARD_COMMAND).expect("ASCII guard")
    );
    assert!(
        PendingSlashGuardEcho::filter(&mut parser.pending_slash_guard_echo, redraw.as_bytes())
            .is_empty()
    );

    parser
        .resolve_pending_slash_guard_echo("/cancel")
        .expect("resolve guard redraw");
    let mut presentation = PromptPresentation::new(true);
    presentation.observe(&mut parser);
    let mut output = Vec::new();
    presentation
        .write_range(&parser, 0, parser.display.position(), &mut output)
        .expect("present reconstructed prompt");

    assert_eq!(output, b"\r\x1b[K\rprompt$ /cancel\r\n");
}

#[test]
fn bash44_wrapped_guard_reconstructs_owned_prompt_without_internal_text() {
    let wrap_at = GUARD_COMMAND
        .windows(b";; *)".len())
        .position(|window| window == b";; *)")
        .expect("guard wrap point")
        + 3;
    for (name, repeats_previous_byte) in [("bare-cr", false), ("repeated-byte", true)] {
        let mut parser = parser_for_test(name);
        parser.pending_slash_guard_echo = Some(PendingSlashGuardEcho::new_with_prompt(
            b"prompt$ /cancel\r\x1b[K\r",
            b"prompt$ ",
        ));
        let mut redraw = b"\r\x1b[K\rprompt$ ".to_vec();
        redraw.extend_from_slice(&GUARD_COMMAND[..wrap_at]);
        redraw.push(b'\r');
        if repeats_previous_byte {
            redraw.push(GUARD_COMMAND[wrap_at - 1]);
        }
        redraw.extend_from_slice(&GUARD_COMMAND[wrap_at..]);
        redraw.extend_from_slice(b"\r\n");
        assert!(
            PendingSlashGuardEcho::filter(&mut parser.pending_slash_guard_echo, &redraw).is_empty(),
            "{name}"
        );

        parser
            .resolve_pending_slash_guard_echo("/cancel")
            .expect("resolve Bash 4.4 guard redraw");
        let mut presentation = PromptPresentation::new(true);
        presentation.observe(&mut parser);
        let mut output = Vec::new();
        presentation
            .write_range(&parser, 0, parser.display.position(), &mut output)
            .expect("present Bash 4.4 reconstructed prompt");

        assert_eq!(output, b"\r\x1b[K\rprompt$ /cancel\r\n", "{name}");
        assert!(
            !output
                .windows(b"__cosh_slash_guard__".len())
                .any(|window| { window == b"__cosh_slash_guard__" }),
            "{name}"
        );
    }
}

#[test]
fn unrelated_guard_prefix_does_not_publish_prompt_start() {
    for (name, prefix) in [
        ("no-carriage-return", "BACKGROUND_PARTIAL"),
        ("carriage-return", "BEFORE\rBACKGROUND_PARTIAL"),
    ] {
        let mut parser = parser_for_test(name);
        parser.pending_slash_guard_echo = Some(PendingSlashGuardEcho::new(b""));
        let redraw = format!(
            "{prefix}{}\r\n",
            std::str::from_utf8(GUARD_COMMAND).expect("ASCII guard")
        );
        assert!(PendingSlashGuardEcho::filter(
            &mut parser.pending_slash_guard_echo,
            redraw.as_bytes()
        )
        .is_empty());

        parser
            .resolve_pending_slash_guard_echo("/cancel")
            .expect("resolve guard redraw");
        let mut presentation = PromptPresentation::new(true);
        presentation.observe(&mut parser);
        let mut output = Vec::new();
        presentation
            .write_range(&parser, 0, parser.display.position(), &mut output)
            .expect("present unrelated prefix");

        assert_eq!(output, format!("{prefix}/cancel\r\n").as_bytes());
    }
}

#[test]
fn claimed_prompt_epoch_arms_slash_guard_without_echo_pollution() {
    let mut parser = parser_for_test("prompt-epoch-lifecycle");
    let generation = crate::raw_input::UserPtyInputGeneration::default();
    parser.set_prompt_epoch_exchange(generation.prompt_epoch_exchange());
    feed_prompt_ready(&mut parser);
    parser.feed(b"prompt$ ").expect("feed prompt");
    parser.publish_quiescent_prompt_snapshot();

    generation.bump();
    parser.feed("你".as_bytes()).expect("feed first-key echo");
    generation.bump();
    parser
        .feed(b"\x08\x08\x1b[K/cancel\r\x1b[K\r")
        .expect("feed edited line");
    parser
        .feed(b"\x1b]1337;COSH;{\"e\":\"slash_guard\",\"t\":\"test-marker-token\"}\x07")
        .expect("arm slash guard");
    assert_eq!(
        parser
            .pending_slash_guard_echo
            .as_ref()
            .expect("pending slash guard")
            .prompt_before_input,
        b"prompt$ "
    );

    feed_prompt_ready(&mut parser);
    parser
        .feed(b"\x1b]1337;COSH;{\"e\":\"slash_guard\",\"t\":\"test-marker-token\"}\x07")
        .expect("arm next epoch without claim");
    assert!(parser
        .pending_slash_guard_echo
        .as_ref()
        .expect("pending slash guard")
        .prompt_before_input
        .is_empty());
}

#[test]
fn stable_prompt_snapshot_does_not_mark_background_prefixes() {
    for (name, prefix) in [
        ("no-carriage-return", "BACKGROUND_PARTIAL"),
        ("carriage-return", "BEFORE\rBACKGROUND_PARTIAL"),
    ] {
        let mut parser = parser_for_test(name);
        parser.pending_slash_guard_echo =
            Some(PendingSlashGuardEcho::new_with_prompt(b"", b"prompt$ "));
        let redraw = format!(
            "{prefix}{}\r\n",
            std::str::from_utf8(GUARD_COMMAND).expect("ASCII guard")
        );
        assert!(PendingSlashGuardEcho::filter(
            &mut parser.pending_slash_guard_echo,
            redraw.as_bytes()
        )
        .is_empty());

        parser
            .resolve_pending_slash_guard_echo("/cancel")
            .expect("resolve background redraw");
        let mut presentation = PromptPresentation::new(true);
        presentation.observe(&mut parser);
        let mut output = Vec::new();
        presentation
            .write_range(&parser, 0, parser.display.position(), &mut output)
            .expect("present background prefix");

        assert_eq!(output, format!("{prefix}/cancel\r\n").as_bytes());
    }
}

#[test]
fn slash_guard_bounds_prompt_snapshot_without_rendering_it() {
    let snapshot = vec![b's'; MAX_PENDING_BYTES + 17];
    let pending = PendingSlashGuardEcho::new_with_prompt(b"", &snapshot);

    assert_eq!(pending.prompt_before_input.len(), MAX_PENDING_BYTES);
    assert_eq!(pending.prompt_before_input, snapshot[17..]);
    assert!(pending.before_arm.is_empty());
    assert!(pending.line.is_empty());
}

#[test]
fn direct_suffix_proof_controls_authenticated_command_insertion() {
    let resolve = |pending: &mut Option<PendingSlashGuardEcho>| {
        PendingSlashGuardEcho::resolve(pending, b"/mode").expect("suppressed guard redraw")
    };
    let guard = b"guard$ case $- in *x*) builtin set +x; builtin true __cosh_slash_guard__; builtin set -x ;; *) : ;; esac\r\n";

    let mut exact = Some(PendingSlashGuardEcho::new(b"guard$ /mode\r\n"));
    assert!(PendingSlashGuardEcho::filter(&mut exact, guard).is_empty());
    assert!(!resolve(&mut exact).insert_command);

    let mut private_rewrite = Some(PendingSlashGuardEcho::new(
        b"guard$ /mode\x08\x08\x08\x08\x08 /mode\x08\x08\x08\x08\x08/mode\r\n",
    ));
    assert!(PendingSlashGuardEcho::filter(&mut private_rewrite, guard).is_empty());
    assert!(!resolve(&mut private_rewrite).insert_command);

    let mut rewrite_mismatch = Some(PendingSlashGuardEcho::new(
        b"guard$ /mode\x08\x08\x08\x08 /mode\x08\x08\x08\x08\x08/mode\r\n",
    ));
    assert!(PendingSlashGuardEcho::filter(&mut rewrite_mismatch, guard).is_empty());
    assert!(resolve(&mut rewrite_mismatch).insert_command);

    let mut dirty = Some(PendingSlashGuardEcho::new(b"guard$ /mo\x1b[?2004hde\r\n"));
    assert!(PendingSlashGuardEcho::filter(&mut dirty, guard).is_empty());
    assert!(resolve(&mut dirty).insert_command);
}

#[test]
fn private_prompt_repaint_requires_exact_carriage_return_boundary() {
    assert!(PendingSlashGuardEcho::proves_painted_command(
        b"previous bytes\rguard$  /mode",
        b"/mode",
        b"guard$ ",
    ));
    for unproven in [
        b"previous bytesguard$  /mode".as_slice(),
        b"previous bytes\rother$  /mode",
        b"previous bytes\rguard$   /mode",
        b"previous bytes\rguard$  /mode extra",
    ] {
        assert!(!PendingSlashGuardEcho::proves_painted_command(
            unproven, b"/mode", b"guard$ ",
        ));
    }
}
