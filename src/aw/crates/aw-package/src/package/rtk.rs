//! Reproduce Tokenless's pinned RTK source layout and receipt its build inputs.

use super::{add, command, copy};
use crate::{digest, Error, Payload, Result};
use std::{collections::BTreeMap, fs, path::Path, sync::atomic::AtomicBool};

/// Fetch and patch an independent RTK source tree without reusing local edits.
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
    let shared = root.path().join("crates/tokenless-stats/src");
    fs::create_dir_all(&shared)?;
    for name in ["home.rs", "path_policy.rs"] {
        copy(
            &repo
                .join("src/tokenless/crates/tokenless-stats/src")
                .join(name),
            &shared.join(name),
            cancel,
        )?;
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
            "setup-rtk.sh",
            repo.join("src/tokenless/scripts/setup-rtk.sh"),
            "src/tokenless/scripts/setup-rtk.sh",
        ),
        (
            "rtk-tokenless-stats.patch",
            repo.join("src/tokenless/third_party/patches/rtk-tokenless-stats.patch"),
            "src/tokenless/third_party/patches/rtk-tokenless-stats.patch",
        ),
        (
            "rtk-pytest-error-report.patch",
            repo.join("src/tokenless/third_party/patches/rtk-pytest-error-report.patch"),
            "src/tokenless/third_party/patches/rtk-pytest-error-report.patch",
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
    let provenance = root.join("rtk-provenance.json");
    fs::write(
        &provenance,
        serde_json::to_vec_pretty(&serde_json::json!({
            "upstream":{"repository":"https://github.com/rtk-ai/rtk.git","release":"v0.49.0",
                "commit":fs::read_to_string(rtk.join(".anolisa-rtk-commit"))?.trim()},
            "anolisa_source_commit":source_commit,
            "shared_sources_reason":"The Tokenless statistics patch includes home.rs and path_policy.rs by relative path to share passwd-based home and state-path rules.",
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
