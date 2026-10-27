//! Build Tokenless from tracked sources with a fresh pinned RTK dependency.

use super::{add, command, copy};
use crate::{digest, Error, Payload, Result};
use std::{collections::BTreeMap, fs, path::Path, sync::atomic::AtomicBool};

/// Copy tracked Tokenless sources and prepare RTK without reusing local edits.
pub(super) fn prepare(repo: &Path, cancel: &AtomicBool) -> Result<tempfile::TempDir> {
    prepare_using(repo, cancel, |root| {
        command(
            repo,
            "bash",
            &[
                "src/tokenless/scripts/setup-rtk.sh".into(),
                root.join("third_party/rtk").display().to_string(),
            ],
            180,
            cancel,
        )
        .map(|_| ())
    })
}

/// Keep setup replaceable in tests so no fork inherits another test's file locks.
pub(super) fn prepare_using(
    repo: &Path,
    cancel: &AtomicBool,
    setup: impl FnOnce(&Path) -> Result<()>,
) -> Result<tempfile::TempDir> {
    // A source under AW's target would be captured by its Cargo workspace.
    let root = tempfile::Builder::new()
        .prefix("aw-rtk-source-")
        .tempdir()?;
    let tracked = command(
        repo,
        "git",
        &["ls-files".into(), "-z".into(), "src/tokenless".into()],
        10,
        cancel,
    )?;
    for name in tracked
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
    {
        let name = std::str::from_utf8(name).map_err(|error| Error::Invalid(error.to_string()))?;
        let relative = Path::new(name)
            .strip_prefix("src/tokenless")
            .map_err(|error| Error::Invalid(error.to_string()))?;
        let destination = root.path().join(relative);
        fs::create_dir_all(
            destination
                .parent()
                .ok_or_else(|| Error::Invalid("source has no parent".into()))?,
        )?;
        let source = repo.join(name);
        if source.symlink_metadata()?.file_type().is_symlink() {
            // Adapter directory links must retain their relative targets in the snapshot.
            std::os::unix::fs::symlink(fs::read_link(&source)?, &destination)?;
        } else {
            copy(&source, &destination, cancel)?;
        }
    }
    setup(root.path())?;
    Ok(root)
}

/// Own licenses, exact patched-build inputs and their source/hash provenance.
pub(super) fn stage(
    repo: &Path,
    root: &Path,
    rtk: &Path,
    source_commit: &str,
    files: &mut BTreeMap<String, Payload>,
    cancel: &AtomicBool,
) -> Result<()> {
    let directory = "share/doc/aw-provider-tokenless/rtk";
    let shared = rtk.join("../../crates/tokenless-stats/src");
    let mut inputs = Vec::new();
    for (name, source, origin) in [
        ("LICENSE", rtk.join("LICENSE"), "upstream:LICENSE"),
        (
            "source-commit",
            rtk.join(".anolisa-rtk-commit"),
            "setup-rtk.sh:.anolisa-rtk-commit",
        ),
        (
            "tokenless-stats/home.rs",
            shared.join("home.rs"),
            "src/tokenless/crates/tokenless-stats/src/home.rs",
        ),
        (
            "tokenless-stats/path_policy.rs",
            shared.join("path_policy.rs"),
            "src/tokenless/crates/tokenless-stats/src/path_policy.rs",
        ),
        (
            "tokenless-stats/rtk_outcome.rs",
            shared.join("rtk_outcome.rs"),
            "src/tokenless/crates/tokenless-stats/src/rtk_outcome.rs",
        ),
        (
            "setup-rtk.sh",
            repo.join("src/tokenless/scripts/setup-rtk.sh"),
            "src/tokenless/scripts/setup-rtk.sh",
        ),
        (
            "patches-sha256",
            rtk.join(".anolisa-rtk-patches"),
            "setup-rtk.sh:.anolisa-rtk-patches",
        ),
    ] {
        add(
            root,
            &format!("{directory}/{name}"),
            &source,
            false,
            files,
            cancel,
        )?;
        inputs.push(serde_json::json!({"source":origin,"payload":name,"sha256":digest(&source)?}));
    }
    let setup = fs::read_to_string(repo.join("src/tokenless/scripts/setup-rtk.sh"))?;
    let patches = setup
        .lines()
        .find_map(|line| {
            line.strip_prefix("PATCHES=(")
                .and_then(|line| line.strip_suffix(')'))
        })
        .filter(|list| !list.trim().is_empty())
        .ok_or_else(|| Error::Invalid("RTK patch list is missing".into()))?;
    for name in patches.split_whitespace() {
        let source = repo.join("src/tokenless/third_party/patches").join(name);
        add(
            root,
            &format!("{directory}/{name}"),
            &source,
            false,
            files,
            cancel,
        )?;
        inputs.push(serde_json::json!({
            "source":format!("src/tokenless/third_party/patches/{name}"),
            "payload":name,"sha256":digest(&source)?
        }));
    }
    let provenance = root.join("rtk-provenance.json");
    fs::write(
        &provenance,
        serde_json::to_vec_pretty(&serde_json::json!({
            "upstream":{"repository":"https://github.com/rtk-ai/rtk.git","release":"v0.49.0",
                "commit":fs::read_to_string(rtk.join(".anolisa-rtk-commit"))?.trim()},
            "anolisa_source_commit":source_commit,
            "shared_sources_reason":"The Tokenless patch includes home.rs, path_policy.rs and rtk_outcome.rs by relative path to share home, state-path and call-scoped receipt rules.",
            "inputs":inputs
        }))?,
    )?;
    add(
        root,
        &format!("{directory}/provenance.json"),
        &provenance,
        false,
        files,
        cancel,
    )?;
    fs::remove_file(provenance).map_err(Error::from)
}
