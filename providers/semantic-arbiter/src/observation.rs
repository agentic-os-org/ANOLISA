//! Candidate-only local file facts; no commands, content persistence or policy configuration.

mod fs;
mod inventory;
#[cfg(test)]
mod tests;

pub(crate) use inventory::Inventory;

use semantic_arbiter::contract::{candidate_revision as revision, Candidate, Operation};
use semantic_arbiter::deadline::Ingress;
use semantic_arbiter::local_policy::{ContentDelta, FileObservation, FileState, LocalContext};
use sha2::{Digest, Sha256};

/// Collects bounded facts under the original ingress deadline. Targets are a
/// trusted consumer projection, checked against every candidate resource before
/// any access; expired output is never given a renewed validity window.
pub(crate) fn collect(
    candidate: &Candidate,
    targets: Option<Vec<(String, String)>>,
    workspace: &str,
    inventory: Option<&Inventory>,
    candidate_revision: &str,
    ingress: Ingress,
    deadline_ms: u64,
) -> LocalContext {
    let deadline = ingress.event_deadline_at(deadline_ms);
    let mut context = LocalContext {
        candidate_revision: candidate_revision.into(),
        captured_at_ms: ingress.now_ms(),
        valid_until_ms: deadline_ms,
        files: vec![],
        signals: vec![],
        gaps: vec![],
    };
    if let Err(code) = fs::timely(deadline) {
        gap(&mut context, code);
        return context;
    }
    if revision(candidate).ok().as_deref() != Some(candidate_revision) {
        gap(&mut context, "candidate_binding_mismatch");
        return context;
    }
    let Some(targets) = targets else {
        gap(&mut context, "observation_targets_unknown");
        return context;
    };
    if targets.len() != candidate.changes.len()
        || targets
            .iter()
            .zip(&candidate.changes)
            .any(|((id, _), change)| id != &change.resource_id)
    {
        gap(&mut context, "observation_targets_mismatch");
        return context;
    }
    let root = match fs::root(workspace, deadline) {
        Ok(root) => root,
        Err(code) => {
            gap(&mut context, code);
            return context;
        }
    };
    let root_meta = match root.metadata() {
        Ok(meta) => meta,
        Err(_) => {
            gap(&mut context, "workspace_unavailable");
            return context;
        }
    };
    match inventory {
        Some(inventory) => {
            for code in inventory.gaps() {
                gap(&mut context, code);
            }
        }
        None => gap(&mut context, "inventory_unavailable"),
    }
    let mut remaining = fs::TOTAL_LIMIT;
    for (id, target) in targets {
        if let Err(code) = fs::timely(deadline) {
            gap(&mut context, code);
            break;
        }
        let mut observation = FileObservation {
            resource_id: id,
            state: FileState::Unknown,
            delta: ContentDelta::Unknown,
            existed_at_start: None,
            sha256: None,
        };
        let Some(path) = fs::relative(&target, workspace) else {
            gap(&mut context, "observation_path_unsupported");
            context.files.push(observation);
            continue;
        };
        let before = match fs::metadata(&root, &path) {
            Ok(meta) => Some(meta),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                observation.state = FileState::Absent;
                None
            }
            Err(_) => {
                gap(&mut context, "file_stat_failed");
                context.files.push(observation);
                continue;
            }
        };
        observation.existed_at_start = inventory.and_then(|inventory| {
            if inventory.is_git() {
                inventory.existed_git(&root_meta, workspace, &path, deadline)
            } else {
                inventory.existed(&root_meta, &path, before.as_ref())
            }
        });
        if let Some(before) = before {
            observation.state = if before.file_type().is_symlink() {
                gap(&mut context, "file_symlink_rejected");
                FileState::Symlink
            } else if before.is_dir() {
                gap(&mut context, "directory_not_inspected");
                FileState::Directory
            } else if before.is_file() {
                FileState::Regular
            } else {
                gap(&mut context, "not_regular_file");
                FileState::Unknown
            };
            if before.is_file() {
                match fs::read_stable(&root, &path, &before, &mut remaining, deadline, || {}) {
                    Ok(bytes) => {
                        observation.sha256 = Some(format!("sha256:{:x}", Sha256::digest(&bytes)));
                        observation.delta =
                            literal_delta(candidate, workspace, &path, &bytes, deadline);
                        if observation.delta == ContentDelta::Unknown {
                            gap(&mut context, "content_delta_unknown");
                        }
                    }
                    Err(code) => gap(&mut context, code),
                }
            }
        }
        context.files.push(observation);
    }
    if let Err(code) = fs::timely(deadline) {
        // A syscall or hashing can consume the final budget; never publish
        // partially computed facts as though a fresh collection had succeeded.
        context.files.clear();
        gap(&mut context, code);
    }
    context
}

