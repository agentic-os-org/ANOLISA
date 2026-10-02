use std::path::{Path, PathBuf};

use super::personal_crypto::{hex, hmac_sha256};
use super::personal_model::ActivityContext;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DiscoveredRepoContext {
    pub(crate) root: PathBuf,
    pub(crate) normalized_identity: Option<String>,
}

pub(crate) fn discover_repo_context(cwd: &Path) -> Option<DiscoveredRepoContext> {
    let root = cwd
        .ancestors()
        .find(|ancestor| ancestor.join(".git").exists())?
        .to_path_buf();
    let dot_git = root.join(".git");
    let git_dir = if dot_git.is_dir() {
        dot_git
    } else {
        let pointer = std::fs::read_to_string(&dot_git).ok()?;
        let value = pointer.trim().strip_prefix("gitdir:")?.trim();
        let path = PathBuf::from(value);
        if path.is_absolute() {
            path
        } else {
            root.join(path)
        }
    };
    let common_dir = std::fs::read_to_string(git_dir.join("commondir"))
        .ok()
        .map(|value| git_dir.join(value.trim()))
        .unwrap_or(git_dir);
    let normalized_identity = read_origin_url(&common_dir.join("config"))
        .and_then(|remote| normalize_remote(&remote))
        .or_else(|| {
            common_dir
                .canonicalize()
                .ok()
                .map(|path| format!("gitdir:{}", path.to_string_lossy()))
        });
    Some(DiscoveredRepoContext {
        root,
        normalized_identity,
    })
}

fn read_origin_url(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.len() > 64 * 1024 {
        return None;
    }
    let content = std::str::from_utf8(&bytes).ok()?;
    let mut in_origin = false;
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_origin = line.eq_ignore_ascii_case("[remote \"origin\"]");
            continue;
        }
        if in_origin {
            if let Some((key, value)) = line.split_once('=') {
                if key.trim().eq_ignore_ascii_case("url") {
                    return Some(value.trim().to_string());
                }
            }
        }
    }
    None
}

pub(crate) fn build_activity_context(
    epoch_key: &[u8],
    host_material: &str,
    cwd: &Path,
    repo_root: Option<&Path>,
    normalized_remote: Option<&str>,
    home: &Path,
) -> ActivityContext {
    let host_id = build_host_id(epoch_key, host_material);
    let Some(repo_root) = repo_root else {
        return ActivityContext {
            host_id,
            repo_id: None,
            repo_name: None,
            cwd_relative: Some(normalize_outside_repo_path(cwd, home)),
        };
    };

    let repo_identity = normalized_remote
        .filter(|value| !value.trim().is_empty())
        .map(str::as_bytes)
        .unwrap_or_else(|| repo_root.as_os_str().as_encoded_bytes());
    let repo_id = format!(
        "repo:hmac:v1:{}",
        hex(&hmac_sha256(epoch_key, repo_identity))
    );
    let repo_name = repo_root
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(valid_repo_name);
    let cwd_relative = cwd
        .strip_prefix(repo_root)
        .ok()
        .map(|relative| {
            if relative.as_os_str().is_empty() {
                ".".to_string()
            } else {
                relative.to_string_lossy().to_string()
            }
        })
        .or_else(|| Some(normalize_outside_repo_path(cwd, home)));

    ActivityContext {
        host_id,
        repo_id: Some(repo_id),
        repo_name,
        cwd_relative,
    }
}

pub(crate) fn build_host_id(epoch_key: &[u8], host_material: &str) -> Option<String> {
    (!host_material.is_empty()).then(|| {
        format!(
            "host:hmac:v1:{}",
            hex(&hmac_sha256(epoch_key, host_material.as_bytes()))
        )
    })
}

pub(crate) fn normalize_remote(remote: &str) -> Option<String> {
    let remote = remote.trim();
    if remote.is_empty() {
        return None;
    }
    let without_credentials = if let Some((scheme, rest)) = remote.split_once("://") {
        let authority_and_path = rest
            .split_once('@')
            .map(|(_, suffix)| suffix)
            .unwrap_or(rest);
        format!("{}://{}", scheme.to_ascii_lowercase(), authority_and_path)
    } else if let Some((_, suffix)) = remote.split_once('@') {
        suffix.to_string()
    } else {
        remote.to_string()
    };
    Some(
        without_credentials
            .trim_end_matches('/')
            .trim_end_matches(".git")
            .to_ascii_lowercase(),
    )
}

fn normalize_outside_repo_path(cwd: &Path, home: &Path) -> String {
    let value = cwd
        .strip_prefix(home)
        .ok()
        .map(|relative| format!("$HOME/{}", relative.to_string_lossy()))
        .unwrap_or_else(|| cwd.to_string_lossy().to_string());
    truncate_utf8(&value, 512)
}

fn valid_repo_name(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty()
        || value.len() > 64
        || !value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        return None;
    }
    Some(value.to_string())
}

fn truncate_utf8(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}
