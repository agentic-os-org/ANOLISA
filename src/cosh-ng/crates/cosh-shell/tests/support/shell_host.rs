use std::io::Read;
use std::path::Path;
use std::time::Duration;

use cosh_shell::journal::read_shell_events;
use cosh_shell::ledger::{build_command_blocks, LedgerOutput};
use cosh_shell::shell_host::ShellHostOutput;
use cosh_shell::types::CommandBlock;

pub(crate) fn unique_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

pub(crate) fn ledger_from_output(output: &ShellHostOutput) -> LedgerOutput {
    let replayed_events = read_shell_events(&output.journal_path).expect("journal events");
    let ledger = build_command_blocks(&replayed_events);
    assert!(ledger.errors.is_empty(), "{:?}", ledger.errors);
    ledger
}

pub(crate) fn ledger_output_refs_text(ledger: &LedgerOutput) -> String {
    let mut text = String::new();
    for block in &ledger.blocks {
        let Some(path) = block.output.terminal_output_ref.as_deref() else {
            continue;
        };
        if let Ok(output) = std::fs::read_to_string(path) {
            text.push_str(&output);
            text.push('\n');
        }
    }
    text
}

pub(crate) fn assert_no_osc_marker(output: &[u8]) {
    assert!(!output
        .windows(b"\x1b]1337;COSH;".len())
        .any(|window| window == b"\x1b]1337;COSH;"));
}

/// Models observed ASCII bytes and controls, not Unicode cell widths or graphemes.
pub(crate) fn render_terminal_screen(output: &[u8], width: usize, height: usize) -> Vec<String> {
    assert!(width > 0, "terminal width must be non-zero");
    assert!(height > 0, "terminal height must be non-zero");
    let mut rows = vec![Vec::<u8>::new(); height];
    let (mut row, mut column, mut index) = (0usize, 0usize, 0usize);
    let mut saved_cursor = None;

    while index < output.len() {
        match output[index] {
            b'\r' => {
                column = 0;
                index += 1;
            }
            b'\n' => {
                terminal_line_feed(&mut rows, &mut row);
                column = 0;
                index += 1;
            }
            b'\x08' => {
                column = column.saturating_sub(1);
                index += 1;
            }
            b'\t' => {
                column = (((column / 8) + 1) * 8).min(width - 1);
                index += 1;
            }
            b'\x1b' if output.get(index + 1) == Some(&b']') => {
                index += 2;
                while index < output.len() {
                    if output[index] == b'\x07' {
                        index += 1;
                        break;
                    }
                    if output[index] == b'\x1b' && output.get(index + 1) == Some(&b'\\') {
                        index += 2;
                        break;
                    }
                    index += 1;
                }
            }
            b'\x1b' if output.get(index + 1) == Some(&b'[') => {
                let params_start = index + 2;
                index = params_start;
                while index < output.len() && !(0x40..=0x7e).contains(&output[index]) {
                    index += 1;
                }
                if index == output.len() {
                    break;
                }
                let final_byte = output[index];
                let params = &output[params_start..index];
                let first = terminal_csi_param(params, 0, 1);
                match final_byte {
                    b'A' => row = row.saturating_sub(first),
                    b'B' => row = (row + first).min(height - 1),
                    b'C' => column = (column + first).min(width - 1),
                    b'D' => column = column.saturating_sub(first),
                    b'G' => column = first.saturating_sub(1).min(width - 1),
                    b'H' | b'f' => {
                        row = terminal_csi_param(params, 0, 1)
                            .saturating_sub(1)
                            .min(height - 1);
                        column = terminal_csi_param(params, 1, 1).saturating_sub(1);
                        column = column.min(width - 1);
                    }
                    b'K' => {
                        let mode = terminal_csi_param(params, 0, 0);
                        match mode {
                            1 => {
                                let end = column.saturating_add(1).min(rows[row].len());
                                rows[row][..end].fill(b' ');
                            }
                            2 => rows[row].clear(),
                            _ => rows[row].truncate(column),
                        }
                    }
                    _ => {}
                }
                index += 1;
            }
            b'\x1b' if output.get(index + 1) == Some(&b'7') => {
                saved_cursor = Some((row, column));
                index += 2;
            }
            b'\x1b' if output.get(index + 1) == Some(&b'8') => {
                if let Some((saved_row, saved_column)) = saved_cursor {
                    row = saved_row;
                    column = saved_column;
                }
                index += 2;
            }
            b'\x1b' => index += usize::from(index + 1 < output.len()) + 1,
            byte if byte >= b' ' => {
                if column == width {
                    terminal_line_feed(&mut rows, &mut row);
                    column = 0;
                }
                if rows[row].len() <= column {
                    rows[row].resize(column + 1, b' ');
                }
                rows[row][column] = byte;
                column += 1;
                index += 1;
            }
            _ => index += 1,
        }
    }

    rows.into_iter()
        .map(|mut row| {
            while row.last() == Some(&b' ') {
                row.pop();
            }
            String::from_utf8_lossy(&row).into_owned()
        })
        .collect()
}

