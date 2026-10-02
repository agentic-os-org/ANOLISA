//! Zsh path-prompt candidate prefixes shared by parsing and relay ownership.

pub(super) fn zsh_path_candidate_should_hold(bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return false;
    }
    // A raw Tab is a ZLE editing action, so only spaces can be held while
    // waiting to learn whether the first textual token is a path prompt.
    let candidate = bytes.iter().copied().find(|byte| *byte != b' ');
    candidate.is_none_or(|byte| byte == b'/' || !byte.is_ascii())
}

#[cfg(test)]
mod tests {
    use super::zsh_path_candidate_should_hold;

    #[test]
    fn holds_space_prefixed_paths_until_the_prefix_resolves() {
        assert!(zsh_path_candidate_should_hold(b" "));
        assert!(zsh_path_candidate_should_hold(b"  /missing"));
        assert!(zsh_path_candidate_should_hold("  你".as_bytes()));
        assert!(!zsh_path_candidate_should_hold(b"  echo"));
        assert!(!zsh_path_candidate_should_hold(b"\t/missing"));
    }
}
