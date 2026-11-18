use crate::evidence::ControlSequenceScanner;

fn scan(text: &str) -> String {
    let mut s = ControlSequenceScanner::new();
    let mut out = String::new();
    for ch in text.chars() {
        if let Some(v) = s.push(ch) {
            out.push(v);
        }
    }
    out
}

#[test]
fn scanner_output_is_identical_under_every_chunk_boundary() {
    let corpus = "plain ASCII 0123456789 ~!@#$%^&*()\n\tline2\r\n\u{1b}[31mred\u{1b}[0m \u{1b}]1337;COSH;{}\u{7}osc \u{9b}32mC1 \u{9d}str\u{7} \u{0}ctl \u{ad}soft \u{200b}zw \u{feff}bom 你好🦀\n end";
    let whole = scan(corpus);
    let chars: Vec<char> = corpus.chars().collect();
    for split in 0..=chars.len() {
        let a: String = chars[..split].iter().collect();
        let b: String = chars[split..].iter().collect();
        let mut s = ControlSequenceScanner::new();
        let mut out = String::new();
        for part in [&a, &b] {
            for ch in part.chars() {
                if let Some(v) = s.push(ch) {
                    out.push(v);
                }
            }
        }
        assert_eq!(out, whole, "split at {split}");
    }
}
