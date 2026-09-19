//! Bounded replacement for Bash Readline's internal slash-guard redisplay.
//!
//! An authenticated arm suppresses the static guard until the matching slash
//! intercept supplies Readline's exact line; unrelated output stays ordered.

use std::{borrow::Cow, io};

use super::{marker_sequence::Marker, OscParser};

const GUARD_COMMAND: &[u8] = b"case $- in *x*) builtin set +x; builtin true __cosh_slash_guard__; builtin set -x ;; *) : ;; esac";
const GUARD_REDRAW_TAIL: &[u8] = b"__cosh_slash_guard__; builtin set -x ;; *) : ;; esac";
// A 40-column Bash redraw exposes this exact 25-byte tail. Shorter suffixes
// are too weak to distinguish safely from unrelated terminal output.
const MIN_PROVEN_HSCROLL_SUFFIX: &[u8] = b"in set -x ;; *) : ;; esac";
const MAX_PENDING_BYTES: usize = 64 * 1024;
const MAX_PENDING_LINES: usize = 16;

pub(super) fn safe_display_command(command: &str) -> bool {
    // Preserve Readline's ordinary tab-stop rendering, but reject controls
    // that could make authenticated marker text affect another screen line.
    !command.is_empty()
        && command.len() <= MAX_PENDING_BYTES
        && !command
            .chars()
            .any(|character| character.is_control() && character != '\t')
}

enum GuardEcho {
    ReadlineRedraw { starts_at: usize },
    Subsequent { starts_at: usize },
}

#[derive(Debug)]
struct SuppressedRedraw {
    prefix: Vec<u8>,
    line_ending: Vec<u8>,
    deferred: Vec<u8>,
}

pub(super) struct SlashGuardResolution {
    pub(super) prefix: Vec<u8>,
    pub(super) suffix: Vec<u8>,
    pub(super) insert_command: bool,
}

#[derive(Debug)]
pub(super) struct PendingSlashGuardEcho {
    line: Vec<u8>,
    lines: usize,
    bytes_seen: usize,
    redraw_suppressed: bool,
    suppressed_redraw: Option<SuppressedRedraw>,
    // Used only to avoid duplicating a direct submission that Readline had
    // already painted exactly; any dirty-redisplay mismatch is reconstructed.
    before_arm: Vec<u8>,
    prompt_before_input: Vec<u8>,
}

impl PendingSlashGuardEcho {
    pub(super) fn new(before_arm: &[u8]) -> Self {
        Self::new_with_prompt(before_arm, &[])
    }

    fn new_with_prompt(before_arm: &[u8], prompt_before_input: &[u8]) -> Self {
        let retained_from = before_arm.len().saturating_sub(MAX_PENDING_BYTES);
        let prompt_retained_from = prompt_before_input.len().saturating_sub(MAX_PENDING_BYTES);
        Self {
            line: Vec::new(),
            lines: 0,
            bytes_seen: 0,
            redraw_suppressed: false,
            suppressed_redraw: None,
            before_arm: before_arm[retained_from..].to_vec(),
            prompt_before_input: prompt_before_input[prompt_retained_from..].to_vec(),
        }
    }

    #[cfg(test)]
    pub(super) fn prompt_before_input_for_test(&self) -> &[u8] {
        &self.prompt_before_input
    }

