//! Transcript storage construction and absolute display-range delegation.

use std::borrow::Cow;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use super::super::transcript::TranscriptRetention;
use super::marker_sequence::discard_abandoned_prefix;
use super::{AltScreenTracker, OscParser, Transcript, VisibleTailTracker};
use crate::types::SESSION_OUTPUT_REF_MAX_BYTES;

use super::super::osc_output::{
    write_output_ref_range_with_session_cap, write_output_ref_with_session_cap, OutputRefCapture,
};

impl OscParser {
    pub(in super::super) fn flush_pending(&mut self) -> io::Result<()> {
        let mut pending = std::mem::take(&mut self.pending);
        discard_abandoned_prefix(&mut pending, &mut self.abandoned_prefix_len);
        if !pending.starts_with(super::OSC_PREFIX) {
            self.append_passthrough(&pending)?;
        }
        self.flush_pending_slash_guard_echo()?;
        self.flush_pending_handoff_echo()?;
        self.flush_pending_clean_control()
    }

    pub(crate) fn new(session_id: String, output_ref_dir: PathBuf, marker_token: String) -> Self {
        let work_dir = output_ref_dir
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        Self::with_retention(
            session_id,
            output_ref_dir,
            marker_token,
            TranscriptRetention::Full,
            &work_dir,
        )
        .unwrap_or_else(|_| unreachable!("full transcript storage does not open files"))
    }

    pub(crate) fn with_retention(
        session_id: String,
        output_ref_dir: PathBuf,
        marker_token: String,
        retention: TranscriptRetention,
        work_dir: &Path,
    ) -> io::Result<Self> {
        Self::with_optional_marker(
            session_id,
            output_ref_dir,
            Some(marker_token),
            retention,
            work_dir,
        )
    }

    pub(crate) fn passthrough_with_retention(
        session_id: String,
        output_ref_dir: PathBuf,
        retention: TranscriptRetention,
        work_dir: &Path,
    ) -> io::Result<Self> {
        Self::with_optional_marker(session_id, output_ref_dir, None, retention, work_dir)
    }

    fn with_optional_marker(
        session_id: String,
        output_ref_dir: PathBuf,
        marker_token: Option<String>,
        retention: TranscriptRetention,
        work_dir: &Path,
    ) -> io::Result<Self> {
        // Enhanced marker tokens are random hex. Native sessions use a fixed
        // suffix because each session already owns a private work directory.
        let spool_suffix = marker_token
            .as_deref()
            .unwrap_or("native")
            .chars()
            .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
            .collect::<String>();
        Ok(Self {
            session_id,
            output_ref_dir,
            events: super::EventStore::new(
                retention,
                &work_dir.join(format!("events-{spool_suffix}.jsonl")),
            )?,
            clean: Transcript::new(
                retention,
                &work_dir.join(format!("terminal-output-{spool_suffix}.transcript")),
            )?,
            display: Transcript::new(
                retention,
                &work_dir.join(format!("display-{spool_suffix}.transcript")),
            )?,
            marker_token,
            pending: Vec::new(),
            abandoned_prefix_len: 0,
            pending_clean_control: Vec::new(),
            current: None,
            command_seq: 0,
            intervention_cuts: Vec::new(),
            intervention_display_cuts: Vec::new(),
            last_prompt_display_start: None,
            last_prompt_display: Vec::new(),
            capture_prompt_display: false,
            prompt_ready_display_start: None,
            prompt_presentation_display_starts: Vec::new(),
            prompt_epoch_exchange: None,
            prompt_epoch: None,
            synthetic_prompt_repaint_armed: false,
            captured_output_ref_bytes: 0,
            pending_command_origin: None,
            expired_handoff_staging: false,
            pending_handoff_echo: None,
            pending_slash_guard_echo: None,
            shell_environment_snapshot: None,
            environment_observer: None,
            history_file_tracker: super::HistoryFileTracker::default(),
            main_prompt_gate: crate::raw_input::MainPromptGate::default(),
            pending_prompt_intercepts: Vec::new(),
            submission_boundary_observed: false,
            assistance_control: None,
            shell_prompt_cwd: crate::input::ShellPromptCwd::default(),
            shell_path_command_names: crate::input::ShellPathCommandNames::default(),
            pty_input_barrier_pushed: false,
            visible_tail: VisibleTailTracker::default(),
            alt_screen: AltScreenTracker::default(),
        })
    }

