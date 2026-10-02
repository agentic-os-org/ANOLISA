use crate::provider::Message;
use crate::truncator::truncate_to_byte_limit;

const BYTES_PER_TOKEN: usize = 4;
const SUMMARY_MESSAGE_BYTES: usize = 200;

pub struct ChatCompression {
    context_limit: u64,
    threshold_ratio: f64,
}

impl ChatCompression {
    pub fn new(context_limit: u64) -> Self {
        Self {
            context_limit,
            threshold_ratio: 0.7,
        }
    }

    pub fn estimate_tokens(messages: &[Message]) -> u64 {
        let total_bytes: usize = messages
            .iter()
            .map(|m| m.content.as_text().len() + m.role.len() + 4)
            .sum();
        (total_bytes / BYTES_PER_TOKEN) as u64
    }

    pub fn needs_compression(&self, messages: &[Message]) -> bool {
        let estimated = Self::estimate_tokens(messages);
        estimated > (self.context_limit as f64 * self.threshold_ratio) as u64
    }

    pub fn compress(&self, messages: &[Message]) -> Vec<Message> {
        if messages.len() <= 4 {
            return messages.to_vec();
        }

        let keep_count = (messages.len() as f64 * 0.3).ceil() as usize;
        let keep_count = keep_count.max(2);

        let to_summarize = &messages[..messages.len() - keep_count];
        let to_keep = &messages[messages.len() - keep_count..];

        let summary = Self::build_summary(to_summarize);

        let mut compressed = vec![Message::system(&format!(
            "[Conversation summary of {} earlier messages]\n\n{}",
            to_summarize.len(),
            summary
        ))];
        compressed.extend_from_slice(to_keep);

        compressed
    }

    fn build_summary(messages: &[Message]) -> String {
        let mut parts = Vec::new();

        for msg in messages {
            let text = msg.content.as_text();
            if text.is_empty() {
                continue;
            }
            let truncated = if text.len() > SUMMARY_MESSAGE_BYTES {
                format!(
                    "{}...",
                    truncate_to_byte_limit(&text, SUMMARY_MESSAGE_BYTES)
                )
            } else {
                text.to_string()
            };
            parts.push(format!("[{}] {}", msg.role, truncated));
        }

        parts.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimate_tokens_basic() {
        let messages = vec![Message::user("hello world"), Message::assistant("hi there")];
        let tokens = ChatCompression::estimate_tokens(&messages);
        assert!(tokens > 0);
    }

    #[test]
    fn no_compression_needed_for_short_conversations() {
        let cc = ChatCompression::new(128_000);
        let messages = vec![Message::user("hello"), Message::assistant("hi")];
        assert!(!cc.needs_compression(&messages));
    }

    #[test]
    fn compression_needed_for_long_conversations() {
        let cc = ChatCompression::new(100);
        let messages: Vec<Message> = (0..50)
            .map(|i| {
                Message::user(&format!(
                    "message {i} with some content to inflate token count"
                ))
            })
            .collect();
        assert!(cc.needs_compression(&messages));
    }

    #[test]
    fn compress_preserves_recent_messages() {
        let cc = ChatCompression::new(128_000);
        let messages: Vec<Message> = (0..10)
            .map(|i| Message::user(&format!("message {i}")))
            .collect();

        let compressed = cc.compress(&messages);
        assert!(compressed.len() < messages.len());

        let last = &compressed[compressed.len() - 1];
        assert_eq!(last.content.as_text(), "message 9");
    }

    #[test]
    fn compress_too_short_returns_original() {
        let cc = ChatCompression::new(128_000);
        let messages = vec![Message::user("a"), Message::assistant("b")];
        let compressed = cc.compress(&messages);
        assert_eq!(compressed.len(), messages.len());
    }

    #[test]
    fn summary_includes_role_prefix() {
        let messages = vec![
            Message::user("what is rust?"),
            Message::assistant("A systems programming language."),
        ];
        let summary = ChatCompression::build_summary(&messages);
        assert!(summary.contains("[user]"));
        assert!(summary.contains("[assistant]"));
    }

    fn assert_summary_truncation(input: &str, expected: &str) {
        let summary = ChatCompression::build_summary(&[Message::user(input)]);
        assert_eq!(summary, format!("[user] {expected}..."));
    }

    #[test]
    fn summary_preserves_ascii_truncation_behavior() {
        assert_summary_truncation(&"a".repeat(201), &"a".repeat(200));
    }

    #[test]
    fn summary_truncates_multibyte_text_at_exact_utf8_boundaries() {
        assert_summary_truncation(
            &format!("{}中z", "a".repeat(197)),
            &format!("{}中", "a".repeat(197)),
        );
        assert_summary_truncation(
            &format!("{}😀z", "a".repeat(196)),
            &format!("{}😀", "a".repeat(196)),
        );
    }

    #[test]
    fn summary_rounds_multibyte_limits_down_to_utf8_boundaries() {
        assert_summary_truncation(&format!("{}中", "a".repeat(199)), &"a".repeat(199));
        assert_summary_truncation(&format!("{}😀", "a".repeat(198)), &"a".repeat(198));
    }
}