    pub(super) fn filter<'a>(slot: &mut Option<Self>, data: &'a [u8]) -> Cow<'a, [u8]> {
        if slot.is_none() {
            return Cow::Borrowed(data);
        }
        let mut output = Vec::with_capacity(data.len());
        for byte in data.iter().copied() {
            let Some(pending) = slot.as_mut() else {
                output.push(byte);
                continue;
            };
            pending.line.push(byte);
            pending.bytes_seen += 1;
            if pending.bytes_seen >= MAX_PENDING_BYTES {
                output.extend_from_slice(&pending.release_suppress_only());
                *slot = None;
                continue;
            }
            if byte == b'\n' {
                match pending.classify_guard_echo() {
                    Some(GuardEcho::ReadlineRedraw { starts_at }) => {
                        pending.suppressed_redraw = Some(SuppressedRedraw {
                            prefix: pending.line[..starts_at].to_vec(),
                            line_ending: pending.line_ending().to_vec(),
                            deferred: Vec::new(),
                        });
                        pending.line.clear();
                        pending.redraw_suppressed = true;
                        continue;
                    }
                    Some(GuardEcho::Subsequent { starts_at }) => {
                        let ending = pending.line_ending().to_vec();
                        if let Some(redraw) = pending.suppressed_redraw.as_mut() {
                            redraw
                                .deferred
                                .extend_from_slice(&pending.line[..starts_at]);
                            redraw.deferred.extend_from_slice(&ending);
                        }
                        pending.line.clear();
                        continue;
                    }
                    None => {}
                }
                // Before the redraw, complete unrelated lines can be released
                // immediately. After it, retain them so the replacement stays
                // ahead of asynchronous job output when the intercept arrives.
                if let Some(redraw) = pending.suppressed_redraw.as_mut() {
                    redraw.deferred.extend_from_slice(&pending.line);
                } else {
                    output.extend_from_slice(&pending.line);
                }
                pending.line.clear();
                pending.lines += 1;
                if pending.lines >= MAX_PENDING_LINES {
                    output.extend_from_slice(&pending.release_suppress_only());
                    *slot = None;
                }
            }
        }
        Cow::Owned(output)
    }

    pub(super) fn flush(slot: &mut Option<Self>) -> Vec<u8> {
        slot.take()
            .map_or_else(Vec::new, |mut pending| pending.release_suppress_only())
    }

    pub(super) fn resolve(slot: &mut Option<Self>, command: &[u8]) -> Option<SlashGuardResolution> {
        let mut pending = slot.take()?;
        let Some(redraw) = pending.suppressed_redraw.take() else {
            *slot = Some(pending);
            return None;
        };
        // Exact suffix matching deliberately biases ambiguous Readline state
        // toward inserting the authenticated command instead of dropping it.
        let insert_command = !pending
            .before_arm
            .strip_suffix(redraw.line_ending.as_slice())
            .is_some_and(|before_ending| {
                Self::proves_painted_command(before_ending, command, &redraw.prefix)
            });
        let mut suffix = redraw.line_ending;
        suffix.extend_from_slice(&redraw.deferred);
        suffix.extend_from_slice(&pending.line);
        Some(SlashGuardResolution {
            prefix: redraw.prefix,
            suffix,
            insert_command,
        })
    }

    fn proves_painted_command(before_ending: &[u8], command: &[u8], prefix: &[u8]) -> bool {
        let Some(before_command) = before_ending.strip_suffix(command) else {
            return false;
        };
        if before_command.ends_with(prefix) {
            return true;
        }
        // In a UTF-8 locale, Home-Space-End can repaint the full prompt
        // after CR. The single blank is the existing private-history guard;
        // require that exact prompt boundary before suppressing a duplicate.
        if !prefix.is_empty()
            && before_command
                .strip_suffix(b" ")
                .and_then(|before_blank| before_blank.strip_suffix(prefix))
                .is_some_and(|before_prompt| before_prompt.ends_with(b"\r"))
        {
            return true;
        }
        if !before_command.ends_with(b"\x08") {
            return false;
        }

        // Readline may expose Home-Space-End as an exact repaint sequence:
        // command, back to its start, padded command, back again, command.
        let Ok(command) = std::str::from_utf8(command) else {
            return false;
        };
        if command.contains('\t') {
            return false;
        }
        let columns = unicode_width::UnicodeWidthStr::width(command);
        if columns == 0 {
            return false;
        }
        let command = command.as_bytes();
        Self::strip_backspaces(before_command, columns)
            .and_then(|before_end_move| before_end_move.strip_suffix(command))
            .and_then(|before_padding| before_padding.strip_suffix(b" "))
            .and_then(|before_home| Self::strip_backspaces(before_home, columns))
            .and_then(|before_original| before_original.strip_suffix(command))
            .is_some_and(|before_rewrite| before_rewrite.ends_with(prefix))
    }

    fn strip_backspaces(bytes: &[u8], count: usize) -> Option<&[u8]> {
        let split = bytes.len().checked_sub(count)?;
        bytes[split..]
            .iter()
            .all(|byte| *byte == b'\x08')
            .then_some(&bytes[..split])
    }

    fn classify_guard_echo(&self) -> Option<GuardEcho> {
        let body = self.line_body()?;
        let starts_at = Self::unique_static_guard_start(body)?;
        if self.redraw_suppressed {
            Some(GuardEcho::Subsequent { starts_at })
        } else {
            Some(GuardEcho::ReadlineRedraw { starts_at })
        }
    }

    fn unique_static_guard_start(body: &[u8]) -> Option<usize> {
        if let Some(start) = Self::unique_unwrapped_static_guard_start(body) {
            return Some(start);
        }
        if !body.contains(&b'\r') {
            return None;
        }
        // Bash 4.4 can insert either a bare CR or `previous byte, CR,
        // previous byte` when Readline wraps the static guard. Match against
        // the same exact bounded command after collapsing only those repaint
        // bytes, then map the proven start back without changing the display.
        let mut unwrapped = Vec::with_capacity(body.len());
        let mut original_offsets = Vec::with_capacity(body.len());
        let mut index = 0;
        while index < body.len() {
            if body[index] == b'\r' {
                index += 1;
                if unwrapped.last().copied() == body.get(index).copied() {
                    index += 1;
                }
                continue;
            }
            unwrapped.push(body[index]);
            original_offsets.push(index);
            index += 1;
        }
        let unwrapped_start = Self::unique_unwrapped_static_guard_start(&unwrapped)?;
        original_offsets.get(unwrapped_start).copied()
    }

    fn unique_unwrapped_static_guard_start(body: &[u8]) -> Option<usize> {
        // Readline may replace the omitted left side with '<'. Require the
        // remaining suffix to be long, exact, and the only guard-shaped text;
        // ambiguity fails open so unrelated terminal output is never deleted.
        let trimmed_end = body
            .iter()
            .rposition(|byte| !matches!(byte, b' ' | b'\x08'))?
            + 1;
        let trimmed = &body[..trimmed_end];
        let full = trimmed
            .strip_suffix(GUARD_COMMAND)
            .map(|prefix| prefix.len());
        let hscroll = trimmed
            .iter()
            .enumerate()
            .filter(|(_, byte)| **byte == b'<')
            .filter_map(|(start, _)| {
                let suffix = &trimmed[start + 1..];
                (suffix.len() >= MIN_PROVEN_HSCROLL_SUFFIX.len()
                    && suffix.len() < GUARD_COMMAND.len()
                    && GUARD_COMMAND.ends_with(suffix))
                .then_some(start)
            });
        let mut matches = full.into_iter().chain(hscroll);
        let starts_at = matches.next()?;
        if matches.next().is_some() {
            return None;
        }

        let static_suffix = if trimmed[starts_at] == b'<' {
            &trimmed[starts_at + 1..]
        } else {
            &trimmed[starts_at..]
        };
        let expected_tails = usize::from(
            static_suffix
                .windows(GUARD_REDRAW_TAIL.len())
                .any(|window| window == GUARD_REDRAW_TAIL),
        );
        let observed_tails = trimmed
            .windows(GUARD_REDRAW_TAIL.len())
            .filter(|window| *window == GUARD_REDRAW_TAIL)
            .count();
        (observed_tails == expected_tails).then_some(starts_at)
    }

    fn line_body(&self) -> Option<&[u8]> {
        let without_lf = self.line.strip_suffix(b"\n")?;
        Some(without_lf.strip_suffix(b"\r").unwrap_or(without_lf))
    }

    fn line_ending(&self) -> &'static [u8] {
        if self.line.ends_with(b"\r\n") {
            b"\r\n"
        } else {
            b"\n"
        }
    }

    fn release_suppress_only(&mut self) -> Vec<u8> {
        let Some(redraw) = self.suppressed_redraw.take() else {
            return std::mem::take(&mut self.line);
        };
        let mut output = redraw.prefix;
        output.extend_from_slice(&redraw.line_ending);
        output.extend_from_slice(&redraw.deferred);
        output.append(&mut self.line);
        output
    }
}

