//! Absolute-offset terminal transcript storage for shell-host sessions.

use std::borrow::Cow;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::ops::Range;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

/// In-memory callers retain the historical complete-output contract. The
/// interactive runtime opts into a secure spool plus a bounded working window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TranscriptRetention {
    Full,
    Bounded { window_bytes: usize },
}

#[derive(Debug)]
pub(super) struct Transcript {
    bytes: Vec<u8>,
    base: usize,
    end: usize,
    window_bytes: Option<usize>,
    spool: Option<File>,
    // Absolute offsets let a failed backspace's scan survive appends and compaction.
    checked_continuations: Range<usize>,
}

impl Transcript {
    pub(super) fn new(retention: TranscriptRetention, spool_path: &Path) -> io::Result<Self> {
        let (window_bytes, spool) = match retention {
            TranscriptRetention::Full => (None, None),
            TranscriptRetention::Bounded { window_bytes } => {
                let mut options = OpenOptions::new();
                options.create_new(true).read(true).write(true);
                options.mode(0o600);
                let file = options.open(spool_path)?;
                std::fs::set_permissions(spool_path, std::fs::Permissions::from_mode(0o600))?;
                (Some(window_bytes.max(4)), Some(file))
            }
        };
        Ok(Self {
            bytes: Vec::new(),
            base: 0,
            end: 0,
            window_bytes,
            spool,
            checked_continuations: 0..0,
        })
    }

    /// Absolute byte position immediately after the transcript.
    pub(super) fn position(&self) -> usize {
        self.end
    }

    pub(super) fn len(&self) -> usize {
        self.position()
    }

    pub(super) fn is_empty(&self) -> bool {
        self.end == 0
    }

    pub(super) fn window_len(&self) -> usize {
        self.bytes.len()
    }

    pub(super) fn window_bytes(&self) -> Option<usize> {
        self.window_bytes
    }

    pub(super) fn is_full(&self) -> bool {
        self.spool.is_none()
    }

    pub(super) fn resident_slice(&self) -> &[u8] {
        &self.bytes
    }

    pub(super) fn append(&mut self, data: &[u8]) -> io::Result<()> {
        if data.is_empty() {
            return Ok(());
        }
        if let Some(spool) = self.spool.as_mut() {
            spool.seek(SeekFrom::Start(self.end as u64))?;
            spool.write_all(data)?;
        }
        self.bytes.extend_from_slice(data);
        self.end = self.end.saturating_add(data.len());
        self.compact_window();
        Ok(())
    }

    pub(super) fn pop_last_utf8_char(&mut self, lower_bound: usize) -> io::Result<()> {
        let floor = lower_bound.max(self.base);
        if floor >= self.end {
            return Ok(());
        }

        let start = self
            .backspace_scan_ranges(floor)
            .into_iter()
            .find_map(|range| {
                self.bytes[range.start - self.base..range.end - self.base]
                    .iter()
                    .rposition(|byte| byte & 0b1100_0000 != 0b1000_0000)
                    .map(|start| range.start + start)
            });
        let end = match start {
            Some(start) => start,
            // Preserve the historical fallback for malformed bytes at absolute zero.
            None if floor == 0 => 0,
            // A continuation-only tail may start before the deletion boundary.
            None => {
                let checked = &self.checked_continuations;
                let start = if floor <= checked.end {
                    floor.min(checked.start)
                } else {
                    floor
                };
                self.checked_continuations = start..self.end;
                return Ok(());
            }
        };
        if let Some(spool) = self.spool.as_mut() {
            spool.set_len(end as u64)?;
        }
        self.bytes.truncate(end - self.base);
        self.end = end;
        if end < self.checked_continuations.end {
            self.checked_continuations = 0..0;
        }
        Ok(())
    }

    fn backspace_scan_ranges(&self, floor: usize) -> [Range<usize>; 2] {
        let checked = &self.checked_continuations;
        [
            floor.max(checked.end)..self.end,
            floor..floor.max(checked.start),
        ]
    }

