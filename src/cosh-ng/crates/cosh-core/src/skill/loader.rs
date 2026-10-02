use std::path::Path;

use super::types::{SkillConfig, SkillLevel};
use super::SKILL_MANIFEST;

/// Load skills from a directory, supporting two layouts:
/// - Directory format (preferred): `<base>/<name>/SKILL.md`
/// - Flat format (legacy compat): `<base>/<name>.md`
///
/// Mixed layouts are tolerated; directory-format takes precedence over a flat
/// file with the same skill name.
pub fn load_skills_from_dir(base_dir: &Path, level: SkillLevel) -> Vec<SkillConfig> {
    let entries = match std::fs::read_dir(base_dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };

    let mut skills: Vec<SkillConfig> = Vec::new();
    let mut flat_files: Vec<std::path::PathBuf> = Vec::new();

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let manifest = path.join(SKILL_MANIFEST);
            if manifest.exists() {
                if let Some(skill) = parse_skill_file(&manifest, level, &path) {
                    skills.push(skill);
                }
            }
        } else if path.extension().is_some_and(|ext| ext == "md") {
            // Skip a top-level SKILL.md sitting directly in the base dir
            if path.file_name().is_some_and(|n| n == SKILL_MANIFEST) {
                continue;
            }
            flat_files.push(path);
        }
    }

    for path in flat_files {
        let skill_base = path.parent().unwrap_or(base_dir).to_path_buf();
        if let Some(skill) = parse_skill_file(&path, level, &skill_base) {
            if !skills.iter().any(|s| s.name == skill.name) {
                skills.push(skill);
            }
        }
    }

    skills
}

/// Parse a single skill file (SKILL.md inside a dir or a flat .md).
pub fn parse_skill_file(path: &Path, level: SkillLevel, base_dir: &Path) -> Option<SkillConfig> {
    let content = std::fs::read_to_string(path).ok()?;
    parse_skill_content(&content, path, level, base_dir)
}

/// Parse skill content (YAML frontmatter + markdown body).
pub fn parse_skill_content(
    content: &str,
    file_path: &Path,
    level: SkillLevel,
    base_dir: &Path,
) -> Option<SkillConfig> {
    let normalized = normalize_skill_file_content(content);
    let trimmed = normalized.trim_start();

    if !trimmed.starts_with("---") {
        return None;
    }

    // Skip the opening `---` line and scan line-by-line for the closing
    // `---` marker (must be the only content on its line, ignoring whitespace).
    let after_open = &trimmed[3..];
    let after_open = after_open.strip_prefix('\n').unwrap_or(after_open);

    let mut frontmatter_lines: Vec<&str> = Vec::new();
    let mut body_rest: Option<&str> = None;

    for (i, line) in after_open.lines().enumerate() {
        if line.trim() == "---" {
            // Everything after this line is the body.
            let consumed: usize = after_open
                .lines()
                .take(i + 1)
                .map(|l| l.len() + 1) // +1 for '\n'
                .sum();
            body_rest = Some(&after_open[consumed.min(after_open.len())..]);
            break;
        }
        frontmatter_lines.push(line);
    }

    // If no closing `---` found, invalid frontmatter
    let body_raw = body_rest?;
    let frontmatter = frontmatter_lines.join("\n");
    let body = body_raw.trim_start_matches(['\r', '\n']).to_string();

    let yaml: serde_yaml::Value = serde_yaml::from_str(&frontmatter).ok()?;

    let name = yaml
        .get("name")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| derive_name_from_path(file_path))?;

    let description = yaml
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let allowed_tools = parse_allowed_tools(&yaml);

    Some(SkillConfig {
        name,
        description,
        allowed_tools,
        body,
        level,
        file_path: file_path.to_path_buf(),
        base_dir: base_dir.to_path_buf(),
    })
}

fn derive_name_from_path(file_path: &Path) -> Option<String> {
    let file_name = file_path.file_name()?.to_str()?;
    if file_name == SKILL_MANIFEST {
        // Directory format: use parent dir name
        file_path
            .parent()?
            .file_name()?
            .to_str()
            .map(|s| s.to_string())
    } else {
        // Flat format: use file stem
        file_path.file_stem()?.to_str().map(|s| s.to_string())
    }
}

