//! Free-text buffering for question captures.

use super::CardInputState;

impl CardInputState {
    pub(super) fn append_free_text(&mut self, input: &[u8], idx: &mut usize) -> bool {
        if input[*idx].is_ascii() {
            self.free_text.push(input[*idx] as char);
            *idx += 1;
            return true;
        }
        let start = *idx;
        while *idx < input.len() && !input[*idx].is_ascii_control() && input[*idx] != 0x1b {
            *idx += 1;
        }
        let bytes = &input[start..*idx];
        match std::str::from_utf8(bytes) {
            Ok(text) => {
                self.free_text.push_str(text);
                true
            }
            Err(error) if error.error_len().is_none() => {
                let valid_len = error.valid_up_to();
                if valid_len > 0 {
                    self.free_text.push_str(
                        std::str::from_utf8(&bytes[..valid_len]).expect("validated UTF-8 prefix"),
                    );
                }
                self.pending_input.extend_from_slice(&bytes[valid_len..]);
                valid_len > 0
            }
            Err(_) => {
                self.free_text.push_str(&String::from_utf8_lossy(bytes));
                true
            }
        }
    }
}
