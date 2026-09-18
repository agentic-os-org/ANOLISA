//! Shared test helpers, fixture builders, and integration test modules
//! for the `install` command.
//!
//! Unit tests are inlined in each source module under `#[cfg(test)] mod tests`.
//! Integration tests are split by theme into sub-modules here:

use super::*;

use anolisa_core::lock::{InstallLock, LockError};
use anolisa_core::transaction::Transaction;
use anolisa_platform::fs_layout::FsLayout;
use anolisa_platform::pkg_query::{PackageInfo, PackageQuery, PackageQueryError};
use anolisa_platform::pkg_transaction::{PackageTransaction, PackageTransactionError};

use crate::commands::common;
use crate::context::InstallMode;
use crate::repo_config::RepoConfig;
use crate::resolution::rpm_component_provide;
use anolisa_platform::pkg_query::PackageVersion;
use flate2::Compression;
use flate2::write::GzEncoder;
use sha2::{Digest, Sha256};
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use tar::{Builder, Header};
use tempfile::tempdir;

pub fn ctx_with_prefix(json: bool, prefix: Option<PathBuf>) -> CliContext {
    let root = prefix
        .as_deref()
        .unwrap_or_else(|| std::path::Path::new("/tmp/anolisa-install-validation"));
    let install_mode = if prefix.is_some() {
        InstallMode::System
    } else {
        InstallMode::User
    };
    crate::test_support::context_for_root(
        root,
        install_mode,
        prefix.clone(),
        crate::test_support::TestContextOptions {
            json,
            ..Default::default()
        },
    )
}

pub fn args(component: &str) -> InstallArgs {
    InstallArgs {
        component: Some(component.to_string()),
        all: false,
        fail_fast: false,
        version: None,
        backend: None,
        repo: None,
        package: None,
    }
}

pub fn handle_with_fake_rpm(args: InstallArgs, ctx: &CliContext) -> Result<(), CliError> {
    let component = args
        .component
        .clone()
        .expect("single-component install test sets args.component");
    handle_one_with_query(component, args, ctx, &FakeQuery::default()).map(|_| ())
}

pub fn handle_with_effects(
    args: InstallArgs,
    ctx: &CliContext,
    effects: RawEffectFactories<'_>,
) -> Result<Vec<String>, CliError> {
    let component = args.component.as_deref().expect("single-component test");
    let mut reporter = crate::progress::Activity::start(
        crate::progress::feedback_for_stderr(ctx.json, ctx.quiet),
        "test install",
    );
    let outcome = application::run_with_dependencies(
        application::InstallRequest {
            component,
            args: &args,
            intent: execution_intent(ctx),
        },
        ctx,
        &anolisa_env::EnvService::detect(),
        &RpmdbProbe::absent(),
        &FakeQuery::default(),
        &NoTxn,
        false,
        &Default::default(),
        &mut reporter,
        effects,
    )?;
    let warnings = outcome.warnings().to_vec();
    render_outcome(ctx, outcome)?;
    Ok(warnings)
}

pub fn toml_string_array(values: &[&str]) -> String {
    let quoted: Vec<String> = values.iter().map(|value| format!("\"{value}\"")).collect();
    format!("[{}]", quoted.join(", "))
}

pub fn component_manifest_toml(component: &str, version: &str, modes: &[&str]) -> String {
    component_manifest_toml_with_conflicts(component, version, modes, &[])
}

pub fn component_manifest_toml_with_conflicts(
    component: &str,
    version: &str,
    modes: &[&str],
    conflicts: &[&str],
) -> String {
    let modes = toml_string_array(modes);
    let conflicts_line = if conflicts.is_empty() {
        String::new()
    } else {
        format!("conflicts = {}\n", toml_string_array(conflicts))
    };
    format!(
        r#"[component]
name = "{component}"
version = "{version}"
{conflicts_line}
[component.layout]
modes = {modes}

[[component.layout.files]]
source = "bin/{component}"
target = "{{bindir}}/{component}"
mode = "0755"
type = "executable"
"#
    )
}

pub fn build_tar_gz(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let buf = Vec::new();
    let enc = GzEncoder::new(buf, Compression::default());
    let mut tar = Builder::new(enc);
    for (path, data) in entries {
        let mut header = Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append_data(&mut header, *path, *data)
            .expect("append tar entry");
    }
    let enc = tar.into_inner().expect("finish tar");
    enc.finish().expect("finish gzip")
}

/// Replace the single artifact in a local fixture and keep its digest authoritative.
pub fn replace_repo_artifact(root: &Path, component: &str, artifact: &[u8]) {
    let artifact_path = root.join("v1").join(format!("{component}.tar.gz"));
    let old = std::fs::read(&artifact_path).expect("old artifact");
    let index_path = root.join("v1/index.toml");
    let index = std::fs::read_to_string(&index_path).expect("index");
    let old_digest = format!("{:x}", Sha256::digest(old));
    assert!(index.contains(&old_digest));
    std::fs::write(artifact_path, artifact).expect("artifact");
    std::fs::write(
        index_path,
        index.replace(&old_digest, &format!("{:x}", Sha256::digest(artifact))),
    )
    .expect("index digest");
}

pub fn build_component_artifact(component: &str, version: &str, modes: &[&str]) -> Vec<u8> {
    let manifest = component_manifest_toml(component, version, modes);
    let bin_path = format!("bin/{component}");
    let payload = format!("#!/bin/sh\necho {component}\n");
    build_tar_gz(&[
        (".anolisa/component.toml", manifest.as_bytes()),
        (bin_path.as_str(), payload.as_bytes()),
    ])
}

pub fn adapter_manifest(framework: &str, source: Option<&str>, dest: Option<&str>) -> String {
    let mut toml = String::from(
        "[component]\nname = \"tokenless\"\nversion = \"0.1.0\"\n\n\
         [component.layout]\nmodes = [\"system\"]\n\n\
         [[adapters]]\n",
    );
    toml.push_str(&format!("framework = \"{framework}\"\n"));
    if let Some(s) = source {
        toml.push_str(&format!("source = \"{s}\"\n"));
    }
    if let Some(d) = dest {
        toml.push_str(&format!("dest = \"{d}\"\n"));
    }
    toml
}

