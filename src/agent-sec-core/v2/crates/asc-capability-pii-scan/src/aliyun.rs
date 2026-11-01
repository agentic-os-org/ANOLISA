//! Documented Alibaba Cloud environment-variable credentials.
//!
//! Provider attribution comes from the assignment name, never from the value.
//! Every listed name is a published environment variable of an official Alibaba
//! Cloud product or SDK; see `docs/design/PII_V2_MIGRATION.md` for the sources.

use crate::models::Severity;

/// Credential role of one documented environment variable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Role {
    /// Long-lived `AccessKey` identifier; an identifier is not a secret.
    AccessKeyId,
    /// Long-lived `AccessKey` secret.
    AccessKeySecret,
    /// STS session token.
    SecurityToken,
    /// Bearer token accepted by the CLI.
    BearerToken,
    /// Model Studio API key.
    ModelStudioApiKey,
}

pub(crate) struct Alias {
    pub(crate) name: &'static str,
    pub(crate) role: Role,
}

impl Role {
    /// Public detection type retained for this role.
    pub(crate) const fn kind(self) -> &'static str {
        match self {
            Self::AccessKeyId => "aliyun_access_key_id",
            // STS tokens keep the generic type so existing consumers that
            // branch on `generic_secret_field` continue to see a denial.
            Self::AccessKeySecret => "aliyun_access_key_secret",
            Self::SecurityToken => "generic_secret_field",
            Self::BearerToken => "bearer_token",
            Self::ModelStudioApiKey => "api_key",
        }
    }

    /// An identifier is not usable on its own, so it only warns.
    pub(crate) const fn severity(self) -> Severity {
        match self {
            Self::AccessKeyId => Severity::Warn,
            _ => Severity::Deny,
        }
    }

    /// Stable `metadata.credential_kind` value.
    pub(crate) const fn credential_kind(self) -> &'static str {
        match self {
            Self::AccessKeyId => "access_key_id",
            Self::AccessKeySecret => "access_key_secret",
            Self::SecurityToken => "security_token",
            Self::BearerToken => "bearer_token",
            Self::ModelStudioApiKey => "model_studio_api_key",
        }
    }

    /// Full masks for secrets; identifiers retain the normal partial mask.
    pub(crate) const fn mask(self) -> Option<&'static str> {
        match self {
            Self::AccessKeyId => None,
            Self::AccessKeySecret => Some("[REDACTED_SECRET]"),
            Self::SecurityToken | Self::BearerToken => Some("[REDACTED_TOKEN]"),
            Self::ModelStudioApiKey => Some("[REDACTED_API_KEY]"),
        }
    }
}

/// Longest access-key ID that still takes a whole-value mask.
///
/// The identifier mask keeps the first and last four characters, so for a value
/// this short it conceals nothing, and an enclosing generic finding would
/// republish the whole ID through its own tail-four suffix.
pub(crate) const SHORT_ID_MAX_LEN: usize = 8;

const fn alias(name: &'static str, role: Role) -> Alias {
    Alias { name, role }
}

/// The 21 names approved for the first slice, grouped by documented role.
///
/// `ACCESS_KEY_ID`, `ACCESS_KEY_SECRET` and `SECURITY_TOKEN` are deliberately
/// absent: the CLI accepts them as fallbacks, but such a generic spelling does
/// not establish Alibaba Cloud ownership. Role ARN, OIDC token *paths*, profile,
/// region, endpoint and header-name selectors are configuration, not secrets.
pub(crate) const ALIASES: &[Alias] = &[
    alias("ALIBABA_CLOUD_ACCESS_KEY_ID", Role::AccessKeyId),
    alias("ALIBABACLOUD_ACCESS_KEY_ID", Role::AccessKeyId),
    alias("ALICLOUD_ACCESS_KEY_ID", Role::AccessKeyId),
    alias("ALICLOUD_ACCESS_KEY", Role::AccessKeyId),
    alias("OSS_ACCESS_KEY_ID", Role::AccessKeyId),
    alias("ALIYUN_ACCESS_KEY_ID", Role::AccessKeyId),
    alias("ALIBABA_CLOUD_ACCESS_KEY_SECRET", Role::AccessKeySecret),
    alias("ALIBABACLOUD_ACCESS_KEY_SECRET", Role::AccessKeySecret),
    alias("ALICLOUD_ACCESS_KEY_SECRET", Role::AccessKeySecret),
    alias("ALICLOUD_SECRET_KEY", Role::AccessKeySecret),
    alias("OSS_ACCESS_KEY_SECRET", Role::AccessKeySecret),
    alias("ALIYUN_ACCESS_KEY_SECRET", Role::AccessKeySecret),
    alias("ALIBABA_CLOUD_SECURITY_TOKEN", Role::SecurityToken),
    alias("ALIBABACLOUD_SECURITY_TOKEN", Role::SecurityToken),
    alias("ALICLOUD_SECURITY_TOKEN", Role::SecurityToken),
    alias("ALICLOUD_ACCESS_KEY_STS_TOKEN", Role::SecurityToken),
    alias("OSS_SESSION_TOKEN", Role::SecurityToken),
    alias("OSS_SECURITY_TOKEN", Role::SecurityToken),
    alias("ALIYUN_SECURITY_TOKEN", Role::SecurityToken),
    alias("ALIBABA_CLOUD_BEARER_TOKEN", Role::BearerToken),
    alias("DASHSCOPE_API_KEY", Role::ModelStudioApiKey),
];