fn parse_allowed_tools(yaml: &serde_yaml::Value) -> Vec<String> {
    match yaml.get("allowedTools") {
        Some(serde_yaml::Value::Sequence(seq)) => seq
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.trim().to_string()))
            .filter(|s| !s.is_empty())
            .collect(),
        Some(serde_yaml::Value::String(s)) => s
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        _ => Vec::new(),
    }
}

/// Strip BOM and normalize CRLF -> LF.
fn normalize_skill_file_content(content: &str) -> String {
    let stripped = content.strip_prefix('\u{feff}').unwrap_or(content);
    stripped.replace("\r\n", "\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn parse_skill_md_with_frontmatter() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join("my-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        let skill_path = skill_dir.join("SKILL.md");
        let mut f = std::fs::File::create(&skill_path).unwrap();
        writeln!(
            f,
            "---\nname: my-skill\ndescription: A test skill\nallowedTools:\n  - shell\n  - read_file\n---\n\nYou are a test skill."
        )
        .unwrap();

        let skill = parse_skill_file(&skill_path, SkillLevel::User, &skill_dir).unwrap();
        assert_eq!(skill.name, "my-skill");
        assert_eq!(skill.description, "A test skill");
        assert_eq!(skill.allowed_tools, vec!["shell", "read_file"]);
        assert!(skill.body.contains("You are a test skill"));
        assert_eq!(skill.level, SkillLevel::User);
        assert_eq!(skill.base_dir, skill_dir);
    }

    #[test]
    fn parse_flat_md_backward_compat() {
        let dir = tempfile::tempdir().unwrap();
        let skill_path = dir.path().join("old-skill.md");
        let mut f = std::fs::File::create(&skill_path).unwrap();
        writeln!(
            f,
            "---\ndescription: Old format skill\n---\n\nOld skill prompt."
        )
        .unwrap();

        let skill = parse_skill_file(&skill_path, SkillLevel::User, dir.path()).unwrap();
        assert_eq!(skill.name, "old-skill");
        assert_eq!(skill.description, "Old format skill");
        assert!(skill.body.contains("Old skill prompt"));
    }

    #[test]
    fn parse_inline_allowed_tools() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.md");
        std::fs::write(
            &path,
            "---\nname: a\ndescription: x\nallowedTools: shell, read_file\n---\n\nbody",
        )
        .unwrap();
        let skill = parse_skill_file(&path, SkillLevel::User, dir.path()).unwrap();
        assert_eq!(skill.allowed_tools, vec!["shell", "read_file"]);
    }

    #[test]
    fn load_skills_from_dir_mixed_layouts() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path();

        // Directory format
        let skill_a_dir = skills_dir.join("skill-a");
        std::fs::create_dir_all(&skill_a_dir).unwrap();
        std::fs::write(
            skill_a_dir.join("SKILL.md"),
            "---\nname: skill-a\ndescription: Dir format\n---\n\nSkill A body.",
        )
        .unwrap();

        // Flat format
        std::fs::write(
            skills_dir.join("skill-b.md"),
            "---\nname: skill-b\ndescription: Flat format\n---\n\nSkill B body.",
        )
        .unwrap();

        let skills = load_skills_from_dir(skills_dir, SkillLevel::User);
        assert_eq!(skills.len(), 2);
        assert!(skills.iter().any(|s| s.name == "skill-a"));
        assert!(skills.iter().any(|s| s.name == "skill-b"));
    }

    #[test]
    fn normalize_strips_bom_and_crlf() {
        let content = "\u{feff}---\r\nname: test\r\ndescription: x\r\n---\r\n\r\nBody.";
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.md");
        std::fs::write(&path, content).unwrap();
        let skill = parse_skill_file(&path, SkillLevel::User, dir.path()).unwrap();
        assert_eq!(skill.name, "test");
        assert!(skill.body.contains("Body."));
    }

    #[test]
    fn missing_frontmatter_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nofm.md");
        std::fs::write(&path, "Just a markdown body without frontmatter.").unwrap();
        assert!(parse_skill_file(&path, SkillLevel::User, dir.path()).is_none());
    }
}