pub fn capability_manifest(path: Option<&str>, caps: &[&str], optional: bool) -> String {
    let mut toml = String::from(
        "[component]\nname = \"agentsight\"\nversion = \"0.1.0\"\n\n\
         [component.layout]\nmodes = [\"system\"]\n\n\
         [[component.layout.files]]\n\
         source = \"bin/agentsight\"\ntarget = \"{bindir}/agentsight\"\n\
         mode = \"0755\"\ntype = \"executable\"\n\n\
         [[component.capabilities]]\n",
    );
    if let Some(p) = path {
        toml.push_str(&format!("path = \"{p}\"\n"));
    }
    toml.push_str(&format!("caps = {}\n", toml_string_array(caps)));
    if optional {
        toml.push_str("optional = true\n");
    }
    toml
}

/// Publish a generation-2 component index at `v1/components-v2.toml`.
///
/// Entries are identity-only (no backend rows): identity resolution accepts
/// the names while package resolution keeps flowing through `package_map`
/// and RPM `Provides` exactly as before the index existed.
pub fn write_component_index_v2(v1: &Path, components: &[&str]) {
    let env = anolisa_env::EnvService::detect();
    let mut index = String::from("schema_version = 2\n");
    for component in components {
        index.push_str(&format!(
            "\n[[components]]\nname = \"{component}\"\ntargets = [{{ os = \"{os}\", arch = \"{arch}\" }}]\n",
            os = env.os,
            arch = env.arch,
        ));
    }
    std::fs::create_dir_all(v1).expect("create repo dirs");
    std::fs::write(v1.join("components-v2.toml"), index).expect("write component index");
}

/// Component names the shared unit-test contexts accept as supported
/// identities. Covers the targets exercised across install/adopt/repair
/// fixtures; tests probing unsupported names use targets outside this set.
pub const TEST_INDEX_COMPONENTS: &[&str] = &[
    "agentsight",
    "cosh",
    "cosh-ng",
    "copilot-shell",
    "tokenless",
    "sec-core",
];

/// Seed `<etc>/repo.toml` with a raw backend whose file:// repository
/// publishes a component index naming `components`, so identity resolution
/// can validate fresh targets inside the test sandbox.
pub fn seed_repo_config_with_index(layout: &FsLayout, components: &[&str]) -> String {
    let v1 = layout.etc_dir.join("test-index-repo").join("v1");
    write_component_index_v2(&v1, components);
    let base_url = format!("file://{}", v1.display());
    std::fs::create_dir_all(&layout.etc_dir).expect("etc dir");
    std::fs::write(
        layout.etc_dir.join("repo.toml"),
        format!(
            "schema_version = 1\ndefault_backend = \"raw\"\n\n[backends.raw]\nbase_url = \"{base_url}\"\n"
        ),
    )
    .expect("write repo.toml");
    base_url
}

pub fn write_empty_repo(root: &Path) -> String {
    let v1 = root.join("v1");
    std::fs::create_dir_all(&v1).expect("create repo dirs");
    std::fs::write(
        v1.join("index.toml"),
        r#"schema_version = 1
channel = "stable"
publisher = "test"
"#,
    )
    .expect("write index");
    write_component_index_v2(&v1, &[]);
    format!("file://{}", v1.display())
}

pub fn write_local_repo(root: &Path) -> String {
    write_local_repo_component(root, "agentsight", "0.2.0", &["system"])
}

/// Local file:// repo publishing several versions of one component, so a
/// version-pinned install can select a non-latest entry.
pub fn write_local_repo_component_versions(
    root: &Path,
    component: &str,
    versions: &[&str],
    modes: &[&str],
) -> String {
    let v1 = root.join("v1");
    std::fs::create_dir_all(&v1).expect("create repo dirs");
    write_component_index_v2(&v1, &[component]);

    let env = anolisa_env::EnvService::detect();
    let modes_arr = toml_string_array(modes);
    let mut index =
        String::from("schema_version = 1\nchannel = \"stable\"\npublisher = \"test\"\n");
    for version in versions {
        let artifact = build_component_artifact(component, version, modes);
        let artifact_name = format!("{component}-{version}.tar.gz");
        std::fs::write(v1.join(&artifact_name), &artifact).expect("write artifact");
        let sha = format!("{:x}", Sha256::digest(&artifact));
        index.push_str(&format!(
            r#"
[[entries]]
component = "{component}"
version = "{version}"
channel = "stable"
artifact_type = "tar_gz"
backend = "raw"
url = "{artifact_name}"
os = "{os}"
arch = "{arch}"
install_modes = {modes_arr}
sha256 = "{sha}"
"#,
            os = env.os,
            arch = env.arch,
        ));
    }
    std::fs::write(v1.join("index.toml"), index).expect("write index");
    format!("file://{}", v1.display())
}

pub fn write_local_repo_component(
    root: &Path,
    component: &str,
    version: &str,
    modes: &[&str],
) -> String {
    write_local_repo_component_with_modes(root, component, version, modes, modes)
}

pub fn write_local_repo_component_with_modes(
    root: &Path,
    component: &str,
    version: &str,
    index_modes: &[&str],
    manifest_modes: &[&str],
) -> String {
    let v1 = root.join("v1");
    std::fs::create_dir_all(&v1).expect("create repo dirs");
    write_component_index_v2(&v1, &[component]);

    let artifact = build_component_artifact(component, version, manifest_modes);
    let artifact_name = format!("{component}.tar.gz");
    std::fs::write(v1.join(&artifact_name), &artifact).expect("write artifact");
    let sha = format!("{:x}", Sha256::digest(&artifact));
    let modes = toml_string_array(index_modes);

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
install_modes = {modes}
sha256 = "{sha}"
"#,
        os = env.os,
        arch = env.arch,
    );
    std::fs::write(v1.join("index.toml"), index).expect("write index");
    format!("file://{}", v1.display())
}