fn terminal_line_feed(rows: &mut [Vec<u8>], row: &mut usize) {
    if *row + 1 < rows.len() {
        *row += 1;
    } else {
        rows.rotate_left(1);
        rows.last_mut().expect("non-empty terminal").clear();
    }
}

fn terminal_csi_param(params: &[u8], index: usize, default: usize) -> usize {
    params
        .split(|byte| *byte == b';')
        .nth(index)
        .and_then(|value| std::str::from_utf8(value).ok())
        .and_then(|value| value.trim_start_matches('?').parse().ok())
        .unwrap_or(default)
}

pub(crate) fn assert_clean_shell_output_ref(block: &CommandBlock, expected: &str) {
    let output_ref = block
        .output
        .terminal_output_ref
        .as_deref()
        .expect("terminal output ref");
    let text = std::fs::read_to_string(output_ref).expect("output ref text");
    assert!(text.contains(expected), "{text:?}");
    assert!(!text.contains("\x1b[?2004"), "{text:?}");
    assert!(!text.contains('\u{0008}'), "{text:?}");
    assert!(!text.contains("\x1b[0m"), "{text:?}");
    assert!(!text.contains("\x1b[27m"), "{text:?}");
    assert!(!text.contains("\x1b[24m"), "{text:?}");
    assert!(!text.contains("\x1b[J"), "{text:?}");
    assert!(!text.contains("\x1b[K"), "{text:?}");
}

pub(crate) fn shell_arg(path: &Path) -> String {
    let value = path.display().to_string();
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub(crate) fn stty_flag_probe(
    flag: &str,
    on_marker: &str,
    off_marker: &str,
    cleanup: &str,
) -> String {
    format!(
        "if stty -a | tr ' ;' '\\n\\n' | grep -qx -- {flag}; then printf '%s\\n' {on_marker}; else printf '%s\\n' {off_marker}; fi; {cleanup}",
    )
}

pub(crate) fn assert_no_synthetic_terminal_restore_after_interrupt(rendered: &[u8]) {
    for sequence in [
        b"\x1b[?1049l".as_slice(),
        b"\x1b[2J".as_slice(),
        b"\x1bc".as_slice(),
        b"COSH_INTERNAL_RESTORE".as_slice(),
        b"stty echo icanon".as_slice(),
    ] {
        assert!(
            !rendered
                .windows(sequence.len())
                .any(|window| window == sequence),
            "unexpected synthetic terminal restore sequence {:?} in {}",
            sequence,
            String::from_utf8_lossy(rendered)
        );
    }
}

pub(crate) fn assert_fullscreen_terminal_modes_balanced(rendered: &[u8]) {
    for (enter, leave) in [
        (b"\x1b[?1049h".as_slice(), b"\x1b[?1049l".as_slice()),
        (b"\x1b[?25l".as_slice(), b"\x1b[?25h".as_slice()),
        (b"\x1b[?2004h".as_slice(), b"\x1b[?2004l".as_slice()),
        (b"\x1b[?7l".as_slice(), b"\x1b[?7h".as_slice()),
    ] {
        let Some(enter_pos) = find_bytes(rendered, enter) else {
            continue;
        };
        let Some(leave_pos) = find_bytes(rendered, leave) else {
            panic!(
                "terminal mode {:?} was entered but {:?} was not restored in {}",
                enter,
                leave,
                String::from_utf8_lossy(rendered)
            );
        };
        assert!(
            leave_pos > enter_pos,
            "terminal restore {:?} appeared before enter {:?} in {}",
            leave,
            enter,
            String::from_utf8_lossy(rendered)
        );
    }
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(unix)]
pub(crate) fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = std::fs::metadata(path)
        .expect("tool metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).expect("tool permissions");
}

pub(crate) struct DelayedInput {
    chunks: Vec<(Vec<u8>, Duration)>,
    index: usize,
}

impl DelayedInput {
    pub(crate) fn new(chunks: Vec<(Vec<u8>, Duration)>) -> Self {
        Self { chunks, index: 0 }
    }
}

impl Read for DelayedInput {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let Some((chunk, delay)) = self.chunks.get(self.index) else {
            return Ok(0);
        };

        std::thread::sleep(*delay);
        let len = chunk.len().min(buf.len());
        buf[..len].copy_from_slice(&chunk[..len]);
        self.index += 1;
        Ok(len)
    }
}
