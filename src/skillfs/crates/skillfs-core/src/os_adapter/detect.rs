//! Target-OS selection and `/etc/os-release` detection.
//!
//! Detection runs at most once, at mount startup (for `target_os = "auto"`).
//! The per-read hot path never touches these functions.

use super::error::OsAdapterError;

/// The OS whose conventions the served `SKILL.md` should match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OsTarget {
    /// Ubuntu/Debian (apt/dpkg) conventions.
    Ubuntu,
    /// Alinux/Anolis (dnf/rpm) conventions.
    Alinux,
}

impl OsTarget {
    /// Content-free label for diagnostics.
    pub fn as_str(self) -> &'static str {
        match self {
            OsTarget::Ubuntu => "ubuntu",
            OsTarget::Alinux => "alinux",
        }
    }
}

/// How the target OS is chosen for the adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetSelector {
    /// Detect from `/etc/os-release` at mount startup.
    Auto,
    /// Force the Ubuntu/Debian target.
    Ubuntu,
    /// Force the Alinux/Anolis target.
    Alinux,
}

impl TargetSelector {
    /// Parse a `target_os` config value. Returns `None` for unknown values so
    /// the caller can surface an actionable configuration error.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "auto" => Some(TargetSelector::Auto),
            "ubuntu" => Some(TargetSelector::Ubuntu),
            "alinux" => Some(TargetSelector::Alinux),
            _ => None,
        }
    }
}

/// Resolve a [`TargetSelector`] into a concrete [`OsTarget`], reading
/// `/etc/os-release` only for [`TargetSelector::Auto`].
pub(crate) fn resolve_target(selector: TargetSelector) -> Result<OsTarget, OsAdapterError> {
    match selector {
        TargetSelector::Ubuntu => Ok(OsTarget::Ubuntu),
        TargetSelector::Alinux => Ok(OsTarget::Alinux),
        TargetSelector::Auto => {
            let content = std::fs::read_to_string("/etc/os-release")
                .map_err(|source| OsAdapterError::ReadOsRelease { source })?;
            parse_os_release(&content).ok_or(OsAdapterError::UnknownOs)
        }
    }
}

/// Map `/etc/os-release` content to an [`OsTarget`].
///
/// Matches the exact `ID` only — `ubuntu`/`debian` map to Ubuntu and
/// `alinux`/`anolis` map to Alinux. Every other distribution returns `None`.
///
/// Detection is deliberately **fail-closed**: `ID_LIKE` is not consulted, so a
/// generic `ID_LIKE=rhel fedora centos` (Rocky, AlmaLinux, RHEL, …) is *not*
/// silently treated as Alinux, and a generic apt derivative is *not* treated as
/// Ubuntu. Operators on unrecognized distributions must set `target_os`
/// explicitly rather than rely on a fuzzy guess.
pub fn parse_os_release(content: &str) -> Option<OsTarget> {
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    let mut id: Option<String> = None;
    for line in content.lines() {
        let line = line.trim_matches(is_ascii_blank);
        if let Some(rest) = line.strip_prefix("ID=") {
            id = parse_os_release_value(rest).map(|val| val.to_ascii_lowercase());
        }
    }

    match id.as_deref() {
        Some("ubuntu") | Some("debian") => Some(OsTarget::Ubuntu),
        Some("alinux") | Some("anolis") => Some(OsTarget::Alinux),
        _ => None,
    }
}

/// Only ASCII space and horizontal tab act as shell blanks in `os-release(5)`.
fn is_ascii_blank(ch: char) -> bool {
    matches!(ch, ' ' | '\t')
}

