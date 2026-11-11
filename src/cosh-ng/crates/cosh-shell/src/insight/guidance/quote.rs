pub(crate) fn bash_arg(value: &str) -> Option<String> {
    use std::fmt::Write;

    if value.contains('\0') {
        return None;
    }
    let mut quoted = String::from("$'");
    for ch in value.chars() {
        match ch {
            '\'' => quoted.push_str("\\'"),
            '\\' => quoted.push_str("\\\\"),
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            '\t' => quoted.push_str("\\t"),
            ch if ch.is_ascii_control() => {
                write!(quoted, "\\x{:02x}", ch as u32).ok()?;
            }
            ch if ch.is_control() => {
                write!(quoted, "\\u{:04x}", ch as u32).ok()?;
            }
            ch => quoted.push(ch),
        }
    }
    quoted.push('\'');
    Some(quoted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bash_arg_quotes_metacharacters_as_one_literal_argument() {
        assert_eq!(
            bash_arg("a'b\\c\n\t\r\x1b$(id);\"中文"),
            Some("$'a\\'b\\\\c\\n\\t\\r\\x1b$(id);\"中文'".to_string())
        );
        assert_eq!(bash_arg(""), Some("$''".to_string()));
    }

    #[test]
    fn nul_cannot_be_an_argv_argument() {
        assert_eq!(bash_arg("before\0after"), None);
    }

    #[test]
    fn ascii_controls_are_visible_and_unambiguous() {
        assert_eq!(bash_arg("\x01f\x7f"), Some("$'\\x01f\\x7f'".to_string()));
    }

    #[test]
    fn bash_arg_round_trips_as_one_argument_in_bash() {
        for value in ["", "a'b\\c\n\t\r\x1b$(printf injected);\"中文", "\x01f\x7f"] {
            let quoted = bash_arg(value).unwrap();
            let script = format!("set -- {quoted}; test \"$#\" -eq 1 && printf '%s' \"$1\"");
            use std::io::Read;
            use std::process::{Command, Stdio};
            use std::time::{Duration, Instant};

            let mut child = Command::new("/bin/bash")
                .args(["--noprofile", "--norc", "-c", &script])
                .env_remove("BASH_ENV")
                .env_remove("ENV")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(1);
            let status = loop {
                if let Some(status) = child.try_wait().unwrap() {
                    break status;
                }
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("Bash argv protocol exceeded its deadline");
                }
                std::thread::sleep(Duration::from_millis(2));
            };
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            child
                .stdout
                .take()
                .unwrap()
                .take(1025)
                .read_to_end(&mut stdout)
                .unwrap();
            child
                .stderr
                .take()
                .unwrap()
                .take(1025)
                .read_to_end(&mut stderr)
                .unwrap();
            assert!(status.success());
            assert!(stderr.is_empty());
            assert_eq!(stdout, value.as_bytes());
        }
    }
}
