//! raw_e2e tests for the  command.

use super::super::tests::*;

use anolisa_core::ComponentManifest;
use anolisa_core::domain::{Installation, LifecycleStatus, OwnedArtifact, ProviderBinding};
use anolisa_core::download::DownloadError;
use anolisa_core::state::{
    InstallMode as StateInstallMode, InstalledObject, ObjectKind, ObjectStatus,
};
use anolisa_core::state_store::StateStore;
use anolisa_platform::fs_layout::FsLayout;

use crate::commands::common;
use crate::context::InstallMode;
use crate::test_support::{TestContextOptions, TestSandbox};
use tempfile::tempdir;

const JSON_REASON_ENV: &str = "ANOLISA_TEST_MISSING_INDEX_REASON";
const PLATFORM_JSON_REASON_ENV: &str = "ANOLISA_TEST_PLATFORM_MISMATCH_REASON";
const JSON_BEGIN: &str = "ANOLISA_TEST_JSON_BEGIN";
const JSON_END: &str = "ANOLISA_TEST_JSON_END";

#[test]
#[ignore = "invoked as an isolated child by the missing-index regression"]
fn render_missing_index_error_json_child() {
    let reason = std::env::var(JSON_REASON_ENV)
        .expect("missing-index JSON child must be invoked by its parent regression test");
    let tmp = tempdir().expect("tmpdir");
    let err = crate::response::CliError::Runtime {
        command: "install agentsight".to_string(),
        reason,
    };

    crate::output::flush_stdout();
    crate::output::write_stdout(format_args!("{JSON_BEGIN}"), true);
    crate::output::flush_stdout();
    let exit_code =
        crate::response::render_error(&ctx_with_prefix(true, Some(tmp.path().join("sys"))), &err);
    crate::output::flush_stdout();
    crate::output::write_stdout(format_args!("{JSON_END}"), true);
    crate::output::flush_stdout();
    assert_eq!(exit_code, std::process::ExitCode::from(1));
}

/// Isolated child for the platform-mismatch regression: renders the reason the
/// real resolver produced through the production `--json` path, so the parent
/// asserts the multi-line platform list survives the envelope instead of
/// assuming a string escapes correctly.
#[test]
#[ignore = "invoked as an isolated child by the platform-mismatch regression"]
fn render_platform_mismatch_error_json_child() {
    let reason = std::env::var(PLATFORM_JSON_REASON_ENV)
        .expect("platform-mismatch JSON child must be invoked by its parent regression test");
    let tmp = tempdir().expect("tmpdir");
    let err = crate::response::CliError::InvalidArgument {
        command: "install agentsight".to_string(),
        reason,
    };

    crate::output::flush_stdout();
    crate::output::write_stdout(format_args!("{JSON_BEGIN}"), true);
    crate::output::flush_stdout();
    let exit_code =
        crate::response::render_error(&ctx_with_prefix(true, Some(tmp.path().join("sys"))), &err);
    crate::output::flush_stdout();
    crate::output::write_stdout(format_args!("{JSON_END}"), true);
    crate::output::flush_stdout();
    assert_eq!(exit_code, std::process::ExitCode::from(2));
}

/// v5 store as the pipeline persisted it for a system-prefix layout.
fn load_v5_store(layout: &FsLayout) -> StateStore {
    StateStore::load(&layout.state_dir.join("installed.toml"), 0).expect("state must load")
}

/// The owned artifact behind a component's binding; fails the test on a
/// delegated binding, which also asserts the raw family was recorded.
fn owned_artifact(installation: &Installation) -> &OwnedArtifact {
    match &installation.binding {
        ProviderBinding::Owned { artifact } => artifact,
        other => panic!("expected an owned binding, got {other:?}"),
    }
}