pub fn write_published_layout_repo_with_meta(
    root: &Path,
    component: &str,
    version: &str,
    modes: &[&str],
) -> String {
    let env = anolisa_env::EnvService::detect();
    let version_dir = root.join("v1").join(component).join(version);
    let artifact_dir = version_dir.join(&env.os).join(&env.arch);
    std::fs::create_dir_all(&artifact_dir).expect("create artifact dirs");

    let manifest = component_manifest_toml(component, version, modes);
    std::fs::write(version_dir.join("meta.toml"), &manifest).expect("write meta");

    let artifact = build_component_artifact(component, version, modes);
    let artifact_name = format!(
        "{component}-{version}-{os}-{arch}.tar.gz",
        os = env.os,
        arch = env.arch
    );
    std::fs::write(artifact_dir.join(&artifact_name), &artifact).expect("write artifact");
    let sha = format!("{:x}", Sha256::digest(&artifact));
    let modes = toml_string_array(modes);
    let url = format!(
        "{component}/{version}/{os}/{arch}/{artifact_name}",
        os = env.os,
        arch = env.arch
    );

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
url = "{url}"
os = "{os}"
arch = "{arch}"
install_modes = {modes}
sha256 = "{sha}"
"#,
        os = env.os,
        arch = env.arch,
    );
    std::fs::write(root.join("v1/index.toml"), index).expect("write index");
    write_component_index_v2(&root.join("v1"), &[component]);
    format!("file://{}", root.join("v1").display())
}

pub fn write_binary_repo_component(
    root: &Path,
    component: &str,
    version: &str,
    modes: &[&str],
) -> String {
    let v1 = root.join("v1");
    std::fs::create_dir_all(&v1).expect("create repo dirs");
    write_component_index_v2(&v1, &[component]);

    let artifact = format!("#!/bin/sh\necho {component}\n").into_bytes();
    let artifact_name = component.to_string();
    std::fs::write(v1.join(&artifact_name), &artifact).expect("write artifact");
    let sha = format!("{:x}", Sha256::digest(&artifact));
    let modes = toml_string_array(modes);

    let env = anolisa_env::EnvService::detect();
    let index = format!(
        r#"schema_version = 1
channel = "stable"
publisher = "test"

[[entries]]
component = "{component}"
version = "{version}"
channel = "stable"
artifact_type = "binary"
backend = "raw"
url = "{artifact_name}"
os = "{os}"
arch = "{arch}"
install_modes = {modes}
sha256 = "{sha}"
"#,
        os = env.os,
        arch = env.arch,
    );
    std::fs::write(v1.join("index.toml"), index).expect("write index");
    format!("file://{}", v1.display())
}

pub fn write_conventional_repo(root: &Path) -> String {
    let env = anolisa_env::EnvService::detect();
    let artifact_dir = root
        .join("v1/agentsight/0.2.0")
        .join(&env.os)
        .join(&env.arch);
    std::fs::create_dir_all(&artifact_dir).expect("create repo dirs");
    write_component_index_v2(&root.join("v1"), &["agentsight"]);

    let artifact = build_component_artifact("agentsight", "0.2.0", &["system"]);
    let file_name = format!("agentsight-0.2.0-{}-{}.tar.gz", env.os, env.arch);
    std::fs::write(artifact_dir.join(file_name), &artifact).expect("write artifact");
    let sha = format!("{:x}", Sha256::digest(&artifact));

    let index = format!(
        r#"schema_version = 1
channel = "stable"
publisher = "test"

[[entries]]
component = "agentsight"
version = "0.2.0"
channel = "stable"
artifact_type = "tar_gz"
backend = "raw"
os = "{os}"
arch = "{arch}"
install_modes = ["system"]
sha256 = "{sha}"
"#,
        os = env.os,
        arch = env.arch,
    );
    std::fs::write(root.join("v1/index.toml"), index).expect("write index");
    format!("file://{}", root.join("v1").display())
}

pub fn component_manifest_toml_with_capability(
    component: &str,
    version: &str,
    modes: &[&str],
    cap_path: &str,
    caps: &[&str],
    optional: bool,
) -> String {
    let mut toml = component_manifest_toml(component, version, modes);
    toml.push_str("\n[[component.capabilities]]\n");
    toml.push_str(&format!("path = \"{cap_path}\"\n"));
    toml.push_str(&format!("caps = {}\n", toml_string_array(caps)));
    if optional {
        toml.push_str("optional = true\n");
    }
    toml
}

pub fn build_component_artifact_with_capability(
    component: &str,
    version: &str,
    modes: &[&str],
    cap_path: &str,
    caps: &[&str],
    optional: bool,
) -> Vec<u8> {
    let manifest = component_manifest_toml_with_capability(
        component, version, modes, cap_path, caps, optional,
    );
    let bin_path = format!("bin/{component}");
    let payload = format!("#!/bin/sh\necho {component}\n");
    build_tar_gz(&[
        (".anolisa/component.toml", manifest.as_bytes()),
        (bin_path.as_str(), payload.as_bytes()),
    ])
}

pub fn write_local_repo_component_with_capability(
    root: &Path,
    component: &str,
    version: &str,
    modes: &[&str],
    cap_path: &str,
    caps: &[&str],
    optional: bool,
) -> String {
    let v1 = root.join("v1");
    std::fs::create_dir_all(&v1).expect("create repo dirs");
    write_component_index_v2(&v1, &[component]);

    let artifact = build_component_artifact_with_capability(
        component, version, modes, cap_path, caps, optional,
    );
    let artifact_name = format!("{component}.tar.gz");
    std::fs::write(v1.join(&artifact_name), &artifact).expect("write artifact");
    let sha = format!("{:x}", Sha256::digest(&artifact));
    let modes_arr = toml_string_array(modes);

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
install_modes = {modes_arr}
sha256 = "{sha}"
"#,
        os = env.os,
        arch = env.arch,
    );
    std::fs::write(v1.join("index.toml"), index).expect("write index");
    format!("file://{}", v1.display())
}

