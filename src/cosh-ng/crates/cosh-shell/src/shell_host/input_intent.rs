use std::sync::OnceLock;

const SHELL_INTENT_HELPERS: &str = include_str!("input_intent.sh");

pub(super) fn shell_intent_helpers() -> &'static str {
    static HELPERS: OnceLock<String> = OnceLock::new();
    HELPERS
        .get_or_init(|| {
            let high_bytes = (128_u16..=255)
                .map(|byte| {
                    format!("    $'\\x{byte:02X}') _COSH_BYTE_AT_RESULT='{byte}'; return 0 ;;")
                })
                .collect::<Vec<_>>()
                .join("\n");
            SHELL_INTENT_HELPERS.replace("__COSH_HIGH_BYTE_CASES__", &high_bytes)
        })
        .as_str()
}