/// Parse one `os-release(5)` assignment value, honoring single or double
/// quotes, shell-style backslash escapes, and trailing `#` comments while
/// rejecting unclosed quotes or trailing non-comment tokens fail-closed.
fn parse_os_release_value(raw: &str) -> Option<String> {
    let s = raw.trim_end_matches(is_ascii_blank);
    if s.is_empty() {
        return Some(String::new());
    }
    if s.starts_with(is_ascii_blank) {
        return allows_trailing_comment(s).then(String::new);
    }
    if let Some(rest) = s.strip_prefix('\'') {
        let end = rest.find('\'')?;
        let value = &rest[..end];
        let tail = &rest[end + 1..];
        return allows_trailing_comment(tail).then(|| value.to_string());
    }
    if let Some(rest) = s.strip_prefix('"') {
        let mut out = String::with_capacity(rest.len());
        let mut chars = rest.char_indices();
        while let Some((idx, ch)) = chars.next() {
            match ch {
                '"' => {
                    let tail = &rest[idx + 1..];
                    return allows_trailing_comment(tail).then_some(out);
                }
                '\\' => {
                    let (_, next) = chars.next()?;
                    if !matches!(next, '$' | '"' | '\\' | '`') {
                        out.push('\\');
                    }
                    out.push(next);
                }
                _ => out.push(ch),
            }
        }
        return None;
    }

    let mut out = String::with_capacity(s.len());
    let mut chars = s.char_indices();
    while let Some((idx, ch)) = chars.next() {
        match ch {
            '\\' => {
                let (_, next) = chars.next()?;
                out.push(next);
            }
            '"' | '\'' => return None,
            c if is_ascii_blank(c) => {
                let tail = &s[idx..];
                return allows_trailing_comment(tail).then_some(out);
            }
            _ => out.push(ch),
        }
    }
    Some(out)
}

