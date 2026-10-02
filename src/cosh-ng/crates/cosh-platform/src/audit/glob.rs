//! Tiny glob matcher: `*` (any sequence including empty) and `?`
//! (exactly one byte). No regex, no character classes, no escaping —
//! see `docs/audit-design.md` §3.3 ("not regex, avoid ReDoS").
//!
//! Iterative DP. O(n · m) time, no recursion → safe against adversarial
//! patterns like `********...`.

/// Match `s` against `pat`. Returns true on a full match.
pub fn glob_match(pat: &str, s: &str) -> bool {
    glob_match_bytes(pat.as_bytes(), s.as_bytes())
}

fn glob_match_bytes(pat: &[u8], s: &[u8]) -> bool {
    let n = pat.len();
    let m = s.len();
    // dp[i][j] = pat[..i] matches s[..j]
    let mut dp = vec![vec![false; m + 1]; n + 1];
    dp[0][0] = true;
    // Pattern of leading `*`s can match an empty string.
    for i in 1..=n {
        if pat[i - 1] == b'*' {
            dp[i][0] = dp[i - 1][0];
        }
    }
    for i in 1..=n {
        for j in 1..=m {
            let pc = pat[i - 1];
            if pc == b'*' {
                // `*` either consumes nothing (dp[i-1][j]) or one more byte (dp[i][j-1]).
                dp[i][j] = dp[i - 1][j] || dp[i][j - 1];
            } else if pc == b'?' || pc == s[j - 1] {
                dp[i][j] = dp[i - 1][j - 1];
            }
        }
    }
    dp[n][m]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_pattern_matches_empty() {
        assert!(glob_match("", ""));
        assert!(!glob_match("", "x"));
    }

    #[test]
    fn star_matches_any_sequence() {
        assert!(glob_match("*", ""));
        assert!(glob_match("*", "abcdef"));
        assert!(glob_match("ng*", "ng"));
        assert!(glob_match("ng*", "nginx"));
        assert!(glob_match("*ng", "ng"));
        assert!(glob_match("*ng", "fooooong"));
        assert!(glob_match("a*b*c", "abc"));
        assert!(glob_match("a*b*c", "axxxbxxxc"));
        assert!(!glob_match("a*b*c", "axxxbxxx"));
    }

    #[test]
    fn question_matches_exactly_one_byte() {
        assert!(glob_match("?", "a"));
        assert!(!glob_match("?", ""));
        assert!(!glob_match("?", "ab"));
        assert!(glob_match("a?c", "abc"));
        assert!(!glob_match("a?c", "ac"));
    }

    #[test]
    fn star_question_combinations_for_sed_inplace() {
        // -i variants: -i, -i.bak, -i=foo, -ipattern
        assert!(glob_match("-i*", "-i"));
        assert!(glob_match("-i*", "-i.bak"));
        assert!(glob_match("-i*", "-i=foo"));
        assert!(glob_match("-i?*", "-i.bak"));
        assert!(!glob_match("-i?*", "-i"));
    }

    #[test]
    fn does_not_recurse_on_adversarial_pattern() {
        // Many stars must not blow the stack — DP handles it linearly.
        let pat: String = "*".repeat(50) + "abc";
        assert!(glob_match(&pat, "xxxxxxxxxxabc"));
        assert!(!glob_match(&pat, "xxxxxxxxxxabd"));
    }
}