    pub(super) fn read_range(&self, start: usize, end: usize) -> io::Result<Cow<'_, [u8]>> {
        self.validate_range(start, end)?;
        if start >= self.base {
            return Ok(Cow::Borrowed(
                &self.bytes[start - self.base..end - self.base],
            ));
        }
        let mut bytes = vec![0_u8; end - start];
        let mut file = self
            .spool
            .as_ref()
            .ok_or_else(|| io::Error::other("transcript range is no longer resident"))?
            .try_clone()?;
        file.seek(SeekFrom::Start(start as u64))?;
        file.read_exact(&mut bytes)?;
        Ok(Cow::Owned(bytes))
    }

    pub(super) fn visit_range_chunks(
        &self,
        start: usize,
        end: usize,
        mut visit: impl FnMut(&[u8]) -> io::Result<()>,
    ) -> io::Result<()> {
        self.validate_range(start, end)?;
        if start >= self.base {
            return visit(&self.bytes[start - self.base..end - self.base]);
        }
        self.visit_spooled_chunks(start, end, visit)
    }

    // Keep the replay buffer's stack probes off the resident output path.
    #[inline(never)]
    fn visit_spooled_chunks(
        &self,
        start: usize,
        end: usize,
        mut visit: impl FnMut(&[u8]) -> io::Result<()>,
    ) -> io::Result<()> {
        let mut file = self
            .spool
            .as_ref()
            .ok_or_else(|| io::Error::other("transcript range is no longer resident"))?
            .try_clone()?;
        file.seek(SeekFrom::Start(start as u64))?;
        let mut remaining = end - start;
        let mut buffer = [0_u8; 64 * 1024];
        while remaining > 0 {
            let take = remaining.min(buffer.len());
            file.read_exact(&mut buffer[..take])?;
            visit(&buffer[..take])?;
            remaining -= take;
        }
        Ok(())
    }

    pub(super) fn starts_with_at(&self, start: usize, prefix: &[u8]) -> io::Result<bool> {
        let end = start.saturating_add(prefix.len());
        if end > self.end {
            return Ok(false);
        }
        Ok(self.read_range(start, end)?.as_ref() == prefix)
    }

    pub(super) fn copy_range_to<W: Write>(
        &self,
        start: usize,
        end: usize,
        output: &mut W,
    ) -> io::Result<()> {
        self.visit_range_chunks(start, end, |bytes| output.write_all(bytes))
    }

    pub(super) fn into_output_bytes(self) -> Vec<u8> {
        if self.is_full() {
            return self.bytes;
        }
        Vec::new()
    }

    fn validate_range(&self, start: usize, end: usize) -> io::Result<()> {
        if start > end || end > self.end {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "invalid transcript range {start}..{end}: base={} position={} resident={} window={:?}",
                    self.base,
                    self.end,
                    self.bytes.len(),
                    self.window_bytes
                ),
            ));
        }
        Ok(())
    }

    fn compact_window(&mut self) {
        let Some(window_bytes) = self.window_bytes else {
            return;
        };
        if self.bytes.len() <= window_bytes.saturating_mul(2) {
            return;
        }
        let drop = self.bytes.len() - window_bytes;
        self.bytes.drain(..drop);
        self.base = self.base.saturating_add(drop);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "cosh-transcript-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ))
    }

    #[test]
    fn bounded_transcript_keeps_absolute_ranges_across_compactions() {
        let path = temp_path("ranges");
        let _ = std::fs::remove_file(&path);
        let mut transcript =
            Transcript::new(TranscriptRetention::Bounded { window_bytes: 8 }, &path)
                .expect("transcript");
        transcript.append(b"abcdefgh").expect("first window");
        transcript.append(b"ijklmnop").expect("second window");
        transcript.append(b"qrstuvwx").expect("third window");

        assert_eq!(transcript.position(), 24);
        assert_eq!(transcript.window_len(), 8);
        assert_eq!(
            transcript.read_range(2, 22).expect("range"),
            &b"cdefghijklmnopqrstuv"[..]
        );
        let mut output = Vec::new();
        transcript
            .copy_range_to(2, 17, &mut output)
            .expect("spooled");
        transcript
            .copy_range_to(17, 22, &mut output)
            .expect("resident");
        assert_eq!(output, b"cdefghijklmnopqrstuv");
        assert_eq!(
            std::fs::metadata(&path)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn clean_backspace_updates_compacted_spool_and_absolute_end() {
        let path = temp_path("backspace");
        let _ = std::fs::remove_file(&path);
        let mut transcript =
            Transcript::new(TranscriptRetention::Bounded { window_bytes: 4 }, &path)
                .expect("transcript");
        transcript.append(b"abcdefgh").expect("prefix");
        transcript.append("x头".as_bytes()).expect("append");
        assert_eq!(transcript.resident_slice(), "x头".as_bytes());

        transcript
            .pop_last_utf8_char(11)
            .expect("crosses command start");
        assert_eq!(transcript.position(), 12);
        assert_eq!(
            std::fs::read(&path).expect("spool"),
            "abcdefghx头".as_bytes()
        );

        transcript
            .pop_last_utf8_char(9)
            .expect("pop whole character");
        transcript.pop_last_utf8_char(9).expect("at command start");
        assert_eq!(transcript.position(), 9);
        assert_eq!(std::fs::read(&path).expect("spool"), b"abcdefghx");

        transcript
            .pop_last_utf8_char(8)
            .expect("pop final resident byte");
        transcript.pop_last_utf8_char(0).expect("no resident bytes");
        assert_eq!(transcript.position(), 8);
        assert!(transcript.resident_slice().is_empty());
        transcript.append(b"d").expect("append replacement");
        assert_eq!(transcript.position(), 9);
        assert_eq!(
            transcript.read_range(0, 9).expect("range"),
            &b"abcdefghd"[..]
        );
        assert_eq!(std::fs::read(&path).expect("spool"), b"abcdefghd");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn clean_backspace_preserves_character_split_by_compaction() {
        let path = temp_path("split-backspace");
        let _ = std::fs::remove_file(&path);
        let mut transcript =
            Transcript::new(TranscriptRetention::Bounded { window_bytes: 4 }, &path)
                .expect("transcript");
        transcript
            .append("abcdefgh头xyz".as_bytes())
            .expect("append");
        for _ in 0..4 {
            transcript.pop_last_utf8_char(0).expect("backspace");
        }

        assert_eq!(transcript.position(), 11);
        assert_eq!(
            std::fs::read(&path).expect("spool"),
            "abcdefgh头".as_bytes()
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn clean_backspace_skips_checked_continuations() {
        const WINDOW: usize = 256 * 1024;
        const FLOOR: usize = 2;
        let mut output = vec![0x80; 2 * WINDOW - 16];
        output[..5].copy_from_slice("a🙂".as_bytes());

        for retention in [
            TranscriptRetention::Full,
            TranscriptRetention::Bounded {
                window_bytes: WINDOW,
            },
        ] {
            let path = temp_path("checked-continuations");
            let _ = std::fs::remove_file(&path);
            let mut transcript = Transcript::new(retention, &path).expect("transcript");
            transcript.append(&output).expect("output");
            transcript
                .pop_last_utf8_char(FLOOR)
                .expect("first backspace");
            for _ in 0..64 {
                assert!(transcript
                    .backspace_scan_ranges(FLOOR)
                    .iter()
                    .all(|r| r.is_empty()));
                transcript
                    .pop_last_utf8_char(FLOOR)
                    .expect("repeated backspace");
            }
            assert_eq!(transcript.resident_slice(), output);

            transcript.append(b"x").expect("append ordinary output");
            transcript
                .pop_last_utf8_char(FLOOR)
                .expect("pop appended byte");
            assert!(transcript
                .backspace_scan_ranges(FLOOR)
                .iter()
                .all(|r| r.is_empty()));

            transcript.append(b"\x80").expect("append continuation");
            assert_eq!(
                transcript.backspace_scan_ranges(FLOOR),
                [output.len()..output.len() + 1, FLOOR..FLOOR]
            );
            transcript
                .pop_last_utf8_char(FLOOR)
                .expect("check appended byte");
            assert_eq!(transcript.position(), output.len() + 1);

            assert_eq!(
                transcript.backspace_scan_ranges(1),
                [transcript.position()..transcript.position(), 1..FLOOR]
            );
            transcript.pop_last_utf8_char(1).expect("earlier boundary");
            assert_eq!(transcript.resident_slice(), b"a");
            transcript.append(b"bc").expect("replace deleted bytes");
            transcript.pop_last_utf8_char(1).expect("pop replacement");
            assert_eq!(transcript.resident_slice(), b"ab");
            if !transcript.is_full() {
                assert_eq!(std::fs::read(&path).expect("spool"), b"ab");
                std::fs::remove_file(&path).expect("remove spool");
            }
        }
    }

    #[test]
    fn invalid_range_reports_absolute_window_context() {
        let path = temp_path("invalid-range");
        let _ = std::fs::remove_file(&path);
        let mut transcript =
            Transcript::new(TranscriptRetention::Bounded { window_bytes: 4 }, &path)
                .expect("transcript");
        transcript.append(b"abcdefghijkl").expect("append");

        let error = transcript.read_range(0, 13).expect_err("invalid range");
        let message = error.to_string();
        assert!(message.contains("base=8"), "{message}");
        assert!(message.contains("position=12"), "{message}");
        assert!(message.contains("resident=4"), "{message}");
        assert!(message.contains("window=Some(4)"), "{message}");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn full_transcript_returns_every_byte_unchanged() {
        let path = temp_path("full");
        let mut transcript =
            Transcript::new(TranscriptRetention::Full, &path).expect("full transcript");
        let first = vec![b'a'; 1024];
        let second = b"\x1b[31mterminal\x1b[0m\n";
        transcript.append(&first).expect("first append");
        transcript.append(second).expect("second append");

        let mut expected = first;
        expected.extend_from_slice(second);
        assert_eq!(transcript.into_output_bytes(), expected);
        assert!(!path.exists());
    }
}
