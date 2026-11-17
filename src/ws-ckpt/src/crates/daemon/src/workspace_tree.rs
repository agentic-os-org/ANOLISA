//! Workspace fork provenance tree.
//!
//! `fork` stores `fork-origin.json` beside the new workspace's snapshot index.
//! This module reads those records back into a tree so an operator can see
//! which workspace came from which snapshot, and which workspaces share an
//! ancestor. It never mutates storage: an unreadable or dangling record
//! becomes a warning, not an error, because the rest of the tree is still
//! meaningful.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use ws_ckpt_common::{ErrorCode, Response, WorkspaceTreeNode};

use crate::state::DaemonState;
use crate::workspace_mgr::{ForkOrigin, FORK_ORIGIN_FILE};

struct Registered {
    path: String,
    origin: Option<ForkOrigin>,
}

fn error_resp(code: ErrorCode, message: impl Into<String>) -> Response {
    Response::Error {
        code,
        message: message.into(),
    }
}

/// Resolve the fork provenance tree around `workspace`.
///
/// Without `all`, the tree covers the target, its direct parent, the parent's
/// other children (siblings) and the target's own children — the neighbourhood
/// an operator needs after a fork. With `all`, it walks to the root and shows
/// every descendant.
pub async fn tree(
    state: &Arc<DaemonState>,
    workspace: &str,
    all: bool,
) -> anyhow::Result<Response> {
    let Some(target) = state.resolve_workspace(workspace).await else {
        return Ok(error_resp(
            ErrorCode::WorkspaceNotFound,
            format!("workspace not found: {workspace}"),
        ));
    };
    let target_ws_id = target.read().await.ws_id.clone();

    let mut warnings = Vec::new();
    let mut registry: BTreeMap<String, Registered> = BTreeMap::new();
    for workspace in state.all_workspaces() {
        let (ws_id, path) = {
            let workspace = workspace.read().await;
            (workspace.ws_id.clone(), workspace.path.display().to_string())
        };
        let origin = match tokio::fs::read(state.index_dir(&ws_id).join(FORK_ORIGIN_FILE)).await {
            Ok(bytes) => match serde_json::from_slice::<ForkOrigin>(&bytes) {
                Ok(origin) => Some(origin),
                Err(error) => {
                    warnings.push(format!("{ws_id}: unreadable fork provenance: {error}"));
                    None
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                warnings.push(format!("{ws_id}: cannot read fork provenance: {error}"));
                None
            }
        };
        registry.insert(ws_id, Registered { path, origin });
    }

    // Children are sorted by path so a tree listing is stable and readable;
    // the workspace ID is the tie-breaker for identical paths.
    let mut children: HashMap<String, Vec<String>> = HashMap::new();
    for (ws_id, entry) in &registry {
        if let Some(origin) = &entry.origin {
            children
                .entry(origin.source_ws_id.clone())
                .or_default()
                .push(ws_id.clone());
        }
    }
    for kids in children.values_mut() {
        kids.sort_by(|left, right| {
            registry
                .get(left)
                .map(|entry| entry.path.as_str())
                .cmp(&registry.get(right).map(|entry| entry.path.as_str()))
                .then_with(|| left.cmp(right))
        });
    }

    let root = match root_of(&registry, &target_ws_id, all, &mut warnings) {
        Ok(root) => root,
        Err(cycle) => {
            return Ok(error_resp(
                ErrorCode::InternalError,
                format!("fork provenance cycle: {}", cycle.join(" -> ")),
            ))
        }
    };

    let included = if all {
        registry.keys().cloned().collect::<BTreeSet<_>>()
    } else {
        let mut included = BTreeSet::new();
        included.insert(target_ws_id.clone());
        included.insert(root.clone());
        if let Some(siblings) = children.get(&root) {
            included.extend(siblings.iter().cloned());
        }
        if let Some(descendants) = children.get(&target_ws_id) {
            included.extend(descendants.iter().cloned());
        }
        included
    };

    // Pre-order traversal with a visited set; a corrupted record that points
    // at an ancestor cannot make this loop forever.
    let mut nodes = Vec::new();
    let mut visited = BTreeSet::new();
    let mut stack = vec![(root.clone(), 0u32)];
    while let Some((ws_id, depth)) = stack.pop() {
        if !visited.insert(ws_id.clone()) {
            warnings.push(format!(
                "{ws_id}: reached twice through fork provenance; stopping that branch"
            ));
            continue;
        }
        if !included.contains(&ws_id) {
            continue;
        }
        let Some(entry) = registry.get(&ws_id) else {
            continue;
        };
        let origin = entry.origin.as_ref();
        let source_registered = origin
            .map(|origin| registry.contains_key(&origin.source_ws_id))
            .unwrap_or(false);
        nodes.push(WorkspaceTreeNode {
            ws_id: ws_id.clone(),
            path: entry.path.clone(),
            depth,
            source_ws_id: origin.map(|origin| origin.source_ws_id.clone()),
            source_snapshot_id: origin.map(|origin| origin.source_snapshot_id.clone()),
            source_registered,
            target: ws_id == target_ws_id,
        });
        if let Some(kids) = children.get(&ws_id) {
            for kid in kids.iter().rev() {
                stack.push((kid.clone(), depth + 1));
            }
        }
    }

    Ok(Response::WorkspaceTreeOk {
        target_ws_id,
        root_ws_id: root,
        nodes,
        warnings,
    })
}

/// Find the root of the displayed tree, rejecting provenance cycles.
fn root_of(
    registry: &BTreeMap<String, Registered>,
    target_ws_id: &str,
    all: bool,
    warnings: &mut Vec<String>,
) -> Result<String, Vec<String>> {
    let mut chain = vec![target_ws_id.to_string()];
    let mut current = target_ws_id.to_string();
    let mut visited = BTreeSet::from([current.clone()]);
    loop {
        let Some(origin) = registry.get(&current).and_then(|entry| entry.origin.as_ref()) else {
            break;
        };
        let parent = origin.source_ws_id.clone();
        if !visited.insert(parent.clone()) {
            chain.push(parent);
            return Err(chain);
        }
        if !registry.contains_key(&parent) {
            warnings.push(format!(
                "{current}: forked from {parent}, which is not registered"
            ));
            break;
        }
        chain.push(parent.clone());
        current = parent;
        if !all {
            break;
        }
    }
    Ok(current)
}
