//! ANOLISA data-home syntax shared by local CLI consumers.

/// Returns whether `value` is an absolute ANOLISA data-home path.
///
/// The check is lexical: it does not access the filesystem or normalize the
/// path, so callers can distinguish unsafe dot segments before normalization.
#[must_use]
pub fn is_valid_anolisa_data_home(value: &str) -> bool {
    !value.is_empty()
        && value.starts_with('/')
        && !value
            .split('/')
            .any(|segment| matches!(segment, "." | ".."))
}

#[cfg(test)]
mod tests {
    use super::is_valid_anolisa_data_home;

    #[test]
    fn accepts_only_absolute_paths_without_dot_segments() {
        for value in ["/data", "/data with spaces/用户", "//data//share/", "/"] {
            assert!(is_valid_anolisa_data_home(value), "{value}");
        }
        for value in ["", "relative/data", "/data/./share", "/data/../share"] {
            assert!(!is_valid_anolisa_data_home(value), "{value}");
        }
    }
}
