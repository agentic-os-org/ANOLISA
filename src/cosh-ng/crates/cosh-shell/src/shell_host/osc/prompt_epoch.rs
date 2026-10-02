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
        exchange.publish(epoch, self.last_prompt_display());
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