pub fn service_manifest(unit: &str, enable: bool, start: bool, instance: Option<&str>) -> String {
    let mut toml = component_manifest_toml("agentsight", "0.2.0", &["system"]);
    toml.push_str("\n[[component.services]]\n");
    toml.push_str(&format!(
        "unit = \"{unit}\"\nenable = {enable}\nstart = {start}\n"
    ));
    if let Some(i) = instance {
        toml.push_str(&format!("instance = \"{i}\"\n"));
    }
    toml
}

pub fn hooks_manifest(specs: &[(&str, &str, bool)]) -> String {
    let mut toml = component_manifest_toml("demo", "0.1.0", &["system"]);
    for (phase, script, strict) in specs {
        toml.push_str("\n[[component.hooks]]\n");
        toml.push_str(&format!(
            "phase = \"{phase}\"\nscript = \"{script}\"\nstrict = {strict}\n"
        ));
    }
    toml
}

pub fn build_component_artifact_with_service(
    component: &str,
    version: &str,
    modes: &[&str],
    unit: &str,
    enable: bool,
    start: bool,
) -> Vec<u8> {
    let mut manifest = component_manifest_toml(component, version, modes);
    manifest.push_str("\n[[component.services]]\n");
    manifest.push_str(&format!(
        "unit = \"{unit}\"\nenable = {enable}\nstart = {start}\n"
    ));
    let bin_path = format!("bin/{component}");
    let payload = format!("#!/bin/sh\necho {component}\n");
    build_tar_gz(&[
        (".anolisa/component.toml", manifest.as_bytes()),
        (bin_path.as_str(), payload.as_bytes()),
    ])
}

pub fn write_local_repo_component_with_service(
    root: &Path,
    component: &str,
    version: &str,
    modes: &[&str],
    unit: &str,
    enable: bool,
    start: bool,
) -> String {
    let v1 = root.join("v1");
    std::fs::create_dir_all(&v1).expect("create repo dirs");
    write_component_index_v2(&v1, &[component]);

    let artifact =
        build_component_artifact_with_service(component, version, modes, unit, enable, start);
    let artifact_name = format!("{component}.tar.gz");
    std::fs::write(v1.join(&artifact_name), &artifact).expect("write artifact");
    let sha = format!("{:x}", Sha256::digest(&artifact));
    let modes_arr = toml_string_array(modes);

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
install_modes = {modes_arr}
sha256 = "{sha}"
"#,
        os = env.os,
        arch = env.arch,
    );
    std::fs::write(v1.join("index.toml"), index).expect("write index");
    format!("file://{}", v1.display())
}

pub fn write_local_repo_component_with_hook(
    root: &Path,
    component: &str,
    version: &str,
    phase: &str,
    strict: bool,
    script_body: &str,
) -> String {
    let v1 = root.join("v1");
    std::fs::create_dir_all(&v1).expect("create repo dirs");
    write_component_index_v2(&v1, &[component]);

    let script_rel = format!("hooks/{component}/{}.sh", phase.replace('_', "-"));
    let mut manifest = component_manifest_toml(component, version, &["system"]);
    // Hook script is itself a laid-down layout file, mode 0755 so the
    // runner can spawn it.
    manifest.push_str("\n[[component.layout.files]]\n");
    manifest.push_str(&format!(
        "source = \"hook.sh\"\ntarget = \"{{datadir}}/{script_rel}\"\nmode = \"0755\"\n"
    ));
    manifest.push_str("\n[[component.hooks]]\n");
    manifest.push_str(&format!(
        "phase = \"{phase}\"\nscript = \"{{datadir}}/{script_rel}\"\nstrict = {strict}\n"
    ));

    let bin_path = format!("bin/{component}");
    let bin_payload = format!("#!/bin/sh\necho {component}\n");
    let artifact = build_tar_gz(&[
        (".anolisa/component.toml", manifest.as_bytes()),
        (bin_path.as_str(), bin_payload.as_bytes()),
        ("hook.sh", script_body.as_bytes()),
    ]);
    let artifact_name = format!("{component}.tar.gz");
    std::fs::write(v1.join(&artifact_name), &artifact).expect("write artifact");
    let sha = format!("{:x}", Sha256::digest(&artifact));

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
install_modes = ["system"]
sha256 = "{sha}"
"#,
        os = env.os,
        arch = env.arch,
    );
    std::fs::write(v1.join("index.toml"), index).expect("write index");
    format!("file://{}", v1.display())
}

pub struct NoTxn;

impl PackageTransaction for NoTxn {
    fn check_install(&self, _packages: &[&str]) -> Result<(), PackageTransactionError> {
        Ok(())
    }

    fn install(&self, _packages: &[&str]) -> Result<(), PackageTransactionError> {
        panic!("adopt-path test reached a delegated dnf install");
    }
    fn update(&self, _packages: &[&str]) -> Result<(), PackageTransactionError> {
        panic!("adopt-path test reached a dnf update");
    }
    fn reinstall(&self, _packages: &[&str]) -> Result<(), PackageTransactionError> {
        panic!("adopt-path test reached a dnf reinstall");
    }
    fn remove(&self, _packages: &[&str]) -> Result<(), PackageTransactionError> {
        panic!("adopt-path test reached a dnf remove");
    }
}

pub fn handle_one_with_query(
    component: String,
    args: InstallArgs,
    ctx: &CliContext,
    query: &dyn PackageQuery,
) -> Result<InstallOutcome, CliError> {
    let txn = NoTxn;
    install_component_with_deps(&component, &args, ctx, query, &txn, false)
}

