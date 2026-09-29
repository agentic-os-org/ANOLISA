//! Validates Agent Composer references and carries them to the provider prompt.

use std::path::{Component, Path, PathBuf};

use crate::input::composer::{slash_completions, token_prefix_at_cursor};

use crate::types::composer::{
    replace_composer_submission, ComposerReference, ComposerReferenceKind, ComposerSubmission,
};
use crate::types::AgentRequest;

const MAX_REFERENCE_COUNT: usize = 16;
const MAX_REFERENCE_BYTES: usize = 256;
const MAX_SKILL_BYTES: usize = 128;
const MAX_COMPLETION_COUNT: usize = 6;
const MAX_COMPLETION_SCAN: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ComposerCompletion {
    pub(crate) display: String,
    pub(crate) replacement: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RejectedComposerReference {
    pub(crate) path: String,
    pub(crate) reason: ComposerReferenceRejection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ComposerReferenceRejection {
    WorkspaceUnavailable,
    InvalidPath,
    UnavailablePath,
    OutsideWorkspace,
    LimitExceeded,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct ComposerFeedback {
    pub(crate) rejected_references: Vec<RejectedComposerReference>,
}

pub(crate) fn attach_submission(
    request: &mut AgentRequest,
    workspace_cwd: Option<&str>,
) -> ComposerFeedback {
    let (submission, feedback) = parse_submission(
        request.user_input.as_deref().unwrap_or_default(),
        workspace_cwd,
    );
    replace_composer_submission(request, &submission);
    feedback
}

pub(crate) fn completions(
    input: &str,
    cursor_row: usize,
    cursor_col: usize,
    workspace_cwd: Option<&str>,
    skill_names: &[String],
) -> Vec<ComposerCompletion> {
    let Some((prefix, first_token)) = token_prefix_at_cursor(input, cursor_row, cursor_col) else {
        return Vec::new();
    };
    if let Some(path_prefix) = prefix.strip_prefix('@') {
        return path_completions(path_prefix, workspace_cwd);
    }
    if first_token && (prefix == "/skill" || prefix.starts_with("/skill:")) {
        let name_prefix = prefix.strip_prefix("/skill:").unwrap_or_default();
        let mut names = skill_names
            .iter()
            .filter(|name| name.starts_with(name_prefix))
            .filter(|name| leading_skill(&format!("/skill:{name}")).is_some())
            .cloned()
            .collect::<Vec<_>>();
        names.sort();
        names.dedup();
        return names
            .into_iter()
            .take(MAX_COMPLETION_COUNT)
            .map(|name| {
                let display = format!("/skill:{name}");
                ComposerCompletion {
                    replacement: format!("{display} "),
                    display,
                }
            })
            .collect();
    }
    slash_completions(input, cursor_row, cursor_col)
        .into_iter()
        .map(|name| ComposerCompletion {
            display: name.to_string(),
            replacement: format!("{name} "),
        })
        .collect()
}

fn path_completions(prefix: &str, workspace_cwd: Option<&str>) -> Vec<ComposerCompletion> {
    if prefix.len() > MAX_REFERENCE_BYTES
        || prefix
            .chars()
            .any(|ch| ch.is_control() || ch.is_whitespace())
        || prefix.starts_with('~')
    {
        return Vec::new();
    }
    let relative = Path::new(prefix);
    if relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Vec::new();
    }

    let (parent, name_prefix) = if prefix.ends_with('/') {
        (prefix, "")
    } else {
        prefix
            .rsplit_once('/')
            .map_or(("", prefix), |(dir, name)| (&prefix[..dir.len() + 1], name))
    };
    let Some(workspace) = workspace_cwd.and_then(canonical_workspace) else {
        return Vec::new();
    };
    let directory = workspace.join(parent);
    let Ok(canonical_directory) = std::fs::canonicalize(&directory) else {
        return Vec::new();
    };
    if !canonical_directory.starts_with(&workspace) || !canonical_directory.is_dir() {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(canonical_directory) else {
        return Vec::new();
    };

    let mut matches = entries
        .take(MAX_COMPLETION_SCAN)
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            if !name.starts_with(name_prefix)
                || name.chars().any(|ch| ch.is_control() || ch.is_whitespace())
                || (name_prefix.is_empty() && name.starts_with('.'))
            {
                return None;
            }
            let canonical = std::fs::canonicalize(entry.path()).ok()?;
            if !canonical.starts_with(&workspace) {
                return None;
            }
            let is_dir = canonical.is_dir();
            if !is_dir && !canonical.is_file() {
                return None;
            }
            let suffix = if is_dir { "/" } else { "" };
            let display = format!("@{parent}{name}{suffix}");
            Some((
                display.clone(),
                ComposerCompletion {
                    replacement: if is_dir {
                        display.clone()
                    } else {
                        format!("{display} ")
                    },
                    display,
                },
            ))
        })
        .collect::<Vec<_>>();
    matches.sort_by(|left, right| left.0.cmp(&right.0));
    matches
        .into_iter()
        .take(MAX_COMPLETION_COUNT)
        .map(|(_, completion)| completion)
        .collect()
}

fn parse_submission(
    input: &str,
    workspace_cwd: Option<&str>,
) -> (ComposerSubmission, ComposerFeedback) {
    let selected_skill = leading_skill(input);
    let mut rejected_references = 0;
    let mut rejected_reference_details = Vec::new();
    let mut references = Vec::new();
    let canonical_cwd = workspace_cwd.and_then(canonical_workspace);

    for candidate in reference_tokens(input) {
        let Some(base) = canonical_cwd.as_deref() else {
            rejected_references += 1;
            rejected_reference_details.push(RejectedComposerReference {
                path: candidate,
                reason: ComposerReferenceRejection::WorkspaceUnavailable,
            });
            continue;
        };
        match validate_reference(base, &candidate) {
            Ok(reference) if references.len() < MAX_REFERENCE_COUNT => {
                references.push(reference);
            }
            Ok(_) => {
                rejected_references += 1;
                rejected_reference_details.push(RejectedComposerReference {
                    path: candidate,
                    reason: ComposerReferenceRejection::LimitExceeded,
                });
            }
            Err(reason) => {
                rejected_references += 1;
                rejected_reference_details.push(RejectedComposerReference {
                    path: candidate,
                    reason,
                });
            }
        }
    }

    (
        ComposerSubmission {
            references,
            selected_skill,
            rejected_references,
        },
        ComposerFeedback {
            rejected_references: rejected_reference_details,
        },
    )
}

fn canonical_workspace(value: &str) -> Option<PathBuf> {
    if value.is_empty() || value == "<unknown>" {
        return None;
    }
    std::fs::canonicalize(value)
        .ok()
        .filter(|path| path.is_dir())
}

fn validate_reference(
    workspace: &Path,
    candidate: &str,
) -> Result<ComposerReference, ComposerReferenceRejection> {
    if candidate.is_empty()
        || candidate.len() > MAX_REFERENCE_BYTES
        || candidate.chars().any(char::is_control)
        || candidate.starts_with('~')
    {
        return Err(ComposerReferenceRejection::InvalidPath);
    }
    let relative = Path::new(candidate);
    if relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(ComposerReferenceRejection::InvalidPath);
    }

    let canonical = std::fs::canonicalize(workspace.join(relative))
        .map_err(|_| ComposerReferenceRejection::UnavailablePath)?;
    if !canonical.starts_with(workspace) {
        return Err(ComposerReferenceRejection::OutsideWorkspace);
    }
    let normalized = canonical
        .strip_prefix(workspace)
        .map_err(|_| ComposerReferenceRejection::OutsideWorkspace)?;
    let kind = if canonical.is_file() {
        ComposerReferenceKind::File
    } else if canonical.is_dir() {
        ComposerReferenceKind::Directory
    } else {
        return Err(ComposerReferenceRejection::UnavailablePath);
    };
    Ok(ComposerReference {
        path: normalized.to_string_lossy().into_owned(),
        kind,
    })
}

fn leading_skill(input: &str) -> Option<String> {
    let token = input.split_whitespace().next()?;
    let name = token.strip_prefix("/skill:")?;
    if name.is_empty()
        || name.len() > MAX_SKILL_BYTES
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'/'))
        || name
            .split('/')
            .any(|segment| segment.is_empty() || matches!(segment, "." | ".."))
    {
        return None;
    }
    Some(name.to_string())
}

fn reference_tokens(input: &str) -> impl Iterator<Item = String> + '_ {
    input
        .split_whitespace()
        .filter_map(|token| token.strip_prefix('@'))
        .filter(|path| !path.is_empty())
        .map(|path| path.trim_matches('"').to_string())
}
