//! Check the Adapter-resolved Skill through the public read-only Ledger command.

use std::{
    fs::File,
    io::Read,
    os::unix::fs::OpenOptionsExt,
    path::{Component, Path},
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

use aw_exec::{CommandSpec, Limits};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Skill {
    name: String,
    directory: String,
    source: String,
    digest: Option<String>,
}

#[derive(Deserialize)]
struct Check {
    status: String,
    #[serde(rename = "manifestHash")]
    manifest_hash: Option<String>,
}

pub(super) fn call(
    cli: &Path,
    socket: &Path,
    event: &Value,
    timeout: Duration,
) -> Result<&'static str, &'static str> {
    let submission = event["name"] == "input.submit";
    if submission
        && event["native"].get("aw_skill") == Some(&Value::Null)
        && event["native"].get("aw_skill_request") == Some(&Value::Null)
    {
        return Ok("skill_not_requested");
    }
    let requested = if submission {
        &event["native"]["aw_skill_request"]
    } else {
        &event["tool"]["input"]["skill"]
    };
    let skill: Skill = serde_json::from_value(event["native"]["aw_skill"].clone())
        .map_err(|_| "skill_resolution_error")?;
    let directory = Path::new(&skill.directory);
    if requested.as_str() != Some(skill.name.as_str())
        || skill.name.is_empty()
        || skill.name.len() > 1024
        || skill.source.is_empty()
        || skill.source.len() > 128
        || !directory.is_absolute()
        || directory.parent().is_none()
        || skill.directory.len() > 4096
        || skill.directory.contains('\0')
        || directory
            .components()
            .any(|part| matches!(part, Component::ParentDir))
    {
        return Err("skill_resolution_error");
    }
    if submission && !skill.digest.as_deref().is_some_and(valid_digest) {
        return Err("skill_resolution_error");
    }
    let timeout_ms = timeout.as_millis().min(u32::MAX as u128);
    if timeout_ms == 0 {
        return Err("deadline_exceeded");
    }
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or("invalid_budget")?;
    let command = CommandSpec {
        program: cli.into(),
        args: vec![
            "--trace-context".into(),
            super::trace_context::from_event(event).to_string().into(),
            "--socket".into(),
            socket.into(),
            "--timeout-ms".into(),
            timeout_ms.to_string().into(),
            "skill-ledger".into(),
            "check".into(),
            skill.directory.clone().into(),
        ],
        cwd: std::env::current_dir().map_err(|_| "cli_context_error")?,
        environment: std::env::vars_os().collect(),
    };
    let output = aw_exec::run(
        &command,
        &[],
        Limits {
            input_bytes: 0,
            stdout_bytes: 1024 * 1024,
            stderr_bytes: 65536,
        },
        deadline,
        &AtomicBool::new(false),
    )
    .map_err(|error| match error {
        aw_exec::Error::DeadlineExceeded => "deadline_exceeded",
        aw_exec::Error::OutputLimit { .. } => "cli_output_limit",
        aw_exec::Error::Cleanup { .. } => "cli_cleanup_error",
        _ => "cli_transport_error",
    })?;
    // Ledger uses exit 1 for completed checks with negative trust states as
    // well as backend errors. Only a recognized, consistent state is a verdict.
    if !matches!(output.status.code(), Some(0 | 1)) {
        return Err("cli_failed");
    }
    let check: Check =
        serde_json::from_slice(&output.stdout).map_err(|_| "invalid_skill_result")?;
    match (output.status.code(), check.status.as_str()) {
        (Some(0), "pass") => {
            if submission {
                let checked = check
                    .manifest_hash
                    .as_deref()
                    .filter(|value| valid_digest(value))
                    .ok_or("invalid_skill_result")?;
                // Bind the local artifact to the digest authenticated by this check.
                // An unreadable, swapped or forged artifact cannot establish source trust.
                if manifest_skill_digest(directory, checked).as_deref() != skill.digest.as_deref() {
                    return Ok("skill_drifted");
                }
            }
            Ok("skill_pass")
        }
        (_, "none") => Ok("skill_unsigned"),
        (_, "drifted") => Ok("skill_drifted"),
        (_, "warn") => Ok("skill_warn"),
        (_, "deny") => Ok("skill_deny"),
        (_, "tampered") => Ok("skill_tampered"),
        (_, "error") => Err("skill_check_error"),
        _ => Err("invalid_skill_result"),
    }
}

fn valid_digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

/// Read the existing V2 Ledger artifact without importing backend code or keys.
/// The CLI authenticates manifestHash; the artifact only supplies its preimage.
fn manifest_skill_digest(directory: &Path, authenticated: &str) -> Option<String> {
    const MAX_MANIFEST_BYTES: u64 = 8 * 1024 * 1024;
    let file = File::options()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
        .open(directory.join(".skill-meta/latest.json"))
        .ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(MAX_MANIFEST_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_MANIFEST_BYTES {
        return None;
    }
    let mut manifest: Value = serde_json::from_slice(&bytes).ok()?;
    if manifest["version"] != 2
        || manifest["canonicalSkillDir"].as_str() != directory.to_str()
        || manifest["scanStatus"] != "pass"
        || manifest["manifestHash"] != authenticated
    {
        return None;
    }
    let digest = manifest["fileHashes"]["SKILL.md"].as_str()?.to_owned();
    if !valid_digest(&digest) {
        return None;
    }
    let fields = manifest.as_object_mut()?;
    fields.remove("manifestHash");
    fields.remove("signature");
    // V2 hashes compact JSON with recursively sorted object keys, excluding these fields.
    manifest.sort_all_objects();
    let hash = format!(
        "sha256:{:x}",
        Sha256::digest(serde_json::to_vec(&manifest).ok()?)
    );
    (hash == authenticated).then_some(digest)
}