/// [`handle_one_with_query`] with injected host facts, for tests pinning
/// package-family-sensitive behavior (deb degrade vs rpm/unknown fail
/// closed) independently of the runner's real /etc/os-release.
pub fn handle_one_with_query_env(
    component: String,
    args: InstallArgs,
    ctx: &CliContext,
    env: &anolisa_env::EnvFacts,
    query: &dyn PackageQuery,
) -> Result<InstallOutcome, CliError> {
    handle_one_with_query_env_rpmdb(component, args, ctx, env, &RpmdbProbe::absent(), query)
}

/// Variant with the host rpmdb evidence probe injected, for tests staging
/// database files under isolated roots instead of the runner's real host.
pub fn handle_one_with_query_env_rpmdb(
    component: String,
    args: InstallArgs,
    ctx: &CliContext,
    env: &anolisa_env::EnvFacts,
    rpmdb: &RpmdbProbe,
    query: &dyn PackageQuery,
) -> Result<InstallOutcome, CliError> {
    let txn = NoTxn;
    install_component_with_deps_and_env(&component, &args, ctx, env, rpmdb, query, &txn, false)
}

/// Load the v5 store the pipeline writes, for post-install assertions.
pub fn load_store(ctx: &CliContext) -> anolisa_core::state_store::StateStore {
    let layout = common::resolve_layout(ctx);
    anolisa_core::state_store::StateStore::load(&layout.state_dir.join("installed.toml"), 0)
        .expect("load store")
}

pub struct FakeInstaller {
    pub package: String,
    /// PackageInfo rpmdb reports after a successful install.
    pub installs_to: PackageInfo,
    pub origin: Option<String>,
    pub available: Vec<PackageInfo>,
    /// `false` makes the dnf install transaction fail.
    pub install_succeeds: bool,
    pub preflight_failure_on_call: Option<usize>,
    pub preflight_calls: Cell<usize>,
    pub installed: RefCell<Option<PackageInfo>>,
    pub install_calls: Cell<usize>,
    /// Package spec(s) each `install` call received, joined per call, so tests
    /// can pin the exact NEVRA a version-pinned install hands to dnf.
    pub install_specs: RefCell<Vec<String>>,
    /// Expected `install` argument (comma-joined). When `None`, the fake
    /// asserts the bare package name; a pinned test sets the expected NEVRA.
    pub expected_install: Option<String>,
    /// Optional lock path probed from inside `install`.
    pub lock_probe: Option<PathBuf>,
    pub lock_was_held: Cell<bool>,
    package_appears_under_lock_path: Option<PathBuf>,
    /// Lock path after which rpm tooling (and the external RPM) exists: every
    /// query fails with `CommandMissing` until the install lock is held, then
    /// behaves as if the package was installed while tooling was appearing.
    tooling_appears_under_lock_path: Option<PathBuf>,
    /// `(lock, rpmdb file)` simulating an external process that installs
    /// RPMs with a binary this process cannot see: queries always fail with
    /// `CommandMissing`, but once the install lock is held the rpmdb file
    /// is written before the failure is returned.
    rpmdb_appears_under_lock: Option<(PathBuf, PathBuf)>,
    /// Optional installed.toml path replaced with a directory after dnf.
    pub block_state_save: Option<PathBuf>,
    pending_race: Option<(PathBuf, PathBuf, String)>,
    pending_race_injected: Cell<bool>,
}

impl FakeInstaller {
    pub fn new(package: &str, installs_to: PackageInfo) -> Self {
        Self {
            package: package.to_string(),
            installs_to,
            origin: None,
            available: Vec::new(),
            install_succeeds: true,
            preflight_failure_on_call: None,
            preflight_calls: Cell::new(0),
            installed: RefCell::new(None),
            install_calls: Cell::new(0),
            install_specs: RefCell::new(Vec::new()),
            expected_install: None,
            lock_probe: None,
            lock_was_held: Cell::new(false),
            package_appears_under_lock_path: None,
            tooling_appears_under_lock_path: None,
            rpmdb_appears_under_lock: None,
            block_state_save: None,
            pending_race: None,
            pending_race_injected: Cell::new(false),
        }
    }
    pub fn with_origin(mut self, repo: &str) -> Self {
        self.origin = Some(repo.to_string());
        self
    }
    /// Repository candidates `query_available` returns for the package, so a
    /// version-pinned install can resolve a concrete NEVRA.
    pub fn with_available(mut self, available: Vec<PackageInfo>) -> Self {
        self.available = available;
        self
    }
    /// Assert `install` receives this exact spec (e.g. a pinned NEVRA) instead
    /// of the bare package name.
    pub fn expecting_install(mut self, spec: &str) -> Self {
        self.expected_install = Some(spec.to_string());
        self
    }
    pub fn failing_install(mut self) -> Self {
        self.install_succeeds = false;
        self
    }
    pub fn expect_lock_held(mut self, path: PathBuf) -> Self {
        self.lock_probe = Some(path);
        self
    }
    pub fn package_appears_under_lock(mut self, path: PathBuf) -> Self {
        self.package_appears_under_lock_path = Some(path);
        self
    }
    /// Simulate a host without rpm tooling at planning time that gains both
    /// the tooling and an external RPM before the install lock is taken.
    pub fn tooling_appears_under_lock(mut self, path: PathBuf) -> Self {
        self.tooling_appears_under_lock_path = Some(path);
        self
    }
    /// Simulate an rpmdb growing behind this process's back: rpm tooling
    /// stays invisible to every query, but under the install lock the
    /// database file appears on disk.
    pub fn rpmdb_appears_under_lock(mut self, lock: PathBuf, rpmdb_file: PathBuf) -> Self {
        self.rpmdb_appears_under_lock = Some((lock, rpmdb_file));
        self
    }
    /// `CommandMissing` while the install lock is free in the
    /// tooling-appears-under-lock mode; `None` otherwise.
    fn tooling_still_missing(&self) -> Option<PackageQueryError> {
        if let Some((lock, rpmdb_file)) = &self.rpmdb_appears_under_lock {
            if matches!(InstallLock::acquire(lock), Err(LockError::Held { .. })) {
                std::fs::create_dir_all(rpmdb_file.parent().expect("rpmdb dir"))
                    .expect("create rpmdb dir");
                std::fs::write(rpmdb_file, b"").expect("write rpmdb file");
            }
            return Some(PackageQueryError::CommandMissing {
                command: "rpm".to_string(),
            });
        }
        let path = self.tooling_appears_under_lock_path.as_ref()?;
        if matches!(InstallLock::acquire(path), Err(LockError::Held { .. })) {
            None
        } else {
            Some(PackageQueryError::CommandMissing {
                command: "rpm".to_string(),
            })
        }
    }
    pub fn failing_state_save(mut self, path: PathBuf) -> Self {
        self.block_state_save = Some(path);
        self
    }

