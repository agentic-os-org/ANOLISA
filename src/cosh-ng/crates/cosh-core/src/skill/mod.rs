pub mod loader;
pub mod manager;
pub mod types;

pub use manager::SkillManager;
#[allow(unused_imports)]
pub use types::{SkillConfig, SkillLevel};

/// Configuration directory name used by copilot-shell / cosh.
pub const COPILOT_CONFIG_DIR: &str = ".copilot-shell";
/// Sub-directory containing skill definitions.
pub const SKILLS_DIR: &str = "skills";
/// Canonical skill manifest file name inside a skill directory.
pub const SKILL_MANIFEST: &str = "SKILL.md";

/// Byte ceiling for rendered frontmatter metadata (name, description).
pub(crate) const METADATA_RENDER_MAX: usize = 512;
/// Byte ceiling for rendered skill paths (provenance, base directory hint).
pub(crate) const PATH_RENDER_MAX: usize = 1024;

/// Render attacker-influenced metadata on a single harness-authored line.
/// Escapes line breaks and control characters so skill names, descriptions,
/// and paths cannot forge extra lines outside the content fence.
pub(crate) fn single_line(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\u{2028}' | '\u{2029}' => out.push(' '),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// Single-line render within a fixed byte ceiling, so attacker-controlled
/// metadata can never exhaust the tool-output budget reserved for the fence
/// closing marker and framing. Over-ceiling renders end with a harness
/// ellipsis marker.
pub(crate) fn metadata_render(value: &str, max_bytes: usize) -> String {
    let escaped = single_line(value);
    if escaped.len() <= max_bytes {
        return escaped;
    }
    const ELLIPSIS: &str = "...[truncated]";
    let keep = max_bytes.saturating_sub(ELLIPSIS.len());
    let mut boundary = keep.min(escaped.len());
    while !escaped.is_char_boundary(boundary) {
        boundary -= 1;
    }
    format!("{}{}", &escaped[..boundary], ELLIPSIS)
}