    pub(crate) fn display_position(&self) -> usize {
        self.display.position()
    }

    pub(crate) fn read_display_range(&self, start: usize, end: usize) -> io::Result<Cow<'_, [u8]>> {
        self.display.read_range(start, end)
    }

    pub(crate) fn write_display_range<W: Write>(
        &self,
        start: usize,
        end: usize,
        output: &mut W,
    ) -> io::Result<()> {
        self.display.copy_range_to(start, end, output)
    }

    pub(crate) fn display_starts_with_at(&self, start: usize, prefix: &[u8]) -> io::Result<bool> {
        self.display.starts_with_at(start, prefix)
    }

    pub(super) fn append_prompt_display_tail(&mut self, data: &[u8]) {
        if !self.capture_prompt_display {
            return;
        }
        let Some(max) = self.display.window_bytes() else {
            return;
        };
        if data.len() >= max {
            self.last_prompt_display.clear();
            self.last_prompt_display
                .extend_from_slice(&data[data.len() - max..]);
            return;
        }
        let drop = self
            .last_prompt_display
            .len()
            .saturating_add(data.len())
            .saturating_sub(max);
        if drop > 0 {
            self.last_prompt_display.drain(..drop);
        }
        self.last_prompt_display.extend_from_slice(data);
        debug_assert!(self.last_prompt_display.len() <= max);
    }

    pub(super) fn capture_command_output_ref(
        &mut self,
        command_id: &str,
        output_start: usize,
        output_end: usize,
    ) -> io::Result<OutputRefCapture> {
        let capture = if self.clean.is_full() {
            let output = self
                .clean
                .read_range(output_start, output_end)?
                .into_owned();
            write_output_ref_with_session_cap(
                &self.output_ref_dir,
                command_id,
                &output,
                self.captured_output_ref_bytes,
                SESSION_OUTPUT_REF_MAX_BYTES,
            )?
        } else {
            write_output_ref_range_with_session_cap(
                &self.output_ref_dir,
                command_id,
                &self.clean,
                output_start,
                output_end,
                self.captured_output_ref_bytes,
                SESSION_OUTPUT_REF_MAX_BYTES,
            )?
        };
        self.captured_output_ref_bytes = self
            .captured_output_ref_bytes
            .saturating_add(capture.captured_bytes);
        Ok(capture)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distinct_session_tokens_do_not_collide_in_a_reused_work_dir() {
        let work_dir = std::env::temp_dir().join(format!(
            "cosh-transcript-reused-work-dir-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&work_dir);
        let output_ref_dir = work_dir.join("output-refs");
        std::fs::create_dir_all(&output_ref_dir).expect("output ref dir");

        let first = OscParser::with_retention(
            "first".to_string(),
            output_ref_dir.clone(),
            "a1b2c3d4".to_string(),
            TranscriptRetention::Bounded { window_bytes: 64 },
            &work_dir,
        )
        .expect("first parser");
        let second = OscParser::with_retention(
            "second".to_string(),
            output_ref_dir,
            "e5f6a7b8".to_string(),
            TranscriptRetention::Bounded { window_bytes: 64 },
            &work_dir,
        )
        .expect("second parser");

        assert!(work_dir
            .join("terminal-output-a1b2c3d4.transcript")
            .is_file());
        assert!(work_dir.join("display-a1b2c3d4.transcript").is_file());
        assert!(work_dir
            .join("terminal-output-e5f6a7b8.transcript")
            .is_file());
        assert!(work_dir.join("display-e5f6a7b8.transcript").is_file());
        drop((first, second));
        let _ = std::fs::remove_dir_all(work_dir);
    }
}
