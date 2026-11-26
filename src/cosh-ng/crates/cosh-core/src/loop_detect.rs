use std::collections::VecDeque;

const MAX_HISTORY: usize = 10;
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
        // A digest of the whole input keeps the fingerprint bounded without
        // collapsing distinct actions that share a long prefix.
        let fingerprint = format!("{}:{}", tool_name, input_digest(tool_input));

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

fn input_digest(input: &str) -> String {
    use sha2::{Digest, Sha256};
    use std::fmt::Write as _;

    let digest = Sha256::digest(input.as_bytes());
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(output, "{byte:02x}");
    }
    output
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
    fn fingerprint_covers_the_whole_input() {
        let mut d = LoopDetector::new();
        let long_input = "x".repeat(500);
        let slightly_different = format!("{}y", "x".repeat(499));
        for _ in 0..3 {
            assert!(!d.record_action("shell", &long_input));
            assert!(!d.record_action("shell", &slightly_different));
        }
        assert!(!d.record_action("shell", &long_input));
        assert!(!d.record_action("shell", &long_input));
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
