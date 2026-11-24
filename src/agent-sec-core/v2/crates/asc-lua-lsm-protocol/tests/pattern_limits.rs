use asc_file_policy::{FilePathPattern, PatternSyntax};
use asc_lua_lsm_protocol::is_supported_pattern;

#[test]
fn pattern_limits_count_wildcards_in_the_authored_language() {
    for (syntax, value, supported) in [
        (PatternSyntax::Glob, format!("/a/{}", "?".repeat(64)), true),
        (PatternSyntax::Glob, format!("/a/{}", "?".repeat(65)), false),
        (PatternSyntax::Glob, "/a/x**y".into(), false),
        (PatternSyntax::Glob, "/a/*/../b".into(), false),
        (PatternSyntax::CedarLike, "*x".repeat(65), false),
        (PatternSyntax::CedarLike, r"\*?".repeat(65), true),
        (PatternSyntax::CedarLike, "x".repeat(4096), true),
        (PatternSyntax::CedarLike, "x".repeat(4097), false),
    ] {
        assert_eq!(
            is_supported_pattern(&FilePathPattern {
                syntax,
                value,
                negated: false
            }),
            supported
        );
    }
}