/// After a complete quoted or unquoted value, only ASCII blanks or a
/// blank-separated `#` comment may follow on the same line.
fn allows_trailing_comment(tail: &str) -> bool {
    let trimmed = tail.trim_start_matches(is_ascii_blank);
    trimmed.is_empty() || (tail.len() > trimmed.len() && trimmed.starts_with('#'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_release_maps_supported_ids() {
        assert_eq!(
            parse_os_release("ID=ubuntu\nID_LIKE=debian\n"),
            Some(OsTarget::Ubuntu)
        );
        assert_eq!(parse_os_release("ID=\"alinux\"\n"), Some(OsTarget::Alinux));
        assert_eq!(parse_os_release("ID=anolis\n"), Some(OsTarget::Alinux));
        assert_eq!(parse_os_release("ID=debian\n"), Some(OsTarget::Ubuntu));
    }

    #[test]
    fn os_release_is_fail_closed_for_rhel_family() {
        // ID_LIKE must NOT silently classify these as Alinux.
        assert_eq!(
            parse_os_release("ID=rocky\nID_LIKE=\"rhel centos fedora\"\n"),
            None
        );
        assert_eq!(parse_os_release("ID=rhel\nID_LIKE=fedora\n"), None);
        assert_eq!(parse_os_release("ID=fedora\n"), None);
        assert_eq!(parse_os_release("ID=centos\nID_LIKE=rhel\n"), None);
        assert_eq!(
            parse_os_release("ID=almalinux\nID_LIKE=\"rhel centos fedora\"\n"),
            None
        );
    }

    #[test]
    fn os_release_does_not_guess_from_id_like_ubuntu() {
        // Apt derivatives are not silently treated as Ubuntu either.
        assert_eq!(
            parse_os_release("ID=linuxmint\nID_LIKE=\"ubuntu debian\"\n"),
            None
        );
    }

    #[test]
    fn os_release_unknown_returns_none() {
        assert_eq!(parse_os_release("ID=arch\n"), None);
        assert_eq!(parse_os_release(""), None);
    }

    #[test]
    fn target_selector_parses_known_values() {
        assert_eq!(TargetSelector::parse("auto"), Some(TargetSelector::Auto));
        assert_eq!(
            TargetSelector::parse("ubuntu"),
            Some(TargetSelector::Ubuntu)
        );
        assert_eq!(
            TargetSelector::parse("alinux"),
            Some(TargetSelector::Alinux)
        );
        assert_eq!(TargetSelector::parse("fedora"), None);
    }

    #[test]
    fn explicit_selector_does_not_read_os_release() {
        assert_eq!(
            resolve_target(TargetSelector::Ubuntu).unwrap(),
            OsTarget::Ubuntu
        );
        assert_eq!(
            resolve_target(TargetSelector::Alinux).unwrap(),
            OsTarget::Alinux
        );
    }

    #[test]
    fn os_release_strips_leading_utf8_bom_and_inline_comments() {
        assert_eq!(
            parse_os_release("\u{feff}ID=ubuntu\nID_LIKE=debian\n"),
            Some(OsTarget::Ubuntu)
        );
        assert_eq!(
            parse_os_release("ID=ubuntu # Ubuntu LTS\n"),
            Some(OsTarget::Ubuntu)
        );
        assert_eq!(
            parse_os_release("ID=\"alinux\" # Alibaba Cloud Linux\n"),
            Some(OsTarget::Alinux)
        );
        assert_eq!(
            parse_os_release("ID='debian' # Debian GNU/Linux\n"),
            Some(OsTarget::Ubuntu)
        );
        assert_eq!(
            parse_os_release("ID=\"ubuntu\" # \"quoted comment\"\n"),
            Some(OsTarget::Ubuntu)
        );
        assert_eq!(
            parse_os_release("ID='anolis' # 'quoted comment'\n"),
            Some(OsTarget::Alinux)
        );
    }

    #[test]
    fn os_release_unescapes_shell_escapes_and_rejects_malformed_values() {
        assert_eq!(parse_os_release("ID=u\\buntu\n"), Some(OsTarget::Ubuntu));
        assert_eq!(
            parse_os_release("ID=\"al\\inux\"\n"),
            None,
            "unknown backslash sequence inside double quotes stays literal"
        );
        // Unclosed quotes, escaped closing quotes, or trailing non-comment tokens fail closed.
        assert_eq!(parse_os_release("ID=\"ubuntu\n"), None);
        assert_eq!(parse_os_release("ID='alinux\n"), None);
        assert_eq!(parse_os_release("ID=\"ubuntu\\\"\n"), None);
        assert_eq!(parse_os_release("ID=\"ubuntu\"debian\n"), None);
        assert_eq!(parse_os_release("ID=\"ubuntu\"#nospace\n"), None);
        assert_eq!(parse_os_release("ID=ubuntu debian\n"), None);
        assert_eq!(parse_os_release("ID=ub\"untu\n"), None);
        // A later malformed ID= assignment overrides an earlier valid one fail-closed.
        assert_eq!(parse_os_release("ID=ubuntu\nID=\"alinux\n"), None);
    }

    #[test]
    fn os_release_rejects_unicode_whitespace_and_lone_cr() {
        // Positive controls: ASCII space/tab blanks and CRLF line endings remain accepted.
        assert_eq!(
            parse_os_release("ID=ubuntu\t#tab\n"),
            Some(OsTarget::Ubuntu)
        );
        assert_eq!(
            parse_os_release(" \tID=\"ubuntu\"\t#tab\r\n"),
            Some(OsTarget::Ubuntu)
        );
        assert_eq!(parse_os_release("ID=ubuntu\r\n"), Some(OsTarget::Ubuntu));

        // Non-ASCII Unicode whitespace (e.g. U+00A0 NBSP) and lone CR are not shell blanks.
        assert_eq!(parse_os_release("ID=ubuntu\u{00a0}#comment\n"), None);
        assert_eq!(parse_os_release("ID=\"ubuntu\"\u{00a0}#comment\n"), None);
        assert_eq!(parse_os_release("ID='ubuntu'\u{00a0}#comment\n"), None);
        assert_eq!(parse_os_release("ID=ubuntu\u{00a0}\n"), None);
        assert_eq!(parse_os_release("\u{00a0}ID=ubuntu\n"), None);
        assert_eq!(parse_os_release("ID=\u{00a0}ubuntu\n"), None);
        assert_eq!(parse_os_release("ID=ubuntu\r"), None);
    }

    #[test]
    fn os_release_enforces_bash_assignment_blank_semantics() {
        // An unescaped ASCII blank directly after '=' cannot precede a value.
        assert_eq!(parse_os_release("ID= ubuntu\n"), None);
        assert_eq!(parse_os_release("ID=\tubuntu\n"), None);
        assert_eq!(parse_os_release("ID= \"ubuntu\"\n"), None);
        assert_eq!(parse_os_release("ID=ubuntu\nID= ubuntu\n"), None);

        // Blank + '#' after '=' is an empty assignment with a trailing comment,
        // whereas '#' immediately after '=' is literal unquoted text.
        assert_eq!(parse_os_release_value(" #comment"), Some(String::new()));
        assert_eq!(
            parse_os_release_value("#comment"),
            Some("#comment".to_string())
        );
        assert_eq!(parse_os_release("ID= #comment\n"), None);
        assert_eq!(parse_os_release("ID=#comment\n"), None);
        assert_eq!(parse_os_release("ID=ubuntu\nID= #comment\n"), None);
    }
}
