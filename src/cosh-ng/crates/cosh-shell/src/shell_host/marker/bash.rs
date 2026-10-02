// Owner: shell_host (bash marker script). The standalone asset keeps Bash
// syntax out of the Rust owner while preserving the emitted bytes verbatim.
const BASH_MARKER_SCRIPT: &str = include_str!("bash.sh");

pub(in crate::shell_host) fn bash_marker_script() -> &'static str {
    BASH_MARKER_SCRIPT
}
