use std::collections::VecDeque;

const MAX_HISTORY: usize = 10;
const FINGERPRINT_PREFIX_LEN: usize = 200;
const LOOP_THRESHOLD: usize = 3;

pub struct LoopDetector {
    history: VecDeque<String>,
}

impl LoopDetector {
    pub fn new() -> Self {
        Self {
            history: VecDeque::new(),
        }
    }

    pub fn record_action(&mut self, tool_name: &str, tool_input: &str) -> bool {
        let fingerprint = format!(
            "{}:{}",
            tool_name,
            utf8_prefix(tool_input, FINGERPRINT_PREFIX_LEN)
        );

        self.history.push_back(fingerprint.clone());
        if self.history.len() > MAX_HISTORY {
            self.history.pop_front();
        }

        if self.history.len() < LOOP_THRESHOLD {
            return false;
        }

        let recent: Vec<&String> = self.history.iter().rev().take(LOOP_THRESHOLD).collect();
        recent.iter().all(|f| *f == &fingerprint)
    }

    pub fn loop_warning() -> &'static str {
        "You appear to be repeating the same action. Please try a different approach or ask the user for guidance."
    }
}

fn utf8_prefix(input: &str, max_bytes: usize) -> &str {
    if input.len() <= max_bytes {
        return input;
    }

    let mut end = 0;
    for (idx, ch) in input.char_indices() {
        let next = idx + ch.len_utf8();
        if next > max_bytes {
            break;
        }
        end = next;
    }
    &input[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_loop_on_different_actions() {
        let mut d = LoopDetector::new();
        assert!(!d.record_action("shell", "echo 1"));
        assert!(!d.record_action("shell", "echo 2"));
        assert!(!d.record_action("shell", "echo 3"));
    }

    #[test]
    fn detects_loop_on_repeated_action() {
        let mut d = LoopDetector::new();
        assert!(!d.record_action("shell", "echo hello"));
        assert!(!d.record_action("shell", "echo hello"));
        assert!(d.record_action("shell", "echo hello"));
    }

    #[test]
    fn resets_after_different_action() {
        let mut d = LoopDetector::new();
        assert!(!d.record_action("shell", "echo hello"));
        assert!(!d.record_action("shell", "echo hello"));
        assert!(!d.record_action("read_file", "/tmp/file.txt"));
        assert!(!d.record_action("shell", "echo hello"));
    }

    #[test]
    fn fingerprint_uses_prefix_only() {
        let mut d = LoopDetector::new();
        let long_input = "x".repeat(500);
        let slightly_different = format!("{}y", "x".repeat(499));
        assert!(!d.record_action("shell", &long_input));
        assert!(!d.record_action("shell", &slightly_different));
        assert!(d.record_action("shell", &long_input));
    }

    #[test]
    fn fingerprint_does_not_split_utf8() {
        let mut d = LoopDetector::new();
        let long_input = "最近一次 OOM 事件 ".repeat(50);

        assert!(!d.record_action("shell", &long_input));
        assert!(!d.record_action("shell", &long_input));
        assert!(d.record_action("shell", &long_input));
    }
}
