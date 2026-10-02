//! Input validation for CLI arguments.

use cosh_types::error::{CoshError, ErrorCode};

/// Validate a package name. Allows `[a-zA-Z0-9._+:-]`.
pub fn validate_pkg_name(name: &str) -> Result<(), CoshError> {
    if name.is_empty() {
        return Err(CoshError::new(
            ErrorCode::InvalidInput,
            "Package name cannot be empty",
            "pkg",
        ));
    }
    if name.len() > 256 {
        return Err(CoshError::new(
            ErrorCode::InvalidInput,
            "Package name too long (max 256 characters)",
            "pkg",
        ));
    }
    if let Some(c) = name.chars().find(|c| !is_valid_pkg_char(*c)) {
        return Err(CoshError::new(
            ErrorCode::InvalidInput,
            format!("Invalid character '{}' in package name '{}'", c, name),
            "pkg",
        )
        .with_hint("Package names may only contain: a-z A-Z 0-9 . _ + - :"));
    }
    Ok(())
}

/// Validate a package search query before forwarding it as a backend argument.
///
/// Accepts a portable subset of package-search syntax: package-name characters
/// plus `*`, `?`, `[`, and `]`. This CLI safety boundary does not expose each
/// backend's full syntax; apt-cache anchors such as `^...$` and Homebrew
/// `/regex/` queries are intentionally rejected.
pub fn validate_pkg_search_query(query: &str) -> Result<(), CoshError> {
    if query.is_empty() {
        return Err(CoshError::new(
            ErrorCode::InvalidInput,
            "Search query cannot be empty",
            "pkg",
        ));
    }
    if query.len() > 256 {
        return Err(CoshError::new(
            ErrorCode::InvalidInput,
            "Search query too long (max 256 characters)",
            "pkg",
        ));
    }
    if query.starts_with('-') {
        return Err(CoshError::new(
            ErrorCode::InvalidInput,
            "Search query cannot start with '-'",
            "pkg",
        )
        .with_hint("Start the search query with a package-name character or a pattern"));
    }
    if let Some(c) = query.chars().find(|c| !is_valid_pkg_search_char(*c)) {
        return Err(CoshError::new(
            ErrorCode::InvalidInput,
            format!("Invalid character {c:?} in search query"),
            "pkg",
        )
        .with_hint(
            "Search queries use the portable pattern subset: a-z A-Z 0-9 . _ + - : * ? [ ]",
        ));
    }
    Ok(())
}

/// Validate a service name. Allows `[a-zA-Z0-9._@:-]`.
pub fn validate_svc_name(name: &str) -> Result<(), CoshError> {
    if name.is_empty() {
        return Err(CoshError::new(
            ErrorCode::InvalidInput,
            "Service name cannot be empty",
            "svc",
        ));
    }
    if name.len() > 256 {
        return Err(CoshError::new(
            ErrorCode::InvalidInput,
            "Service name too long (max 256 characters)",
            "svc",
        ));
    }
    if let Some(c) = name.chars().find(|c| !is_valid_svc_char(*c)) {
        return Err(CoshError::new(
            ErrorCode::InvalidInput,
            format!("Invalid character '{}' in service name '{}'", c, name),
            "svc",
        )
        .with_hint("Service names may only contain: a-z A-Z 0-9 . _ @ - :"));
    }
    Ok(())
}

fn is_valid_pkg_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-' | ':')
}

fn is_valid_pkg_search_char(c: char) -> bool {
    is_valid_pkg_char(c) || matches!(c, '*' | '?' | '[' | ']')
}

fn is_valid_svc_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '@' | '-' | ':')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_pkg_names() {
        assert!(validate_pkg_name("nginx").is_ok());
        assert!(validate_pkg_name("nginx-1.24.0").is_ok());
        assert!(validate_pkg_name("lib_ssl3").is_ok());
        assert!(validate_pkg_name("gcc-c++").is_ok());
        assert!(validate_pkg_name("perl:5.38").is_ok());
    }

    #[test]
    fn test_invalid_pkg_names() {
        assert!(validate_pkg_name("").is_err());
        assert!(validate_pkg_name("nginx;rm -rf /").is_err());
        assert!(validate_pkg_name("pkg|cat /etc/passwd").is_err());
        assert!(validate_pkg_name("pkg&bg").is_err());
        assert!(validate_pkg_name("pkg$VAR").is_err());
        assert!(validate_pkg_name("pkg`cmd`").is_err());
        assert!(validate_pkg_name("pkg\nnewline").is_err());
        assert!(validate_pkg_name("pkg name").is_err());
    }

    #[test]
    fn test_pkg_name_too_long() {
        let long_name = "a".repeat(257);
        let err = validate_pkg_name(&long_name).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidInput);
    }

    #[test]
    fn test_valid_pkg_search_queries() {
        assert!(validate_pkg_search_query("nginx*").is_ok());
        assert!(validate_pkg_search_query("python3-?").is_ok());
        assert!(validate_pkg_search_query("lib[0-9]*").is_ok());
    }

    #[test]
    fn test_invalid_pkg_search_queries() {
        for query in [
            "",
            "pkg\nname",
            "pkg\rname",
            "pkg\tname",
            "pkg;cmd",
            "pkg|cmd",
            "pkg&cmd",
            "pkg$VAR",
            "pkg`cmd`",
            "pkg$(cmd)",
            "pkg>file",
            "pkg<input",
            "pkg(cmd)",
            "pkg{cmd}",
        ] {
            assert!(
                validate_pkg_search_query(query).is_err(),
                "query should be rejected: {query:?}"
            );
        }

        let err = validate_pkg_search_query("pkg|cmd").unwrap_err();
        assert!(err.message.contains("search query"));
        assert!(!err.message.contains("package name"));
        assert!(err
            .hint
            .as_deref()
            .is_some_and(|hint| hint.contains("Search queries")));
    }

    #[test]
    fn test_pkg_search_query_rejects_backend_specific_regex_syntax() {
        for query in ["^nginx$", "/nginx.*/"] {
            assert!(
                validate_pkg_search_query(query).is_err(),
                "backend-specific query should be rejected: {query:?}"
            );
        }
    }

    #[test]
    fn test_pkg_search_query_too_long() {
        let long_query = "a".repeat(257);
        let err = validate_pkg_search_query(&long_query).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidInput);
        assert!(err.message.contains("Search query"));
    }

    #[test]
    fn test_pkg_search_query_rejects_leading_option() {
        let err = validate_pkg_search_query("-nginx").unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidInput);
        assert!(err.message.contains("Search query"));
    }

    #[test]
    fn test_valid_svc_names() {
        assert!(validate_svc_name("nginx").is_ok());
        assert!(validate_svc_name("nginx.service").is_ok());
        assert!(validate_svc_name("user@1000").is_ok());
        assert!(validate_svc_name("dbus-org.freedesktop.timesync1").is_ok());
    }

    #[test]
    fn test_invalid_svc_names() {
        assert!(validate_svc_name("").is_err());
        assert!(validate_svc_name("svc;rm").is_err());
        assert!(validate_svc_name("svc|cat").is_err());
        assert!(validate_svc_name("svc$VAR").is_err());
    }
}