#[test]
fn install_dry_run_resolves_without_writing_files() {
    let tmp = tempdir().expect("tmpdir");
    let prefix = tmp.path().join("sys");
    let repo_url = write_local_repo(&tmp.path().join("repo"));

    let mut a = args("agentsight");
    a.repo = Some(repo_url);
    let mut ctx = ctx_with_prefix(false, Some(prefix.clone()));
    ctx.dry_run = true;
    handle_with_fake_rpm(a, &ctx).expect("dry-run must succeed");

    let layout = FsLayout::system(Some(prefix));
    assert!(
        !layout.bin_dir.join("agentsight").exists(),
        "dry-run must not install the binary"
    );
    assert!(
        !layout.state_dir.join("installed.toml").exists(),
        "dry-run must not write state"
    );
    let cached_names: Vec<String> = std::fs::read_dir(layout.cache_dir.join("downloads"))
        .expect("downloads cache exists")
        .map(|entry| {
            entry
                .expect("cache entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert!(
        cached_names
            .iter()
            .all(|name| !name.ends_with("agentsight.tar.gz")),
        "dry-run must not download the install artifact; cache entries: {cached_names:?}"
    );
}

#[test]
fn install_dry_run_does_not_download_the_artifact() {
    let tmp = tempdir().expect("tmpdir");
    let prefix = tmp.path().join("sys");
    let repo_url = write_published_layout_repo_with_meta(
        &tmp.path().join("repo"),
        "remote-only",
        "1.0.0",
        &["system"],
    );
    let mut ctx = ctx_with_prefix(false, Some(prefix.clone()));
    ctx.dry_run = true;
    let layout = FsLayout::system(Some(prefix));

    let mut a = args("remote-only");
    a.repo = Some(repo_url);
    handle_with_fake_rpm(a, &ctx).expect("dry-run must succeed");

    let cached_names: Vec<String> = std::fs::read_dir(layout.cache_dir.join("downloads"))
        .expect("downloads cache exists")
        .map(|entry| {
            entry
                .expect("cache entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert!(
        cached_names
            .iter()
            .all(|name| !name.ends_with("remote-only-1.0.0-linux-x86_64.tar.gz")),
        "dry-run must not download the install artifact; cache entries: {cached_names:?}"
    );
    assert!(
        cached_names.iter().any(|name| name.ends_with("meta.toml")),
        "dry-run must fetch the lightweight contract; cache entries: {cached_names:?}"
    );
}

#[test]
fn install_reports_missing_local_repository_index_from_override() {
    let tmp = tempdir().expect("tmpdir");
    let prefix = tmp.path().join("sys");
    let repo_root = tmp.path().join("repo");
    let repo_url = write_local_repo(&repo_root);
    let index_path = repo_root.join("v1/index.toml");
    // Remove only the distribution index: the published component index
    // stays so identity validation passes and the missing-index diagnostic
    // below is what surfaces.
    std::fs::remove_file(&index_path).expect("remove repository index");

    let mut a = args("agentsight");
    a.repo = Some(repo_url);
    let err = handle_with_fake_rpm(a, &ctx_with_prefix(false, Some(prefix.clone())))
        .expect_err("missing repository index must fail");
    let reason = err.reason();

    assert_eq!(err.code(), "EXECUTION_FAILED");
    assert_eq!(err.exit_code(), 1);
    assert_eq!(err.command(), "install agentsight");
    assert!(
        reason.contains(&index_path.display().to_string()),
        "missing-index diagnostic must identify {index_path:?}; got: {reason}"
    );
    assert!(
        reason.contains("one-off --repo <URL> override"),
        "missing-index diagnostic must identify the one-off override; got: {reason}"
    );
    assert!(
        !reason.contains("repo.toml"),
        "override diagnostic must not blame repo.toml; got: {reason}"
    );
    assert!(
        !reason.contains("failed to fetch distribution index"),
        "missing-index diagnostic must not retain the generic wrapper; got: {reason}"
    );

    // Run this test executable as an isolated child so the production renderer's
    // stdout can be asserted without replacing process-global output in-process.
    let child_module = module_path!()
        .split_once("::")
        .map_or(module_path!(), |(_, module)| module);
    let child_test = format!("{child_module}::render_missing_index_error_json_child");
    let output = std::process::Command::new(std::env::current_exe().expect("current test binary"))
        .args([child_test.as_str(), "--exact", "--ignored", "--nocapture"])
        .env(JSON_REASON_ENV, &reason)
        .output()
        .expect("run isolated JSON renderer child");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "JSON renderer child failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    let (_, after_begin) = stdout
        .split_once(JSON_BEGIN)
        .expect("child stdout must contain the JSON start marker");
    let (json, _) = after_begin
        .split_once(JSON_END)
        .expect("child stdout must contain the JSON end marker");
    let parsed: serde_json::Value =
        serde_json::from_str(json.trim()).expect("rendered error must be valid JSON");
    assert_eq!(parsed["ok"], false);
    assert_eq!(parsed["schema_version"], crate::response::SCHEMA_VERSION);
    assert_eq!(parsed["command"], "install agentsight");
    assert_eq!(parsed["error"]["code"], "EXECUTION_FAILED");
    assert_eq!(parsed["error"]["reason"], reason);
    let error = parsed["error"]
        .as_object()
        .expect("rendered error payload must be an object");
    assert!(!error.contains_key("hint"));
}

#[test]
fn install_reports_config_path_for_missing_local_repository_index() {
    let tmp = tempdir().expect("tmpdir");
    let prefix = tmp.path().join("sys");
    let layout = FsLayout::system(Some(prefix.clone()));
    let repo_root = tmp.path().join("repo");
    let repo_url = write_local_repo(&repo_root);
    let index_path = repo_root.join("v1/index.toml");
    // Remove only the distribution index: the published component index
    // stays so identity validation passes and the config-attributed
    // missing-index diagnostic below is what surfaces.
    std::fs::remove_file(&index_path).expect("remove repository index");

    std::fs::create_dir_all(&layout.etc_dir).expect("create repo config directory");
    let config_path = layout.etc_dir.join("repo.toml");
    std::fs::write(
        &config_path,
        format!(
            "schema_version = 1\ndefault_backend = \"raw\"\n\n[backends.raw]\nbase_url = \"{repo_url}\"\n"
        ),
    )
    .expect("write repo config");

    let err = handle_with_fake_rpm(args("agentsight"), &ctx_with_prefix(false, Some(prefix)))
        .expect_err("missing repository index must fail");
    let reason = err.reason();

    assert!(
        reason.contains(&index_path.display().to_string()),
        "missing-index diagnostic must identify {index_path:?}; got: {reason}"
    );
    assert!(
        reason.contains(&config_path.display().to_string()),
        "missing-index diagnostic must identify active config {config_path:?}; got: {reason}"
    );
    assert!(
        reason.contains("move it aside and retry"),
        "config diagnostic must recommend a non-destructive recovery; got: {reason}"
    );
    assert!(
        reason.contains("--repo <URL>"),
        "config diagnostic must mention the one-off override escape hatch; got: {reason}"
    );
    assert!(
        !reason.contains("one-off --repo <URL> override selected this repository"),
        "config diagnostic must not claim a CLI override was used; got: {reason}"
    );
}

#[test]
fn install_preserves_non_not_found_index_fetch_error() {
    let tmp = tempdir().expect("tmpdir");
    let index_path = tmp.path().join("repo/v1/index.toml");
    let index_url = format!("file://{}", index_path.display());
    let download_error = DownloadError::Io {
        path: index_path,
        source: std::io::Error::new(std::io::ErrorKind::PermissionDenied, "index access denied"),
    };
    let rendered_download_error = download_error.to_string();
    let err = super::super::raw::index_fetch_error(&index_url, download_error, None);

    assert_eq!(
        err.reason(),
        format!("failed to fetch distribution index {index_url}: {rendered_download_error}"),
        "non-NotFound I/O errors must retain the generic diagnostic"
    );
    assert_eq!(err.code(), "EXECUTION_FAILED");
    assert_eq!(err.exit_code(), 1);
    assert!(!err.reason().contains("repo.toml"));
    assert!(!err.reason().contains("--repo <URL>"));
}

#[test]
fn install_preserves_not_found_away_from_index_path() {
    let tmp = tempdir().expect("tmpdir");
    let index_path = tmp.path().join("repo/v1/index.toml");
    let other_path = tmp.path().join("cache/downloads");
    let index_url = format!("file://{}", index_path.display());
    let download_error = DownloadError::Io {
        path: other_path,
        source: std::io::Error::new(std::io::ErrorKind::NotFound, "cache path missing"),
    };
    let rendered_download_error = download_error.to_string();
    let err = super::super::raw::index_fetch_error(&index_url, download_error, None);

    assert_eq!(
        err.reason(),
        format!("failed to fetch distribution index {index_url}: {rendered_download_error}"),
        "NotFound away from the index path must retain the generic diagnostic"
    );
    assert!(!err.reason().contains("repo.toml"));
    assert!(!err.reason().contains("--repo <URL>"));
}

#[test]
fn install_rejects_local_binary_before_artifact_fetch() {
    let tmp = tempdir().expect("tmpdir");
    let prefix = tmp.path().join("sys");

    let mut a = args("legacy-bin");
    a.repo = Some(write_binary_repo_component(
        &tmp.path().join("repo"),
        "legacy-bin",
        "1.0.0",
        &["system"],
    ));
    let err = handle_with_fake_rpm(a, &ctx_with_prefix(false, Some(prefix.clone())))
        .expect_err("local binary must be rejected");

    assert!(
        err.reason().contains("cannot resolve package 'legacy-bin'")
            && err.reason().contains("no distribution entry matches"),
        "got: {}",
        err.reason()
    );
    assert!(
        !err.reason().contains("publish it as"),
        "got: {}",
        err.reason()
    );
    let downloads = FsLayout::system(Some(prefix)).cache_dir.join("downloads");
    let cached_names: Vec<String> = std::fs::read_dir(downloads)
        .expect("downloads cache exists")
        .map(|entry| {
            entry
                .expect("cache entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert!(
        cached_names
            .iter()
            .all(|name| !name.ends_with("legacy-bin") && !name.ends_with("meta.toml")),
        "rejected binary must not fetch artifact or sidecar: {cached_names:?}"
    );
}

#[test]
fn install_dry_run_rejects_remote_binary_before_artifact_fetch() {
    let tmp = tempdir().expect("tmpdir");
    let prefix = tmp.path().join("sys");
    let repo_root = tmp.path().join("repo");
    let repo_url = write_binary_repo_component(&repo_root, "remote-bin", "1.0.0", &["system"]);
    let index_path = repo_root.join("v1/index.toml");
    let index = std::fs::read_to_string(&index_path)
        .expect("read index")
        .replace(
            "url = \"remote-bin\"",
            "url = \"https://example.test/remote-bin\"",
        );
    std::fs::write(index_path, index).expect("write remote binary index");

    let mut a = args("remote-bin");
    a.repo = Some(repo_url);
    let mut ctx = ctx_with_prefix(false, Some(prefix.clone()));
    ctx.dry_run = true;
    let err = handle_with_fake_rpm(a, &ctx).expect_err("remote binary must be rejected");

    assert!(
        err.reason().contains("cannot resolve package 'remote-bin'")
            && err.reason().contains("no distribution entry matches"),
        "got: {}",
        err.reason()
    );
    assert!(
        !err.reason().contains("publish it as"),
        "got: {}",
        err.reason()
    );
    let downloads = FsLayout::system(Some(prefix)).cache_dir.join("downloads");
    let cached_names: Vec<String> = std::fs::read_dir(downloads)
        .expect("downloads cache exists")
        .map(|entry| {
            entry
                .expect("cache entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert!(
        cached_names
            .iter()
            .all(|name| !name.ends_with("remote-bin") && !name.ends_with("meta.toml")),
        "rejected binary must not fetch artifact or sidecar: {cached_names:?}"
    );
}

#[test]
fn install_ignores_binary_entries_when_resolving_tar_gz() {
    let tmp = tempdir().expect("tmpdir");
    let prefix = tmp.path().join("sys");
    let repo_root = tmp.path().join("repo");
    let repo_url = write_local_repo(&repo_root);
    let index_path = repo_root.join("v1/index.toml");
    let mut index = std::fs::read_to_string(&index_path).expect("read index");
    let env = anolisa_env::EnvService::detect();

    for version in ["0.2.0", "9.0.0"] {
        index.push_str(&format!(
            r#"

[[entries]]
component = "agentsight"
version = "{version}"
channel = "stable"
artifact_type = "binary"
backend = "raw"
url = "legacy-agentsight-{version}"
os = "{os}"
arch = "{arch}"
install_modes = ["system"]
sha256 = "{sha}"
"#,
            os = env.os,
            arch = env.arch,
            sha = "0".repeat(64),
        ));
    }
    std::fs::write(index_path, index).expect("write mixed index");

    let mut a = args("agentsight");
    a.repo = Some(repo_url.clone());
    handle_with_fake_rpm(a, &ctx_with_prefix(false, Some(prefix.clone())))
        .expect("tar_gz entry must remain installable");

    let layout = FsLayout::system(Some(prefix));
    assert!(layout.bin_dir.join("agentsight").exists());
    let store = load_v5_store(&layout);
    let installation = store
        .find(ObjectKind::Component, "agentsight")
        .expect("installed object");
    assert_eq!(
        owned_artifact(installation).version,
        "0.2.0",
        "the higher binary-only 9.0.0 entry must be ignored by resolution"
    );
}

#[test]
fn config_edits_pass_doctor_but_data_edits_fail() {
    use crate::commands::tier1::doctor::{DoctorArgs, handle as doctor};
    use sha2::{Digest, Sha256};

    for reverse in [false, true] {
        let tmp = tempdir().expect("tmpdir");
        let prefix = tmp.path().join("sys");
        let repo = tmp.path().join("repo");
        let repo_url = write_local_repo(&repo);
        let manifest = format!(
            "{}\n{}",
            component_manifest_toml("agentsight", "0.2.0", &["system"]),
            r#"[[component.layout.files]]
source = "share/settings.toml"
target = "{datadir}/components/agentsight/settings.toml"
mode = "0644"
type = "config"

[[component.layout.files]]
source = "conf.d/"
target = "{datadir}/components/agentsight/conf.d"
type = "config"

[[component.layout.files]]
source = "data/"
target = "{datadir}/components/agentsight"
mode = "0644"

[[component.layout.files]]
source = "bin/tool"
target = "{datadir}/components/agentsight/conf.d/tool"
mode = "0755"
type = "executable"

[[component.layout.files]]
source = "immutable/"
target = "{datadir}/components/agentsight/conf.d/assets"
mode = "0644"

[[component.layout.files]]
source = "shared-data/"
target = "{datadir}/components/agentsight/shared"
mode = "0600"

[[component.layout.files]]
source = "shared-config/"
target = "{datadir}/components/agentsight/shared"
mode = "0644"
type = "config"

[[component.layout.files]]
source = "unambiguous/"
target = "{datadir}/isolated-config"
type = "config"
"#
        );
        let mut manifest: toml::Value = toml::from_str(&manifest).expect("manifest");
        if reverse {
            manifest["component"]["layout"]["files"]
                .as_array_mut()
                .expect("files")
                .reverse();
        }
        let manifest = toml::to_string(&manifest).expect("serialize manifest");
        let artifact = build_tar_gz(&[
            (".anolisa/component.toml", manifest.as_bytes()),
            ("bin/agentsight", b"#!/bin/sh\necho agentsight\n"),
            ("share/settings.toml", b"enabled = true\n"),
            ("conf.d/extra.toml", b"enabled = true\n"),
            ("data/plain.dat", b"immutable\n"),
            ("data/conf.d/from-data.dat", b"immutable\n"),
            ("shared-data/plain.dat", b"immutable\n"),
            ("shared-config/settings.toml", b"enabled = true\n"),
            ("unambiguous/settings.toml", b"enabled = true\n"),
            ("bin/tool", b"#!/bin/sh\necho tool\n"),
            ("immutable/plain.dat", b"immutable\n"),
        ]);
        std::fs::write(repo.join("v1/agentsight.tar.gz"), &artifact).expect("artifact");
        let index_path = repo.join("v1/index.toml");
        let mut index: toml::Value =
            toml::from_str(&std::fs::read_to_string(&index_path).expect("index")).expect("parse");
        index["entries"][0]["sha256"] = format!("{:x}", Sha256::digest(&artifact)).into();
        std::fs::write(index_path, toml::to_string(&index).expect("serialize")).expect("index");

        let ctx = ctx_with_prefix(false, Some(prefix.clone()));
        let mut a = args("agentsight");
        a.repo = Some(repo_url);
        handle_with_fake_rpm(a, &ctx).expect("install");
        let layout = FsLayout::system(Some(prefix));
        let files_dir = layout.datadir.join("components/agentsight");
        std::fs::write(
            layout.datadir.join("isolated-config/settings.toml"),
            b"enabled = false\n",
        )
        .expect("edit unambiguous directory config");
        let store = load_v5_store(&layout);
        let installed = store
            .find(ObjectKind::Component, "agentsight")
            .expect("installed");
        for name in ["settings.toml", "conf.d/extra.toml", "shared/settings.toml"] {
            let row = owned_artifact(installed)
                .files
                .iter()
                .find(|file| file.path == files_dir.join(name))
                .expect("config row");
            assert_eq!(row.kind, anolisa_core::OwnedFileKind::Config);
            assert!(row.sha256.is_some(), "retain the install-time digest");
            std::fs::write(&row.path, b"enabled = false\n").expect("edit config");
        }
        for name in [
            "plain.dat",
            "conf.d/tool",
            "conf.d/assets/plain.dat",
            "conf.d/from-data.dat",
            "shared/plain.dat",
        ] {
            let row = owned_artifact(installed)
                .files
                .iter()
                .find(|file| file.path == files_dir.join(name))
                .expect("immutable row");
            assert_eq!(row.kind, anolisa_core::OwnedFileKind::File);
        }
        let diagnose = || {
            doctor(
                DoctorArgs {
                    component: Some("agentsight".into()),
                    fix: false,
                },
                &ctx,
            )
        };
        diagnose().expect("operator config edits must stay healthy");

        // Legacy directory rows have no source provenance; ambiguous kinds must
        // retain hashing. Exact config mappings remain safe to recover.
        for name in ["conf.d/extra.toml", "shared/settings.toml"] {
            std::fs::write(files_dir.join(name), b"enabled = true\n").expect("restore config");
        }
        let state_path = layout.state_dir.join("installed.toml");
        let legacy = std::fs::read_to_string(&state_path)
            .expect("state")
            .replace("kind = \"config\"\n", "");
        std::fs::write(&state_path, &legacy).expect("legacy state");
        diagnose().expect("exact legacy config edits must stay healthy");
        assert_eq!(std::fs::read_to_string(&state_path).expect("state"), legacy);

        for name in [
            "plain.dat",
            "conf.d/tool",
            "conf.d/assets/plain.dat",
            "conf.d/from-data.dat",
            "shared/plain.dat",
            "conf.d/extra.toml",
            "shared/settings.toml",
        ] {
            let path = files_dir.join(name);
            let original = std::fs::read(&path).expect("read original");
            std::fs::write(&path, b"tampered\n").expect("tamper hash-checked file");
            assert!(
                matches!(diagnose(), Err(CliError::DiagnosticsFound { .. })),
                "{name}"
            );
            std::fs::write(&path, original).expect("restore original");
            diagnose().expect("restored file is healthy");
        }
    }
}

#[test]
fn install_raw_end_to_end_from_local_repo() {
    let tmp = tempdir().expect("tmpdir");
    let prefix = tmp.path().join("sys");
    let repo_url = write_local_repo(&tmp.path().join("repo"));

    let mut a = args("agentsight");
    a.repo = Some(repo_url.clone());
    handle_with_fake_rpm(a, &ctx_with_prefix(false, Some(prefix.clone())))
        .expect("install must succeed");

    let layout = FsLayout::system(Some(prefix));
    let bin = layout.bin_dir.join("agentsight");
    assert!(bin.exists(), "binary must be installed at {{bindir}}");
    let manifest_path = common::installed_component_manifest_path(&layout, "agentsight", COMMAND)
        .expect("manifest path");
    assert!(
        manifest_path.exists(),
        "installed component manifest must be persisted"
    );
    let saved_manifest =
        ComponentManifest::from_file(&manifest_path).expect("saved manifest parses");
    assert_eq!(saved_manifest.component.name, "agentsight");
    assert_eq!(saved_manifest.component.version, "0.2.0");

    let store = load_v5_store(&layout);
    assert_eq!(store.install_mode, StateInstallMode::System);
    assert_eq!(store.prefix, layout.prefix);
    let installation = store
        .find(ObjectKind::Component, "agentsight")
        .expect("component object must be recorded");
    assert_eq!(installation.status, LifecycleStatus::Installed);
    // `owned_artifact` panics on a delegated binding, so this also asserts
    // the raw family was recorded as the authority.
    let artifact = owned_artifact(installation);
    assert_eq!(artifact.version, "0.2.0");
    assert_eq!(artifact.files.len(), 2);
    assert!(
        artifact.files.iter().any(|file| file.path == manifest_path),
        "installed manifest must be tracked as an owned file"
    );
    assert!(
        artifact
            .distribution_source
            .as_deref()
            .is_some_and(|u| u.starts_with(&repo_url)),
        "distribution_source must record the resolved artifact URL"
    );
    assert_eq!(
        artifact.raw_package.as_deref(),
        Some("agentsight"),
        "raw_package must record the resolved package so update can reuse it"
    );
    assert!(
        artifact.services.iter().all(|s| !s.enabled),
        "install must not mark services enabled"
    );
    assert_eq!(store.operations.len(), 1);
    assert!(store.operations[0].id.starts_with("op-install-"));
}

#[test]
fn system_raw_install_rechecks_native_absence_under_lock() {
    let tmp = tempdir().expect("tmpdir");
    let prefix = tmp.path().join("sys");
    let repo_url = write_local_repo(&tmp.path().join("repo"));
    let ctx = ctx_with_prefix(false, Some(prefix.clone()));
    let layout = FsLayout::system(Some(prefix));
    let fake = FakeInstaller::new(
        "agentsight",
        pkg_info("agentsight", "0.2.0", Some("1.al8"), "x86_64"),
    )
    .package_appears_under_lock(layout.lock_file.clone());
    let mut a = args("agentsight");
    a.repo = Some(repo_url);

    let err = install_component_with_deps("agentsight", &a, &ctx, &fake, &NoTxn, true)
        .expect_err("an external RPM appearing before raw placement must block install");

    assert!(err.reason().contains("appeared"), "got: {}", err.reason());
    assert!(
        !layout.bin_dir.join("agentsight").exists(),
        "a refused race must not place owned files"
    );
    assert!(
        load_v5_store(&layout)
            .find(ObjectKind::Component, "agentsight")
            .is_none(),
        "a refused race must not claim an owned record"
    );
}

#[test]
fn deb_host_raw_install_rechecks_native_absence_under_lock() {
    // The deb-family relaxation covers a missing rpm binary only. With
    // working tooling on a deb-family host the planning probe resolves a
    // native package, so an RPM appearing between planning and placement
    // must still trip the locked recheck — the family never mutes obtained
    // presence evidence.
    let tmp = tempdir().expect("tmpdir");
    let prefix = tmp.path().join("sys");
    let repo_url = write_local_repo(&tmp.path().join("repo"));
    let ctx = ctx_with_prefix(false, Some(prefix.clone()));
    let layout = FsLayout::system(Some(prefix));
    let fake = FakeInstaller::new(
        "agentsight",
        pkg_info("agentsight", "0.2.0", Some("1.al8"), "x86_64"),
    )
    .package_appears_under_lock(layout.lock_file.clone());
    let mut a = args("agentsight");
    a.repo = Some(repo_url);

    let err = install_component_with_deps_and_env(
        "agentsight",
        &a,
        &ctx,
        &deb_host_env(),
        &RpmdbProbe::absent(),
        &fake,
        &NoTxn,
        true,
    )
    .expect_err("an external RPM appearing before raw placement must block install");

    assert!(err.reason().contains("appeared"), "got: {}", err.reason());
    assert!(
        load_v5_store(&layout)
            .find(ObjectKind::Component, "agentsight")
            .is_none(),
        "a refused race must not claim an owned record"
    );
}

#[test]
fn deb_host_tooling_appearing_under_lock_still_refuses() {
    // The race the planning degrade must not hide: rpm tooling is missing
    // while the deb-family plan resolves (CommandMissing degrades to
    // NotProbed), then tooling plus an external same-named RPM appear
    // before raw placement. The probe identity survives planning, so the
    // locked recheck re-probes, obtains presence evidence, and refuses.
    let tmp = tempdir().expect("tmpdir");
    let prefix = tmp.path().join("sys");
    let repo_url = write_local_repo(&tmp.path().join("repo"));
    let ctx = ctx_with_prefix(false, Some(prefix.clone()));
    let layout = FsLayout::system(Some(prefix));
    let fake = FakeInstaller::new(
        "agentsight",
        pkg_info("agentsight", "0.2.0", Some("1.al8"), "x86_64"),
    )
    .tooling_appears_under_lock(layout.lock_file.clone());
    let mut a = args("agentsight");
    a.repo = Some(repo_url);

    let err = install_component_with_deps_and_env(
        "agentsight",
        &a,
        &ctx,
        &deb_host_env(),
        &RpmdbProbe::absent(),
        &fake,
        &NoTxn,
        true,
    )
    .expect_err("presence evidence gained under the lock must block the raw install");

    assert!(err.reason().contains("appeared"), "got: {}", err.reason());
    assert!(
        load_v5_store(&layout)
            .find(ObjectKind::Component, "agentsight")
            .is_none(),
        "a refused race must not claim an owned record"
    );
}

#[test]
fn deb_host_rpmdb_growing_under_lock_still_refuses() {
    // The degraded CommandMissing policy is a planning-time snapshot: an
    // external process may install RPMs with a binary this process cannot
    // see (absolute path outside PATH), so its queries keep failing while
    // an rpmdb grows on disk. The locked recheck must re-verify the
    // filesystem evidence instead of trusting the captured verdict.
    let tmp = tempdir().expect("tmpdir");
    let prefix = tmp.path().join("sys");
    // The rpmdb grows under the host probe root, deliberately separate
    // from the install prefix: evidence follows the host, not the layout.
    let host_root = tmp.path().join("host");
    let repo_url = write_local_repo(&tmp.path().join("repo"));
    let ctx = ctx_with_prefix(false, Some(prefix.clone()));
    let layout = FsLayout::system(Some(prefix.clone()));
    let rpmdb = RpmdbProbe::with_roots(host_root.clone(), None);
    let fake = FakeInstaller::new(
        "agentsight",
        pkg_info("agentsight", "0.2.0", Some("1.al8"), "x86_64"),
    )
    .rpmdb_appears_under_lock(
        layout.lock_file.clone(),
        host_root.join("var/lib/rpm/rpmdb.sqlite"),
    );
    let mut a = args("agentsight");
    a.repo = Some(repo_url);

    let err = install_component_with_deps_and_env(
        "agentsight",
        &a,
        &ctx,
        &deb_host_env(),
        &rpmdb,
        &fake,
        &NoTxn,
        true,
    )
    .expect_err("an rpmdb appearing under the lock must block the raw install");

    assert!(
        err.reason().contains("rpm database appeared"),
        "got: {}",
        err.reason()
    );
    assert!(
        load_v5_store(&layout)
            .find(ObjectKind::Component, "agentsight")
            .is_none(),
        "a refused race must not claim an owned record"
    );
}

#[test]
fn deb_host_package_override_survives_planning_degrade() {
    // Same race as above but with an explicit `--package` naming an RPM
    // that differs from the component: the override is already the known
    // probe identity, so the CommandMissing degrade must not collapse it
    // to the component name. The locked recheck then probes the RPM the
    // user named, sees it appeared with the tooling, and refuses.
    let tmp = tempdir().expect("tmpdir");
    let prefix = tmp.path().join("sys");
    let repo_url = write_local_repo(&tmp.path().join("repo"));
    // The raw index is keyed by the backend-native package name, so the
    // override needs an alternate publication of the same artifact under
    // `custom-rpm` for the pre-lock artifact resolution to succeed.
    let index_path = tmp.path().join("repo/v1/index.toml");
    let index = std::fs::read_to_string(&index_path).expect("read index");
    let alternate = index
        .split("[[entries]]")
        .nth(1)
        .expect("fixture entry")
        .replace("component = \"agentsight\"", "component = \"custom-rpm\"");
    std::fs::write(index_path, format!("{index}\n[[entries]]{alternate}"))
        .expect("write alternate publication");
    let ctx = ctx_with_prefix(false, Some(prefix.clone()));
    let layout = FsLayout::system(Some(prefix));
    let fake = FakeInstaller::new(
        "custom-rpm",
        pkg_info("custom-rpm", "0.2.0", Some("1.al8"), "x86_64"),
    )
    .tooling_appears_under_lock(layout.lock_file.clone());
    let mut a = args("agentsight");
    a.repo = Some(repo_url);
    a.package = Some("custom-rpm".to_string());

    let err = install_component_with_deps_and_env(
        "agentsight",
        &a,
        &ctx,
        &deb_host_env(),
        &RpmdbProbe::absent(),
        &fake,
        &NoTxn,
        true,
    )
    .expect_err("the overridden RPM appearing under the lock must block the raw install");

    assert!(
        err.reason().contains("'custom-rpm' appeared"),
        "the recheck must probe the --package identity, got: {}",
        err.reason()
    );
    assert!(
        load_v5_store(&layout)
            .find(ObjectKind::Component, "agentsight")
            .is_none(),
        "a refused race must not claim an owned record"
    );
}

#[test]
fn prepare_raw_execution_resolves_declared_capabilities() {
    let tmp = tempdir().expect("tmpdir");
    let prefix = tmp.path().join("sys");
    let repo_url = write_local_repo_component_with_capability(
        &tmp.path().join("repo"),
        "agentsight",
        "0.2.0",
        &["system"],
        "{bindir}/agentsight",
        &["CAP_BPF", "CAP_PERFMON"],
        true,
    );
    let ctx = ctx_with_prefix(false, Some(prefix.clone()));
    let layout = FsLayout::system(Some(prefix.clone()));
    let env = anolisa_env::EnvService::detect();
    let resolution = resolve_raw(
        &ctx,
        &layout,
        &env,
        ResolveInputs {
            component: "agentsight".to_string(),
            package: "agentsight".to_string(),
            backend: "raw".to_string(),
            base_url: repo_url,
            repository_origin: None,
            version: None,
            warnings: Vec::new(),
        },
    )
    .expect("resolve");
    let prepared = prepare_raw_execution(&ctx, &layout, resolution).expect("prepare");

    assert_eq!(prepared.capabilities.len(), 1);
    assert_eq!(
        prepared.capabilities[0].path,
        layout.bin_dir.join("agentsight")
    );
    assert_eq!(
        prepared.capabilities[0].caps,
        vec!["CAP_BPF".to_string(), "CAP_PERFMON".to_string()]
    );
    assert!(prepared.capabilities[0].optional);
    // Resolve-only: no setcap, no file laid, no state.
    assert!(!layout.bin_dir.join("agentsight").exists());
    assert!(!layout.state_dir.join("installed.toml").exists());
}

#[test]
fn install_raw_end_to_end_applies_optional_capability() {
    let tmp = tempdir().expect("tmpdir");
    let prefix = tmp.path().join("sys");
    let repo_url = write_local_repo_component_with_capability(
        &tmp.path().join("repo"),
        "agentsight",
        "0.2.0",
        &["system"],
        "{bindir}/agentsight",
        &["CAP_BPF"],
        true,
    );

    let mut a = args("agentsight");
    a.repo = Some(repo_url);
    let effects = crate::test_support::RawEffectRecorder::default();
    effects
        .with(|factories| {
            handle_with_effects(a, &ctx_with_prefix(false, Some(prefix.clone())), factories)
        })
        .expect("install with fake capability must succeed");
    assert_eq!(
        effects.calls(),
        vec![
            "capability factory system".to_string(),
            format!(
                "apply {} CAP_BPF",
                FsLayout::system(Some(prefix.clone()))
                    .bin_dir
                    .join("agentsight")
                    .display()
            ),
            "service factory system System".to_string(),
        ]
    );

    let layout = FsLayout::system(Some(prefix));
    assert!(
        layout.bin_dir.join("agentsight").exists(),
        "binary must be installed even when the optional setcap is skipped"
    );
    let store = load_v5_store(&layout);
    assert!(
        store.find(ObjectKind::Component, "agentsight").is_some(),
        "component must be recorded despite optional capability outcome"
    );
}

#[test]
fn prepare_raw_execution_resolves_declared_services() {
    let tmp = tempdir().expect("tmpdir");
    let prefix = tmp.path().join("sys");
    let repo_url = write_local_repo_component_with_service(
        &tmp.path().join("repo"),
        "agentsight",
        "0.2.0",
        &["system"],
        "agentsight.service",
        true,
        true,
    );
    let ctx = ctx_with_prefix(false, Some(prefix.clone()));
    let layout = FsLayout::system(Some(prefix.clone()));
    let env = anolisa_env::EnvService::detect();
    let resolution = resolve_raw(
        &ctx,
        &layout,
        &env,
        ResolveInputs {
            component: "agentsight".to_string(),
            package: "agentsight".to_string(),
            backend: "raw".to_string(),
            base_url: repo_url,
            repository_origin: None,
            version: None,
            warnings: Vec::new(),
        },
    )
    .expect("resolve");
    let prepared = prepare_raw_execution(&ctx, &layout, resolution).expect("prepare");

    assert_eq!(prepared.services.len(), 1);
    assert_eq!(prepared.services[0].unit, "agentsight.service");
    assert!(prepared.services[0].enable && prepared.services[0].start);
    // Resolve-only: nothing activated or laid.
    assert!(!layout.bin_dir.join("agentsight").exists());
    assert!(!layout.state_dir.join("installed.toml").exists());
}

#[test]
fn install_raw_end_to_end_records_declared_service() {
    let tmp = tempdir().expect("tmpdir");
    let prefix = tmp.path().join("sys");
    let repo_url = write_local_repo_component_with_service(
        &tmp.path().join("repo"),
        "agentsight",
        "0.2.0",
        &["system"],
        "agentsight.service",
        true,
        true,
    );

    let mut a = args("agentsight");
    a.repo = Some(repo_url);
    let effects = crate::test_support::RawEffectRecorder::default();
    effects
        .with(|factories| {
            handle_with_effects(a, &ctx_with_prefix(false, Some(prefix.clone())), factories)
        })
        .expect("install with a declared service must succeed");
    assert_eq!(
        effects.calls(),
        [
            "capability factory system",
            "service factory system System",
            "DaemonReload System ",
            "Enable System agentsight.service",
            "Start System agentsight.service",
        ]
    );

    let layout = FsLayout::system(Some(prefix));
    assert!(
        layout.bin_dir.join("agentsight").exists(),
        "binary installed"
    );
    let store = load_v5_store(&layout);
    let installation = store
        .find(ObjectKind::Component, "agentsight")
        .expect("component recorded");
    let artifact = owned_artifact(installation);
    assert_eq!(artifact.services.len(), 1);
    assert_eq!(artifact.services[0].name, "agentsight.service");
}

#[test]
#[cfg(unix)]
fn install_raw_records_post_hook_bytes_and_detects_later_drift() {
    use anolisa_core::integrity::{IntegrityStatus, check_owned_file};

    for phase in ["post_install", "post_enable"] {
        let tmp = tempdir().expect("tmpdir");
        let prefix = tmp.path().join("sys");
        let layout = FsLayout::system(Some(prefix.clone()));
        let binary = layout.bin_dir.join("agentsight");
        let body = format!("#!/bin/sh\necho '# hook edit' >> '{}'\n", binary.display());
        let repo_url = write_local_repo_component_with_hook(
            &tmp.path().join("repo"),
            "agentsight",
            "0.2.0",
            phase,
            true,
            &body,
        );
        let mut a = args("agentsight");
        a.repo = Some(repo_url);
        handle_with_fake_rpm(a, &ctx_with_prefix(false, Some(prefix)))
            .expect("install with a modifying hook must succeed");

        let bytes = std::fs::read(&binary).expect("installed binary");
        assert!(
            bytes.ends_with(b"# hook edit\n"),
            "{phase} must modify the file"
        );
        let store = load_v5_store(&layout);
        let artifact = owned_artifact(
            store
                .find(ObjectKind::Component, "agentsight")
                .expect("record"),
        );
        for file in &artifact.files {
            assert_eq!(
                check_owned_file(&layout, file),
                IntegrityStatus::Ok,
                "{phase}: {}",
                file.path.display()
            );
        }

        std::fs::write(&binary, b"external edit").expect("modify installed binary");
        let file = artifact
            .files
            .iter()
            .find(|file| file.path == binary)
            .expect("binary record");
        assert!(matches!(
            check_owned_file(&layout, file),
            IntegrityStatus::ShaMismatch { .. }
        ));
    }
}

#[test]
#[cfg(unix)]
fn install_raw_post_hook_digest_failure_rolls_back() {
    for replacement in ["", "ln -s \"$0\"", "mkfifo"] {
        let tmp = tempdir().expect("tmpdir");
        let prefix = tmp.path().join("sys");
        let layout = FsLayout::system(Some(prefix.clone()));
        let binary = layout.bin_dir.join("agentsight");
        let replace = if replacement.is_empty() {
            String::new()
        } else {
            format!("{replacement} '{}'\n", binary.display())
        };
        let body = format!("#!/bin/sh\nset -e\nrm '{}'\n{replace}", binary.display());
        let repo_url = write_local_repo_component_with_hook(
            &tmp.path().join("repo"),
            "agentsight",
            "0.2.0",
            "post_install",
            true,
            &body,
        );
        let mut a = args("agentsight");
        a.repo = Some(repo_url);
        let err = handle_with_fake_rpm(a, &ctx_with_prefix(false, Some(prefix)))
            .expect_err("a removed or non-regular payload must abort record commit");
        assert!(
            err.reason().contains("failed to record post-hook digest"),
            "{err}"
        );
        assert!(
            std::fs::symlink_metadata(&binary).is_err(),
            "rollback removes the replacement"
        );
        let snapshot = common::installed_component_manifest_path(&layout, "agentsight", COMMAND)
            .expect("manifest path");
        assert!(!snapshot.exists(), "rollback removes the manifest snapshot");
        assert!(
            load_v5_store(&layout)
                .find(ObjectKind::Component, "agentsight")
                .is_none()
        );
    }
}

#[test]
#[cfg(unix)]
fn install_raw_strict_post_install_failure_rolls_back() {
    let tmp = tempdir().expect("tmpdir");
    let prefix = tmp.path().join("sys");
    let repo_url = write_local_repo_component_with_hook(
        &tmp.path().join("repo"),
        "agentsight",
        "0.2.0",
        "post_install",
        true,
        "#!/bin/sh\nexit 1\n",
    );

    let mut a = args("agentsight");
    a.repo = Some(repo_url);
    let err = handle_with_fake_rpm(a, &ctx_with_prefix(false, Some(prefix.clone())))
        .expect_err("strict post_install failure must abort the install");
    assert!(matches!(err, CliError::Runtime { .. }));

    let layout = FsLayout::system(Some(prefix));
    assert!(
        !layout.bin_dir.join("agentsight").exists(),
        "installed files must be rolled back after a strict hook failure"
    );
    let snapshot = common::installed_component_manifest_path(&layout, "agentsight", COMMAND)
        .expect("manifest path");
    assert!(
        !snapshot.exists(),
        "installed manifest snapshot must be rolled back"
    );
    let state_path = layout.state_dir.join("installed.toml");
    if state_path.exists() {
        let store = load_v5_store(&layout);
        assert!(
            store.find(ObjectKind::Component, "agentsight").is_none(),
            "component must not be recorded after rollback"
        );
    }
}

#[test]
#[cfg(unix)]
fn install_raw_pre_install_hook_skipped_as_missing_on_fresh_install() {
    let tmp = tempdir().expect("tmpdir");
    let prefix = tmp.path().join("sys");
    let sentinel = tmp.path().join("pre-install.ran");
    let body = format!("#!/bin/sh\ntouch {}\n", sentinel.display());
    let repo_url = write_local_repo_component_with_hook(
        &tmp.path().join("repo"),
        "agentsight",
        "0.2.0",
        "pre_install",
        false,
        &body,
    );

    let mut a = args("agentsight");
    a.repo = Some(repo_url);
    handle_with_fake_rpm(a, &ctx_with_prefix(false, Some(prefix.clone())))
        .expect("install must succeed; pre_install script is not yet laid");

    let layout = FsLayout::system(Some(prefix));
    assert!(
        layout.bin_dir.join("agentsight").exists(),
        "binary installed"
    );
    assert!(
        !sentinel.exists(),
        "pre_install must skip when its script is not yet on disk"
    );
}

#[test]
fn install_raw_uses_embedded_manifest_without_local_catalog() {
    let tmp = tempdir().expect("tmpdir");
    let prefix = tmp.path().join("sys");
    let repo_url = write_local_repo_component(
        &tmp.path().join("repo"),
        "remote-only",
        "1.0.0",
        &["system"],
    );

    let mut a = args("remote-only");
    a.repo = Some(repo_url);
    handle_with_fake_rpm(a, &ctx_with_prefix(false, Some(prefix.clone())))
        .expect("install must succeed");

    let layout = FsLayout::system(Some(prefix));
    assert!(
        layout.bin_dir.join("remote-only").exists(),
        "component absent from local manifests must install from embedded artifact contract"
    );
    let store = load_v5_store(&layout);
    assert!(
        store.find(ObjectKind::Component, "remote-only").is_some(),
        "remote-only component must be recorded"
    );
}

#[test]
fn install_existing_component_with_different_backend_is_invalid_argument() {
    let tmp = tempdir().expect("tmpdir");
    let prefix = tmp.path().join("sys");
    let layout = FsLayout::system(Some(prefix.clone()));
    std::fs::create_dir_all(&layout.etc_dir).expect("etc dir");
    std::fs::create_dir_all(&layout.state_dir).expect("state dir");
    std::fs::write(
        layout.etc_dir.join("repo.toml"),
        r#"schema_version = 1
default_backend = "raw"

[backends.raw]
base_url = "https://example.com/anolisa"

[backends.npm]
base_url = "https://registry.npmjs.org"
scope = "@anolisa"
"#,
    )
    .expect("write repo.toml");

    let mut state = anolisa_core::InstalledState {
        install_mode: StateInstallMode::System,
        prefix: layout.prefix.clone(),
        ..Default::default()
    };
    state.objects.push(InstalledObject {
        kind: ObjectKind::Component,
        name: "agentsight".to_string(),
        version: "0.2.0".to_string(),
        status: ObjectStatus::Installed,
        manifest_digest: None,
        distribution_source: Some("file:///repo/v1/agentsight-bin".to_string()),
        raw_package: None,
        install_backend: Some("raw".to_string()),
        ownership: None,
        rpm_metadata: None,
        installed_at: "2026-06-01T10:00:00Z".to_string(),
        last_operation_id: Some("op-prior".to_string()),
        managed: true,
        adopted: false,
        subscription_scope: Default::default(),
        enabled_features: Vec::new(),
        component_refs: Vec::new(),
        files: Vec::new(),
        external_modified_files: Vec::new(),
        services: Vec::new(),
        health: Vec::new(),
        provisioned_packages: Vec::new(),
    });
    crate::test_support::write_legacy_state(&state, &layout.state_dir.join("installed.toml"))
        .expect("save state");

    let mut a = args("agentsight");
    a.backend = Some("npm".to_string());
    let err = handle(a, &ctx_with_prefix(false, Some(prefix))).expect_err("must error");

    assert_eq!(err.code(), "INVALID_ARGUMENT");
    assert!(
        err.reason().contains("already installed via backend 'raw'")
            && err.reason().contains("backend 'npm'"),
        "reason must explain backend conflict: {}",
        err.reason()
    );
}

#[test]
fn install_derives_artifact_url_from_convention_when_index_omits_url() {
    let tmp = tempdir().expect("tmpdir");
    let prefix = tmp.path().join("sys");
    let repo_url = write_conventional_repo(&tmp.path().join("repo"));

    let mut a = args("agentsight");
    a.repo = Some(repo_url.clone());
    handle_with_fake_rpm(a, &ctx_with_prefix(false, Some(prefix.clone())))
        .expect("install must succeed");

    let layout = FsLayout::system(Some(prefix));
    assert!(layout.bin_dir.join("agentsight").exists());

    let store = load_v5_store(&layout);
    let installation = store
        .find(ObjectKind::Component, "agentsight")
        .expect("component object must be recorded");
    let env = anolisa_env::EnvService::detect();
    assert_eq!(
        owned_artifact(installation).distribution_source.as_deref(),
        Some(
            format!(
                "{repo_url}/agentsight/0.2.0/{os}/{arch}/agentsight-0.2.0-{os}-{arch}.tar.gz",
                os = env.os,
                arch = env.arch
            )
            .as_str()
        ),
        "distribution_source must record the convention-derived URL"
    );
}

#[test]
fn install_resolves_legacy_template_form_repo_url() {
    let tmp = tempdir().expect("tmpdir");
    let prefix = tmp.path().join("sys");
    let repo_root = tmp.path().join("repo");
    // write_conventional_repo puts the tree under <root>/v1/; point the
    // template's static prefix at that same directory.
    let _ = write_conventional_repo(&repo_root);
    let template_url = format!(
        "file://{}/v1/{{component}}/{{version}}/{{os}}/{{arch}}/",
        repo_root.display()
    );

    let mut a = args("agentsight");
    a.repo = Some(template_url);
    handle_with_fake_rpm(a, &ctx_with_prefix(false, Some(prefix.clone())))
        .expect("install must succeed");

    let layout = FsLayout::system(Some(prefix));
    assert!(layout.bin_dir.join("agentsight").exists());

    let store = load_v5_store(&layout);
    let installation = store
        .find(ObjectKind::Component, "agentsight")
        .expect("component object must be recorded");
    let env = anolisa_env::EnvService::detect();
    assert_eq!(
        owned_artifact(installation).distribution_source.as_deref(),
        Some(
            format!(
                "file://{}/v1/agentsight/0.2.0/{os}/{arch}/agentsight-0.2.0-{os}-{arch}.tar.gz",
                repo_root.display(),
                os = env.os,
                arch = env.arch
            )
            .as_str()
        ),
        "distribution_source must record the convention-derived URL"
    );
}

#[test]
fn install_unpublished_version_is_invalid_argument() {
    let tmp = tempdir().expect("tmpdir");
    let prefix = tmp.path().join("sys");
    // Two published versions, so the refusal must enumerate the complete
    // list (highest-first), not just whichever entry resolution saw last.
    let repo_url = write_local_repo_component_versions(
        &tmp.path().join("repo"),
        "agentsight",
        &["0.1.0", "0.2.0"],
        &["system"],
    );

    let mut a = args("agentsight");
    a.repo = Some(repo_url);
    a.version = Some("9.9.9".to_string());
    let err = handle_with_fake_rpm(a, &ctx_with_prefix(false, Some(prefix)))
        .expect_err("must fail to resolve");
    assert_eq!(err.code(), "INVALID_ARGUMENT");
    assert!(err.reason().contains("9.9.9"), "got: {}", err.reason());
    // The pin refusal must name what the repository actually publishes, so
    // the caller can correct the request without a second query.
    assert!(
        err.reason().contains("not published"),
        "pin refusal must be a dedicated message: {}",
        err.reason()
    );
    assert!(
        err.reason().contains("installable versions: 0.2.0, 0.1.0"),
        "pin refusal must list every installable version, highest first: {}",
        err.reason()
    );
}

#[test]
fn install_pinned_unsupported_artifact_type_is_reported_as_not_installable() {
    let tmp = tempdir().expect("tmpdir");
    let prefix = tmp.path().join("sys");
    let repo_root = tmp.path().join("repo");
    let repo_url = write_local_repo(&repo_root);
    // Publish 9.0.0 as a binary-only entry: present in the repository, but
    // outside what the raw backend can install.
    let index_path = repo_root.join("v1/index.toml");
    let mut index = std::fs::read_to_string(&index_path).expect("read index");
    let env = anolisa_env::EnvService::detect();
    index.push_str(&format!(
        r#"
[[entries]]
component = "agentsight"
version = "9.0.0"
channel = "stable"
artifact_type = "binary"
backend = "raw"
url = "legacy-agentsight-9.0.0"
os = "{os}"
arch = "{arch}"
install_modes = ["system"]
sha256 = "{sha}"
"#,
        os = env.os,
        arch = env.arch,
        sha = "0".repeat(64),
    ));
    std::fs::write(index_path, index).expect("write mixed index");

    let mut a = args("agentsight");
    a.repo = Some(repo_url);
    a.version = Some("9.0.0".to_string());
    let err = handle_with_fake_rpm(a, &ctx_with_prefix(false, Some(prefix)))
        .expect_err("must fail to resolve");
    assert_eq!(err.code(), "INVALID_ARGUMENT");
    // A published-but-binary-only pin must not be misreported as
    // unpublished; the refusal names the artifact-type gap instead.
    assert!(
        !err.reason().contains("not published"),
        "published version must not be reported as unpublished: {}",
        err.reason()
    );
    assert!(
        err.reason()
            .contains("artifact types the raw backend cannot install"),
        "refusal must name the artifact-type gap: {}",
        err.reason()
    );
    assert!(
        err.reason().contains("installable versions: 0.2.0"),
        "refusal must still list installable versions: {}",
        err.reason()
    );
}

/// Overwrite the fixture repo's `v1/index.toml` with exactly these rows,
/// leaving the published component index as the fixture wrote it: identity
/// resolution keeps accepting the component while the distribution resolver
/// sees only what this wrote. Rows are
/// `(component, version, os, arch, pkg_base)`, where a `None` `pkg_base` omits
/// the selector. Artifact bytes and checksums stay placeholders — every caller
/// fails resolution, so nothing is fetched.
fn rewrite_index_rows(repo_root: &Path, rows: &[(&str, &str, &str, &str, Option<&str>)]) {
    let mut index =
        String::from("schema_version = 1\nchannel = \"stable\"\npublisher = \"test\"\n");
    for (component, version, os, arch, pkg_base) in rows {
        let pkg_base_line = match pkg_base {
            Some(base) => format!("pkg_base = \"{base}\"\n"),
            None => String::new(),
        };
        index.push_str(&format!(
            "\n[[entries]]\ncomponent = \"{component}\"\nversion = \"{version}\"\nchannel = \"stable\"\nartifact_type = \"tar_gz\"\nbackend = \"raw\"\nurl = \"{component}-{version}-{os}-{arch}.tar.gz\"\nos = \"{os}\"\narch = \"{arch}\"\n{pkg_base_line}install_modes = [\"system\"]\nsha256 = \"{sha}\"\n",
            sha = "0".repeat(64),
        ));
    }
    std::fs::write(repo_root.join("v1/index.toml"), index).expect("write index");
}

/// A component the repository publishes but not for this host is a platform
/// gap, not an unknown name. The refusal must name the requested platform and
/// list every platform the repository does carry — deduplicated, in a stable
/// order, and identical through the `--json` envelope.
#[test]
fn install_platform_mismatch_lists_published_platforms() {
    let tmp = tempdir().expect("tmpdir");
    let prefix = tmp.path().join("sys");
    let repo_root = tmp.path().join("repo");
    let repo_url =
        write_local_repo_component_versions(&repo_root, "agentsight", &["0.2.0"], &["system"]);
    // Neither architecture is one ANOLISA runs on, so the rows stay foreign on
    // every supported host. ppc64le is published twice (one row per version) to
    // pin the dedup, and s390x is listed first to pin the sort.
    rewrite_index_rows(
        &repo_root,
        &[
            ("agentsight", "0.3.0", "linux", "s390x", None),
            ("agentsight", "0.2.0", "linux", "ppc64le", None),
            ("agentsight", "0.1.0", "linux", "ppc64le", None),
        ],
    );
    let env = anolisa_env::EnvService::detect();

    let mut a = args("agentsight");
    a.repo = Some(repo_url);
    let err = handle_with_fake_rpm(a, &ctx_with_prefix(false, Some(prefix)))
        .expect_err("a host the repository publishes nothing for must fail");
    let reason = err.reason();
    assert_eq!(err.code(), "INVALID_ARGUMENT");
    assert_eq!(err.exit_code(), 2);
    assert!(
        reason.contains(&format!("is not available for {}/{}", env.os, env.arch)),
        "mismatch must name the requested platform: {reason}"
    );
    assert!(
        reason.contains("available platforms:\n  - linux/ppc64le\n  - linux/s390x"),
        "mismatch must list every published platform, deduplicated and sorted: {reason}"
    );
    assert_eq!(
        reason.matches("linux/ppc64le").count(),
        1,
        "one line per platform, not one per published version: {reason}"
    );
    assert!(
        !reason.contains("no distribution entry matches the query"),
        "mismatch must replace the generic resolver rendering: {reason}"
    );

    // Run this test executable as an isolated child so the production
    // renderer's stdout can be asserted without replacing process-global
    // output in-process.
    let child_module = module_path!()
        .split_once("::")
        .map_or(module_path!(), |(_, module)| module);
    let child_test = format!("{child_module}::render_platform_mismatch_error_json_child");
    let output = std::process::Command::new(std::env::current_exe().expect("current test binary"))
        .args([child_test.as_str(), "--exact", "--ignored", "--nocapture"])
        .env(PLATFORM_JSON_REASON_ENV, &reason)
        .output()
        .expect("run isolated JSON renderer child");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "JSON renderer child failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    let (_, after_begin) = stdout
        .split_once(JSON_BEGIN)
        .expect("child stdout must contain the JSON start marker");
    let (json, _) = after_begin
        .split_once(JSON_END)
        .expect("child stdout must contain the JSON end marker");
    let parsed: serde_json::Value =
        serde_json::from_str(json.trim()).expect("rendered error must be valid JSON");
    assert_eq!(parsed["ok"], false);
    assert_eq!(parsed["command"], "install agentsight");
    assert_eq!(parsed["error"]["code"], "INVALID_ARGUMENT");
    assert_eq!(parsed["error"]["reason"], reason);
}

/// An unknown package keeps the generic not-found rendering: reporting a
/// platform gap for a name the index never carried would send the operator to
/// a different build host instead of a different component name.
#[test]
fn install_unknown_package_keeps_generic_not_found_diagnostic() {
    let tmp = tempdir().expect("tmpdir");
    let prefix = tmp.path().join("sys");
    let repo_root = tmp.path().join("repo");
    let repo_url =
        write_local_repo_component_versions(&repo_root, "agentsight", &["0.2.0"], &["system"]);
    let env = anolisa_env::EnvService::detect();
    // The repository is not empty — it publishes this host, for another
    // package — so the refusal has to come from the name and not the platform.
    rewrite_index_rows(
        &repo_root,
        &[(
            "tokenless",
            "0.2.0",
            env.os.as_str(),
            env.arch.as_str(),
            None,
        )],
    );

    let mut a = args("agentsight");
    a.repo = Some(repo_url);
    let err = handle_with_fake_rpm(a, &ctx_with_prefix(false, Some(prefix)))
        .expect_err("an unpublished package must fail");
    let reason = err.reason();
    assert_eq!(err.code(), "INVALID_ARGUMENT");
    assert!(
        reason.contains("cannot resolve package 'agentsight'"),
        "unknown package must keep the generic rendering: {reason}"
    );
    assert!(
        reason.contains("no distribution entry matches the query"),
        "unknown package must keep the resolver's own wording: {reason}"
    );
    assert!(
        !reason.contains("available platforms"),
        "unknown package must not be reported as a platform gap: {reason}"
    );
}

/// A platform the repository does publish for is not a platform gap. Here a
/// selector the resolver applies after os/arch (`pkg_base`) causes the
/// refusal, and it must keep the generic rendering rather than be
/// misattributed to the build host.
#[test]
fn install_host_platform_published_keeps_generic_not_found_diagnostic() {
    let tmp = tempdir().expect("tmpdir");
    let prefix = tmp.path().join("sys");
    let repo_root = tmp.path().join("repo");
    let repo_url =
        write_local_repo_component_versions(&repo_root, "agentsight", &["0.2.0"], &["system"]);
    let env = anolisa_env::EnvService::detect();
    rewrite_index_rows(
        &repo_root,
        &[(
            "agentsight",
            "0.2.0",
            env.os.as_str(),
            env.arch.as_str(),
            Some("zzz-no-such-package-base"),
        )],
    );

    let mut a = args("agentsight");
    a.repo = Some(repo_url);
    let err = handle_with_fake_rpm(a, &ctx_with_prefix(false, Some(prefix)))
        .expect_err("an unsatisfiable package base must fail");
    let reason = err.reason();
    assert_eq!(err.code(), "INVALID_ARGUMENT");
    assert!(
        reason.contains("cannot resolve package 'agentsight'"),
        "a later-filter refusal must keep the generic rendering: {reason}"
    );
    assert!(
        !reason.contains("available platforms"),
        "a published platform must not be reported as a platform gap: {reason}"
    );
}

/// A version pin does not outrank the platform gap. With no row for this host
/// at all, the pin-specific wording would report the version as unpublished
/// for this platform and hide that the repository publishes nothing here.
#[test]
fn install_pinned_version_on_unpublished_platform_lists_platforms() {
    let tmp = tempdir().expect("tmpdir");
    let prefix = tmp.path().join("sys");
    let repo_root = tmp.path().join("repo");
    let repo_url =
        write_local_repo_component_versions(&repo_root, "agentsight", &["0.2.0"], &["system"]);
    rewrite_index_rows(
        &repo_root,
        &[
            ("agentsight", "0.2.0", "linux", "s390x", None),
            ("agentsight", "0.1.0", "linux", "ppc64le", None),
        ],
    );
    let env = anolisa_env::EnvService::detect();

    let mut a = args("agentsight");
    a.repo = Some(repo_url);
    a.version = Some("0.2.0".to_string());
    let err = handle_with_fake_rpm(a, &ctx_with_prefix(false, Some(prefix)))
        .expect_err("a pinned version on an unpublished platform must fail");
    let reason = err.reason();
    assert_eq!(err.code(), "INVALID_ARGUMENT");
    assert!(
        reason.contains(&format!("is not available for {}/{}", env.os, env.arch)),
        "the platform gap must win over the pin wording: {reason}"
    );
    assert!(
        reason.contains("available platforms:\n  - linux/ppc64le\n  - linux/s390x"),
        "the pin refusal must still list the published platforms: {reason}"
    );
    assert!(
        !reason.contains("not published"),
        "the pin wording would hide the platform gap: {reason}"
    );
}

#[test]
fn install_pinned_version_places_exact_published_version() {
    let tmp = tempdir().expect("tmpdir");
    let prefix = tmp.path().join("sys");
    let repo_url = write_local_repo_component_versions(
        &tmp.path().join("repo"),
        "agentsight",
        &["0.1.0", "0.2.0"],
        &["system"],
    );

    let mut a = args("agentsight");
    a.repo = Some(repo_url);
    a.version = Some("0.1.0".to_string());
    handle_with_fake_rpm(a, &ctx_with_prefix(false, Some(prefix.clone())))
        .expect("pinned install must succeed");

    let layout = FsLayout::system(Some(prefix));
    assert!(layout.bin_dir.join("agentsight").exists());
    let store = load_v5_store(&layout);
    let installation = store
        .find(ObjectKind::Component, "agentsight")
        .expect("installed object");
    let artifact = owned_artifact(installation);
    assert_eq!(
        artifact.version, "0.1.0",
        "the pin must beat the higher published 0.2.0"
    );
    assert!(
        artifact
            .distribution_source
            .as_deref()
            .is_some_and(|url| url.ends_with("agentsight-0.1.0.tar.gz")),
        "distribution_source must record the pinned artifact URL: {:?}",
        artifact.distribution_source
    );
}

#[test]
fn install_pinned_version_dry_run_previews_without_writing_files() {
    let tmp = tempdir().expect("tmpdir");
    let prefix = tmp.path().join("sys");
    let repo_url = write_local_repo_component_versions(
        &tmp.path().join("repo"),
        "agentsight",
        &["0.1.0", "0.2.0"],
        &["system"],
    );

    let mut a = args("agentsight");
    a.repo = Some(repo_url);
    a.version = Some("0.1.0".to_string());
    let mut ctx = ctx_with_prefix(false, Some(prefix.clone()));
    ctx.dry_run = true;
    handle_with_fake_rpm(a, &ctx).expect("pinned dry-run must succeed");

    let layout = FsLayout::system(Some(prefix));
    assert!(
        !layout.bin_dir.join("agentsight").exists(),
        "dry-run must not install the binary"
    );
    assert!(
        !layout.state_dir.join("installed.toml").exists(),
        "dry-run must not write state"
    );
}

// ---------------------------------------------------------------------------
// Component-level mutual-exclusion (conflicts) tests
// ---------------------------------------------------------------------------

/// Build a local file:// repo with a single component whose manifest declares
/// `conflicts = [...]`. Returns the repo URL.
fn write_local_repo_with_conflicts(
    root: &std::path::Path,
    component: &str,
    version: &str,
    modes: &[&str],
    conflicts: &[&str],
) -> String {
    let v1 = root.join("v1");
    std::fs::create_dir_all(&v1).expect("create repo dirs");
    write_component_index_v2(&v1, &[component]);

    let manifest = component_manifest_toml_with_conflicts(component, version, modes, conflicts);
    let manifest_sha = format!("{:x}", Sha256::digest(manifest.as_bytes()));
    std::fs::write(v1.join("meta.toml"), &manifest).expect("write sidecar manifest");
    let bin_path = format!("bin/{component}");
    let payload = format!("#!/bin/sh\necho {component}\n");
    let artifact = build_tar_gz(&[
        (".anolisa/component.toml", manifest.as_bytes()),
        (bin_path.as_str(), payload.as_bytes()),
    ]);
    let artifact_name = format!("{component}.tar.gz");
    std::fs::write(v1.join(&artifact_name), &artifact).expect("write artifact");
    let sha = format!("{:x}", Sha256::digest(&artifact));
    let modes_str = toml_string_array(modes);

    let env = anolisa_env::EnvService::detect();
    let index = format!(
        r#"schema_version = 1
channel = "stable"
publisher = "test"

[[entries]]
component = "{component}"
version = "{version}"
channel = "stable"
artifact_type = "tar_gz"
backend = "raw"
url = "{artifact_name}"
os = "{os}"
arch = "{arch}"
install_modes = {modes_str}
sha256 = "{sha}"
manifest_digest = "sha256:{manifest_sha}"
"#,
        os = env.os,
        arch = env.arch,
    );
    std::fs::write(v1.join("index.toml"), index).expect("write index");
    format!("file://{}", v1.display())
}

fn write_published_batch_conflict_repo(root: &Path) -> String {
    let v1 = root.join("v1");
    std::fs::create_dir_all(&v1).expect("create repo dirs");
    let env = anolisa_env::EnvService::detect();
    let mut index =
        String::from("schema_version = 1\nchannel = \"stable\"\npublisher = \"test\"\n");

    for (component, conflicts) in [("cosh", &[][..]), ("cosh-ng", &["cosh"][..])] {
        let version = "1.0.0";
        let version_dir = v1.join(component).join(version);
        let artifact_dir = version_dir.join(&env.os).join(&env.arch);
        std::fs::create_dir_all(&artifact_dir).expect("create artifact dirs");

        let manifest =
            component_manifest_toml_with_conflicts(component, version, &["user"], conflicts);
        std::fs::write(version_dir.join("meta.toml"), &manifest).expect("write sidecar manifest");
        let manifest_sha = format!("{:x}", Sha256::digest(manifest.as_bytes()));
        let bin_path = format!("bin/{component}");
        let payload = format!("#!/bin/sh\necho {component}\n");
        let artifact = build_tar_gz(&[
            (".anolisa/component.toml", manifest.as_bytes()),
            (bin_path.as_str(), payload.as_bytes()),
        ]);
        let artifact_name = format!(
            "{component}-{version}-{os}-{arch}.tar.gz",
            os = env.os,
            arch = env.arch
        );
        std::fs::write(artifact_dir.join(&artifact_name), &artifact).expect("write artifact");
        let artifact_sha = format!("{:x}", Sha256::digest(&artifact));
        let url = format!(
            "{component}/{version}/{os}/{arch}/{artifact_name}",
            os = env.os,
            arch = env.arch
        );
        index.push_str(&format!(
            r#"
[[entries]]
component = "{component}"
version = "{version}"
channel = "stable"
artifact_type = "tar_gz"
backend = "raw"
url = "{url}"
os = "{os}"
arch = "{arch}"
install_modes = ["user"]
sha256 = "{artifact_sha}"
manifest_digest = "sha256:{manifest_sha}"
"#,
            os = env.os,
            arch = env.arch,
        ));
    }
    std::fs::write(v1.join("index.toml"), index).expect("write distribution index");
    std::fs::write(
        v1.join("components-v2.toml"),
        format!(
            r#"schema_version = 2

[[components]]
name = "cosh"
targets = [{{ os = "{os}", arch = "{arch}" }}]

[[components.backends]]
kind = "raw"
package = "cosh"

[[components]]
name = "cosh-ng"
targets = [{{ os = "{os}", arch = "{arch}" }}]

[[components.backends]]
kind = "raw"
package = "cosh-ng"
"#,
            os = env.os,
            arch = env.arch,
        ),
    )
    .expect("write component index");
    format!("file://{}", v1.display())
}

fn seed_installed_component(layout: &FsLayout, component: &str, version: &str) {
    std::fs::create_dir_all(&layout.state_dir).expect("create state dir");
    let mut state = anolisa_core::InstalledState {
        install_mode: StateInstallMode::System,
        prefix: layout.prefix.clone(),
        ..Default::default()
    };
    state.objects.push(InstalledObject {
        kind: ObjectKind::Component,
        name: component.to_string(),
        version: version.to_string(),
        status: ObjectStatus::Installed,
        manifest_digest: None,
        distribution_source: Some(format!("file:///repo/v1/{component}.tar.gz")),
        raw_package: Some(component.to_string()),
        install_backend: Some("raw".to_string()),
        ownership: None,
        rpm_metadata: None,
        installed_at: "2026-06-01T10:00:00Z".to_string(),
        last_operation_id: Some("op-prior".to_string()),
        managed: true,
        adopted: false,
        subscription_scope: Default::default(),
        enabled_features: Vec::new(),
        component_refs: Vec::new(),
        files: Vec::new(),
        external_modified_files: Vec::new(),
        services: Vec::new(),
        health: Vec::new(),
        provisioned_packages: Vec::new(),
    });
    crate::test_support::write_legacy_state(&state, &layout.state_dir.join("installed.toml"))
        .expect("save state");
}

#[test]
fn install_conflict_blocks_when_conflicting_component_is_installed() {
    let tmp = tempdir().expect("tmpdir");
    let prefix = tmp.path().join("sys");
    let layout = FsLayout::system(Some(prefix.clone()));

    seed_installed_component(&layout, "cosh", "2.6.0");

    // Write a repo with cosh-ng that declares conflicts = ["cosh"].
    let repo_url = write_local_repo_with_conflicts(
        &tmp.path().join("repo"),
        "cosh-ng",
        "0.11.0",
        &["system"],
        &["cosh"],
    );

    let mut a = args("cosh-ng");
    a.repo = Some(repo_url);
    let err = handle_with_fake_rpm(a, &ctx_with_prefix(false, Some(prefix)))
        .expect_err("install must fail due to conflict");

    assert_eq!(err.code(), "INVALID_ARGUMENT");
    assert!(
        err.reason()
            .contains("conflicts with installed component 'cosh'"),
        "error must identify the conflicting component: {}",
        err.reason()
    );
    assert!(
        err.reason().contains("v2.6.0"),
        "error must show the installed version: {}",
        err.reason()
    );
    assert!(
        err.reason().contains("uninstall 'cosh' first"),
        "error must provide remediation: {}",
        err.reason()
    );
}

#[test]
fn install_dry_run_reports_conflict_without_downloading_artifact() {
    let tmp = tempdir().expect("tmpdir");
    let prefix = tmp.path().join("sys");
    let layout = FsLayout::system(Some(prefix.clone()));
    seed_installed_component(&layout, "cosh", "2.6.0");
    let repo_url = write_local_repo_with_conflicts(
        &tmp.path().join("repo"),
        "cosh-ng",
        "0.11.0",
        &["system"],
        &["cosh"],
    );

    let mut a = args("cosh-ng");
    a.repo = Some(repo_url);
    let mut ctx = ctx_with_prefix(false, Some(prefix));
    ctx.dry_run = true;
    let err = handle_with_fake_rpm(a, &ctx).expect_err("dry-run must report the conflict");

    assert_eq!(err.code(), "INVALID_ARGUMENT");
    assert!(
        err.reason()
            .contains("conflicts with installed component 'cosh'"),
        "got: {}",
        err.reason()
    );
    let cached_names: Vec<String> = std::fs::read_dir(layout.cache_dir.join("downloads"))
        .expect("downloads cache exists")
        .map(|entry| {
            entry
                .expect("cache entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert!(
        cached_names.iter().any(|name| name.ends_with("meta.toml")),
        "dry-run must fetch the lightweight contract; cache entries: {cached_names:?}"
    );
    assert!(
        cached_names
            .iter()
            .all(|name| !name.ends_with("cosh-ng.tar.gz")),
        "dry-run must not fetch the install artifact; cache entries: {cached_names:?}"
    );
}

#[test]
fn install_all_dry_run_rejects_conflicts_between_planned_components() {
    let sandbox = TestSandbox::new();
    let repo_url = write_published_batch_conflict_repo(sandbox.repo_root());
    let ctx = sandbox.context_with(
        InstallMode::User,
        TestContextOptions {
            dry_run: true,
            ..Default::default()
        },
    );
    let layout = common::resolve_layout(&ctx);
    std::fs::create_dir_all(&layout.etc_dir).expect("create config dir");
    std::fs::write(
        layout.etc_dir.join("repo.toml"),
        format!(
            "schema_version = 1\ndefault_backend = \"raw\"\n\n[backends.raw]\nbase_url = \"{repo_url}\"\n"
        ),
    )
    .expect("write repo config");
    let batch_args = InstallArgs {
        component: None,
        all: true,
        fail_fast: false,
        version: None,
        backend: Some("raw".to_string()),
        repo: None,
        package: None,
    };

    let err = handle_all(batch_args, &ctx)
        .expect_err("the second component must conflict with the first planned component");

    assert!(matches!(err, CliError::BatchPartial { .. }), "got: {err}");
    assert!(
        !layout.state_dir.join("installed.toml").exists(),
        "dry-run must not persist simulated batch state"
    );
    let cached_names: Vec<String> = std::fs::read_dir(layout.cache_dir.join("downloads"))
        .expect("downloads cache exists")
        .map(|entry| {
            entry
                .expect("cache entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert_eq!(
        cached_names
            .iter()
            .filter(|name| name.ends_with("meta.toml"))
            .count(),
        2,
        "both batch contracts must be checked; cache entries: {cached_names:?}"
    );
    assert!(
        cached_names.iter().all(|name| !name.ends_with(".tar.gz")),
        "dry-run must not fetch batch artifacts; cache entries: {cached_names:?}"
    );
}

/// `install --all --repo <URL>` enumerates the override repository's
/// component index — the same authority identity and package resolution
/// consult — instead of the repo.toml chain, so an override-only component
/// installs while the default index plays no part (issue #2630 review
/// follow-up). Runs in user mode like the batch conflict test above, so the
/// pipeline never consults the host's real rpm tooling.
#[test]
fn install_all_with_repo_override_enumerates_the_override_index() {
    let sandbox = TestSandbox::new();
    // repo.toml points at a repository whose index lists nothing.
    let default_repo = write_empty_repo(&sandbox.root().join("default-repo"));
    let ctx = sandbox.context_with(InstallMode::User, TestContextOptions::default());
    let layout = common::resolve_layout(&ctx);
    std::fs::create_dir_all(&layout.etc_dir).expect("create config dir");
    std::fs::write(
        layout.etc_dir.join("repo.toml"),
        format!(
            "schema_version = 1\ndefault_backend = \"raw\"\n\n[backends.raw]\nbase_url = \"{default_repo}\"\n"
        ),
    )
    .expect("write repo config");
    // The override repository publishes agentsight with its meta sidecar. The
    // batch enumeration only lists entries with a matching backend row, so
    // the override index declares the raw backend explicitly.
    let override_root = sandbox.root().join("override");
    let override_url =
        write_published_layout_repo_with_meta(&override_root, "agentsight", "0.2.0", &["user"]);
    let env = anolisa_env::EnvService::detect();
    std::fs::write(
        override_root.join("v1/components-v2.toml"),
        format!(
            r#"schema_version = 2

[[components]]
name = "agentsight"
targets = [{{ os = "{os}", arch = "{arch}" }}]

[[components.backends]]
kind = "raw"
package = "agentsight"
"#,
            os = env.os,
            arch = env.arch,
        ),
    )
    .expect("override component index");

    let batch_args = InstallArgs {
        component: None,
        all: true,
        fail_fast: false,
        version: None,
        backend: Some("raw".to_string()),
        repo: Some(override_url),
        package: None,
    };
    handle_all(batch_args, &ctx)
        .expect("the override-only component must be enumerated and installed");

    assert!(
        layout.bin_dir.join("agentsight").exists(),
        "the override-only component must be installed"
    );
    let store = load_v5_store(&layout);
    assert!(
        store.find(ObjectKind::Component, "agentsight").is_some(),
        "the override-only component must be recorded"
    );
}

#[test]
fn install_dry_run_rejects_mismatched_sidecar_digest() {
    let tmp = tempdir().expect("tmpdir");
    let repo_root = tmp.path().join("repo");
    let repo_url =
        write_local_repo_with_conflicts(&repo_root, "cosh-ng", "0.11.0", &["system"], &["cosh"]);
    std::fs::write(
        repo_root.join("v1/meta.toml"),
        component_manifest_toml("cosh-ng", "0.11.0", &["system"]),
    )
    .expect("replace sidecar manifest");

    let mut a = args("cosh-ng");
    a.repo = Some(repo_url);
    let prefix = tmp.path().join("sys");
    let layout = FsLayout::system(Some(prefix.clone()));
    let mut ctx = ctx_with_prefix(false, Some(prefix));
    ctx.dry_run = true;
    let err = handle_with_fake_rpm(a, &ctx).expect_err("digest mismatch must fail dry-run");

    assert_eq!(err.code(), "EXECUTION_FAILED");
    assert!(err.reason().contains("sha256 mismatch"), "got: {err}");
    let cached_names: Vec<String> = std::fs::read_dir(layout.cache_dir.join("downloads"))
        .expect("downloads cache exists")
        .map(|entry| {
            entry
                .expect("cache entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert!(
        cached_names
            .iter()
            .all(|name| !name.ends_with("cosh-ng.tar.gz")),
        "digest failure must not fetch the install artifact; cache entries: {cached_names:?}"
    );
}

// ---------------------------------------------------------------------------
// Target-specific sidecar metadata
// ---------------------------------------------------------------------------

/// Local `file://` repo publishing one component for two targets, each with
/// its own install contract — the shape a raw repository takes once a
/// component's payload and lifecycle differ per platform.
///
/// The Linux build is system-only and its `meta.toml` sits at the version
/// root (`<component>/<version>/meta.toml`); the macOS build also supports
/// user mode and publishes `meta.toml` beside its own artifact. Each index
/// entry carries the `manifest_digest` of *its own* contract.
///
/// `macos_sidecar = false` drops the macOS sidecar, leaving the legacy shape:
/// one version-level contract covering every target. `digests = false` drops
/// `manifest_digest` from every entry, the shape of a repository published
/// before the field existed.
fn write_local_repo_target_specific_meta(
    root: &Path,
    macos_sidecar: bool,
    digests: bool,
) -> String {
    const COMPONENT: &str = "agentsight";
    const VERSION: &str = "0.10.1";

    let v1 = root.join("v1");
    let version_dir = v1.join(COMPONENT).join(VERSION);
    std::fs::create_dir_all(&version_dir).expect("create version dir");

    let linux_meta = component_manifest_toml(COMPONENT, VERSION, &["system"]);
    std::fs::write(version_dir.join("meta.toml"), &linux_meta).expect("write version-level meta");
    let macos_meta = component_manifest_toml(COMPONENT, VERSION, &["user"]);

    let mut index =
        String::from("schema_version = 1\nchannel = \"stable\"\npublisher = \"test\"\n");
    for (os, arch, modes, meta) in [
        ("linux", "x86_64", &["system"][..], &linux_meta),
        ("macos", "aarch64", &["user"][..], &macos_meta),
    ] {
        let artifact_dir = version_dir.join(os).join(arch);
        std::fs::create_dir_all(&artifact_dir).expect("create artifact dir");
        if os == "macos" && macos_sidecar {
            std::fs::write(artifact_dir.join("meta.toml"), meta).expect("write macos sidecar");
        }

        // `build_component_artifact` embeds the same manifest text, so the
        // executed contract digests to the value published for this entry.
        let artifact = build_component_artifact(COMPONENT, VERSION, modes);
        let artifact_name = format!("{COMPONENT}-{VERSION}-{os}-{arch}.tar.gz");
        std::fs::write(artifact_dir.join(&artifact_name), &artifact).expect("write artifact");

        let digest_line = if digests {
            format!(
                "manifest_digest = \"sha256:{:x}\"\n",
                Sha256::digest(meta.as_bytes())
            )
        } else {
            String::new()
        };
        index.push_str(&format!(
            r#"
[[entries]]
component = "{COMPONENT}"
version = "{VERSION}"
channel = "stable"
artifact_type = "tar_gz"
backend = "raw"
url = "{COMPONENT}/{VERSION}/{os}/{arch}/{artifact_name}"
os = "{os}"
arch = "{arch}"
install_modes = {modes_arr}
sha256 = "{artifact_sha:x}"
{digest_line}"#,
            modes_arr = toml_string_array(modes),
            artifact_sha = Sha256::digest(&artifact),
        ));
    }
    std::fs::write(v1.join("index.toml"), index).expect("write distribution index");
    format!("file://{}", v1.display())
}

fn agentsight_resolve_inputs(repo_url: String) -> ResolveInputs<'static> {
    ResolveInputs {
        component: "agentsight".to_string(),
        package: "agentsight".to_string(),
        backend: "raw".to_string(),
        base_url: repo_url,
        repository_origin: None,
        version: None,
        warnings: Vec::new(),
    }
}

/// File names cached under `<cache>/downloads`, for asserting what a dry-run
/// did and did not fetch.
fn cached_download_names(layout: &FsLayout) -> Vec<String> {
    std::fs::read_dir(layout.cache_dir.join("downloads"))
        .expect("downloads cache exists")
        .map(|entry| {
            entry
                .expect("cache entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect()
}

/// The regression, in the shape that fails silently: with no
/// `manifest_digest` to cross-check, deriving the metadata URL from the
/// version root loads the *Linux* contract for a resolved macOS artifact.
/// That contract declares system mode only, so a `--install-mode user`
/// dry-run refuses an install the execution path — which reads the artifact's
/// own embedded contract — would have accepted.
#[test]
fn dry_run_contract_reads_the_target_sidecar_without_a_digest() {
    let sandbox = TestSandbox::new();
    let repo_url = write_local_repo_target_specific_meta(sandbox.repo_root(), true, false);
    let ctx = sandbox.context(InstallMode::User);
    let layout = common::resolve_layout(&ctx);

    let resolution = resolve_raw(
        &ctx,
        &layout,
        &macos_arm_env(),
        agentsight_resolve_inputs(repo_url),
    )
    .expect("macos/aarch64 entry must resolve");

    let contract = load_dry_run_install_contract(&ctx, &layout, &resolution)
        .expect("the macOS sidecar must satisfy the user-mode dry-run")
        .expect("a published sidecar must yield a contract");

    assert_eq!(
        contract.manifest.install.modes,
        vec!["user".to_string()],
        "dry-run must validate the macOS contract, not the version-level Linux one"
    );
}

/// The same resolution with `manifest_digest` published per entry: the
/// sidecar is both selected by target and verified against the digest the
/// execution path checks the embedded contract against.
#[test]
fn dry_run_contract_reads_the_resolved_target_sidecar() {
    let sandbox = TestSandbox::new();
    let repo_url = write_local_repo_target_specific_meta(sandbox.repo_root(), true, true);
    let ctx = sandbox.context(InstallMode::User);
    let layout = common::resolve_layout(&ctx);

    let resolution = resolve_raw(
        &ctx,
        &layout,
        &macos_arm_env(),
        agentsight_resolve_inputs(repo_url),
    )
    .expect("macos/aarch64 entry must resolve");
    assert!(
        resolution
            .artifact_url
            .ends_with("/macos/aarch64/agentsight-0.10.1-macos-aarch64.tar.gz"),
        "fixture must resolve the macOS artifact, got: {}",
        resolution.artifact_url
    );

    let contract = load_dry_run_install_contract(&ctx, &layout, &resolution)
        .expect("the macOS sidecar must satisfy the user-mode dry-run")
        .expect("a published sidecar must yield a contract");

    assert_eq!(
        contract.manifest.install.modes,
        vec!["user".to_string()],
        "dry-run must validate the macOS contract, not the version-level Linux one"
    );
    let cached = cached_download_names(&layout);
    assert!(
        cached.iter().all(|name| !name.ends_with(".tar.gz")),
        "dry-run must stay lightweight and skip the artifact; cache entries: {cached:?}"
    );
}

/// A repository publishing only version-level metadata — every raw repo
/// before target-specific contracts existed — must keep resolving its
/// contract through the version-root fallback.
#[test]
fn dry_run_contract_falls_back_to_version_level_meta() {
    let sandbox = TestSandbox::new();
    let repo_url = write_local_repo_target_specific_meta(sandbox.repo_root(), false, true);
    let ctx = sandbox.context(InstallMode::System);
    let layout = common::resolve_layout(&ctx);

    let sibling = sandbox
        .repo_root()
        .join("v1/agentsight/0.10.1/linux/x86_64/meta.toml");
    assert!(
        !sibling.exists(),
        "the legacy fixture must publish no sibling metadata"
    );

    let resolution = resolve_raw(
        &ctx,
        &layout,
        &linux_env(),
        agentsight_resolve_inputs(repo_url),
    )
    .expect("linux/x86_64 entry must resolve");

    let contract = load_dry_run_install_contract(&ctx, &layout, &resolution)
        .expect("an absent sibling must fall back, not fail")
        .expect("version-level metadata must still yield a contract");

    assert_eq!(contract.manifest.install.modes, vec!["system".to_string()]);
    assert_eq!(contract.source, InstallContractSource::SidecarMeta);
}

/// Fallback is reserved for absence. A published sibling that does not match
/// the index `manifest_digest` must fail the dry-run — falling through to the
/// version root would validate a contract the resolved artifact never had.
#[test]
fn dry_run_contract_rejects_a_tampered_sidecar_without_falling_back() {
    let sandbox = TestSandbox::new();
    let repo_url = write_local_repo_target_specific_meta(sandbox.repo_root(), true, true);
    std::fs::write(
        sandbox
            .repo_root()
            .join("v1/agentsight/0.10.1/macos/aarch64/meta.toml"),
        component_manifest_toml("agentsight", "0.10.1", &["user", "system"]),
    )
    .expect("replace the macOS sidecar");

    let ctx = sandbox.context(InstallMode::User);
    let layout = common::resolve_layout(&ctx);
    let resolution = resolve_raw(
        &ctx,
        &layout,
        &macos_arm_env(),
        agentsight_resolve_inputs(repo_url),
    )
    .expect("macos/aarch64 entry must resolve");

    let Err(err) = load_dry_run_install_contract(&ctx, &layout, &resolution) else {
        panic!("a sidecar digest mismatch must fail the dry-run, not fall back");
    };
    assert_eq!(err.code(), "EXECUTION_FAILED");
    assert!(err.reason().contains("sha256 mismatch"), "got: {err}");
}

#[test]
fn install_no_conflict_when_conflicting_component_not_installed() {
    let tmp = tempdir().expect("tmpdir");
    let prefix = tmp.path().join("sys");

    // Write a repo with cosh-ng that declares conflicts = ["cosh"], but cosh
    // is NOT installed — install should succeed.
    let repo_url = write_local_repo_with_conflicts(
        &tmp.path().join("repo"),
        "cosh-ng",
        "0.11.0",
        &["system"],
        &["cosh"],
    );

    let mut a = args("cosh-ng");
    a.repo = Some(repo_url);
    handle_with_fake_rpm(a, &ctx_with_prefix(false, Some(prefix.clone())))
        .expect("install must succeed when no conflict");

    // Verify cosh-ng is recorded in state.
    let layout = FsLayout::system(Some(prefix));
    let store = load_v5_store(&layout);
    let installation = store
        .find(ObjectKind::Component, "cosh-ng")
        .expect("cosh-ng must be recorded");
    assert_eq!(installation.status, LifecycleStatus::Installed);
    assert_eq!(owned_artifact(installation).version, "0.11.0");
}

/// A future-shaped row for the *queried* component must refuse resolution
/// instead of silently answering with an older parsable version: with
/// 1.0.0 parsable and 2.0.0 unparsable, "latest" would otherwise select
/// 1.0.0 — a silent downgrade for update, a stale install for install.
#[test]
fn skipped_newer_entry_refuses_latest_instead_of_downgrading() {
    let tmp = tempdir().expect("tmpdir");
    let prefix = tmp.path().join("sys");
    let repo_url = write_local_repo_component_versions(
        &tmp.path().join("repo"),
        "sec-core",
        &["1.0.0"],
        &["system"],
    );
    // Publish 2.0.0 with an entry shape this build cannot represent.
    let env = anolisa_env::EnvService::detect();
    let index_path = tmp.path().join("repo/v1/index.toml");
    let mut index = std::fs::read_to_string(&index_path).expect("read index");
    index.push_str(&format!(
        r#"
[[entries]]
component = "sec-core"
version = "2.0.0"
channel = "stable"
artifact_type = "hologram_v9"
backend = "raw"
url = "sec-core-2.0.0.tar.gz"
os = "{os}"
arch = "{arch}"
install_modes = ["system"]
"#,
        os = env.os,
        arch = env.arch,
    ));
    std::fs::write(&index_path, index).expect("write index");

    let ctx = ctx_with_prefix(false, Some(prefix.clone()));
    let layout = FsLayout::system(Some(prefix));
    let inputs = |version: Option<&'static str>| ResolveInputs {
        component: "sec-core".to_string(),
        package: "sec-core".to_string(),
        backend: "raw".to_string(),
        base_url: repo_url.clone(),
        repository_origin: None,
        version,
        warnings: Vec::new(),
    };

    // Latest: refuse — the unreadable 2.0.0 may be the answer.
    let err = match resolve_raw(&ctx, &layout, &env, inputs(None)) {
        Ok(_) => panic!("latest must refuse, not fall back to 1.0.0"),
        Err(err) => err,
    };
    assert!(
        err.reason().contains("cannot parse") && err.reason().contains("self-update"),
        "refusal must explain the unparsable entry and hint self-update, got: {}",
        err.reason()
    );
    assert!(
        !err.reason().contains("not found"),
        "must be a refusal, not a not-found, got: {}",
        err.reason()
    );

    // Pinned 2.0.0: equally refused (that exact row is unreadable).
    let err = match resolve_raw(&ctx, &layout, &env, inputs(Some("2.0.0"))) {
        Ok(_) => panic!("pinned 2.0.0 must refuse"),
        Err(err) => err,
    };
    assert!(
        err.reason().contains("cannot parse"),
        "got: {}",
        err.reason()
    );

    // Pinned 1.0.0: an explicit choice of the parsable version resolves,
    // and the skipped row stays visible as a warning.
    let resolution =
        resolve_raw(&ctx, &layout, &env, inputs(Some("1.0.0"))).expect("pinned 1.0.0 resolves");
    assert_eq!(resolution.entry.version, "1.0.0");
    assert!(
        resolution
            .warnings
            .iter()
            .any(|w| w.contains("skipped index entry")),
        "skipped row must surface as a warning, got: {:?}",
        resolution.warnings
    );
}

/// A future-shaped row for an unrelated component neither blocks nor
/// degrades resolution of the queried one; it only surfaces as a warning.
#[test]
fn skipped_unrelated_entry_keeps_component_resolvable() {
    let tmp = tempdir().expect("tmpdir");
    let prefix = tmp.path().join("sys");
    let repo_url = write_local_repo_component_versions(
        &tmp.path().join("repo"),
        "cosh",
        &["1.2.3"],
        &["system"],
    );
    let env = anolisa_env::EnvService::detect();
    let index_path = tmp.path().join("repo/v1/index.toml");
    let mut index = std::fs::read_to_string(&index_path).expect("read index");
    index.push_str(&format!(
        r#"
[[entries]]
component = "future-thing"
version = "0.1.0"
channel = "stable"
artifact_type = "hologram_v9"
backend = "raw"
url = "future-thing-0.1.0.tar.gz"
os = "{os}"
arch = "{arch}"
install_modes = ["system"]
"#,
        os = env.os,
        arch = env.arch,
    ));
    std::fs::write(&index_path, index).expect("write index");

    let ctx = ctx_with_prefix(false, Some(prefix.clone()));
    let layout = FsLayout::system(Some(prefix));
    let resolution = resolve_raw(
        &ctx,
        &layout,
        &env,
        ResolveInputs {
            component: "cosh".to_string(),
            package: "cosh".to_string(),
            backend: "raw".to_string(),
            base_url: repo_url,
            repository_origin: None,
            version: None,
            warnings: Vec::new(),
        },
    )
    .expect("unrelated skipped row must not block");
    assert_eq!(resolution.entry.version, "1.2.3");
    assert!(
        resolution
            .warnings
            .iter()
            .any(|w| w.contains("future-thing")),
        "skipped row must surface as a warning, got: {:?}",
        resolution.warnings
    );
}

/// A skipped row whose selectors are damaged (valid TOML, wrong shape —
/// here `install_modes = [1]`) must still block "latest" for its
/// component: partial recovery of the selector would under-block and
/// reopen the silent downgrade this gate exists to prevent.
#[test]
fn skipped_row_with_malformed_selector_still_blocks_latest() {
    let tmp = tempdir().expect("tmpdir");
    let prefix = tmp.path().join("sys");
    let repo_url = write_local_repo_component_versions(
        &tmp.path().join("repo"),
        "sec-core",
        &["1.0.0"],
        &["system"],
    );
    let env = anolisa_env::EnvService::detect();
    let index_path = tmp.path().join("repo/v1/index.toml");
    let mut index = std::fs::read_to_string(&index_path).expect("read index");
    index.push_str(&format!(
        r#"
[[entries]]
component = "sec-core"
version = "2.0.0"
channel = "stable"
artifact_type = "hologram_v9"
backend = "raw"
url = "sec-core-2.0.0.tar.gz"
os = "{os}"
arch = "{arch}"
install_modes = [1]
"#,
        os = env.os,
        arch = env.arch,
    ));
    std::fs::write(&index_path, index).expect("write index");

    let ctx = ctx_with_prefix(false, Some(prefix.clone()));
    let layout = FsLayout::system(Some(prefix));
    let err = match resolve_raw(
        &ctx,
        &layout,
        &env,
        ResolveInputs {
            component: "sec-core".to_string(),
            package: "sec-core".to_string(),
            backend: "raw".to_string(),
            base_url: repo_url,
            repository_origin: None,
            version: None,
            warnings: Vec::new(),
        },
    ) {
        Ok(_) => panic!("malformed selector must not unblock a silent downgrade to 1.0.0"),
        Err(err) => err,
    };
    assert!(
        err.reason().contains("cannot parse") && err.reason().contains("self-update"),
        "got: {}",
        err.reason()
    );
}

#[test]
fn raw_effect_capability_policy_matrix() {
    for supported in [false, true] {
        for optional in [false, true] {
            for fail in [false, true] {
                let tmp = tempdir().expect("tmpdir");
                let prefix = tmp.path().join("sys");
                let repo = write_local_repo_component_with_capability(
                    &tmp.path().join("repo"),
                    "agentsight",
                    "0.2.0",
                    &["system"],
                    "{bindir}/agentsight",
                    &["CAP_BPF"],
                    optional,
                );
                let ctx = ctx_with_prefix(false, Some(prefix.clone()));
                let layout = common::resolve_layout(&ctx);
                let mut request = args("agentsight");
                request.repo = Some(repo);
                let mut effects = crate::test_support::RawEffectRecorder::default();
                effects.supported = supported;
                effects.fail_capability = fail;
                let result = effects.with(|f| handle_with_effects(request, &ctx, f));
                let aborted = supported && fail && !optional;
                assert_eq!(result.is_err(), aborted);
                assert_eq!(layout.bin_dir.join("agentsight").exists(), !aborted);
                assert_eq!(
                    effects
                        .calls()
                        .iter()
                        .filter(|s| s.starts_with("apply "))
                        .count(),
                    usize::from(supported)
                );
                assert_eq!(
                    effects
                        .calls()
                        .iter()
                        .filter(|s| s.starts_with("service factory"))
                        .count(),
                    usize::from(!aborted)
                );
                let store = load_v5_store(&layout);
                assert_eq!(
                    store.find(ObjectKind::Component, "agentsight").is_some(),
                    !aborted
                );
                if aborted {
                    assert!(
                        result
                            .unwrap_err()
                            .reason()
                            .contains("required capability application failed")
                    );
                } else {
                    let warnings = result.unwrap();
                    assert_eq!(
                        warnings
                            .iter()
                            .filter(|w| w.contains("optional capability"))
                            .count(),
                        usize::from(supported && fail)
                    );
                    let record = store
                        .find(ObjectKind::Component, "agentsight")
                        .expect("record");
                    let grants = &owned_artifact(record)
                        .files
                        .iter()
                        .find(|f| f.path == layout.bin_dir.join("agentsight"))
                        .expect("binary")
                        .capabilities;
                    assert_eq!(!grants.is_empty(), supported && !fail);
                }
            }
        }
    }
}

#[test]
fn raw_effect_service_failures_remain_ordered_warnings() {
    use anolisa_core::ServiceOp;
    for fail in [
        None,
        Some(ServiceOp::DaemonReload),
        Some(ServiceOp::Enable),
        Some(ServiceOp::Start),
    ] {
        for supported in [false, true] {
            let tmp = tempdir().expect("tmpdir");
            let ctx = ctx_with_prefix(false, Some(tmp.path().join("sys")));
            let mut request = args("agentsight");
            request.repo = Some(write_local_repo_component_with_service(
                &tmp.path().join("repo"),
                "agentsight",
                "0.2.0",
                &["system"],
                "agentsight.service",
                true,
                true,
            ));
            let mut effects = crate::test_support::RawEffectRecorder::default();
            effects.supported = supported;
            effects.fail_service = fail;
            let warnings = effects
                .with(|f| handle_with_effects(request, &ctx, f))
                .expect("best effort");
            let mut expected = vec!["capability factory system", "service factory system System"];
            if supported {
                expected.extend([
                    "DaemonReload System ",
                    "Enable System agentsight.service",
                    "Start System agentsight.service",
                ]);
            }
            assert_eq!(effects.calls(), expected);
            assert_eq!(warnings.len(), usize::from(supported && fail.is_some()));
            if let Some(op) = fail.filter(|_| supported) {
                assert!(warnings[0].contains(op.as_str()), "{warnings:?}");
            }
        }
    }
}

#[test]
fn raw_effect_preview_noop_and_refusal_do_not_create_managers() {
    for case in ["preview", "noop", "refusal"] {
        let tmp = tempdir().expect("tmpdir");
        let mut ctx = ctx_with_prefix(false, Some(tmp.path().join("sys")));
        let mut request = args("agentsight");
        request.repo = Some(write_local_repo_component_with_service(
            &tmp.path().join("repo"),
            "agentsight",
            "0.2.0",
            if case == "refusal" {
                &["user"]
            } else {
                &["system"]
            },
            "agentsight.service",
            true,
            true,
        ));
        if case == "noop" {
            handle_with_fake_rpm(
                {
                    let mut seed = args("agentsight");
                    seed.repo = request.repo.clone();
                    seed
                },
                &ctx,
            )
            .expect("seed installation");
        }
        ctx.dry_run = case == "preview";
        let effects = crate::test_support::RawEffectRecorder::default();
        let result = effects.with(|f| handle_with_effects(request, &ctx, f));
        assert_eq!(result.is_err(), case == "refusal");
        assert!(effects.calls().is_empty(), "{case}: {:?}", effects.calls());
    }
}

#[test]
fn raw_effect_post_enable_failure_compensates_activated_units() {
    let tmp = tempdir().expect("tmpdir");
    let ctx = ctx_with_prefix(false, Some(tmp.path().join("sys")));
    let root = tmp.path().join("repo");
    let repo = write_local_repo_component_with_service(
        &root,
        "agentsight",
        "0.2.0",
        &["system"],
        "agentsight.service",
        true,
        true,
    );
    let manifest = format!(
        "{}\n[[component.hooks]]\nphase = \"post_enable\"\nscript = \"hooks/fail.sh\"\nstrict = true\n",
        service_manifest("agentsight.service", true, true, None)
    );
    replace_repo_artifact(
        &root,
        "agentsight",
        &build_tar_gz(&[
            (".anolisa/component.toml", manifest.as_bytes()),
            ("bin/agentsight", b"binary\n"),
            ("hooks/fail.sh", b"#!/bin/sh\nexit 1\n"),
        ]),
    );
    let mut request = args("agentsight");
    request.repo = Some(repo);
    let effects = crate::test_support::RawEffectRecorder::default();
    let err = effects
        .with(|f| handle_with_effects(request, &ctx, f))
        .expect_err("strict hook");
    assert!(err.reason().contains("post_enable"), "{err}");
    assert_eq!(
        effects.calls(),
        [
            "capability factory system",
            "service factory system System",
            "DaemonReload System ",
            "Enable System agentsight.service",
            "Start System agentsight.service",
            "Stop System agentsight.service",
            "Disable System agentsight.service",
        ]
    );
    let layout = common::resolve_layout(&ctx);
    assert!(!layout.bin_dir.join("agentsight").exists());
    assert!(
        load_v5_store(&layout)
            .find(ObjectKind::Component, "agentsight")
            .is_none()
    );
}

#[test]
fn raw_effect_install_user_scope_keeps_its_manager_routing() {
    let tmp = tempdir().expect("tmpdir");
    let ctx = crate::test_support::context_for_root(
        tmp.path(),
        crate::context::InstallMode::User,
        Some(tmp.path().join("sys")),
        Default::default(),
    );
    let root = tmp.path().join("repo");
    let repo = write_local_repo_component(&root, "agentsight", "0.2.0", &["user"]);
    let manifest = format!(
        "{}\n[[component.services]]\nunit = \"agentsight.service\"\nscope = \"user\"\nenable = true\nstart = true\n",
        component_manifest_toml("agentsight", "0.2.0", &["user"])
    );
    replace_repo_artifact(
        &root,
        "agentsight",
        &build_tar_gz(&[
            (".anolisa/component.toml", manifest.as_bytes()),
            ("bin/agentsight", b"binary\n"),
        ]),
    );
    let mut request = args("agentsight");
    request.repo = Some(repo);
    let effects = crate::test_support::RawEffectRecorder::default();
    effects
        .with(|f| handle_with_effects(request, &ctx, f))
        .expect("user service install");
    assert_eq!(
        effects.calls(),
        [
            "capability factory user",
            "service factory user User",
            "DaemonReload User ",
            "Enable User agentsight.service",
            "Start User agentsight.service",
        ]
    );
}
