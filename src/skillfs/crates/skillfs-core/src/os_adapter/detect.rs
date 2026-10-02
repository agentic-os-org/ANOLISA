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
    let mut id: Option<String> = None;
    for line in content.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("ID=") {
            id = Some(unquote(rest).to_ascii_lowercase());
        }
    }

    match id.as_deref() {
        Some("ubuntu") | Some("debian") => Some(OsTarget::Ubuntu),
        Some("alinux") | Some("anolis") => Some(OsTarget::Alinux),
        _ => None,
    }
}

/// Strip matched surrounding single or double quotes from an os-release value.
fn unquote(s: &str) -> &str {
    let s = s.trim();
    if s.len() >= 2
        && ((s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')))
    {
        &s[1..s.len() - 1]
    } else {
        s
    }
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
}
