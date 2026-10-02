#[test]
fn empty_string() {
    assert_eq!(estimate_tokens(""), 0);
    assert_eq!(estimate_tokens_from_bytes(0), 0);
    assert_eq!(count_chars(""), 0);
}

#[test]
fn ascii_text() {
    // 11 chars / 4 = 3 tokens
    assert_eq!(estimate_tokens("hello world"), 3);
    assert_eq!(count_chars("hello world"), 11);
}

#[test]
fn cjk_text() {
    // 4 CJK chars × 1 token/char = 4 tokens
    assert_eq!(estimate_tokens("你好世界"), 4);
    assert_eq!(count_chars("你好世界"), 4);
}

#[test]
fn emoji() {
    // 2 emoji chars / 4 = 1 token
    assert_eq!(estimate_tokens("🎉🎊"), 1);
    assert_eq!(count_chars("🎉🎊"), 2);
}

#[test]
fn mixed_text() {
    // 5 ASCII chars / 4 = 2 tokens + 2 CJK chars = 2 tokens → 4 total
    let text = "Hello你好";
    assert_eq!(count_chars(text), 7);
    assert_eq!(estimate_tokens(text), 4);
}

#[test]
fn byte_estimate_vs_char_estimate() {
    // For ASCII, byte and char estimates should match
    let text = "abcdef";
    assert_eq!(
        estimate_tokens(text),
        estimate_tokens_from_bytes(text.len())
    );
}

#[test]
fn byte_estimate_undercounts_cjk() {
    // 4 CJK chars are 12 UTF-8 bytes: bytes/4 → 3, character estimator → 4.
    let text = "你好世界";
    assert_eq!(estimate_tokens(text), 4);
    assert_eq!(estimate_tokens_from_bytes(text.len()), 3);
}

#[test]
fn tokenizer_struct_methods() {
    let tokenizer = Tokenizer::new();
    assert_eq!(tokenizer.estimate_tokens("hello world"), 3);
    assert_eq!(tokenizer.count_chars("hello"), 5);
}

#[test]
fn tokenizer_default() {
    let tokenizer = Tokenizer;
    assert_eq!(tokenizer.estimate_tokens(""), 0);
    assert_eq!(tokenizer.count_chars(""), 0);
}

#[test]
fn estimate_tokens_from_bytes_nonzero() {
    assert_eq!(estimate_tokens_from_bytes(1), 1);
    assert_eq!(estimate_tokens_from_bytes(4), 1);
    assert_eq!(estimate_tokens_from_bytes(5), 2);
    assert_eq!(estimate_tokens_from_bytes(100), 25);
}

#[test]
fn count_chars_multibyte() {
    assert_eq!(count_chars("🎉"), 1);
    assert_eq!(count_chars("你好"), 2);
}