impl OscParser {
    /// Handles authenticated slash guard lifecycle markers before routing
    /// records an intervention display cut.
    pub(super) fn handle_slash_guard_marker(&mut self, marker: &Marker) -> io::Result<bool> {
        if marker.event == "slash_guard" {
            self.flush_pending_slash_guard_echo()?;
            let prompt_before_input = self.take_claimed_prompt_snapshot();
            let pending = PendingSlashGuardEcho::new_with_prompt(
                self.last_prompt_display(),
                prompt_before_input.as_deref().unwrap_or_default(),
            );
            self.pending_slash_guard_echo = Some(pending);
            return Ok(true);
        }

        let matching_command = (marker.event == "intercept"
            && marker.reason.as_deref() == Some("slash"))
        .then_some(marker.command.as_deref())
        .flatten()
        .filter(|command| safe_display_command(command));
        if let Some(command) = matching_command {
            self.resolve_pending_slash_guard_echo(command)?;
        } else {
            self.flush_pending_slash_guard_echo()?;
        }
        Ok(false)
    }

    pub(super) fn flush_pending_slash_guard_echo(&mut self) -> io::Result<()> {
        let pending = PendingSlashGuardEcho::flush(&mut self.pending_slash_guard_echo);
        if pending.is_empty() {
            return Ok(());
        }
        self.append_passthrough(&pending)
    }