/// Exact, case-sensitive lookup.
pub(crate) fn role(name: &str) -> Option<Role> {
    ALIASES
        .iter()
        .find(|alias| alias.name == name)
        .map(|alias| alias.role)
}

/// `true` for an exact provider alias.
pub(crate) fn is_official(name: &str) -> bool {
    role(name).is_some()
}

/// Builds `NAME=value`, `export NAME=value` and paired-quoted right-hand sides.
///
/// Names are ordered longest first so an alternation match never ends on a
/// shorter prefix that leaves a residual `_ID`/`_SECRET` suffix unmatched.
pub(crate) fn pattern() -> String {
    let mut names: Vec<&'static str> = ALIASES.iter().map(|alias| alias.name).collect();
    names.sort_by_key(|name| std::cmp::Reverse(name.len()));
    // The value is scanned separately to avoid VM backtracking on long words.
    format!(
        r"(?<![A-Za-z0-9_])(?:export[ \t]+)?(?P<alias>{})[ \t]*=[ \t]*",
        names.join("|"),
    )
}

/// Values that document a variable rather than carry a credential.
///
/// Only complete placeholders and pure shell references are rejected. A real
/// value that merely contains `test` or `example` is still reported, matching
/// the retained V1 negative-context contract.
pub(crate) fn is_placeholder(value: &str, single_quoted: bool) -> bool {
    if matches!(value, "\"\"" | "''") {
        return true;
    }
    // Shell expansion does not occur inside single quotes. Keep the separate
    // percent-delimited placeholder convention regardless of shell quoting.
    if is_reference(value) && (!single_quoted || !value.starts_with('$')) {
        return true;
    }
    // A run of mask characters carries no information.
    if value
        .chars()
        .all(|c| matches!(c, '*' | 'x' | 'X' | '-' | '.' | '_'))
    {
        return true;
    }
    // Exactly one bracketed placeholder. A value that merely starts and ends
    // with brackets — `<placeholder>literalSecret3478<placeholder>` — is a
    // literal, and treating it as documentation leaves the text unredacted.
    let bracketed = value.strip_prefix('<').and_then(|v| v.strip_suffix('>'));
    if bracketed.is_some_and(|inner| !inner.is_empty() && !inner.contains(['<', '>'])) {
        return true;
    }
    let lower = value.to_ascii_lowercase().replace('-', "_");
    matches!(
        lower.as_str(),
        "null"
            | "none"
            | "nil"
            | "todo"
            | "changeme"
            | "placeholder"
            | "redacted"
            | "secret"
            | "password"
            | "your_access_key"
            | "your_access_key_id"
            | "your_access_key_secret"
            | "your_secret_key"
            | "your_security_token"
            | "your_api_key"
            | "your_api_key_here"
    )
}

