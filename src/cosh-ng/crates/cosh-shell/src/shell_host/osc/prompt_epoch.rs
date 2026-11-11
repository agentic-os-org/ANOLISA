//! Bridges parsed prompt boundaries to bounded input-epoch snapshots.

use super::OscParser;

impl OscParser {
    pub(crate) fn set_prompt_epoch_exchange(
        &mut self,
        exchange: crate::raw_input::PromptEpochExchange,
    ) {
        self.prompt_epoch_exchange = Some(exchange);
    }

    pub(super) fn open_prompt_epoch(&mut self) {
        self.prompt_epoch = self
            .prompt_epoch_exchange
            .as_ref()
            .map(crate::raw_input::PromptEpochExchange::open);
    }

    pub(in crate::shell_host) fn publish_quiescent_prompt_snapshot(&self) {
        if self.has_active_foreground_command()
            || !self.main_prompt_gate.is_at_prompt()
            || !self.has_prompt_painted_since_ready()
        {
            return;
        }
        let (Some(exchange), Some(epoch)) = (&self.prompt_epoch_exchange, self.prompt_epoch) else {
            return;
        };
        let starts_at_line_start = self.last_prompt_display_start.is_some_and(|start| {
            self.read_display_range(start.saturating_sub(4096), start)
                .is_ok_and(|prefix| starts_at_line_start(&prefix))
        });
        exchange.publish_with_origin(epoch, self.last_prompt_display(), starts_at_line_start);
    }

    pub(super) fn take_claimed_prompt_snapshot(&self) -> Option<std::sync::Arc<[u8]>> {
        self.prompt_epoch_exchange
            .as_ref()
            .zip(self.prompt_epoch)
            .and_then(|(exchange, epoch)| exchange.take_claimed(epoch))
    }

    #[cfg(test)]
    pub(in crate::shell_host) fn pending_slash_guard_prompt_for_test(&self) -> Option<&[u8]> {
        self.pending_slash_guard_echo
            .as_ref()
            .map(super::PendingSlashGuardEcho::prompt_before_input_for_test)
    }
}

fn starts_at_line_start(prefix: &[u8]) -> bool {
    let Some(newline) = prefix.windows(2).rposition(|bytes| bytes == b"\r\n") else {
        return false;
    };
    let mut remaining = &prefix[newline + 2..];
    while !remaining.is_empty() {
        let Some(title) = remaining.strip_prefix(b"\x1b]") else {
            return false;
        };
        if !title.starts_with(b"0;") && !title.starts_with(b"2;") {
            return false;
        }
        let Some(end) = title.iter().position(|byte| *byte < 0x20 || *byte == 0x7f) else {
            return false;
        };
        let terminator = &title[end..];
        remaining = match terminator
            .strip_prefix(b"\x07")
            .or_else(|| terminator.strip_prefix(b"\x1b\\"))
        {
            Some(rest) => rest,
            None => return false,
        };
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_before_prompt_preserves_a_known_line_origin() {
        assert!(starts_at_line_start(b"\r\n"));
        assert!(starts_at_line_start(b"\r\n\x1b]0;user@host:~/project\x07"));
        assert!(starts_at_line_start(b"output\r\n\x1b]2;title\x1b\\"));
        assert!(starts_at_line_start(b"\r\n\x1b]0;one\x07\x1b]2;two\x07"));
    }

    #[test]
    fn visible_output_or_unknown_controls_keep_the_origin_unknown() {
        for prefix in [
            b"output\x1b]0;title\x07".as_slice(),
            b"\r\nKEPT_PREFIX\x1b]0;title\x07",
            b"\r\n\x1b[2A",
            b"\r\n\x1b]0;unfinished",
            b"\r\n\x1b]52;clipboard\x07",
            b"\r\n\x1b]0;title\x1b[2A\x07",
        ] {
            assert!(!starts_at_line_start(prefix), "{prefix:?}");
        }
    }
}