    pub(super) fn resolve_pending_slash_guard_echo(&mut self, command: &str) -> io::Result<()> {
        let Some(resolution) =
            PendingSlashGuardEcho::resolve(&mut self.pending_slash_guard_echo, command.as_bytes())
        else {
            return self.flush_pending_slash_guard_echo();
        };
        self.append_passthrough(&resolution.prefix)?;
        if resolution.insert_command {
            self.append_display_only(command.as_bytes())?;
        }
        self.append_passthrough(&resolution.suffix)
    }

    fn append_display_only(&mut self, data: &[u8]) -> io::Result<()> {
        if data.is_empty() {
            return Ok(());
        }
        self.alt_screen.observe(data);
        self.visible_tail.feed(data);
        self.display.append(data)?;
        self.append_prompt_display_tail(data);
        Ok(())
    }
}

#[cfg(test)]
#[path = "slash_guard_echo_presentation_tests.rs"]
mod presentation_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn resolve(pending: &mut Option<PendingSlashGuardEcho>) -> SlashGuardResolution {
        PendingSlashGuardEcho::resolve(pending, b"/mode").expect("suppressed guard redraw")
    }

    #[test]
    fn suppresses_fragmented_guard_redisplay() {
        let mut pending = Some(PendingSlashGuardEcho::new(b""));
        assert!(PendingSlashGuardEcho::filter(&mut pending, b"<builtin ").is_empty());
        assert!(PendingSlashGuardEcho::filter(
            &mut pending,
            b"true __cosh_slash_guard__; builtin set -x ;; *) : ;; esac\r\n"
        )
        .is_empty());
        let resolution = resolve(&mut pending);
        assert!(resolution.prefix.is_empty());
        assert_eq!(resolution.suffix, b"\r\n");
    }

    #[test]
    fn suppresses_multiline_prompt_redisplay() {
        let mut pending = Some(PendingSlashGuardEcho::new(b""));
        assert_eq!(
            PendingSlashGuardEcho::filter(&mut pending, b"first prompt line\r\n").as_ref(),
            b"first prompt line\r\n"
        );
        assert!(PendingSlashGuardEcho::filter(
            &mut pending,
            b"<builtin true __cosh_slash_guard__; builtin set -x ;; *) : ;; esac\r\n"
        )
        .is_empty());
        assert_eq!(resolve(&mut pending).suffix, b"\r\n");
    }

    #[test]
    fn suppresses_readline_cleanup_after_shorter_guard() {
        let mut pending = Some(PendingSlashGuardEcho::new(b""));
        assert!(PendingSlashGuardEcho::filter(
            &mut pending,
            b"<builtin true __cosh_slash_guard__; builtin set -x ;; *) : ;; esac      \x08\x08\x08\r\n"
        )
        .is_empty());
        assert_eq!(resolve(&mut pending).suffix, b"\r\n");
    }

    #[test]
    fn preserves_partial_output_before_guard_redisplay() {
        let mut pending = Some(PendingSlashGuardEcho::new(b""));
        assert!(PendingSlashGuardEcho::filter(
            &mut pending,
            b"BACKGROUND_PARTIAL<builtin true __cosh_slash_guard__; builtin set -x ;; *) : ;; esac\r\n"
        )
        .is_empty());
        let resolution = resolve(&mut pending);
        assert_eq!(resolution.prefix, b"BACKGROUND_PARTIAL");
        assert_eq!(resolution.suffix, b"\r\n");
    }

    #[test]
    fn preserves_carriage_return_partial_output_before_guard_redisplay() {
        let mut pending = Some(PendingSlashGuardEcho::new(b""));
        assert!(PendingSlashGuardEcho::filter(
            &mut pending,
            b"BEFORE\rBACKGROUND_PARTIAL<builtin true __cosh_slash_guard__; builtin set -x ;; *) : ;; esac\r\n"
        )
        .is_empty());
        let resolution = resolve(&mut pending);
        assert_eq!(resolution.prefix, b"BEFORE\rBACKGROUND_PARTIAL");
        assert_eq!(resolution.suffix, b"\r\n");
    }

    #[test]
    fn suppresses_verbose_echo_after_guard_redisplay() {
        let mut pending = Some(PendingSlashGuardEcho::new(b""));
        assert!(PendingSlashGuardEcho::filter(
            &mut pending,
            b"<builtin true __cosh_slash_guard__; builtin set -x ;; *) : ;; esac\r\ncase $- in *x*) builtin set +x; builtin true __cosh_slash_guard__; builtin set -x ;; *) : ;; esac\r\n"
        )
        .is_empty());
        assert_eq!(resolve(&mut pending).suffix, b"\r\n\r\n");
    }

    #[test]
    fn suppresses_hscroll_duplicate_after_guard_redisplay() {
        let mut pending = Some(PendingSlashGuardEcho::new(b""));
        assert!(PendingSlashGuardEcho::filter(
            &mut pending,
            b"<builtin true __cosh_slash_guard__; builtin set -x ;; *) : ;; esac\r\n"
        )
        .is_empty());
        assert!(PendingSlashGuardEcho::filter(
            &mut pending,
            b"BACKGROUND<_cosh_slash_guard__; builtin set -x ;; *) : ;; esac\r\n"
        )
        .is_empty());

        let resolution = resolve(&mut pending);
        assert_eq!(resolution.prefix, b"");
        assert_eq!(resolution.suffix, b"\r\nBACKGROUND\r\n");
        assert!(!resolution
            .suffix
            .windows(b"_cosh_slash_guard__".len())
            .any(|window| window == b"_cosh_slash_guard__"));
    }

    #[test]
    fn suppresses_narrow_hscroll_as_first_guard_redisplay() {
        let mut pending = Some(PendingSlashGuardEcho::new(b""));
        assert!(
            PendingSlashGuardEcho::filter(&mut pending, b"<in set -x ;; *) : ;; esac\r\n")
                .is_empty()
        );

        let resolution = resolve(&mut pending);
        assert_eq!(resolution.prefix, b"");
        assert_eq!(resolution.suffix, b"\r\n");
        assert!(resolution.insert_command);
    }

    #[test]
    fn verbose_echo_preserves_unrelated_partial_prefix() {
        let mut pending = Some(PendingSlashGuardEcho::new(b""));
        assert!(PendingSlashGuardEcho::filter(
            &mut pending,
            b"<builtin true __cosh_slash_guard__; builtin set -x ;; *) : ;; esac\r\n"
        )
        .is_empty());
        assert!(PendingSlashGuardEcho::filter(
            &mut pending,
            b"BACKGROUND_PARTIALcase $- in *x*) builtin set +x; builtin true __cosh_slash_guard__; builtin set -x ;; *) : ;; esac\r\n"
        )
        .is_empty());
        assert_eq!(resolve(&mut pending).suffix, b"\r\nBACKGROUND_PARTIAL\r\n");
    }

    #[test]
    fn unproven_sentinel_line_fails_open() {
        let mut pending = Some(PendingSlashGuardEcho::new(b""));
        let line = b"BACKGROUND_PARTIAL builtin true __cosh_slash_guard__ unrelated\r\n";
        assert_eq!(
            PendingSlashGuardEcho::filter(&mut pending, line).as_ref(),
            line
        );
        assert!(pending.is_some());
    }

    #[test]
    fn ambiguous_static_guard_lines_fail_open() {
        let mut pending = Some(PendingSlashGuardEcho::new(b""));
        let mut line = GUARD_REDRAW_TAIL.to_vec();
        line.extend_from_slice(b" unrelated ");
        line.extend_from_slice(GUARD_COMMAND);
        line.extend_from_slice(b"\r\n");

        assert_eq!(
            PendingSlashGuardEcho::filter(&mut pending, &line).as_ref(),
            line
        );
        assert!(pending.is_some());
    }

    #[test]
    fn complete_guard_match_preserves_preceding_angle_bracket() {
        let mut pending = Some(PendingSlashGuardEcho::new(b""));
        assert!(PendingSlashGuardEcho::filter(
            &mut pending,
            b"BACKGROUND_PARTIAL<case $- in *x*) builtin set +x; builtin true __cosh_slash_guard__; builtin set -x ;; *) : ;; esac\r\n"
        )
        .is_empty());
        let resolution = resolve(&mut pending);
        assert_eq!(resolution.prefix, b"BACKGROUND_PARTIAL<");
        assert_eq!(resolution.suffix, b"\r\n");
    }

    #[test]
    fn unarmed_filter_borrows_the_input() {
        let mut pending = None;
        assert!(matches!(
            PendingSlashGuardEcho::filter(&mut pending, b"ordinary output"),
            Cow::Borrowed(b"ordinary output")
        ));
    }

    #[test]
    fn mismatch_and_cap_fail_open() {
        let mut mismatch = Some(PendingSlashGuardEcho::new(b""));
        let bytes = vec![b'x'; MAX_PENDING_BYTES];
        assert_eq!(PendingSlashGuardEcho::filter(&mut mismatch, &bytes), bytes);
        assert!(mismatch.is_none());

        let mut marker = Some(PendingSlashGuardEcho::new(b""));
        assert!(PendingSlashGuardEcho::filter(&mut marker, b"partial").is_empty());
        assert_eq!(PendingSlashGuardEcho::flush(&mut marker), b"partial");
        assert!(marker.is_none());
    }

    #[test]
    fn line_cap_preserves_every_non_matching_line() {
        let mut pending = Some(PendingSlashGuardEcho::new(b""));
        for _ in 0..MAX_PENDING_LINES {
            assert_eq!(
                PendingSlashGuardEcho::filter(&mut pending, b"background\r\n").as_ref(),
                b"background\r\n"
            );
        }
        assert!(pending.is_none());
    }

    #[test]
    fn guard_command_stays_in_sync_with_bash() {
        let guard = std::str::from_utf8(GUARD_COMMAND).expect("ASCII guard command");
        let bash = include_str!("../marker/bash.sh");

        assert_eq!(bash.matches(guard).count(), 2);
        assert!(GUARD_COMMAND.ends_with(GUARD_REDRAW_TAIL));
        assert!(GUARD_COMMAND.ends_with(MIN_PROVEN_HSCROLL_SUFFIX));
    }

    #[test]
    fn display_replacement_rejects_unbounded_or_control_text() {
        assert!(safe_display_command("/mode"));
        assert!(safe_display_command("/mode\targ"));
        assert!(!safe_display_command(""));
        assert!(!safe_display_command("/mode\nnext"));
        assert!(!safe_display_command(&"x".repeat(MAX_PENDING_BYTES + 1)));
    }

    #[test]
    fn narrow_hscroll_proof_requires_the_exact_unique_boundary() {
        let boundary = b"<in set -x ;; *) : ;; esac";
        let below_boundary = b"<n set -x ;; *) : ;; esac";
        let near_miss = b"<in set +x ;; *) : ;; esac";
        assert_eq!(boundary.len() - 1, MIN_PROVEN_HSCROLL_SUFFIX.len());
        assert_eq!(
            PendingSlashGuardEcho::unique_static_guard_start(boundary),
            Some(0)
        );
        assert_eq!(
            PendingSlashGuardEcho::unique_static_guard_start(below_boundary),
            None
        );
        assert_eq!(
            PendingSlashGuardEcho::unique_static_guard_start(near_miss),
            None
        );

        let mut ambiguous = GUARD_REDRAW_TAIL.to_vec();
        ambiguous.extend_from_slice(b" unrelated ");
        ambiguous.extend_from_slice(boundary);
        assert_eq!(
            PendingSlashGuardEcho::unique_static_guard_start(&ambiguous),
            None
        );
    }
}