/// Complete variable references and direct reads of absolute paths do not
/// carry a literal credential in the scanned text.
fn is_reference(value: &str) -> bool {
    fn variable(name: &str) -> bool {
        let mut bytes = name.bytes();
        bytes
            .next()
            .is_some_and(|first| first.is_ascii_alphabetic() || first == b'_')
            && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    }

    if let Some(name) = value.strip_prefix("${").and_then(|s| s.strip_suffix('}')) {
        return variable(name);
    }
    if let Some(command) = value.strip_prefix("$(").and_then(|s| s.strip_suffix(')')) {
        // Only a direct read of an absolute path carries no literal value.
        // Other commands can embed credentials in their arguments.
        let mut parts = command.split_ascii_whitespace();
        return parts.next() == Some("cat")
            && parts.next().is_some_and(|path| {
                path.starts_with('/')
                    && path.len() > 1
                    && path.bytes().all(|b| {
                        b.is_ascii_alphanumeric() || matches!(b, b'/' | b'_' | b'-' | b'.')
                    })
            })
            && parts.next().is_none();
    }
    if let Some(name) = value.strip_prefix('$') {
        return variable(name);
    }
    value
        .strip_prefix('%')
        .and_then(|s| s.strip_suffix('%'))
        .is_some_and(variable)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_alias_has_one_role_and_a_distinct_name() {
        assert_eq!(ALIASES.len(), 21);
        for (index, alias) in ALIASES.iter().enumerate() {
            assert!(
                ALIASES[index + 1..]
                    .iter()
                    .all(|other| other.name != alias.name),
                "duplicate {}",
                alias.name
            );
            assert_eq!(role(alias.name), Some(alias.role));
        }
    }

    #[test]
    fn generic_cli_fallbacks_are_not_attributed() {
        for name in ["ACCESS_KEY_ID", "ACCESS_KEY_SECRET", "SECURITY_TOKEN"] {
            assert!(!is_official(name), "{name}");
        }
        // The bare provider name is unresolved: the Terraform overview mentions
        // it without a role while the current provider does not read it.
        assert!(!is_official("ALIBABA_CLOUD_ACCESS_KEY"));
        assert!(!is_official("alibaba_cloud_access_key_secret"));
        assert!(!is_official("MY_ALIBABA_CLOUD_ACCESS_KEY_SECRET"));
    }

    #[test]
    fn placeholders_and_references_are_rejected_but_real_values_survive() {
        for value in [
            "$MY_KEY",
            "${MY_KEY}",
            "%MY_KEY%",
            "$(cat /run/secret)",
            "xxxxxx",
            "********",
            "<your-access-key-id>",
            "your_access_key_secret",
            "your-api-key-here",
            "changeme",
            "PLACEHOLDER",
        ] {
            assert!(is_placeholder(value, false), "{value}");
        }
        for value in [
            "abcdefghijklmnop1234",
            "LTAI5tQnKxExampleToken12",
            "testValue123456",
            "sampleValue123456",
            "example",
            "${MY_KEY}suffix",
            "$MY_KEY-suffix",
            "$(cat /run/secret)suffix",
            "%MY_KEY%suffix",
            "$1234",
            "$(printf secret123)",
            "$(cat /run/secret extra123)",
            "your_access_key_ABC123456",
            "your-secret-ABC123456",
        ] {
            assert!(!is_placeholder(value, false), "{value}");
        }
    }

    #[test]
    fn roles_map_to_the_approved_type_severity_and_mask() {
        for (role, kind, severity, credential_kind, mask) in [
            (
                Role::AccessKeyId,
                "aliyun_access_key_id",
                Severity::Warn,
                "access_key_id",
                None,
            ),
            (
                Role::AccessKeySecret,
                "aliyun_access_key_secret",
                Severity::Deny,
                "access_key_secret",
                Some("[REDACTED_SECRET]"),
            ),
            (
                Role::SecurityToken,
                "generic_secret_field",
                Severity::Deny,
                "security_token",
                Some("[REDACTED_TOKEN]"),
            ),
            (
                Role::BearerToken,
                "bearer_token",
                Severity::Deny,
                "bearer_token",
                Some("[REDACTED_TOKEN]"),
            ),
            (
                Role::ModelStudioApiKey,
                "api_key",
                Severity::Deny,
                "model_studio_api_key",
                Some("[REDACTED_API_KEY]"),
            ),
        ] {
            assert_eq!(role.kind(), kind);
            assert_eq!(role.severity(), severity);
            assert_eq!(role.credential_kind(), credential_kind);
            assert_eq!(role.mask(), mask);
        }
    }
}
