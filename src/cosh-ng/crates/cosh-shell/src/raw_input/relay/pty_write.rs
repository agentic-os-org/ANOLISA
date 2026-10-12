//! Single PTY write path for real user bytes and their prompt evidence.

use std::fs::File;
use std::io;

use super::super::event_sender::RawInputEventSink;
use super::super::generation::{LineSubmitCounter, UserPtyInputGeneration};
use super::super::{write_all_pty, MainPromptGate, RawInputEvent};

/// Writes real user bytes to the PTY, bumping the shared input generation
/// first so replayed-prompt state armed for an older generation expires
/// before the resulting PTY output can be parsed. The event also reports how
/// many line submissions the write carried, so the output loop can match
/// them against shell prompt boundaries.
pub(in super::super) fn write_user_bytes_to_pty(
    master: &mut File,
    input_generation: &UserPtyInputGeneration,
    line_submits: &mut LineSubmitCounter,
    input_events: &dyn RawInputEventSink,
    main_prompt_gate: &MainPromptGate,
    bytes: &[u8],
) -> io::Result<()> {
    let line_submits = line_submits.count(bytes);
    if line_submits > 0 {
        // A submitted line leaves the primary prompt until the marker emits
        // the next prompt_ready (#1721 D16).
        main_prompt_gate.set_at_prompt(false);
    }
    let generation = input_generation.bump();
    let _ = input_events.send(RawInputEvent::PtyUserWrite {
        generation,
        line_submits,
    });
    write_all_pty(master, bytes)
}
