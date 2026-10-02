use super::marker::{bash_marker_script, zsh_marker_script};
use super::model::{
    ShellEnvironmentObserver, ShellHistoryFileObserver, ShellHostConfig, ShellIntegration,
};
use super::osc::*;
use super::raw_runner::run_raw_relay_bash_with_actions;
use super::transcript::TranscriptRetention;
use crate::ledger::build_command_blocks;
use crate::raw_input::RawRelayAction;
use crate::types::{
    CommandOrigin, ShellEventKind, ShellHandoffRequest, COMMAND_OUTPUT_REF_MAX_BYTES,
    SESSION_OUTPUT_REF_MAX_BYTES,
};
use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, Mutex};

const TEST_MARKER_TOKEN: &str = "test-marker-token";

#[test]
fn bounded_raw_session_spools_output_without_materializing_result() {
    let work_dir = std::env::temp_dir().join(format!(
        "cosh-bounded-raw-session-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&work_dir);
    let mut config = ShellHostConfig::new("bounded-raw-session", &work_dir);
    config.integration = ShellIntegration::Enhanced;
    config.native_mode = false;
    config.bound_interactive_transcript();

    let mut rendered = Vec::new();
    let result = run_raw_relay_bash_with_actions(
        &config,
        vec![RawRelayAction::line("yes x | head -c 700000")],
        &mut rendered,
    )
    .expect("bounded raw session");

    assert!(result.terminal_output.is_empty());
    assert!(result.events.is_empty());
    assert!(rendered.len() >= 700_000);
    let journal = std::fs::read_to_string(&result.journal_path).expect("event journal");
    assert!(journal.lines().count() >= 4, "{journal}");
    let spool_paths = std::fs::read_dir(&work_dir)
        .expect("session files")
        .map(|entry| entry.expect("session entry").path())
        .filter(|path| {
            path.file_name().is_some_and(|name| {
                let name = name.to_string_lossy();
                name.starts_with("terminal-output-") || name.starts_with("display-")
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(spool_paths.len(), 2);
    for path in spool_paths {
        let metadata = std::fs::metadata(&path).expect("spool metadata");
        assert!(metadata.len() >= 700_000, "{}", path.display());
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
    }
    let _ = std::fs::remove_dir_all(work_dir);
}

#[test]
fn routing_markers_require_matching_attempt_generation() {
    let mut parser = parser_for_test("routing-generation");
    parser
        .feed(b"\x1b]1337;COSH;{\"event\":\"preexec\",\"token\":\"test-marker-token\",\"session_id\":\"routing-generation\",\"command\":\"Who are you\",\"cwd\":\"/tmp\",\"generation\":2}\x07")
        .expect("feed preexec");
    parser
        .feed(b"\x1b]1337;COSH;{\"event\":\"top_level_missing\",\"token\":\"test-marker-token\",\"session_id\":\"routing-generation\",\"generation\":1,\"proven\":true,\"intent\":\"ambiguous\",\"sensitive\":false,\"unsafe\":false}\x07")
        .expect("feed stale provenance");
    parser
        .feed(b"\x1b]1337;COSH;{\"event\":\"intercept\",\"token\":\"test-marker-token\",\"session_id\":\"routing-generation\",\"command\":\"stale input\",\"reason\":\"natural_language\",\"generation\":1,\"top_level_missing\":true}\x07")
        .expect("feed stale intercept");

    let stale = parser
        .events
        .iter()
        .find(|event| event.kind == ShellEventKind::CommandRoutingObserved)
        .expect("stale provenance event");
    assert!(stale.command_id.is_none());
    assert!(stale.routing.as_ref().is_some_and(|routing| {
        routing.top_level_missing && !routing.proven && routing.generation == 1
    }));
    assert!(!parser.events.iter().any(|event| {
        event.kind == ShellEventKind::UserInputIntercepted
            && event.input.as_deref() == Some("stale input")
    }));

    parser
        .feed(b"\x1b]1337;COSH;{\"event\":\"intercept\",\"token\":\"test-marker-token\",\"session_id\":\"routing-generation\",\"command\":\"Who are you\",\"reason\":\"natural_language\",\"generation\":2,\"top_level_missing\":true}\x07")
        .expect("feed matching intercept");

    let intercept = parser
        .events
        .iter()
        .find(|event| {
            event.kind == ShellEventKind::UserInputIntercepted
                && event.input.as_deref() == Some("Who are you")
        })
        .expect("matching intercept");
    assert_eq!(intercept.command_id.as_deref(), Some("cmd-1"));
    let ledger = build_command_blocks(&parser.events);
    assert!(ledger.errors.is_empty(), "{:?}", ledger.errors);
    assert!(ledger.blocks.is_empty());
}

#[test]
fn sensitive_intercepts_preserve_routing_and_request_context() {
    let mut parser = parser_for_test("routing-sensitive");
    parser
        .feed(b"\x1b]1337;COSH;{\"event\":\"preexec\",\"token\":\"test-marker-token\",\"session_id\":\"routing-sensitive\",\"command\":\"<redacted sensitive command>\",\"cwd\":\"/tmp\",\"generation\":1}\x07")
        .expect("feed preexec");
    parser
        .feed("\x1b]1337;COSH;{\"event\":\"intercept\",\"token\":\"test-marker-token\",\"session_id\":\"routing-sensitive\",\"command\":\"帮我安装下openclaw,模型使用qwen3.8-max,API Key: sk-fbaa6\",\"reason\":\"natural_language\",\"generation\":1,\"top_level_missing\":true,\"sensitive\":true}\x07".as_bytes())
        .expect("feed sensitive intercept");

    let intercept = parser
        .events
        .iter()
        .find(|event| event.kind == ShellEventKind::UserInputIntercepted)
        .expect("sensitive intercept event");
    // The raw input still reaches the agent path in memory; only durable
    // sinks (journal) redact it, keyed off the sensitive routing flag.
    assert!(intercept
        .input
        .as_deref()
        .is_some_and(|input| input.contains("sk-fbaa6")));
    assert!(intercept.routing.as_ref().is_some_and(|routing| {
        routing.sensitive && routing.top_level_missing && routing.proven
    }));
    raw_input_sensitive_intercept_reaches_routing_metadata();
}

fn raw_input_sensitive_intercept_reaches_routing_metadata() {
    let mut parser = parser_for_test("raw-routing-sensitive");
    parser.push_intercept_event_with_routing(
        "raw-routing-sensitive",
        "打开./missing API Key: sk-fbaa6".to_string(),
        Some("/workspace/after-cd".to_string()),
        "natural_language",
        None,
        false,
        true,
    );

    let intercept = parser
        .events
        .iter()
        .find(|event| event.kind == ShellEventKind::UserInputIntercepted)
        .expect("sensitive raw intercept event");
    assert!(intercept
        .routing
        .as_ref()
        .is_some_and(|routing| routing.sensitive && !routing.top_level_missing));
    let request = crate::parser::agent_request_from_intercepted_input(intercept, 1, true)
        .expect("agent request");
    assert_eq!(request.command_block.cwd, "/workspace/after-cd");
}

#[test]
fn intercept_marker_without_sensitive_field_defaults_to_not_sensitive() {
    let mut parser = parser_for_test("routing-legacy");
    parser
        .feed(b"\x1b]1337;COSH;{\"event\":\"intercept\",\"token\":\"test-marker-token\",\"session_id\":\"routing-legacy\",\"command\":\"/skills detail\",\"reason\":\"slash\",\"cwd\":\"/tmp\"}\x07")
        .expect("feed legacy intercept");

    let intercept = parser
        .events
        .iter()
        .find(|event| event.kind == ShellEventKind::UserInputIntercepted)
        .expect("legacy intercept event");
    // Legacy markers (no sensitive field, no top-level-missing provenance)
    // keep the pre-#2138 shape: no routing metadata at all.
    assert!(intercept.routing.is_none());
}

#[test]
fn unterminated_cosh_candidate_resyncs_to_next_marker() {
    let mut parser = parser_for_test("unterminated-resync");
    parser.feed(b"before\r\n").expect("feed visible prefix");
    parser
        .feed(b"\x1b]1337;COSH;{\"event\":\"intercept\",\"token\":\"test-marker-token\",\"command\":\"INTERNAL_CANDIDATE\"")
        .expect("feed unterminated candidate");
    parser
        .feed(b"\x1b]1337;")
        .expect("feed fragmented restart prefix");
    parser
        .feed(b"COSH;{\"event\":\"precmd\",\"token\":\"test-marker-token\",\"status\":0,\"cwd\":\"/tmp\"}\x07after\r\n")
        .expect("feed valid marker after candidate");

    assert_eq!(parser.precmd_count(), 1);
    assert!(!parser
        .events
        .iter()
        .any(|event| event.kind == ShellEventKind::UserInputIntercepted));
    assert!(!parser
        .events
        .iter()
        .any(|event| event.kind == ShellEventKind::ComponentFailed));
    assert_eq!(parser.display.resident_slice(), b"before\r\nafter\r\n");
    assert_eq!(parser.clean.resident_slice(), b"before\r\nafter\r\n");
}

#[test]
fn nonzero_scan_resyncs_to_last_of_multiple_prefixes() {
    let mut parser = parser_for_test("unterminated-multiple-resync");
    parser
        .feed(b"\x1b]1337;COSH;stale-candidate-with-a-scanned-tail")
        .expect("feed initial candidate");
    parser
        .feed(b"\x1b]1337;COSH;second-stale\x1b]1337;COSH;{\"event\":\"precmd\",\"token\":\"test-marker-token\",\"status\":0,\"cwd\":\"/tmp\"}\x07visible\r\n")
        .expect("feed multiple restart candidates");

    assert_eq!(parser.precmd_count(), 1);
    assert_eq!(parser.display.resident_slice(), b"visible\r\n");
    assert_eq!(parser.clean.resident_slice(), b"visible\r\n");
    assert!(!parser
        .events
        .iter()
        .any(|event| event.kind == ShellEventKind::UserInputIntercepted));
    assert!(!parser
        .events
        .iter()
        .any(|event| event.kind == ShellEventKind::ComponentFailed));
}

#[test]
fn oversized_unterminated_cosh_candidate_recovers_without_leaking() {
    let mut parser = parser_for_test("unterminated-cap");
    let mut candidate = b"\x1b]1337;COSH;".to_vec();
    candidate.extend(std::iter::repeat_n(b'x', 64 * 1024));

    parser.feed(&candidate).expect("feed oversized candidate");
    parser
        .feed(b"visible-after-cap\r\n")
        .expect("feed output after candidate cap");

    assert_eq!(parser.display.resident_slice(), b"visible-after-cap\r\n");
    assert_eq!(parser.clean.resident_slice(), b"visible-after-cap\r\n");
    assert!(parser.events.is_empty());
}

#[test]
fn oversized_candidate_preserves_fragmented_restart_prefix() {
    let mut parser = parser_for_test("unterminated-cap-fragment");
    let mut candidate = b"\x1b]1337;COSH;".to_vec();
    candidate.extend(std::iter::repeat_n(b'x', 64 * 1024));
    candidate.extend_from_slice(b"\x1b]1337;");

    parser
        .feed(&candidate)
        .expect("feed capped prefix fragment");
    parser
        .feed(b"COSH;{\"event\":\"precmd\",\"token\":\"test-marker-token\",\"status\":0,\"cwd\":\"/tmp\"}\x07visible\r\n")
        .expect("finish valid marker after cap");

    assert_eq!(parser.precmd_count(), 1);
    assert_eq!(parser.display.resident_slice(), b"visible\r\n");
    assert_eq!(parser.clean.resident_slice(), b"visible\r\n");
}

#[test]
fn oversized_candidate_drops_stale_prefix_before_fresh_marker() {
    let mut parser = parser_for_test("unterminated-cap-fresh-marker");
    let mut candidate = b"\x1b]1337;COSH;".to_vec();
    candidate.extend(std::iter::repeat_n(b'x', 64 * 1024));
    candidate.extend_from_slice(b"\x1b]1337;");

    parser
        .feed(&candidate)
        .expect("feed capped prefix fragment");
    parser
        .feed(b"\x1b]1337;COSH;{\"event\":\"precmd\",\"token\":\"test-marker-token\",\"status\":0,\"cwd\":\"/tmp\"}\x07visible\r\n")
        .expect("feed fresh marker after capped fragment");

    assert_eq!(parser.precmd_count(), 1);
    assert_eq!(parser.display.resident_slice(), b"visible\r\n");
    assert_eq!(parser.clean.resident_slice(), b"visible\r\n");
}

#[test]
fn oversized_candidate_drops_stale_prefix_and_keeps_output() {
    let mut parser = parser_for_test("unterminated-cap-stale-prefix");
    let mut candidate = b"\x1b]1337;COSH;".to_vec();
    candidate.extend(std::iter::repeat_n(b'x', 64 * 1024));
    candidate.extend_from_slice(b"\x1b]1337;");

    parser
        .feed(&candidate)
        .expect("feed capped prefix fragment");
    parser
        .feed(b"visible-after-cap\r\n")
        .expect("feed output after capped fragment");

    assert_eq!(parser.display.resident_slice(), b"visible-after-cap\r\n");
    assert_eq!(parser.clean.resident_slice(), b"visible-after-cap\r\n");
    assert!(parser.events.is_empty());
}

#[test]
fn oversized_candidate_keeps_tentative_output_after_mismatch() {
    let mut parser = parser_for_test("unterminated-cap-tentative-output");
    let mut candidate = b"\x1b]1337;COSH;".to_vec();
    candidate.extend(std::iter::repeat_n(b'x', 64 * 1024));
    candidate.extend_from_slice(b"\x1b]1337;");

    parser
        .feed(&candidate)
        .expect("feed capped prefix fragment");
    parser.feed(b"CO").expect("feed tentative continuation");
    parser
        .feed(b"PY_UNIQUE_OUTPUT\r\n")
        .expect("feed mismatching output");

    assert_eq!(parser.display.resident_slice(), b"COPY_UNIQUE_OUTPUT\r\n");
    assert_eq!(parser.clean.resident_slice(), b"COPY_UNIQUE_OUTPUT\r\n");
    assert!(parser.events.is_empty());
}

#[test]
fn flush_keeps_tentative_output_after_oversized_fragment() {
    let mut parser = parser_for_test("unterminated-cap-tentative-flush");
    let mut candidate = b"\x1b]1337;COSH;".to_vec();
    candidate.extend(std::iter::repeat_n(b'x', 64 * 1024));
    candidate.extend_from_slice(b"\x1b]1337;");

    parser
        .feed(&candidate)
        .expect("feed capped prefix fragment");
    parser.feed(b"CO").expect("feed tentative continuation");
    parser.flush_pending().expect("flush tentative output");

    assert_eq!(parser.display.resident_slice(), b"CO");
    assert_eq!(parser.clean.resident_slice(), b"CO");
    assert!(parser.events.is_empty());
}

#[test]
fn flush_drops_oversized_candidate_prefix_fragment() {
    let mut parser = parser_for_test("unterminated-cap-fragment-flush");
    let mut candidate = b"\x1b]1337;COSH;".to_vec();
    candidate.extend(std::iter::repeat_n(b'x', 64 * 1024));
    candidate.extend_from_slice(b"\x1b]1337;");

    parser
        .feed(&candidate)
        .expect("feed capped prefix fragment");
    parser.flush_pending().expect("flush capped fragment");

    assert!(parser.display.is_empty());
    assert!(parser.clean.is_empty());
    assert!(parser.events.is_empty());
}

#[test]
fn oversized_terminated_candidate_preserves_following_output() {
    let mut parser = parser_for_test("oversized-terminated-remainder");
    let mut candidate = b"\x1b]1337;COSH;".to_vec();
    candidate.extend(std::iter::repeat_n(b'x', 64 * 1024));
    candidate.extend_from_slice(b"\x07visible-after-frame\r\n");

    parser
        .feed(&candidate)
        .expect("feed oversized terminated candidate");

    assert_eq!(parser.display.resident_slice(), b"visible-after-frame\r\n");
    assert_eq!(parser.clean.resident_slice(), b"visible-after-frame\r\n");
    assert!(parser.events.is_empty());
}

#[test]
fn byte_fragmented_candidate_scanning_stays_linear() {
    let mut parser = parser_for_test("unterminated-linear-scan");
    OscParser::reset_osc_candidate_scan_bytes();
    parser
        .feed(b"\x1b]1337;COSH;")
        .expect("feed candidate prefix");
    const FRAGMENTS: usize = 4 * 1024;
    for _ in 0..FRAGMENTS {
        parser.feed(b"x").expect("feed candidate byte");
    }

    assert!(
        OscParser::osc_candidate_scan_bytes() <= FRAGMENTS * 32,
        "scanned {} bytes for {FRAGMENTS} fragmented bytes",
        OscParser::osc_candidate_scan_bytes()
    );
}

#[test]
fn flush_drops_unterminated_cosh_candidate_without_leaking() {
    let mut parser = parser_for_test("unterminated-flush");
    parser
        .feed(b"\x1b]1337;COSH;{\"event\":\"precmd\",\"token\":\"INTERNAL_TOKEN\"")
        .expect("feed unterminated candidate");
    parser.flush_pending().expect("flush parser at EOF");

    assert!(parser.display.is_empty());
    assert!(parser.clean.is_empty());
    assert!(parser.events.is_empty());
}

#[test]
fn flush_keeps_output_before_mixed_unterminated_cosh_candidate() {
    let mut parser = parser_for_test("unterminated-mixed-flush");
    parser
        .feed(b"visible-before\r\n\x1b]1337;COSH;{\"event\":\"precmd\",\"token\":\"INTERNAL_TOKEN\"visible-looking-private-tail")
        .expect("feed visible output and unterminated candidate");
    parser.flush_pending().expect("flush mixed parser at EOF");

    assert_eq!(parser.display.resident_slice(), b"visible-before\r\n");
    assert_eq!(parser.clean.resident_slice(), b"visible-before\r\n");
    assert!(parser.events.is_empty());
}

#[test]
fn bel_terminated_malformed_cosh_marker_reports_failure_and_recovers() {
    let mut parser = parser_for_test("malformed-recovery");
    parser
        .feed(b"\x1b]1337;COSH;not-json\x07visible\r\n")
        .expect("feed malformed marker");

    assert_eq!(
        parser
            .events
            .iter()
            .filter(|event| event.kind == ShellEventKind::ComponentFailed)
            .count(),
        1
    );
    assert_eq!(parser.display.resident_slice(), b"visible\r\n");
    assert_eq!(parser.clean.resident_slice(), b"visible\r\n");
}

#[test]
fn trusted_history_file_marker_is_private_and_observed() {
    let observed = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&observed);
    let mut parser = parser_for_test("history-file").with_history_file_observer(
        ShellHistoryFileObserver::new(move |path| {
            sink.lock().expect("history observer lock").push(path);
        }),
    );
    let marker = b"\x1b]1337;COSH;{\"event\":\"history_file\",\"token\":\"test-marker-token\",\"session_id\":\"history-file\",\"history_file\":\"/home/test/.bash_history\"}\x07";

    parser.feed(marker).expect("feed history marker");
    parser.feed(marker).expect("feed duplicate history marker");

    assert_eq!(
        *observed.lock().expect("history observer lock"),
        vec![std::path::PathBuf::from("/home/test/.bash_history")]
    );
    assert!(parser.events.is_empty());
    assert!(parser.clean.is_empty());
    assert!(parser.display.is_empty());
}

#[test]
fn history_file_marker_rejects_untrusted_or_relative_paths() {
    let observed = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&observed);
    let mut parser = parser_for_test("history-file-reject").with_history_file_observer(
        ShellHistoryFileObserver::new(move |path| {
            sink.lock().expect("history observer lock").push(path);
        }),
    );

    for marker in [
        b"\x1b]1337;COSH;{\"event\":\"history_file\",\"token\":\"wrong\",\"session_id\":\"history-file-reject\",\"history_file\":\"/tmp/history\"}\x07".as_slice(),
        b"\x1b]1337;COSH;{\"event\":\"history_file\",\"token\":\"test-marker-token\",\"history_file\":\"/tmp/history\"}\x07".as_slice(),
        b"\x1b]1337;COSH;{\"event\":\"history_file\",\"token\":\"test-marker-token\",\"session_id\":\"history-file-reject\",\"history_file\":\"relative/history\"}\x07".as_slice(),
        b"\x1b]1337;COSH;{\"event\":\"history_file\",\"token\":\"test-marker-token\",\"session_id\":\"history-file-reject\",\"history_file\":\"/tmp/line\\nhistory\"}\x07".as_slice(),
    ] {
        parser.feed(marker).expect("feed rejected history marker");
    }

    assert!(observed.lock().expect("history observer lock").is_empty());
    assert!(parser.events.is_empty());
}

#[test]
fn bash_history_file_marker_is_native_only() {
    let script = bash_marker_script();

    assert!(script.contains("_cosh_emit_native_history_file_marker"));
    assert!(script.contains("[[ -n \"${COSH_SHELL_ISOLATED:-}\" ]]"));
    assert!(script.contains("\"event\":\"history_file\""));
    assert!(script.contains("[[:cntrl:]]"));
    assert!(!zsh_marker_script().contains("\"event\":\"history_file\""));
}

#[test]
fn bash_marker_never_enables_global_debug_tracing() {
    let script = bash_marker_script();

    assert!(!script.contains("shopt -s extdebug"));
    assert!(!script.contains("set -T"));
    assert!(!script.contains("set -E"));
    assert!(!script.contains("trap '_cosh"));
    assert!(!script.contains("_cosh_preexec_marker()"));
    assert!(script.contains("_cosh_bounded_preexec_marker()"));
    assert!(!zsh_marker_script().contains("BASHOPTS"));
}

#[test]
fn bash_bounded_preexec_is_observation_only() {
    let script = bash_marker_script();

    let fn_start = script
        .find("_cosh_bounded_preexec_marker() {")
        .expect("bounded preexec marker should exist");
    let fn_body = &script[fn_start..];
    let fn_end = fn_body
        .find("_cosh_bounded_prompt_marker() {")
        .expect("bounded prompt marker should follow preexec");
    let body = &fn_body[..fn_end];
    assert!(body.contains("PS0 is observation-only"));
    assert!(!body.contains("return 1"));
    assert!(!body.contains("trap "));
}

#[test]
fn bash_bounded_preexec_persists_only_the_history_cursor() {
    let script = bash_marker_script();

    assert!(script.contains(".preexec-history-no"));
    assert!(script.contains("_cosh_last_preexec_history_no"));
    assert!(script.contains("_cosh_remember_preexec_history_no \"$history_no\""));
    assert!(!script.contains(".preexec-command"));
}

#[test]
fn bash_bounded_prompt_marker_returns_success() {
    let script = bash_marker_script();
    let fn_start = script
        .find("_cosh_bounded_prompt_marker() {")
        .expect("bounded prompt marker should exist");
    let fn_body = &script[fn_start..];
    let fn_end = fn_body
        .find("_cosh_precmd_marker() {")
        .expect("precmd marker should follow bounded prompt marker");

    assert!(fn_body[..fn_end].contains("  return 0\n"));
}

#[test]
fn prompt_ready_boundaries_preserve_native_prompt_hooks() {
    let bash = bash_marker_script();
    assert!(!bash.contains("_cosh_run_user_prompt_command"));
    assert!(bash.contains("{\"e\":\"p\""));
    assert!(!bash.contains("_cosh_emit_boundary_marker \"prompt_ready\""));
    assert!(bash.contains("_cosh_bounded_prompt_marker \"$_COSH_PROMPT_STATUS\""));
    assert!(!bash.contains("PS1='$("));
    assert!(bash.contains("PROMPT_COMMAND+=(\"${_COSH_USER_PROMPT_COMMAND[@]}\")"));
    assert!(bash.contains("BASH_VERSINFO[1] < 1"));
    assert!(bash.contains("PROMPT_COMMAND=\"$_COSH_PROMPT_COMMAND_SCALAR\""));

    let zsh = zsh_marker_script();
    let precmd = zsh
        .find("_cosh_emit_marker \"precmd\"")
        .expect("zsh precmd marker");
    let zsh_ready = zsh
        .find("_cosh_emit_marker \"prompt_ready\"")
        .expect("zsh prompt-ready marker");
    assert!(precmd < zsh_ready);
    assert!(zsh[..zsh_ready].ends_with(
        "if [[ \"${precmd_functions[-1]:-}\" == \"_cosh_precmd_marker\" ]]; then\n    "
    ));
}

#[test]
fn prompt_hook_output_does_not_count_as_a_painted_prompt() {
    let mut parser = parser_for_test("prompt-ready");
    feed_precmd(&mut parser, 0);

    parser
        .feed(b"hook output")
        .expect("feed prompt hook output");
    assert!(!parser.has_prompt_painted_since_ready());

    let ready = b"\x1b]1337;COSH;{\"event\":\"prompt_ready\",\"token\":\"test-marker-token\"}\x07";
    parser.feed(ready).expect("feed prompt-ready marker");
    assert!(!parser.has_prompt_painted_since_ready());

    parser.feed(b"prompt> ").expect("feed prompt paint");
    assert!(parser.has_prompt_painted_since_ready());
}

#[test]
fn parser_clean_strips_zsh_bracketed_paste_and_applies_backspace() {
    let mut parser = parser_for_test("clean-zsh-control");
    let input =
        b"\x1b[0m\x1b[27m\x1b[24m\x1b[Jcosh-osc$ \x1b[K\x1b[?2004he\x08echo ok\x1b[?2004l\r\n";

    parser.feed(input).expect("feed");

    assert_eq!(
        String::from_utf8_lossy(parser.clean.resident_slice()),
        "cosh-osc$ echo ok\r\n"
    );
    assert_eq!(parser.display.resident_slice(), input);
}

#[test]
fn parser_clean_handles_split_zsh_bracketed_paste_control() {
    let mut parser = parser_for_test("clean-zsh-split-control");

    parser.feed(b"\x1b[?20").expect("feed partial");
    assert!(parser.clean.is_empty());
    parser.feed(b"04hcmd\x1b[?2004l").expect("feed remainder");

    assert_eq!(
        String::from_utf8_lossy(parser.clean.resident_slice()),
        "cmd"
    );
}

#[test]
fn slash_guard_echo_requires_an_authenticated_one_shot_arm() {
    const SENTINEL_LINE: &[u8] = b"\rprompt$ case $- in *x*) builtin set +x; builtin true __cosh_slash_guard__; builtin set -x ;; *) : ;; esac\r\n";
    let mut unarmed = parser_for_test("slash-guard-unarmed");
    unarmed.feed(SENTINEL_LINE).expect("feed unarmed sentinel");
    assert_eq!(unarmed.display.resident_slice(), SENTINEL_LINE);

    let mut wrong_token = parser_for_test("slash-guard-wrong-token");
    wrong_token
        .feed(b"\x1b]1337;COSH;{\"event\":\"slash_guard\",\"token\":\"wrong\",\"session_id\":\"slash-guard-wrong-token\"}\x07")
        .expect("feed untrusted arm");
    wrong_token
        .feed(SENTINEL_LINE)
        .expect("feed sentinel after untrusted arm");
    assert_eq!(wrong_token.display.resident_slice(), SENTINEL_LINE);

    let mut wrong_session = parser_for_test("slash-guard-wrong-session");
    wrong_session
        .feed(b"\x1b]1337;COSH;{\"event\":\"slash_guard\",\"token\":\"test-marker-token\",\"session_id\":\"other-session\"}\x07")
        .expect("feed wrong-session arm");
    wrong_session
        .feed(SENTINEL_LINE)
        .expect("feed sentinel after wrong-session arm");
    assert_eq!(wrong_session.display.resident_slice(), SENTINEL_LINE);

    let mut wrong_reason = parser_for_test("slash-guard-wrong-reason");
    wrong_reason
        .feed(b"\x1b]1337;COSH;{\"e\":\"slash_guard\",\"t\":\"test-marker-token\"}\x07guard$ case $- in *x*) builtin set +x; builtin true __cosh_slash_guard__; builtin set -x ;; *) : ;; esac\r\n")
        .expect("feed trusted arm");
    wrong_reason
        .feed(b"\x1b]1337;COSH;{\"e\":\"intercept\",\"t\":\"test-marker-token\",\"command\":\"/mode\",\"reason\":\"natural_language\"}\x07")
        .expect("feed wrong intercept kind");
    assert_eq!(wrong_reason.display.resident_slice(), b"guard$ \r\n");
}

#[test]
fn slash_guard_echo_keeps_the_original_line_exactly_once() {
    let mut parser = parser_for_test("slash-guard-display");
    parser
        .feed(b"\x1b]1337;COSH;{\"e\":\"slash_guard\",\"t\":\"test-marker-token\"}\x07guard$ case $- in *x*) builtin set +x; builtin true __cosh_slash_guard__; builtin set -x ;; *) : ;; esac\r\n")
        .expect("feed guarded redisplay");
    assert!(parser.display.is_empty());
    parser
        .feed(b"\x1b]1337;COSH;{\"e\":\"intercept\",\"t\":\"test-marker-token\",\"command\":\"/mode\",\"reason\":\"slash\"}\x07")
        .expect("resolve guarded redisplay");
    assert_eq!(parser.display.resident_slice(), b"guard$ /mode\r\n");
    assert_eq!(parser.clean.resident_slice(), b"guard$ \r\n");
}

#[test]
fn slash_guard_echo_does_not_duplicate_an_already_painted_direct_line() {
    let mut parser = parser_for_test("slash-guard-direct-display");
    parser
        .feed(b"\x1b]1337;COSH;{\"e\":\"precmd\",\"t\":\"test-marker-token\",\"status\":0}\x07guard$ /mode\r\n\x1b]1337;COSH;{\"e\":\"slash_guard\",\"t\":\"test-marker-token\"}\x07guard$ case $- in *x*) builtin set +x; builtin true __cosh_slash_guard__; builtin set -x ;; *) : ;; esac\r\n")
        .expect("feed direct line and guarded redisplay");
    parser
        .feed(b"\x1b]1337;COSH;{\"e\":\"intercept\",\"t\":\"test-marker-token\",\"command\":\"/mode\",\"reason\":\"slash\"}\x07")
        .expect("resolve direct slash input");

    assert_eq!(
        parser.display.resident_slice(),
        b"guard$ /mode\r\nguard$ \r\n"
    );
    assert_eq!(
        parser.clean.resident_slice(),
        b"guard$ /mode\r\nguard$ \r\n"
    );
}

#[test]
fn slash_guard_echo_restores_tabbed_input_only_to_display() {
    let mut parser = parser_for_test("slash-guard-tabbed-display");
    parser
        .feed(b"\x1b]1337;COSH;{\"e\":\"slash_guard\",\"t\":\"test-marker-token\"}\x07guard$ case $- in *x*) builtin set +x; builtin true __cosh_slash_guard__; builtin set -x ;; *) : ;; esac\r\n")
        .expect("feed tabbed guard redraw");
    parser
        .feed(b"\x1b]1337;COSH;{\"e\":\"intercept\",\"t\":\"test-marker-token\",\"command\":\"/mode\\targ\",\"reason\":\"slash\"}\x07")
        .expect("resolve tabbed slash input");

    assert_eq!(parser.display.resident_slice(), b"guard$ /mode\targ\r\n");
    assert_eq!(parser.clean.resident_slice(), b"guard$ \r\n");
}

#[test]
fn slash_guard_echo_preserves_complete_background_lines() {
    let mut parser = parser_for_test("slash-guard-background");
    parser
        .feed(b"\x1b]1337;COSH;{\"e\":\"slash_guard\",\"t\":\"test-marker-token\"}\x07background\r\nguard$ case $- in *x*) builtin set +x; builtin true __cosh_slash_guard__; builtin set -x ;; *) : ;; esac\r\nAFTER\r\n")
        .expect("feed background output before guarded redisplay");
    assert_eq!(parser.display.resident_slice(), b"background\r\n");
    parser
        .feed(b"\x1b]1337;COSH;{\"e\":\"intercept\",\"t\":\"test-marker-token\",\"command\":\"/mode\",\"reason\":\"slash\"}\x07")
        .expect("resolve background ordering");
    assert_eq!(
        parser.display.resident_slice(),
        b"background\r\nguard$ /mode\r\nAFTER\r\n"
    );
    assert_eq!(
        parser.clean.resident_slice(),
        b"background\r\nguard$ \r\nAFTER\r\n"
    );
    assert!(!parser
        .display
        .resident_slice()
        .windows(b"__cosh_slash_guard__".len())
        .any(|window| window == b"__cosh_slash_guard__"));
}

#[test]
fn slash_guard_echo_preserves_carriage_return_partial_output_in_both_streams() {
    let mut parser = parser_for_test("slash-guard-partial-output");
    parser
        .feed(b"\x1b]1337;COSH;{\"e\":\"slash_guard\",\"t\":\"test-marker-token\"}\x07BEFORE\rBACKGROUND_PARTIAL<builtin true __cosh_slash_guard__; builtin set -x ;; *) : ;; esac\r\n")
        .expect("feed CR partial output before horizontally scrolled guard");
    assert!(parser.display.is_empty());
    parser
        .feed(b"\x1b]1337;COSH;{\"e\":\"intercept\",\"t\":\"test-marker-token\",\"command\":\"/mode\",\"reason\":\"slash\"}\x07")
        .expect("resolve CR partial output");
    assert_eq!(
        parser.display.resident_slice(),
        b"BEFORE\rBACKGROUND_PARTIAL/mode\r\n"
    );
    assert!(!parser
        .clean
        .resident_slice()
        .windows(b"/mode".len())
        .any(|window| window == b"/mode"));
    assert!(parser
        .clean
        .resident_slice()
        .windows(b"BACKGROUND_PARTIAL".len())
        .any(|window| window == b"BACKGROUND_PARTIAL"));
}

#[test]
fn authenticated_slash_guard_echo_handles_fragmentation_and_fails_open() {
    let mut parser = parser_for_test("slash-guard-fragmented");
    parser
        .feed(b"\x1b]1337;COSH;{\"event\":\"slash_guard\",\"token\":\"test-marker-")
        .expect("feed partial arm");
    parser
        .feed(b"token\",\"session_id\":\"slash-guard-fragmented\"}\x07<builtin true ")
        .expect("finish arm and partial echo");
    parser
        .feed(b"__cosh_slash_guard__; builtin set -x ;; *) : ;; esac    \x08\x08\r\nfollowing")
        .expect("finish shadow echo");
    assert!(parser.display.is_empty());
    parser
        .feed(b"\x1b]1337;COSH;{\"event\":\"prompt_ready\",\"token\":\"test-marker-token\",\"session_id\":\"slash-guard-fragmented\"}\x07")
        .expect("flush following output at lifecycle boundary");
    assert_eq!(parser.display.resident_slice(), b"\r\nfollowing");

    let mut mismatch = parser_for_test("slash-guard-mismatch");
    mismatch
        .feed(b"\x1b]1337;COSH;{\"event\":\"slash_guard\",\"token\":\"test-marker-token\",\"session_id\":\"slash-guard-mismatch\"}\x07ordinary output")
        .expect("feed armed mismatch");
    mismatch
        .feed(b"\x1b]1337;COSH;{\"event\":\"prompt_ready\",\"token\":\"test-marker-token\",\"session_id\":\"slash-guard-mismatch\"}\x07")
        .expect("flush mismatch at lifecycle boundary");
    assert_eq!(mismatch.display.resident_slice(), b"ordinary output");

    let mut nested = parser_for_test("slash-guard-nested");
    nested
        .feed(b"\x1b]1337;COSH;{\"e\":\"slash_guard\",\"t\":\"test-marker-token\"}\x07partial\x1b]1337;COSH;{\"e\":\"slash_guard\",\"t\":\"test-marker-token\"}\x07<builtin true __cosh_slash_guard__; builtin set -x ;; *) : ;; esac\r\n")
        .expect("feed nested arm");
    assert_eq!(nested.display.resident_slice(), b"partial");
    nested
        .feed(b"\x1b]1337;COSH;{\"e\":\"prompt_ready\",\"t\":\"test-marker-token\"}\x07")
        .expect("flush nested arm without intercept");
    assert_eq!(nested.display.resident_slice(), b"partial\r\n");

    let mut capped = parser_for_test("slash-guard-cap");
    capped
        .feed(b"\x1b]1337;COSH;{\"event\":\"slash_guard\",\"token\":\"test-marker-token\",\"session_id\":\"slash-guard-cap\"}\x07")
        .expect("feed capped arm");
    let cap = vec![b'x'; 64 * 1024];
    capped.feed(&cap).expect("feed cap");
    assert_eq!(capped.display.resident_slice(), cap);
}

#[test]
fn precmd_count_tracks_shell_ready_and_command_events() {
    let mut parser = parser_for_test("precmd-count");
    assert_eq!(parser.precmd_count(), 0);

    let mut precmd_no_cmd: Vec<u8> = Vec::new();
    precmd_no_cmd.extend_from_slice(b"\x1b]1337;COSH;");
    precmd_no_cmd
        .extend_from_slice(br#"{"event":"precmd","token":"test-marker-token","cwd":"/tmp"}"#);
    precmd_no_cmd.push(b'\x07');
    parser.feed(&precmd_no_cmd).expect("feed precmd");
    assert_eq!(parser.precmd_count(), 1);

    let mut preexec: Vec<u8> = Vec::new();
    preexec.extend_from_slice(b"\x1b]1337;COSH;");
    preexec.extend_from_slice(
        br#"{"event":"preexec","token":"test-marker-token","command":"echo hi","cwd":"/tmp"}"#,
    );
    preexec.push(b'\x07');
    parser.feed(&preexec).expect("feed preexec");
    assert_eq!(parser.precmd_count(), 1);

    let mut precmd_ok: Vec<u8> = Vec::new();
    precmd_ok.extend_from_slice(b"\x1b]1337;COSH;");
    precmd_ok.extend_from_slice(
        br#"{"event":"precmd","token":"test-marker-token","status":0,"cwd":"/tmp"}"#,
    );
    precmd_ok.push(b'\x07');
    parser.feed(&precmd_ok).expect("feed precmd ok");
    assert_eq!(parser.precmd_count(), 2);

    let mut preexec2: Vec<u8> = Vec::new();
    preexec2.extend_from_slice(b"\x1b]1337;COSH;");
    preexec2.extend_from_slice(
        br#"{"event":"preexec","token":"test-marker-token","command":"false","cwd":"/tmp"}"#,
    );
    preexec2.push(b'\x07');
    parser.feed(&preexec2).expect("feed preexec2");

    let mut precmd_fail: Vec<u8> = Vec::new();
    precmd_fail.extend_from_slice(b"\x1b]1337;COSH;");
    precmd_fail.extend_from_slice(
        br#"{"event":"precmd","token":"test-marker-token","status":1,"cwd":"/tmp"}"#,
    );
    precmd_fail.push(b'\x07');
    parser.feed(&precmd_fail).expect("feed precmd fail");
    assert_eq!(parser.precmd_count(), 3);
}

// #2413: a status-less precmd marker can only be truncated, forged, or
// protocol-drifted — both marker scripts (marker/bash.sh, marker/zsh.rs)
// emit `"status":%s` unconditionally with the shell's `$?` value. Defaulting
// the missing field to success fabricates a CommandCompleted for a command
// whose real outcome is unknown; fall toward the -1 missing-exit-code
// sentinel instead, matching the ledger contract from #2105/PR #2412 and the
// agent host-executed chain.
#[test]
fn precmd_marker_without_status_fails_with_missing_exit_sentinel() {
    let mut parser = parser_for_test("precmd-missing-status");
    feed_preexec(&mut parser, "echo maybe-truncated");
    let marker =
        b"\x1b]1337;COSH;{\"event\":\"precmd\",\"token\":\"test-marker-token\",\"cwd\":\"/tmp\"}\x07";
    parser.feed(marker).expect("feed statusless precmd");

    let finished = parser
        .events
        .iter()
        .find(|event| {
            matches!(
                event.kind,
                ShellEventKind::CommandCompleted | ShellEventKind::CommandFailed
            )
        })
        .expect("command finish event");
    assert_eq!(finished.kind, ShellEventKind::CommandFailed);
    assert_eq!(finished.exit_code, Some(-1));

    // The ledger keeps the explicit -1 verbatim with a Failed status, so the
    // live marker path and the journal-replay path agree on missing status.
    let ledger = build_command_blocks(&parser.events);
    assert!(ledger.errors.is_empty(), "{:?}", ledger.errors);
    assert_eq!(ledger.blocks.len(), 1);
    assert_eq!(ledger.blocks[0].exit_code, -1);
    assert_eq!(ledger.blocks[0].status, crate::types::CommandStatus::Failed);
}

#[test]
fn pending_handoff_origin_is_consumed_by_matching_preexec() {
    let mut parser = parser_for_test("origin-match");
    let request = ShellHandoffRequest::new(
        "echo hi".to_string(),
        "$ echo hi".to_string(),
        "user_analysis_action",
        "user",
        "approval-1".to_string(),
        "run-1".to_string(),
        1,
    )
    .expect("handoff request");
    parser.register_pending_handoff_origin(&request);

    feed_preexec(&mut parser, "echo hi");

    let event = parser
        .events
        .iter()
        .find(|event| event.kind == ShellEventKind::CommandStarted)
        .expect("command started");
    assert_eq!(
        event.command_origin,
        Some(CommandOrigin::UserAnalysisAction)
    );

    feed_precmd(&mut parser, 0);

    let event = parser
        .events
        .iter()
        .find(|event| event.kind == ShellEventKind::CommandCompleted)
        .expect("command completed");
    assert_eq!(
        event.command_origin,
        Some(CommandOrigin::UserAnalysisAction)
    );
}

#[test]
fn pending_handoff_origin_mismatch_stays_user_and_keeps_the_slot() {
    let mut parser = parser_for_test("origin-mismatch");
    let request = ShellHandoffRequest::new(
        "echo expected".to_string(),
        "$ echo expected".to_string(),
        "approved_provider_shell_tool",
        "user",
        "approval-1".to_string(),
        "run-1".to_string(),
        1,
    )
    .expect("handoff request");
    parser.register_pending_handoff_origin(&request);

    feed_preexec(&mut parser, "echo actual");

    // #2142 S3: an unrelated tokenless preexec is ordinary user input and
    // must not burn the pending slot.
    let event = parser
        .events
        .iter()
        .find(|event| event.kind == ShellEventKind::CommandStarted)
        .expect("command started");
    assert_eq!(event.command_origin, Some(CommandOrigin::UserInteractive));

    feed_precmd(&mut parser, 0);
    feed_preexec(&mut parser, "echo expected");

    let event = parser
        .events
        .iter()
        .filter(|event| event.kind == ShellEventKind::CommandStarted)
        .nth(1)
        .expect("second command started");
    assert_eq!(event.command_origin, Some(CommandOrigin::ProviderTool));
}

#[test]
fn trusted_preexec_path_reuses_and_advances_normalized_generation() {
    let mut parser = parser_for_test("path-generation");

    feed_environment_marker(
        &mut parser,
        "precmd",
        None,
        "/first:/first:relative:/second/",
        false,
        Some("path-generation"),
    );
    assert_eq!(
        parser
            .shell_environment_snapshot
            .as_ref()
            .unwrap()
            .generation,
        1
    );
    assert_eq!(
        parser
            .shell_environment_snapshot
            .as_ref()
            .unwrap()
            .marker_sequence,
        1
    );
    assert_eq!(
        parser.shell_environment_snapshot.as_ref().unwrap().path,
        "/first:/second"
    );

    feed_environment_marker(
        &mut parser,
        "preexec",
        Some("echo one"),
        "/first:/second",
        true,
        Some("path-generation"),
    );
    let first = parser
        .events
        .iter()
        .find(|event| event.kind == ShellEventKind::CommandStarted)
        .expect("first command start");
    assert_eq!(first.shell_environment_generation, Some(1));
    assert_eq!(
        parser
            .shell_environment_snapshot
            .as_ref()
            .unwrap()
            .marker_sequence,
        2
    );
    feed_precmd(&mut parser, 0);
    let completed = parser
        .events
        .iter()
        .find(|event| event.kind == ShellEventKind::CommandCompleted)
        .expect("first command completion");
    assert_eq!(completed.shell_environment_generation, Some(1));

    feed_environment_marker(
        &mut parser,
        "preexec",
        Some("echo two"),
        "/third:/second",
        true,
        Some("path-generation"),
    );
    let second = parser
        .events
        .iter()
        .filter(|event| event.kind == ShellEventKind::CommandStarted)
        .nth(1)
        .expect("second command start");
    assert_eq!(second.shell_environment_generation, Some(2));
    assert_eq!(
        parser
            .shell_environment_snapshot
            .as_ref()
            .unwrap()
            .marker_sequence,
        3
    );
}

#[test]
fn untrusted_or_invalid_environment_marker_never_binds_generation() {
    let mut parser = parser_for_test("path-untrusted");

    feed_environment_marker(
        &mut parser,
        "precmd",
        None,
        "/provisional",
        false,
        Some("path-untrusted"),
    );
    feed_environment_marker(
        &mut parser,
        "preexec",
        Some("echo untrusted"),
        "/provisional",
        false,
        Some("path-untrusted"),
    );
    let untrusted = parser
        .events
        .iter()
        .find(|event| event.kind == ShellEventKind::CommandStarted)
        .expect("untrusted command start");
    assert_eq!(untrusted.shell_environment_generation, None);
    feed_precmd(&mut parser, 0);

    feed_environment_marker(
        &mut parser,
        "preexec",
        Some("echo wrong-session"),
        "/wrong",
        true,
        Some("different-session"),
    );
    assert_eq!(
        parser
            .events
            .iter()
            .filter(|event| event.kind == ShellEventKind::CommandStarted)
            .count(),
        1
    );
    assert_eq!(
        parser
            .shell_environment_snapshot
            .as_ref()
            .unwrap()
            .marker_sequence,
        2
    );

    let oversized = format!("/{}", "x".repeat(8192));
    feed_environment_marker(
        &mut parser,
        "preexec",
        Some("echo oversized"),
        &oversized,
        true,
        Some("path-untrusted"),
    );
    let oversized_start = parser
        .events
        .iter()
        .filter(|event| event.kind == ShellEventKind::CommandStarted)
        .nth(1)
        .expect("oversized command start");
    assert_eq!(oversized_start.shell_environment_generation, None);
    assert_eq!(
        parser
            .shell_environment_snapshot
            .as_ref()
            .unwrap()
            .marker_sequence,
        2
    );
}

#[test]
fn path_snapshot_accepts_exact_eight_kibibyte_boundary() {
    let mut parser = parser_for_test("path-eight-kib");
    let path = format!("/{}", "x".repeat(8191));

    feed_environment_marker(
        &mut parser,
        "preexec",
        Some("echo boundary"),
        &path,
        true,
        Some("path-eight-kib"),
    );

    let start = parser
        .events
        .iter()
        .find(|event| event.kind == ShellEventKind::CommandStarted)
        .expect("boundary command start");
    assert_eq!(start.shell_environment_generation, Some(1));
    assert_eq!(
        parser.shell_environment_snapshot.as_ref().unwrap().path,
        path
    );
}

#[test]
fn environment_marker_with_control_path_does_not_update_state() {
    let mut parser = parser_for_test("path-control");

    feed_environment_marker(
        &mut parser,
        "preexec",
        Some("echo unsafe path"),
        "/safe:/control\u{7}entry:/tail",
        true,
        Some("path-control"),
    );

    let start = parser
        .events
        .iter()
        .find(|event| event.kind == ShellEventKind::CommandStarted)
        .expect("command start");
    assert_eq!(start.shell_environment_generation, None);
    assert!(parser.shell_environment_snapshot.is_none());
}

#[test]
fn control_physical_cwd_stays_untrusted_until_clean_prompt() {
    let mut parser = parser_for_test("physical-cwd-control");
    let prompt_cwd = crate::input::ShellPromptCwd::default();
    parser.set_prompt_cwd(prompt_cwd.clone());

    // #2918 may route a path-bearing prompt only from a trusted physical cwd.
    for unsafe_cwd in [
        "/tmp/bell\u{7}cwd",
        "/tmp/st\u{1b}\\cwd",
        "/tmp/del\u{7f}cwd",
    ] {
        let marker = serde_json::json!({
            "e": "p",
            "t": TEST_MARKER_TOKEN,
            "c": unsafe_cwd,
            "pc": unsafe_cwd,
            "s": 0,
        });
        let bytes = format!("\x1b]1337;COSH;{marker}\x07");
        parser.feed(bytes.as_bytes()).expect("feed unsafe prompt");
        assert_eq!(prompt_cwd.current(), None, "{unsafe_cwd:?}");
    }

    let clean_cwd = "/tmp/clean-after-control";
    let marker = serde_json::json!({
        "e": "p",
        "t": TEST_MARKER_TOKEN,
        "c": clean_cwd,
        "pc": clean_cwd,
        "s": 0,
    });
    let bytes = format!("\x1b]1337;COSH;{marker}\x07");
    parser.feed(bytes.as_bytes()).expect("feed clean prompt");
    assert_eq!(prompt_cwd.current().as_deref(), Some(clean_cwd));
}

#[test]
fn environment_marker_with_wrong_token_does_not_update_state() {
    let mut parser = parser_for_test("path-wrong-token");
    let marker = serde_json::json!({
        "event": "preexec",
        "token": "wrong-token",
        "session_id": "path-wrong-token",
        "command": "echo forged",
        "cwd": "/tmp",
        "path": "/forged",
        "path_trusted": true,
        "status": 0,
    });
    let bytes = format!("\x1b]1337;COSH;{marker}\x07");

    parser.feed(bytes.as_bytes()).expect("feed forged marker");

    assert!(parser.shell_environment_snapshot.is_none());
    assert!(parser.events.is_empty());
}

#[test]
fn completion_keeps_generation_captured_at_command_start() {
    let mut parser = parser_for_test("path-completion-stable");
    feed_environment_marker(
        &mut parser,
        "preexec",
        Some("echo stable"),
        "/at-start",
        true,
        Some("path-completion-stable"),
    );

    feed_environment_marker(
        &mut parser,
        "precmd",
        None,
        "/after-command",
        false,
        Some("path-completion-stable"),
    );

    let completed = parser
        .events
        .iter()
        .find(|event| event.kind == ShellEventKind::CommandCompleted)
        .expect("completed command");
    assert_eq!(completed.shell_environment_generation, Some(1));
    assert_eq!(
        parser
            .shell_environment_snapshot
            .as_ref()
            .unwrap()
            .generation,
        2
    );
}

#[test]
fn accepted_environment_snapshots_are_forwarded_without_events_or_journal_fields() {
    let (sender, receiver) = std::sync::mpsc::channel();
    let mut parser = parser_for_test("path-observer").with_environment_observer(
        ShellEnvironmentObserver::new(move |snapshot| {
            sender.send(snapshot).expect("forward snapshot");
        }),
    );

    feed_environment_marker(
        &mut parser,
        "precmd",
        None,
        "/provisional",
        false,
        Some("path-observer"),
    );
    feed_environment_marker(
        &mut parser,
        "preexec",
        Some("echo observed"),
        "/authoritative",
        true,
        Some("path-observer"),
    );

    let provisional = receiver.recv().expect("provisional snapshot");
    let authoritative = receiver.recv().expect("authoritative snapshot");
    assert_eq!(provisional.path, "/provisional");
    assert_eq!(authoritative.path, "/authoritative");
    assert!(parser.events.iter().all(|event| {
        serde_json::to_value(event)
            .expect("serialize event")
            .get("path")
            .is_none()
    }));
}

#[test]
fn parser_preserves_pending_handoff_command_echo_for_crlf() {
    let mut parser = parser_for_test("handoff-echo-crlf");
    let request = ShellHandoffRequest::new(
        "printf hi".to_string(),
        "$ printf hi".to_string(),
        "approved_provider_shell_tool",
        "user",
        "approval-1".to_string(),
        "run-1".to_string(),
        1,
    )
    .expect("handoff request");
    let mut echo = b"prompt$ ".to_vec();
    let mut command = request.pty_bytes().expect("handoff bytes");
    command.pop();
    echo.extend_from_slice(&command);
    echo.extend_from_slice(b"\r\nhi");

    parser.register_pending_handoff_origin(&request);
    parser.feed(&echo).expect("feed handoff echo");

    let display = String::from_utf8_lossy(parser.display.resident_slice());
    assert_eq!(display, "prompt$ printf hi\r\nhi");
    let clean = String::from_utf8_lossy(parser.clean.resident_slice());
    assert_eq!(clean, "prompt$ printf hi\r\nhi");
}

#[test]
fn parser_preserves_pending_handoff_command_echo_for_cr() {
    let mut parser = parser_for_test("handoff-echo-cr");
    let request = ShellHandoffRequest::new(
        "printf hi".to_string(),
        "$ printf hi".to_string(),
        "approved_provider_shell_tool",
        "user",
        "approval-1".to_string(),
        "run-1".to_string(),
        1,
    )
    .expect("handoff request");
    let mut echo = b"prompt$ ".to_vec();
    let mut command = request.pty_bytes().expect("handoff bytes");
    command.pop();
    echo.extend_from_slice(&command);
    echo.extend_from_slice(b"\x1b[?2004l\rhi");

    parser.register_pending_handoff_origin(&request);
    parser.feed(&echo).expect("feed handoff echo");

    let display = String::from_utf8_lossy(parser.display.resident_slice());
    assert_eq!(display, "prompt$ printf hi\x1b[?2004l\rhi");
    let clean = String::from_utf8_lossy(parser.clean.resident_slice());
    assert_eq!(clean, "prompt$ printf hi\rhi");
}

#[test]
fn output_ref_file_uses_private_permissions() {
    let dir =
        std::env::temp_dir().join(format!("cosh-shell-osc-output-ref-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);

    let path = write_output_ref(&dir, "cmd-1", b"secret-ish\n").expect("write output ref");

    assert_eq!(
        std::fs::metadata(&dir)
            .expect("dir metadata")
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::metadata(&path)
            .expect("file metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn output_ref_file_redacts_secrets_before_persistence() {
    let dir = std::env::temp_dir().join(format!(
        "cosh-shell-osc-secret-output-ref-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let secret = "output-secret-value";

    let path = write_output_ref(
        &dir,
        "cmd-1",
        format!("result api_key={secret}\n").as_bytes(),
    )
    .expect("write output ref");
    let output = std::fs::read_to_string(&path).expect("read output ref");

    assert!(!output.contains(secret), "{output}");
    assert!(output.contains("api_key=<redacted>"), "{output}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn output_ref_file_is_capped_but_preserves_head_and_tail() {
    let dir = std::env::temp_dir().join(format!(
        "cosh-shell-osc-output-ref-cap-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let mut output = Vec::new();
    output.extend_from_slice(b"head-line\n");
    output.extend(std::iter::repeat_n(b'x', COMMAND_OUTPUT_REF_MAX_BYTES));
    output.extend_from_slice(b"\ntail-line\n");

    let path = write_output_ref(&dir, "cmd-1", &output).expect("write output ref");
    let captured = std::fs::read(&path).expect("read output ref");
    let captured_text = String::from_utf8(captured.clone()).expect("utf8 capped output");

    assert!(captured.len() <= COMMAND_OUTPUT_REF_MAX_BYTES);
    assert!(captured_text.starts_with("head-line"), "{captured_text}");
    assert!(
        captured_text.contains("[captured output truncated:"),
        "{captured_text}"
    );
    assert!(captured_text.ends_with("tail-line\n"), "{captured_text}");
    assert!(
        captured_text.contains(&format!("original_bytes={}", output.len())),
        "{captured_text}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn capped_output_ref_respects_utf8_boundaries() {
    let input = "头".repeat(COMMAND_OUTPUT_REF_MAX_BYTES / 3 + 10);

    let captured = capped_output_ref_bytes(input.as_bytes(), 4096);

    let captured_text = String::from_utf8(captured).expect("valid utf8");
    assert!(captured_text.contains("[captured output truncated:"));
    assert!(captured_text.starts_with('头'));
    assert!(captured_text.ends_with('头'));
}

#[test]
fn output_ref_session_cap_marks_later_output_unavailable() {
    let dir = std::env::temp_dir().join(format!(
        "cosh-shell-osc-output-ref-session-cap-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);

    let first =
        write_output_ref_with_session_cap(&dir, "cmd-1", b"12345", 0, 8).expect("first ref");
    let second = write_output_ref_with_session_cap(&dir, "cmd-2", b"6789", first.captured_bytes, 8)
        .expect("second ref");

    assert_eq!(first.status, OutputRefCaptureStatus::Captured);
    assert!(first.path.as_ref().is_some_and(|path| path.exists()));
    assert_eq!(second.status, OutputRefCaptureStatus::SessionCapReached);
    assert!(second.path.is_none());
    assert!(!dir.join("cmd-2.txt").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn parser_session_cap_preserves_command_facts_without_output_ref() {
    let mut parser = parser_for_test("session-cap-events");
    parser.captured_output_ref_bytes = SESSION_OUTPUT_REF_MAX_BYTES;

    let mut preexec: Vec<u8> = Vec::new();
    preexec.extend_from_slice(b"\x1b]1337;COSH;");
    preexec.extend_from_slice(
        br#"{"event":"preexec","token":"test-marker-token","command":"printf capped","cwd":"/tmp","timestamp_ms":10}"#,
    );
    preexec.push(b'\x07');
    parser.feed(&preexec).expect("feed preexec");
    parser.feed(b"captured body\n").expect("feed output");

    let mut precmd: Vec<u8> = Vec::new();
    precmd.extend_from_slice(b"\x1b]1337;COSH;");
    precmd.extend_from_slice(
        br#"{"event":"precmd","token":"test-marker-token","status":0,"cwd":"/tmp","timestamp_ms":20}"#,
    );
    precmd.push(b'\x07');
    parser.feed(&precmd).expect("feed precmd");

    let event = parser
        .events
        .iter()
        .find(|event| {
            matches!(
                event.kind,
                ShellEventKind::CommandCompleted | ShellEventKind::CommandFailed
            ) && event.command_id.as_deref() == Some("cmd-1")
        })
        .expect("finished command event");
    assert_eq!(event.command.as_deref(), Some("printf capped"));
    assert_eq!(event.terminal_output_ref, None);
    assert_eq!(
        event.terminal_output_bytes,
        Some("captured body\n".len() as u64)
    );
    assert_eq!(event.component.as_deref(), Some("output_capture"));
    assert_eq!(
        event.message.as_deref(),
        Some("output_capture_status: unavailable; reason: session_output_cap_reached")
    );
}

/// Regression for issue #1811: after a bash intercept, `last_prompt_display()`
/// must not include the user-echoed command text.  bash echoes user input
/// *before* the DEBUG trap fires, so those bytes are in the display buffer
/// ahead of the intercept marker.  The intercept handler must advance
/// `last_prompt_display_start` past the echo so that RestorePrompt only
/// re-emits the new PS1 paint, not the duplicated command.
#[test]
fn intercept_advances_last_prompt_display_start_past_user_echo() {
    let mut parser = parser_for_test("intercept-echo-dedup");
    let sid = "intercept-echo-dedup";

    // 1. Shell ready: precmd sets initial `last_prompt_display_start`.
    parser
        .feed(
            format!(
                "\x1b]1337;COSH;{{\"event\":\"precmd\",\"token\":\"{TEST_MARKER_TOKEN}\",\"session_id\":\"{sid}\",\"cwd\":\"/tmp\",\"status\":0}}\x07"
            )
            .as_bytes(),
        )
        .expect("feed precmd");
    let prompt = b"cosh-replay$ ";
    parser.feed(prompt).expect("feed PS1 paint");

    let prompt_start = parser.last_prompt_display();
    assert!(
        !prompt_start.is_empty(),
        "precmd must set last_prompt_display_start"
    );
    assert_eq!(
        prompt_start, prompt,
        "after precmd, last_prompt_display() returns the PS1 paint"
    );

    // 2. User types `/skills detail\r\n` — bash echoes it before any trap.
    let user_echo = b"/skills detail\r\n";
    parser.feed(user_echo).expect("feed user echo");

    // 3. DEBUG trap fires → intercept marker (bash skips command via extdebug).
    parser
        .feed(
            format!(
                "\x1b]1337;COSH;{{\"event\":\"intercept\",\"token\":\"{TEST_MARKER_TOKEN}\",\"session_id\":\"{sid}\",\"command\":\"/skills detail\",\"reason\":\"slash\",\"cwd\":\"/tmp\"}}\x07"
            )
            .as_bytes(),
        )
        .expect("feed intercept");

    // After intercept, `last_prompt_display_start` must be past the user echo
    // so that `last_prompt_display()` does NOT include the echoed command text.
    let echo_text = std::str::from_utf8(parser.last_prompt_display()).unwrap_or("");
    assert!(
        !echo_text.contains("/skills detail"),
        "last_prompt_display() must not contain the user-echoed command after intercept; got: {echo_text:?}"
    );

    // 4. precmd fires → bash repaints PS1.
    parser
        .feed(
            format!(
                "\x1b]1337;COSH;{{\"event\":\"precmd\",\"token\":\"{TEST_MARKER_TOKEN}\",\"session_id\":\"{sid}\",\"cwd\":\"/tmp\",\"status\":0}}\x07"
            )
            .as_bytes(),
        )
        .expect("feed post-intercept precmd");
    parser.feed(prompt).expect("feed new PS1 paint");

    // `last_prompt_display()` must return only the new PS1, not the echo.
    let final_display = std::str::from_utf8(parser.last_prompt_display()).unwrap_or("");
    assert_eq!(
        final_display, "cosh-replay$ ",
        "after precmd, last_prompt_display() returns only the new PS1 paint"
    );
}

fn parser_for_test(name: &str) -> OscParser {
    let dir =
        std::env::temp_dir().join(format!("cosh-shell-osc-test-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("output ref dir");
    OscParser::new(name.to_string(), dir, TEST_MARKER_TOKEN.to_string())
}

fn bounded_parser_for_test(name: &str, window_bytes: usize) -> (OscParser, std::path::PathBuf) {
    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-bounded-osc-test-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&work_dir);
    let output_ref_dir = work_dir.join("output-refs");
    std::fs::create_dir_all(&output_ref_dir).expect("output ref dir");
    let parser = OscParser::with_retention(
        name.to_string(),
        output_ref_dir,
        TEST_MARKER_TOKEN.to_string(),
        TranscriptRetention::Bounded { window_bytes },
        &work_dir,
    )
    .expect("bounded parser");
    (parser, work_dir)
}

#[test]
fn bounded_parser_caps_long_command_across_multiple_windows() {
    let (mut parser, work_dir) = bounded_parser_for_test("long-command", 128);
    parser
        .feed(b"\x1b]1337;COSH;{\"event\":\"preexec\",\"token\":\"test-marker-token\",\"command\":\"long\",\"cwd\":\"/tmp\",\"timestamp_ms\":10}\x07")
        .expect("preexec");
    for _ in 0..20 {
        parser.feed(&[b'x'; 97]).expect("command output");
        assert!(parser.clean.window_len() <= 256);
        assert!(parser.display.window_len() <= 256);
    }
    parser
        .feed(b"\x1b]1337;COSH;{\"event\":\"precmd\",\"token\":\"test-marker-token\",\"status\":0,\"cwd\":\"/tmp\",\"timestamp_ms\":20}\x07")
        .expect("precmd");

    let completed = parser
        .events
        .iter()
        .find(|event| event.kind == ShellEventKind::CommandCompleted)
        .expect("completed event");
    assert_eq!(completed.terminal_output_bytes, Some(20 * 97));
    let output_ref = completed
        .terminal_output_ref
        .as_deref()
        .expect("output ref");
    let captured = std::fs::read(output_ref).expect("captured output");
    assert_eq!(captured.len(), 20 * 97);
    assert!(parser.clean.window_len() <= 256);
    assert!(parser.display.window_len() <= 256);
    let _ = std::fs::remove_dir_all(work_dir);
}

#[test]
fn bounded_parser_caps_prompt_capture_on_each_append() {
    let (mut parser, work_dir) = bounded_parser_for_test("prompt-capture", 64);
    parser
        .feed(b"\x1b]1337;COSH;{\"event\":\"precmd\",\"token\":\"test-marker-token\",\"cwd\":\"/tmp\"}\x07")
        .expect("precmd");
    parser.feed(&[b'h'; 256]).expect("large prompt output");

    assert_eq!(parser.last_prompt_display(), &[b'h'; 64]);
    parser.feed(b"tail").expect("prompt tail");
    assert_eq!(parser.last_prompt_display().len(), 64);
    assert!(parser.last_prompt_display().ends_with(b"tail"));
    let _ = std::fs::remove_dir_all(work_dir);
}

#[test]
fn bounded_parser_keeps_prompt_replay_after_hook_output_compacts() {
    let (mut parser, work_dir) = bounded_parser_for_test("prompt-replay", 64);
    parser
        .feed(b"\x1b]1337;COSH;{\"event\":\"precmd\",\"token\":\"test-marker-token\",\"cwd\":\"/tmp\"}\x07")
        .expect("precmd");
    parser.feed(&vec![b'h'; 1024]).expect("large prompt hook");
    parser
        .feed(b"\x1b]1337;COSH;{\"event\":\"prompt_ready\",\"token\":\"test-marker-token\"}\x07cosh$ ")
        .expect("prompt ready");

    assert_eq!(parser.last_prompt_display(), b"cosh$ ");
    assert!(parser.has_prompt_painted_since_ready());
    assert!(parser.display.window_len() <= 128);
    let _ = std::fs::remove_dir_all(work_dir);
}

#[test]
fn bounded_parser_handles_many_commands_without_rebasing_offsets() {
    let (mut parser, work_dir) = bounded_parser_for_test("many-commands", 96);
    for command in 0..12 {
        let preexec = format!(
            "\x1b]1337;COSH;{{\"event\":\"preexec\",\"token\":\"{TEST_MARKER_TOKEN}\",\"command\":\"echo {command}\",\"cwd\":\"/tmp\"}}\x07"
        );
        parser.feed(preexec.as_bytes()).expect("preexec");
        parser.feed(&[b'a' + command as u8; 41]).expect("output");
        let precmd = format!(
            "\x1b]1337;COSH;{{\"event\":\"precmd\",\"token\":\"{TEST_MARKER_TOKEN}\",\"status\":0,\"cwd\":\"/tmp\"}}\x07"
        );
        parser.feed(precmd.as_bytes()).expect("precmd");
    }

    let completed = parser
        .events
        .iter()
        .filter(|event| event.kind == ShellEventKind::CommandCompleted)
        .collect::<Vec<_>>();
    assert_eq!(completed.len(), 12);
    assert!(completed
        .iter()
        .all(|event| event.terminal_output_bytes == Some(41)));
    assert!(parser.clean.window_len() <= 192);
    assert!(parser.display.window_len() <= 192);
    let _ = std::fs::remove_dir_all(work_dir);
}

// S3 (stall-class audit 20260803, fixed by #2142): the pending handoff
// origin used to be a single `take()` slot consumed by the FIRST preexec
// after registration, so any unrelated command racing ahead burned it and
// the handoff's own block downgraded to UserInteractive — the same deadlock
// shape as the secret-redaction FAIL baseline, reached without redaction.
// Now the slot survives non-claiming preexecs.
#[test]
fn pending_handoff_origin_slot_survives_an_unrelated_preexec() {
    let mut parser = parser_for_test("origin-slot-race");
    let request = crate::types::ShellHandoffRequest::new(
        "echo handoff-target",
        "$ echo handoff-target",
        "approved_provider_shell_tool",
        "user",
        "req-race",
        "run-race",
        0,
    )
    .expect("handoff request");
    parser.register_pending_handoff_origin(&request);

    // An unrelated command wins the race to the next preexec.
    feed_preexec(&mut parser, "echo user-first");
    feed_precmd(&mut parser, 0);
    // The approved handoff command runs afterwards.
    feed_preexec(&mut parser, "echo handoff-target");
    feed_precmd(&mut parser, 0);

    let started_origins: Vec<_> = parser
        .events
        .iter()
        .filter(|event| event.kind == crate::types::ShellEventKind::CommandStarted)
        .map(|event| (event.command.clone(), event.command_origin))
        .collect();
    assert_eq!(started_origins.len(), 2, "{started_origins:?}");
    // The unrelated command is ordinary user input and leaves the slot alone.
    assert_eq!(
        started_origins[0],
        (
            Some("echo user-first".to_string()),
            Some(crate::types::CommandOrigin::UserInteractive)
        )
    );
    // The handoff's own block keeps ProviderTool, so the tracked closure
    // path stays alive.
    assert_eq!(
        started_origins[1],
        (
            Some("echo handoff-target".to_string()),
            Some(crate::types::CommandOrigin::ProviderTool)
        )
    );
}

// #2142 core mechanism: the marker echoes the staged claim token, so the
// handoff block keeps its origin and audit identity even when the reported
// command text was redacted by the marker script.
#[test]
fn handoff_token_claims_the_slot_despite_redacted_command_text() {
    let mut parser = parser_for_test("token-claim-redacted");
    let request = crate::types::ShellHandoffRequest::new(
        "deploy --api-key sk-secret",
        "$ deploy --api-key sk-secret",
        "approved_provider_shell_tool",
        "user",
        "req-token",
        "run-token",
        0,
    )
    .expect("handoff request");
    assert!(!request.token.is_empty(), "token minted at construction");
    parser.register_pending_handoff_origin(&request);

    feed_preexec_with_handoff(&mut parser, "<redacted sensitive command>", &request.token);

    let event = parser
        .events
        .iter()
        .find(|event| event.kind == ShellEventKind::CommandStarted)
        .expect("command started");
    assert_eq!(
        event.command_origin,
        Some(crate::types::CommandOrigin::ProviderTool)
    );
    let audit = event.audit_identity.as_ref().expect("audit identity");
    assert_eq!(audit.run_id, "run-token");
    assert_eq!(audit.handoff_token.as_deref(), Some(request.token.as_str()));
}

// A marker carrying a token that matches nothing is a stale or forged claim:
// it must neither adopt the handoff origin nor burn the slot silently.
#[test]
fn mismatched_handoff_token_reports_unknown_and_keeps_the_slot() {
    let mut parser = parser_for_test("token-claim-mismatch");
    let request = crate::types::ShellHandoffRequest::new(
        "echo handoff-target",
        "$ echo handoff-target",
        "approved_provider_shell_tool",
        "user",
        "req-stale",
        "run-stale",
        0,
    )
    .expect("handoff request");
    parser.register_pending_handoff_origin(&request);

    feed_preexec_with_handoff(&mut parser, "echo somebody-else", "not-the-token");
    feed_precmd(&mut parser, 0);
    feed_preexec(&mut parser, "echo handoff-target");

    let started_origins: Vec<_> = parser
        .events
        .iter()
        .filter(|event| event.kind == crate::types::ShellEventKind::CommandStarted)
        .map(|event| event.command_origin)
        .collect();
    assert_eq!(
        started_origins,
        vec![
            Some(crate::types::CommandOrigin::Unknown),
            Some(crate::types::CommandOrigin::ProviderTool),
        ]
    );
}

// An explicit token is exclusive: a marker reporting the *same text* as the
// pending request but a wrong token is a replayed or forged claim for that
// command line and must not fall back to the text match (#2142 review).
#[test]
fn mismatched_handoff_token_is_not_rescued_by_identical_command_text() {
    let mut parser = parser_for_test("token-claim-mismatch-same-text");
    let request = crate::types::ShellHandoffRequest::new(
        "echo handoff-target",
        "$ echo handoff-target",
        "approved_provider_shell_tool",
        "user",
        "req-replay",
        "run-replay",
        0,
    )
    .expect("handoff request");
    parser.register_pending_handoff_origin(&request);

    feed_preexec_with_handoff(&mut parser, "echo handoff-target", "not-the-token");

    let event = parser
        .events
        .iter()
        .find(|event| event.kind == ShellEventKind::CommandStarted)
        .expect("command started");
    assert_eq!(
        event.command_origin,
        Some(crate::types::CommandOrigin::Unknown),
        "identical text must not rescue a wrong-token claim"
    );
    assert!(event.audit_identity.is_none(), "no identity adoption");

    // The real handoff line (tokenless legacy shape) still claims afterwards.
    feed_precmd(&mut parser, 0);
    feed_preexec(&mut parser, "echo handoff-target");
    let second = parser
        .events
        .iter()
        .filter(|event| event.kind == ShellEventKind::CommandStarted)
        .nth(1)
        .expect("second command started");
    assert_eq!(
        second.command_origin,
        Some(crate::types::CommandOrigin::ProviderTool)
    );
}

// #2142 R4: a command-less prompt boundary is where the runtime closes an
// unclaimed handoff as untracked; the parser must expire the pending claim
// slot there and raise the staging-expiry flag, so a later user command with
// the same text can neither adopt the closed handoff's identity nor be
// misattributed to it.
#[test]
fn shell_ready_expires_an_unclaimed_pending_handoff_slot() {
    let mut parser = parser_for_test("untracked-expiry");
    let request = crate::types::ShellHandoffRequest::new(
        "echo handoff-target",
        "$ echo handoff-target",
        "approved_provider_shell_tool",
        "user",
        "req-untracked",
        "run-untracked",
        0,
    )
    .expect("handoff request");
    parser.register_pending_handoff_origin(&request);
    assert!(!parser.take_expired_handoff_staging(), "fresh staging");
    parser.register_pending_handoff_origin(&request);

    // Command-less precmd: the preexec marker was lost, the shell is back at
    // a prompt, and the runtime closes the handoff as untracked here.
    feed_precmd(&mut parser, 0);

    assert!(
        parser.take_expired_handoff_staging(),
        "expiry flag must ask the relay to clear the staged sidecars"
    );
    assert!(!parser.take_expired_handoff_staging(), "single shot");

    // The same text typed later by the user is ordinary input: no origin
    // adoption, no audit identity, no token.
    feed_preexec(&mut parser, "echo handoff-target");
    let event = parser
        .events
        .iter()
        .find(|event| event.kind == ShellEventKind::CommandStarted)
        .expect("command started");
    assert_eq!(
        event.command_origin,
        Some(crate::types::CommandOrigin::UserInteractive)
    );
    assert!(event.audit_identity.is_none(), "no stale identity adoption");
}

fn feed_preexec(parser: &mut OscParser, command: &str) {
    let marker = format!(
        "\x1b]1337;COSH;{{\"event\":\"preexec\",\"token\":\"test-marker-token\",\"command\":{command_json},\"cwd\":\"/tmp\"}}\x07",
        command_json = serde_json::to_string(command).expect("command json")
    );
    parser.feed(marker.as_bytes()).expect("feed preexec");
}

fn feed_preexec_with_handoff(parser: &mut OscParser, command: &str, handoff: &str) {
    let marker = format!(
        "\x1b]1337;COSH;{{\"event\":\"preexec\",\"token\":\"test-marker-token\",\"command\":{command_json},\"cwd\":\"/tmp\",\"handoff\":{handoff_json}}}\x07",
        command_json = serde_json::to_string(command).expect("command json"),
        handoff_json = serde_json::to_string(handoff).expect("handoff json")
    );
    parser.feed(marker.as_bytes()).expect("feed preexec");
}

fn feed_precmd(parser: &mut OscParser, status: i32) {
    let marker = format!(
        "\x1b]1337;COSH;{{\"event\":\"precmd\",\"token\":\"test-marker-token\",\"status\":{status},\"cwd\":\"/tmp\"}}\x07"
    );
    parser.feed(marker.as_bytes()).expect("feed precmd");
}

fn feed_environment_marker(
    parser: &mut OscParser,
    event: &str,
    command: Option<&str>,
    path: &str,
    path_trusted: bool,
    session_id: Option<&str>,
) {
    let marker = serde_json::json!({
        "event": event,
        "token": TEST_MARKER_TOKEN,
        "session_id": session_id,
        "command": command,
        "cwd": "/tmp",
        "path": path,
        "path_trusted": path_trusted,
        "status": 0,
    });
    let bytes = format!("\x1b]1337;COSH;{marker}\x07");
    parser
        .feed(bytes.as_bytes())
        .expect("feed environment marker");
}