    pub fn injecting_pending_journal(mut self, layout: &FsLayout, subject: &str) -> Self {
        self.pending_race = Some((
            layout.state_dir.join("installed.toml"),
            layout.state_dir.join("journal"),
            subject.to_string(),
        ));
        self
    }

    fn maybe_inject_pending_journal(&self) {
        let Some((state_path, journal_dir, subject)) = &self.pending_race else {
            return;
        };
        if self.pending_race_injected.replace(true) {
            return;
        }
        let journal = Transaction::begin_with_subject(
            "install",
            Some(subject),
            state_path.clone(),
            journal_dir,
        )
        .expect("inject pending journal between preflight and lock");
        drop(journal);
    }

    fn component_capability(&self) -> String {
        rpm_component_provide(&self.package)
    }
}

impl PackageQuery for FakeInstaller {
    fn query_installed(&self, package: &str) -> Result<Option<PackageInfo>, PackageQueryError> {
        self.maybe_inject_pending_journal();
        if let Some(err) = self.tooling_still_missing() {
            return Err(err);
        }
        if package != self.package {
            return Ok(None);
        }
        if self.tooling_appears_under_lock_path.is_some() {
            *self.installed.borrow_mut() = Some(self.installs_to.clone());
        }
        if let Some(path) = &self.package_appears_under_lock_path
            && matches!(InstallLock::acquire(path), Err(LockError::Held { .. }))
        {
            *self.installed.borrow_mut() = Some(self.installs_to.clone());
        }
        Ok(self.installed.borrow().clone())
    }

    fn query_available(&self, package: &str) -> Result<Vec<PackageInfo>, PackageQueryError> {
        if package != self.package {
            return Ok(Vec::new());
        }
        Ok(self.available.clone())
    }

    fn installed_origin(&self, package: &str) -> Result<Option<String>, PackageQueryError> {
        if package != self.package {
            return Ok(None);
        }
        Ok(self.origin.clone())
    }

    fn what_provides_installed(&self, capability: &str) -> Result<Vec<String>, PackageQueryError> {
        if let Some(err) = self.tooling_still_missing() {
            return Err(err);
        }
        if capability == self.component_capability() && self.installed.borrow().is_some() {
            Ok(vec![self.package.clone()])
        } else {
            Ok(Vec::new())
        }
    }

    fn what_provides_available(&self, capability: &str) -> Result<Vec<String>, PackageQueryError> {
        if let Some(err) = self.tooling_still_missing() {
            return Err(err);
        }
        if capability == self.component_capability() {
            Ok(vec![self.package.clone()])
        } else {
            Ok(Vec::new())
        }
    }

    fn provided_capabilities_installed(
        &self,
        package: &str,
    ) -> Result<Vec<String>, PackageQueryError> {
        if package == self.package && self.installed.borrow().is_some() {
            Ok(vec![self.component_capability()])
        } else {
            Ok(Vec::new())
        }
    }

    fn provided_capabilities_available(
        &self,
        package: &str,
    ) -> Result<Vec<String>, PackageQueryError> {
        if package == self.package {
            Ok(vec![self.component_capability()])
        } else {
            Ok(Vec::new())
        }
    }
}

impl PackageTransaction for FakeInstaller {
    fn check_install(&self, packages: &[&str]) -> Result<(), PackageTransactionError> {
        self.preflight_calls.set(self.preflight_calls.get() + 1);
        assert_eq!(
            packages,
            &[self.expected_install.as_deref().unwrap_or(&self.package)]
        );
        if self.preflight_failure_on_call == Some(self.preflight_calls.get()) {
            return Err(PackageTransactionError::TransactionFailed {
                command: "dnf".to_string(),
                operation: "install preflight".to_string(),
                code: Some(1),
                stderr: "installed cosh-ng conflicts with copilot-shell".to_string(),
            });
        }
        Ok(())
    }

    fn install(&self, packages: &[&str]) -> Result<(), PackageTransactionError> {
        self.install_calls.set(self.install_calls.get() + 1);
        self.install_specs.borrow_mut().push(packages.join(","));
        let expected = self
            .expected_install
            .clone()
            .unwrap_or_else(|| self.package.clone());
        assert_eq!(
            packages.join(","),
            expected,
            "install targeted the wrong package/spec"
        );
        if let Some(path) = &self.lock_probe {
            self.lock_was_held.set(matches!(
                InstallLock::acquire(path),
                Err(LockError::Held { .. })
            ));
        }
        if !self.install_succeeds {
            return Err(PackageTransactionError::TransactionFailed {
                command: "dnf".to_string(),
                operation: "install".to_string(),
                code: Some(1),
                stderr: "No match for argument".to_string(),
            });
        }
        // rpmdb now holds the package, modelling dnf placing it.
        *self.installed.borrow_mut() = Some(self.installs_to.clone());
        if let Some(path) = &self.block_state_save {
            std::fs::create_dir_all(path).expect("block installed.toml save");
        }
        Ok(())
    }
    fn update(&self, _packages: &[&str]) -> Result<(), PackageTransactionError> {
        panic!("delegated-install test must not run a dnf update");
    }
    fn reinstall(&self, _packages: &[&str]) -> Result<(), PackageTransactionError> {
        panic!("delegated-install test must not run a dnf reinstall");
    }
    fn remove(&self, _packages: &[&str]) -> Result<(), PackageTransactionError> {
        panic!("delegated-install test must not run a dnf remove");
    }
}

