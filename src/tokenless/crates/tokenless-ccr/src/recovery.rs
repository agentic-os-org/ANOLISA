//! Recovery instructions and their model-visible references, including historical markers.

use tokenless_protocol::RecoveryMethod;

use crate::marker::{HASH_LEN, MARKER_PREFIX, MARKER_SUFFIX, is_valid_hash};

const SHELL_PREFIX: &str = "If needed, run in shell: tokenless retrieve ";

/// Formats the declared recovery action. `None` emits no instruction.
#[must_use]
pub fn recovery_instruction(hash: &str, method: &RecoveryMethod) -> String {
    match method {
        RecoveryMethod::None => String::new(),
        RecoveryMethod::Shell => format!("{SHELL_PREFIX}{hash}"),
        RecoveryMethod::Tool { name } => {
            format!("{}{}", tool_prefix(name.as_str()), hash)
        }
    }
}

/// Suffix whose actual length must be reserved before string or schema truncation.
#[must_use]
pub fn truncation_suffix_for(hash: &str, method: &RecoveryMethod) -> String {
    format!("… Truncated. {}", recovery_instruction(hash, method))
}

/// Finds complete recovery references, never isolated hashes.
///
/// Shell instructions and historical markers are recognized in any context;
/// tool instructions must name the currently declared static recovery tool.
/// Returned hashes preserve case; authorization callers normalize and deduplicate.
#[must_use]
pub fn recovery_hashes<'a>(text: &'a str, method: &RecoveryMethod) -> Vec<&'a str> {
    let mut references = Vec::new();
    for (start, _) in text.match_indices(MARKER_PREFIX) {
        let tail = &text[start + MARKER_PREFIX.len()..];
        if let Some(hash) = tail.get(..HASH_LEN)
            && is_valid_hash(hash)
            && tail[HASH_LEN..].starts_with(MARKER_SUFFIX)
        {
            references.push((start, hash));
        }
    }
    collect_instruction(text, SHELL_PREFIX, &mut references);
    if let RecoveryMethod::Tool { name } = method {
        collect_instruction(text, &tool_prefix(name.as_str()), &mut references);
    }
    references.sort_unstable_by_key(|(start, _)| *start);
    references.into_iter().map(|(_, hash)| hash).collect()
}

fn tool_prefix(name: &str) -> String {
    format!("If needed, call tool {name} with hash_or_marker=")
}

fn collect_instruction<'a>(text: &'a str, prefix: &str, found: &mut Vec<(usize, &'a str)>) {
    for (start, _) in text.match_indices(prefix) {
        if text[..start]
            .chars()
            .next_back()
            .is_some_and(is_identifier_char)
        {
            continue;
        }
        let tail = &text[start + prefix.len()..];
        if let Some(hash) = tail.get(..HASH_LEN)
            && is_valid_hash(hash)
            && !tail[HASH_LEN..]
                .chars()
                .next()
                .is_some_and(is_identifier_char)
        {
            found.push((start, hash));
        }
    }
}

fn is_identifier_char(c: char) -> bool {
    // Fail closed when a hash or instruction is embedded in a word, including CJK text.
    c.is_alphanumeric() || matches!(c, '_' | '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH: &str = "0123456789abcdef01234567";

    #[test]
    fn shell_and_tool_round_trip_without_legacy_markers() {
        for method in [
            RecoveryMethod::Shell,
            RecoveryMethod::tool("tenant_retrieve").unwrap(),
        ] {
            let instruction = recovery_instruction(HASH, &method);
            assert!(!instruction.contains("<<"));
            assert_eq!(recovery_hashes(&instruction, &method), [HASH]);
            assert!(instruction.contains("If needed"));
        }
        assert!(recovery_instruction(HASH, &RecoveryMethod::None).is_empty());
    }

    #[test]
    fn only_current_tool_name_is_authorizable() {
        let method = RecoveryMethod::tool("tenant_retrieve").unwrap();
        let instruction = recovery_instruction(HASH, &method);
        assert!(recovery_hashes(&instruction, &RecoveryMethod::Shell).is_empty());
        assert!(recovery_hashes(&instruction, &RecoveryMethod::tool("other").unwrap()).is_empty());
        assert_eq!(recovery_hashes(&instruction, &method), [HASH]);
    }

    #[test]
    fn hash_and_instruction_boundaries_are_required() {
        for text in [
            HASH.to_owned(),
            format!("tokenless retrieve {HASH}"),
            format!("x{SHELL_PREFIX}{HASH}"),
            format!("{SHELL_PREFIX}{HASH}f"),
            format!("{SHELL_PREFIX}{HASH}_suffix"),
            format!("{SHELL_PREFIX}{HASH}中"),
            format!("{SHELL_PREFIX}{}", &HASH[..23]),
            format!("{SHELL_PREFIX}{}z", &HASH[..23]),
        ] {
            assert!(
                recovery_hashes(&text, &RecoveryMethod::Shell).is_empty(),
                "{text}"
            );
        }
    }

    #[test]
    fn legacy_and_new_references_retain_text_order_and_case() {
        let upper = HASH.to_ascii_uppercase();
        let text = format!("{SHELL_PREFIX}{upper}\n<<tokenless:{HASH}>>\n{SHELL_PREFIX}{HASH}");
        assert_eq!(
            recovery_hashes(&text, &RecoveryMethod::None),
            [upper.as_str(), HASH, HASH]
        );
    }

    #[test]
    fn malformed_prefixes_do_not_hide_later_references() {
        let text = format!("{}{SHELL_PREFIX}{HASH}", "<<tokenless:bad>> ".repeat(10000));
        assert_eq!(recovery_hashes(&text, &RecoveryMethod::Shell), [HASH]);
    }
}
