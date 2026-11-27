//! Discover package skills using the strict Agent Skills frontmatter contract.

mod frontmatter;

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::SystemTime;

use crate::{ParseStatus, SkillEntry, SkillMetadata, parser::parse_skill_md};

use super::containment::{Kind, OpenError, Opened, PackageRoot, ReadError};
use super::{DiagnosticSeverity, PluginComponent, PluginDiagnostic, fixed_location};

/// A valid Agent Skill found at the package's fixed discovery location.
#[derive(Debug)]
pub struct PackageSkill {
    /// Logical directory name, as it appears in the package.
    pub name: String,
    /// Logical package-relative document path.
    pub path: String,
    /// Resolved physical skill directory.
    pub directory: PathBuf,
    /// Resolved physical skill document.
    pub document: PathBuf,
    entry: SkillEntry,
}

impl PackageSkill {
    /// Create a SkillFS entry from the skill accepted during package discovery.
    pub fn to_skill_entry(&self) -> SkillEntry {
        self.entry.clone()
    }
}

pub(super) fn discover(
    root: &PackageRoot,
    diagnostics: &mut Vec<PluginDiagnostic>,
) -> Option<Vec<PackageSkill>> {
    let container = fixed_location(
        root,
        "skills",
        PluginComponent::Skills,
        Kind::Directory,
        diagnostics,
    )?;
    let Ok(children) = container.names(root) else {
        diagnostics.push(PluginDiagnostic::new(
            DiagnosticSeverity::DisableComponent,
            PluginComponent::Skills,
            "skills",
            &[],
            "§6.2",
            "cannot list skills directory",
        ));
        return None;
    };

    Some(
        children
            .iter()
            .filter_map(|name| discover_one(root, &container, name, diagnostics))
            .collect(),
    )
}

fn discover_one(
    root: &PackageRoot,
    container: &Rc<Opened>,
    name: &OsStr,
    diagnostics: &mut Vec<PluginDiagnostic>,
) -> Option<PackageSkill> {
    let directory = match root.open_below(container, Path::new(name), Kind::Directory) {
        Ok(directory) => directory,
        Err(OpenError::Containment(_)) => {
            emit(
                diagnostics,
                &format!("skills/{}", name.to_string_lossy()),
                None,
                "§4.1",
                "skill path fails containment",
            );
            return None;
        }
        Err(OpenError::Unreadable(_)) => {
            emit(
                diagnostics,
                &format!("skills/{}/SKILL.md", name.to_string_lossy()),
                None,
                "§7.1",
                "cannot read UTF-8 skill document",
            );
            return None;
        }
        Err(OpenError::Absent | OpenError::WrongKind) => return None,
    };
    let logical_path = || format!("skills/{}/SKILL.md", name.to_string_lossy());
    let unreadable = "cannot read UTF-8 skill document";
    let document = match root.open_below(&directory, Path::new("SKILL.md"), Kind::RegularFile) {
        Ok(document) => document,
        Err(OpenError::Containment(_)) => {
            emit(
                diagnostics,
                &logical_path(),
                None,
                "§4.1",
                "skill path fails containment",
            );
            return None;
        }
        Err(OpenError::Unreadable(_)) => {
            emit(diagnostics, &logical_path(), None, "§7.1", unreadable);
            return None;
        }
        Err(OpenError::Absent | OpenError::WrongKind) => return None,
    };

    let path = logical_path();
    let Some(name) = name.to_str() else {
        emit(
            diagnostics,
            &path,
            None,
            "§7.1",
            "skill directory name is not UTF-8",
        );
        return None;
    };

    let read = document
        .read_regular(root)
        .map_err(|error| match error {
            ReadError::TooLarge => "skill document exceeds 1 MiB limit",
            ReadError::Io(_) => unreadable,
        })
        .and_then(|(bytes, meta)| {
            let text = String::from_utf8(bytes).map_err(|_| unreadable)?;
            Ok((text, meta))
        });
    let (text, meta) = match read {
        Ok(read) => read,
        Err(message) => {
            emit(diagnostics, &path, None, "§7.1", message);
            return None;
        }
    };
    let normalized = frontmatter::normalize_line_endings(&text);
    let validated = frontmatter::split(&normalized).and_then(|(yaml, body)| {
        frontmatter::validate(yaml, name).map(|description| (description, body))
    });
    let (description, body) = match validated {
        Ok(result) => result,
        Err(error) => {
            emit(
                diagnostics,
                &path,
                error.field.as_deref(),
                "§7.1",
                error.message,
            );
            return None;
        }
    };

    // The validated body may itself start with a fence. Supply an empty
    // frontmatter region so legacy section extraction sees the entire body.
    let mut entry = parse_skill_md(&format!("---\n---\n{body}"), name);
    entry.metadata = SkillMetadata {
        name: name.to_owned(),
        description,
        ..SkillMetadata::default()
    };
    entry.parse_status = ParseStatus::Ok;
    // Flat projection finds companion resources next to this logical leaf,
    // even if SKILL.md itself links to a shared document elsewhere.
    entry.source_path = directory.path.join("SKILL.md");
    entry.last_modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    Some(PackageSkill {
        name: name.to_owned(),
        path,
        directory: directory.path.clone(),
        document: document.path.clone(),
        entry,
    })
}

fn emit(
    diagnostics: &mut Vec<PluginDiagnostic>,
    path: &str,
    field: Option<&str>,
    reference: &'static str,
    message: &'static str,
) {
    let keys = match &field {
        Some(field) => std::slice::from_ref(field),
        _ => &[],
    };
    diagnostics.push(PluginDiagnostic::new(
        DiagnosticSeverity::SkipSkill,
        PluginComponent::Skills,
        path,
        keys,
        reference,
        message,
    ));
}
