#[derive(Debug, Default)]
pub(super) struct TrialInputState {
    pub(super) pending: Vec<u8>,
    paste: bool,
    selected: Option<usize>,
}

impl TrialInputState {
    pub(super) fn consume(&mut self, bytes: &[u8]) -> Option<&'static str> {
        self.consume_mode(bytes, None, true)
            .and_then(|action| match action.as_str() {
                "settings" => Some("settings"),
                "cancel" => Some("cancel"),
                _ => None,
            })
    }

    pub(super) fn consume_settings(&mut self, bytes: &[u8], selected: usize) -> Option<String> {
        self.consume_mode(bytes, Some(selected), true)
    }

    pub(super) fn consume_transition(&mut self, bytes: &[u8]) -> Option<String> {
        self.consume_mode(bytes, None, false)
    }

    fn consume_mode(
        &mut self,
        bytes: &[u8],
        settings: Option<usize>,
        allow_settings: bool,
    ) -> Option<String> {
        let mut input = std::mem::take(&mut self.pending);
        input.extend_from_slice(bytes);
        let previous = *self.selected.get_or_insert(settings.unwrap_or(0));
        let mut index = 0;
        while index < input.len() {
            let byte = input[index];
            if byte == 3 {
                return Some("cancel".into());
            }
            if byte == 27 {
                if index + 1 == input.len() {
                    self.pending.extend_from_slice(&input[index..]);
                    break;
                }
                if input[index + 1] == b'[' {
                    let Some(end) = input[index + 2..]
                        .iter()
                        .position(|byte| (0x40..=0x7e).contains(byte))
                        .map(|offset| index + 2 + offset)
                    else {
                        self.pending.extend_from_slice(&input[index..]);
                        break;
                    };
                    match &input[index + 2..=end] {
                        b"200~" => self.paste = true,
                        b"201~" => self.paste = false,
                        b"A" | b"D" if settings.is_some() && !self.paste => self.selected = Some(0),
                        b"B" | b"C" if settings.is_some() && !self.paste => self.selected = Some(1),
                        _ => {}
                    }
                    index = end + 1;
                    continue;
                }
                if input[index + 1] == b'O' {
                    if index + 2 >= input.len() {
                        self.pending.extend_from_slice(&input[index..]);
                        break;
                    }
                    match input[index + 2] {
                        b'A' | b'D' if settings.is_some() && !self.paste => self.selected = Some(0),
                        b'B' | b'C' if settings.is_some() && !self.paste => self.selected = Some(1),
                        _ => {}
                    }
                    index += 3;
                    continue;
                }
                if !self.paste {
                    return Some(if settings.is_some() { "back" } else { "cancel" }.into());
                }
            }
            if !self.paste {
                if allow_settings && settings.is_none() && byte == b'n' {
                    return Some("settings".into());
                }
                if settings.is_some() {
                    match byte {
                        b'1' => self.selected = Some(0),
                        b'2' => self.selected = Some(1),
                        b'\r' | b'\n' => {
                            return Some(
                                if self.selected == Some(1) {
                                    "permanent"
                                } else {
                                    "session"
                                }
                                .into(),
                            )
                        }
                        _ => {}
                    }
                }
            }
            index += 1;
        }
        if settings.is_some() && self.selected != Some(previous) {
            return Some(format!("focus_{}", self.selected.unwrap_or(0)));
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn n_is_immediate_and_does_not_submit_the_following_enter() {
        let mut input = TrialInputState::default();
        assert_eq!(input.consume(b"n\n1\r"), Some("settings"));
    }

    #[test]
    fn enter_arrows_and_bracketed_paste_never_continue_or_disable() {
        let mut input = TrialInputState::default();
        assert_eq!(input.consume(b"\n\r12\x1b[A\x1b[200~n\n\x1b[201~"), None);
        assert_eq!(input.consume(b"\x03"), Some("cancel"));
    }

    #[test]
    fn split_csi_paste_and_utf8_are_consumed_without_shortcut_leaks() {
        let mut input = TrialInputState::default();
        for chunk in [
            b"\x1b[2".as_slice(),
            b"00~n",
            "中文".as_bytes(),
            b"\x1b[20",
            b"1~",
        ] {
            assert_eq!(input.consume(chunk), None);
        }
        assert_eq!(input.consume(b"n"), Some("settings"));
    }

    #[test]
    fn a_bare_escape_waits_for_split_sequence_then_cancels_on_timeout_marker() {
        let mut input = TrialInputState::default();
        assert_eq!(input.consume(b"\x1b"), None);
        assert_eq!(input.pending, b"\x1b");
        assert_eq!(input.consume(b"\x1b"), Some("cancel"));
    }
}

#[cfg(test)]
mod settings_tests {
    use super::*;
    #[test]
    fn application_cursor_keys_match_csi_including_split_input() {
        for (sequence, selected, expected) in [
            (b"\x1bOB".as_slice(), 0, "permanent"),
            (b"\x1bOA".as_slice(), 1, "session"),
            (b"\x1bOC".as_slice(), 0, "permanent"),
            (b"\x1bOD".as_slice(), 1, "session"),
        ] {
            for split in 0..=sequence.len() {
                let mut input = TrialInputState::default();
                input.consume_settings(&sequence[..split], selected);
                let mut rest = sequence[split..].to_vec();
                rest.push(b'\r');
                assert_eq!(
                    input.consume_settings(&rest, selected).as_deref(),
                    Some(expected)
                );
            }
        }
        let mut input = TrialInputState::default();
        assert_eq!(
            input.consume_settings(b"\x1b[200~\x1bOB\r\x1b[201~", 0),
            None
        );
        assert_eq!(input.consume_settings(b"\r", 0).as_deref(), Some("session"));
    }

    #[test]
    fn settings_confirmation_is_scoped_and_paste_cannot_confirm() {
        let mut input = TrialInputState::default();
        assert_eq!(input.consume_settings(b"\x1b[200~2\n\x1b[201~", 0), None);
        assert_eq!(
            input.consume_settings(b"\x1b[B\n", 0),
            Some("permanent".into())
        );
    }
}