#[derive(Default)]
pub struct FakeQuery {
    pub installed: Vec<(String, PackageInfo)>,
    pub origins: Vec<(String, String)>,
    pub provides: Vec<(String, Vec<String>)>,
    pub available_provides: Vec<(String, Vec<String>)>,
    pub package_provides: Vec<(String, Vec<String>)>,
    pub available_package_provides: Vec<(String, Vec<String>)>,
    pub unexpected_installed: Vec<(String, String)>,
    pub multi_version: Vec<String>,
    pub origin_fails: bool,
    /// Simulate a host with no rpm/dnf: every rpmdb-touching query returns
    /// [`PackageQueryError::CommandMissing`], exercising the probe's
    /// warn-and-exit guard.
    pub command_missing: bool,
}

impl PackageQuery for FakeQuery {
    fn query_installed(&self, package: &str) -> Result<Option<PackageInfo>, PackageQueryError> {
        if self.command_missing {
            return Err(PackageQueryError::CommandMissing {
                command: "rpm".to_string(),
            });
        }
        if let Some((_, detail)) = self
            .unexpected_installed
            .iter()
            .find(|(name, _)| name == package)
        {
            return Err(PackageQueryError::UnexpectedOutput {
                command: "rpm".to_string(),
                detail: detail.clone(),
            });
        }
        if self.multi_version.iter().any(|p| p == package) {
            return Err(PackageQueryError::UnexpectedOutput {
                command: "rpm".to_string(),
                detail: "2 installed versions".to_string(),
            });
        }
        Ok(self
            .installed
            .iter()
            .find(|(n, _)| n == package)
            .map(|(_, info)| info.clone()))
    }

    fn query_available(&self, _package: &str) -> Result<Vec<PackageInfo>, PackageQueryError> {
        Ok(Vec::new())
    }

    fn installed_origin(&self, package: &str) -> Result<Option<String>, PackageQueryError> {
        if self.origin_fails {
            return Err(PackageQueryError::QueryFailed {
                command: "dnf".to_string(),
                code: Some(1),
                stderr: "boom".to_string(),
            });
        }
        Ok(self
            .origins
            .iter()
            .find(|(n, _)| n == package)
            .map(|(_, repo)| repo.clone()))
    }

    fn what_provides_installed(&self, capability: &str) -> Result<Vec<String>, PackageQueryError> {
        if self.command_missing {
            return Err(PackageQueryError::CommandMissing {
                command: "rpm".to_string(),
            });
        }
        // Same convention as `provided_capabilities_installed`: unless a
        // fixture says otherwise, every installed package provides
        // `anolisa-component(<its own name>)`.
        Ok(self
            .provides
            .iter()
            .find(|(cap, _)| cap == capability)
            .map(|(_, names)| names.clone())
            .or_else(|| {
                self.installed
                    .iter()
                    .map(|(name, _)| name)
                    .find(|name| rpm_component_provide(name) == capability)
                    .map(|name| vec![name.clone()])
            })
            .unwrap_or_default())
    }

    fn what_provides_available(&self, capability: &str) -> Result<Vec<String>, PackageQueryError> {
        if self.command_missing {
            return Err(PackageQueryError::CommandMissing {
                command: "dnf".to_string(),
            });
        }
        Ok(self
            .available_provides
            .iter()
            .find(|(cap, _)| cap == capability)
            .map(|(_, names)| names.clone())
            .unwrap_or_default())
    }

    fn provided_capabilities_installed(
        &self,
        package: &str,
    ) -> Result<Vec<String>, PackageQueryError> {
        if self.command_missing {
            return Err(PackageQueryError::CommandMissing {
                command: "rpm".to_string(),
            });
        }
        Ok(self
            .package_provides
            .iter()
            .find(|(pkg, _)| pkg == package)
            .map(|(_, capabilities)| capabilities.clone())
            .or_else(|| {
                self.installed
                    .iter()
                    .any(|(name, _)| name == package)
                    .then(|| vec![rpm_component_provide(package)])
            })
            .unwrap_or_default())
    }

    fn provided_capabilities_available(
        &self,
        package: &str,
    ) -> Result<Vec<String>, PackageQueryError> {
        if self.command_missing {
            return Err(PackageQueryError::CommandMissing {
                command: "dnf".to_string(),
            });
        }
        Ok(self
            .available_package_provides
            .iter()
            .find(|(pkg, _)| pkg == package)
            .map(|(_, capabilities)| capabilities.clone())
            .unwrap_or_default())
    }
}

pub fn pkg_info(name: &str, version: &str, release: Option<&str>, arch: &str) -> PackageInfo {
    PackageInfo {
        name: name.to_string(),
        version: PackageVersion {
            epoch: None,
            version: version.to_string(),
            release: release.map(str::to_string),
        },
        arch: arch.to_string(),
        origin: None,
    }
}

/// Host architecture the install pipeline resolves against, so version-pinned
/// tests build candidates the running host actually accepts (aarch64 on this
/// CI, x86_64 elsewhere) instead of hard-coding one arch.
pub fn host_arch() -> String {
    anolisa_env::EnvService::detect().arch
}

/// A repository candidate for a version-pinned availability query, carrying an
/// origin (source repo) so the resolved-candidate reporting has a value.
pub fn available_candidate(
    name: &str,
    epoch: Option<&str>,
    version: &str,
    release: &str,
    arch: &str,
    origin: &str,
) -> PackageInfo {
    PackageInfo {
        name: name.to_string(),
        version: PackageVersion {
            epoch: epoch.map(str::to_string),
            version: version.to_string(),
            release: Some(release.to_string()),
        },
        arch: arch.to_string(),
        origin: Some(origin.to_string()),
    }
}