fn gap(context: &mut LocalContext, code: &str) {
    if context.gaps.len() < 32 && !context.gaps.iter().any(|gap| gap == code) {
        context.gaps.push(code.into());
    }
}

fn literal_delta(
    candidate: &Candidate,
    workspace: &str,
    path: &str,
    bytes: &[u8],
    deadline: tokio::time::Instant,
) -> ContentDelta {
    if candidate.changes.len() != 1 || candidate.changes[0].operation != Operation::Replace {
        return ContentDelta::Unknown;
    }
    let Some(native) = &candidate.native_tool else {
        return ContentDelta::Unknown;
    };
    let Some(fields) = native.input.as_object() else {
        return ContentDelta::Unknown;
    };
    let path_key = match (fields.get("file_path"), fields.get("path")) {
        (Some(_), None) => "file_path",
        (None, Some(_)) => "path",
        _ => return ContentDelta::Unknown,
    };
    if fields[path_key]
        .as_str()
        .and_then(|value| fs::relative(value, workspace))
        .as_deref()
        != Some(path)
    {
        return ContentDelta::Unknown;
    }
    let result = if native.name.eq_ignore_ascii_case("write") {
        if fields.len() != 2 {
            return ContentDelta::Unknown;
        }
        fields
            .get("content")
            .and_then(|value| value.as_str())
            .map(|content| content.as_bytes() == bytes)
    } else if native.name.eq_ignore_ascii_case("edit") {
        let (old_key, new_key) = if fields.contains_key("old_string") {
            ("old_string", "new_string")
        } else {
            ("old_str", "new_str")
        };
        if fields
            .keys()
            .any(|key| ![path_key, old_key, new_key, "replace_all"].contains(&key.as_str()))
        {
            return ContentDelta::Unknown;
        }
        let all = match fields.get("replace_all") {
            None => false,
            Some(value) => match value.as_bool() {
                Some(all) => all,
                None => return ContentDelta::Unknown,
            },
        };
        fields
            .get(old_key)
            .and_then(|v| v.as_str())
            .zip(fields.get(new_key).and_then(|v| v.as_str()))
            .and_then(|(old, new)| {
                edited_equal(bytes, old.as_bytes(), new.as_bytes(), all, deadline)
            })
    } else {
        None
    };
    match result {
        Some(true) => ContentDelta::Unchanged,
        Some(false) => ContentDelta::Changed,
        None => ContentDelta::Unknown,
    }
}

fn edited_equal(
    bytes: &[u8],
    old: &[u8],
    new: &[u8],
    all: bool,
    deadline: tokio::time::Instant,
) -> Option<bool> {
    if old.is_empty() || old.len() > bytes.len() {
        return None;
    }
    // Linear-time byte matching avoids quadratic work on repetitive input and
    // does not require the observed file to be valid UTF-8.
    let mut prefixes = vec![0; old.len()];
    let mut matched = 0;
    for index in 1..old.len() {
        if index % 1024 == 0 {
            fs::timely(deadline).ok()?;
        }
        while matched > 0 && old[index] != old[matched] {
            matched = prefixes[matched - 1];
        }
        if old[index] == old[matched] {
            matched += 1;
        }
        prefixes[index] = matched;
    }
    let mut matches = Vec::new();
    matched = 0;
    for (index, byte) in bytes.iter().enumerate() {
        if index % 1024 == 0 {
            fs::timely(deadline).ok()?;
        }
        while matched > 0 && *byte != old[matched] {
            matched = prefixes[matched - 1];
        }
        if *byte == old[matched] {
            matched += 1;
        }
        if matched == old.len() {
            matches.push(index + 1 - old.len());
            if !all && matches.len() > 1 {
                return None;
            }
            matched = 0;
        }
    }
    if matches.is_empty() || (!all && matches.len() != 1) {
        return None;
    }
    let size = bytes
        .len()
        .checked_sub(matches.len().checked_mul(old.len())?)?
        .checked_add(matches.len().checked_mul(new.len())?)?;
    if size != bytes.len() {
        return Some(false);
    }
    // Only allocate when the result is bounded by the already-read file size.
    let mut result = Vec::with_capacity(size);
    let mut offset = 0;
    for found in matches {
        result.extend_from_slice(&bytes[offset..found]);
        result.extend_from_slice(new);
        offset = found + old.len();
    }
    result.extend_from_slice(&bytes[offset..]);
    Some(result == bytes)
}
