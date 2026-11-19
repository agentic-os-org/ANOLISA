//! Canonical artifact encoding shared by generation and deployment validation.

use std::fmt::Write as _;
use std::path::Path;

use asc_file_policy::{
    FilePathPattern, FileProtectionExclusionKind, FileProtectionTargetKind, PatternSyntax,
};
use asc_policy_types::identifiers::{ResourceId, Revision};

use crate::LuaLsmPlan;

/// Reversible ASCII name; no truncation, PID suffix or kernel-issued identifier.
pub fn module_name(id: &ResourceId, revision: Revision) -> String {
    format!("asc_{id}_r{}", revision.get())
}

/// Encodes intent with the supported template for generation and integrity checks.
/// The plan's existing `source` field is ignored. Performs no filesystem I/O.
pub fn render_module(plan: &LuaLsmPlan, directory: &Path) -> String {
    let name = module_name(&plan.binding_id, plan.binding_revision);
    let control = directory.join(&name);
    let mut source = format!(
        "local name = {}\nlocal control = {}\nlocal targets = {{\n",
        lua_string(&name),
        lua_string(&control.to_string_lossy()),
    );
    for (index, target) in plan.policy.targets.iter().enumerate() {
        // Formatting a String cannot fail.
        writeln!(
            &mut source,
            "  {{ path = {}, tree = {}, read = {}, write = {}, delete = {}, exclusions = {{",
            lua_string(&target.path),
            target.kind == FileProtectionTargetKind::DirectoryTree,
            target.protections.read,
            target.protections.ordinary_write,
            target.protections.delete,
        )
        .unwrap_or_else(|_| unreachable!("String formatting is infallible"));
        for exclusion in &target.exclusions {
            writeln!(
                &mut source,
                "    {{ path = {}, tree = {} }},",
                lua_string(&exclusion.path),
                exclusion.kind == FileProtectionExclusionKind::DirectorySubtree,
            )
            .unwrap_or_else(|_| unreachable!("String formatting is infallible"));
        }
        if let Some(predicates) = plan.patterns.get(&index) {
            source.push_str("  }, patterns = {\n");
            for pattern in predicates {
                writeln!(
                    source,
                    "    {{ value = {}, cedar = {}, negated = {} }},",
                    lua_string(&pattern.value),
                    pattern.syntax == asc_file_policy::PatternSyntax::CedarLike,
                    pattern.negated
                )
                .unwrap_or_else(|_| unreachable!("String formatting is infallible"));
            }
        }
        source.push_str("  } },\n");
    }
    source.push_str("}\n");
    // Omit unused extensions so unaffected modules retain their confirmation digest.
    let protects_creation = plan.protects_creation();
    for line in include_str!("file_protection.lua").split_inclusive('\n') {
        if let Some(extension) = line.strip_prefix("--@create ") {
            if protects_creation {
                source.push_str(extension);
            }
            continue;
        }
        match line.strip_prefix("--@glob ") {
            None => source.push_str(line),
            Some(_) if plan.patterns.is_empty() => {}
            Some("include patterns.lua\n") => source.push_str(include_str!("patterns.lua")),
            Some(extension) => source.push_str(extension),
        }
    }
    source
}

fn lua_string(value: &str) -> String {
    let mut encoded = String::from("\"");
    for byte in value.bytes() {
        if (b' '..=b'~').contains(&byte) && !matches!(byte, b'"' | b'\\') {
            encoded.push(char::from(byte));
        } else {
            write!(&mut encoded, "\\{byte:03}")
                .unwrap_or_else(|_| unreachable!("String formatting is infallible"));
        }
    }
    encoded.push('"');
    encoded
}

/// Accepts canonical absolute literal paths supported by the Lua template.
pub fn is_supported_path(path: &str) -> bool {
    path.starts_with('/')
        && !path.contains('\0')
        && !path.contains("//")
        && (path == "/" || !path.ends_with('/'))
        && !path.split('/').any(|part| matches!(part, "." | ".."))
}

/// Checks the predicate encoding consumed by the Lua template. A Cedar predicate
/// can match a suffix if another condition bounds the target.
pub fn is_supported_pattern(pattern: &FilePathPattern) -> bool {
    let value = &pattern.value;
    if value.len() > 4096 || value.contains('\0') {
        return false;
    }
    match pattern.syntax {
        PatternSyntax::Glob => {
            is_supported_path(value)
                && !value.contains(['[', ']', '{', '}', '\\'])
                && value.split('/').all(|s| !s.contains("**") || s == "**")
        }
        PatternSyntax::CedarLike => {
            let mut chars = value.chars();
            while let Some(ch) = chars.next() {
                if ch == '\\' && !matches!(chars.next(), Some('*' | '\\')) {
                    return false;
                }
            }
            true
        }
    }
}