pub fn system_ctx_with_raw_repo(dry_run: bool) -> (tempfile::TempDir, CliContext) {
    let tmp = tempdir().expect("tmpdir");
    let prefix = tmp.path().to_path_buf();
    let layout = FsLayout::system(Some(prefix.clone()));
    std::fs::create_dir_all(&layout.state_dir).expect("state dir");
    // The raw backend points at a local index repo so identity resolution
    // can validate the standard fixture components offline.
    seed_repo_config_with_index(&layout, TEST_INDEX_COMPONENTS);
    let mut ctx = ctx_with_prefix(false, Some(prefix));
    ctx.dry_run = dry_run;
    (tmp, ctx)
}

pub fn system_ctx_with_configured_rpm_repo(dry_run: bool) -> (tempfile::TempDir, CliContext) {
    let tmp = tempdir().expect("tmpdir");
    let prefix = tmp.path().to_path_buf();
    let layout = FsLayout::system(Some(prefix.clone()));
    std::fs::create_dir_all(&layout.state_dir).expect("state dir");
    let v1 = layout.etc_dir.join("test-index-repo").join("v1");
    write_component_index_v2(&v1, TEST_INDEX_COMPONENTS);
    std::fs::write(
        layout.etc_dir.join("repo.toml"),
        format!(
            r#"schema_version = 1
default_backend = "raw"

[backends.raw]
base_url = "file://{}"

[backends.rpm]
base_url = "https://repo.example/anolisa"
gpgcheck = false
"#,
            v1.display()
        ),
    )
    .expect("write repo.toml");
    let mut ctx = ctx_with_prefix(false, Some(prefix));
    ctx.dry_run = dry_run;
    (tmp, ctx)
}

pub fn repo_with_rpm_map(pairs: &[(&str, &str)]) -> RepoConfig {
    let mut map = String::new();
    for (k, v) in pairs {
        map.push_str(&format!("{k} = \"{v}\"\n"));
    }
    RepoConfig::from_toml_str(&format!(
        "schema_version = 1\ndefault_backend = \"rpm\"\n[backends.rpm]\nbase_url = \"https://e/x\"\n[backends.rpm.package_map]\n{map}"
    ))
    .expect("parse repo")
}

pub fn linux_env() -> anolisa_env::EnvFacts {
    anolisa_env::EnvFacts {
        os: "linux".to_string(),
        arch: "x86_64".to_string(),
        libc: Some("glibc".to_string()),
        kernel: Some("5.10.0".to_string()),
        pkg_base: Some("alinux4".to_string()),
        os_id: Some("alinux".to_string()),
        os_id_like: None,
        os_version: Some("4".to_string()),
        btf: Some(true),
        cap_bpf: Some(true),
        container: None,
        user: "root".to_string(),
        uid: 0,
        home: PathBuf::from("/root"),
    }
}

/// Host facts for a deb-family host (Ubuntu), with `os`/`arch` matching the
/// running machine so raw artifact entries built by the repo fixtures still
/// resolve. Injected into planning to cover the rpm-tooling degrade branch
/// deterministically on any CI runner.
pub fn deb_host_env() -> anolisa_env::EnvFacts {
    anolisa_env::EnvFacts {
        pkg_base: None,
        os_id: Some("ubuntu".to_string()),
        os_id_like: Some("debian".to_string()),
        os_version: Some("24.04".to_string()),
        ..anolisa_env::EnvService::detect()
    }
}

/// Host facts for an rpm-family host (alinux), `os`/`arch` matching the
/// running machine. The fail-closed counterpart of [`deb_host_env`].
pub fn rpm_host_env() -> anolisa_env::EnvFacts {
    anolisa_env::EnvFacts {
        pkg_base: Some("alinux4".to_string()),
        os_id: Some("alinux".to_string()),
        os_id_like: None,
        os_version: Some("4".to_string()),
        ..anolisa_env::EnvService::detect()
    }
}

/// Host facts for a macOS/aarch64 target, so raw resolution can select a
/// macOS index entry deterministically on a Linux CI runner. macOS carries
/// no libc or pkg_base selector, and no BPF facts.
pub fn macos_arm_env() -> anolisa_env::EnvFacts {
    anolisa_env::EnvFacts {
        os: "macos".to_string(),
        arch: "aarch64".to_string(),
        libc: None,
        pkg_base: None,
        os_id: Some("macos".to_string()),
        os_id_like: None,
        os_version: Some("15.0".to_string()),
        btf: None,
        cap_bpf: None,
        ..anolisa_env::EnvService::detect()
    }
}

/// Host facts for a distro outside the known rpm/deb families; missing rpm
/// tooling must fail closed here.
pub fn unknown_host_env() -> anolisa_env::EnvFacts {
    anolisa_env::EnvFacts {
        pkg_base: None,
        os_id: Some("alpine".to_string()),
        os_id_like: Some("musl".to_string()),
        os_version: None,
        ..anolisa_env::EnvService::detect()
    }
}

pub fn available_component_provider(component: &str, package: &str) -> (String, Vec<String>) {
    (rpm_component_provide(component), vec![package.to_string()])
}

pub fn package_component_provide(package: &str, component: &str) -> (String, Vec<String>) {
    (
        package.to_string(),
        vec![
            format!("{package} = 1.0.0"),
            rpm_component_provide(component),
        ],
    )
}

pub fn target(component: &str, package: &str) -> RpmTarget {
    RpmTarget {
        component: component.to_string(),
        package: package.to_string(),
    }
}

pub fn seed_datadir_contract(layout: &FsLayout, component: &str, toml: &str) {
    let dir = layout.datadir.join("components").join(component);
    std::fs::create_dir_all(&dir).expect("create datadir component dir");
    std::fs::write(dir.join("component.toml"), toml).expect("write datadir contract");
}

mod contract;
mod raw_e2e;
mod rpm;
