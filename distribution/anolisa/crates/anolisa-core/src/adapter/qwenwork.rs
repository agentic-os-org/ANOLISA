//! QwenWorkCN macOS sideloading with receipt-owned file copies.
//!
//! Desktop activation is deliberately unknown: copying files cannot prove that
//! the running application loaded or enabled a plugin.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use sha2::{Digest, Sha256};

use super::AdapterError;
use super::claim::{
    AdapterClaim, CLAIM_SCHEMA_VERSION, ClaimResource, ClaimResourceKind, ClaimStatus,
    DRIVER_SCHEMA_VERSION, DriverPayload, ManagedFileKind, MaterializedFile, QwenWorkClaim,
    validate_plugin_id,
};
use super::driver::{
    AdapterBundle, AdapterCondition, AdapterConditionKind, AdapterStatusReport, AdapterSummary,
    ConditionStatus, DeclaredSkill, DetectResult, DisableReport, DriverCtx, DriverPlan,
    EnableProgress, FrameworkCommand, FrameworkDriver, HostEnv, PreparedEnable,
    find_binary_in_path, is_executable,
};
use super::managed_files::{MaterializedMapping, copy_materialized_resource};
use super::util::{bool_status, now_iso8601};
use crate::manifest::{AdapterNotice, NoticeLevel, NoticeWhen};
use crate::path_safety::canonicalize_nearest_existing;

const RES_PLUGIN: &str = "qwenwork_plugin";
const RESTART: &str = "Restart QwenWorkCN and open a new session; check plugin activation in the application. Loading and activation are not verified.";
const RUNTIME_FILES: &[&str] = &[
    ".qoder-plugin/plugin.json",
    "hooks/hooks.json",
    "hooks/run-hook.sh",
    "hooks/shared/rewrite_hook.py",
    "hooks/shared/compress_response_hook.py",
    "hooks/shared/hook_utils.py",
    "hooks/shared/tool_categories.json",
];

/// Manage QwenWorkCN plugins without modifying desktop settings or permissions.
pub struct QwenWorkDriver;

impl QwenWorkDriver {
    /// Construct the stateless driver.
    pub fn new() -> Self {
        Self
    }
}

impl Default for QwenWorkDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameworkDriver for QwenWorkDriver {
    fn name(&self) -> &'static str {
        "qwenwork"
    }

    fn detect(&self, env: &HostEnv) -> DetectResult {
        match application(env.user_home.as_deref()) {
            Ok((path, version)) => DetectResult {
                detected: true,
                reason: format!(
                    "QwenWorkCN {version} at {}; macOS native Bash and workspace Shell",
                    path.display()
                ),
            },
            Err(reason) => DetectResult {
                detected: false,
                reason,
            },
        }
    }

    fn probe_bundle(&self, root: &Path, _entry: Option<&str>) -> bool {
        RUNTIME_FILES.iter().all(|file| root.join(file).is_file())
    }

    fn allowed_external_roots(&self, ctx: &DriverCtx) -> Vec<PathBuf> {
        data_home(ctx.user_home.as_deref()).into_iter().collect()
    }

    fn read_bundle(&self, ctx: &DriverCtx) -> Result<AdapterBundle, AdapterError> {
        for file in RUNTIME_FILES {
            let path = ctx.resource_root.join(file);
            if !path.is_file() {
                return Err(invalid(
                    &ctx.resource_root,
                    format!("Missing QwenWork runtime resource: {}", path.display()),
                ));
            }
        }
        let manifest: serde_json::Value = read_json(&ctx.resource_root.join(RUNTIME_FILES[0]))?;
        let plugin = manifest["name"]
            .as_str()
            .ok_or_else(|| invalid(&ctx.resource_root, "QwenWork manifest has no name"))?;
        validate_plugin_id(plugin)?;
        if ctx
            .declared_plugin_id
            .as_deref()
            .is_some_and(|declared| declared != plugin)
            || !manifest["version"]
                .as_str()
                .is_some_and(|version| !version.trim().is_empty() && !version.contains('@'))
        {
            return Err(invalid(
                &ctx.resource_root,
                "QwenWork manifest name/version does not match the stamped adapter",
            ));
        }
        read_json(&ctx.resource_root.join("hooks/hooks.json"))?;
        read_json(&ctx.resource_root.join("hooks/shared/tool_categories.json"))?;
        Ok(AdapterBundle {
            resource_root: ctx.resource_root.clone(),
            plugin_id: Some(plugin.to_string()),
        })
    }

    fn plan_enable(
        &self,
        bundle: &AdapterBundle,
        ctx: &DriverCtx,
    ) -> Result<DriverPlan, AdapterError> {
        let mut actions = preflight(ctx)?;
        let target = destination(bundle, ctx)?;
        actions.push(format!("sideload receipt-owned files to {}; refuse an existing installation without a CLI receipt (uninstall script installations first)", target.display()));
        actions.push(RESTART.to_string());
        Ok(DriverPlan {
            framework: self.name().to_string(),
            component: ctx.component.clone(),
            actions,
            register_command: None,
        })
    }

    fn prepare_enable(
        &self,
        bundle: &AdapterBundle,
        ctx: &DriverCtx,
    ) -> Result<(AdapterClaim, PreparedEnable), AdapterError> {
        let warnings = preflight(ctx)?;
        let path = destination(bundle, ctx)?;
        Ok((
            AdapterClaim {
                claim_schema: CLAIM_SCHEMA_VERSION,
                component: ctx.component.clone(),
                framework: self.name().to_string(),
                plugin_id: bundle.plugin_id.clone(),
                adapter_type: ctx.adapter_type.clone(),
                enabled_at: now_iso8601(),
                resource_root: bundle.resource_root.clone(),
                bundle_digest: None,
                source_revision: None,
                materialized_files: Vec::new(),
                driver_schema: DRIVER_SCHEMA_VERSION,
                status: ClaimStatus::CleanupFailed,
                notices: Vec::new(),
                resources: vec![ClaimResource {
                    id: RES_PLUGIN.to_string(),
                    purpose: "qwenwork_plugin_dir".to_string(),
                    kind: ClaimResourceKind::ExternalPath { path },
                }],
                driver_payload: DriverPayload::QwenWork(QwenWorkClaim {
                    plugin_resource: RES_PLUGIN.to_string(),
                    installation_started: false,
                    previous_files: Vec::new(),
                }),
            },
            PreparedEnable::QwenWork { warnings },
        ))
    }

    fn materialized_mappings(
        &self,
        root: &Path,
        _adapter_type: Option<&str>,
        _skills: &[DeclaredSkill],
    ) -> Vec<MaterializedMapping> {
        vec![MaterializedMapping {
            resource_id: RES_PLUGIN.to_string(),
            source_root: root.to_path_buf(),
            // Legacy lifecycle scripts and their ownership marker must not manage CLI copies.
            excluded_prefixes: vec![
                PathBuf::from("scripts"),
                PathBuf::from(".tokenless-qwenwork-owned.json"),
            ],
        }]
    }

    fn materialized_destination_roots(
        &self,
        bundle: &AdapterBundle,
        ctx: &DriverCtx,
    ) -> Result<BTreeMap<String, PathBuf>, AdapterError> {
        Ok(BTreeMap::from([(
            RES_PLUGIN.to_string(),
            destination(bundle, ctx)?,
        )]))
    }

    fn materialized_verification_applicable(&self, _claim: &AdapterClaim) -> bool {
        true
    }

    fn preserve_reenable_facts(
        &self,
        prior: &AdapterClaim,
        next: &mut AdapterClaim,
    ) -> Result<(), AdapterError> {
        let (old_root, old) = receipt(prior)?;
        let (new_root, _) = receipt(next)?;
        if old_root != new_root {
            return Err(invalid(
                new_root,
                "QwenWork destination changed; disable with the original TOKENLESS_QWENWORK_HOME before enabling at another location",
            ));
        }
        verify_owned(prior, prior.status == ClaimStatus::CleanupFailed)?;
        let mut retained = old.previous_files.clone();
        for file in &prior.materialized_files {
            if !retained.contains(file) {
                retained.push(file.clone());
            }
        }
        if let DriverPayload::QwenWork(payload) = &mut next.driver_payload {
            payload.installation_started = old.installation_started;
            payload.previous_files = retained;
        }
        Ok(())
    }

    fn plan_reenable_cleanup(
        &self,
        prior: &AdapterClaim,
        ctx: &DriverCtx,
    ) -> Result<Vec<String>, AdapterError> {
        ensure_current_home(prior, ctx)?;
        verify_owned(prior, prior.status == ClaimStatus::CleanupFailed)?;
        Ok(Vec::new())
    }

    fn validate_prepared_enable(&self, claim: &AdapterClaim) -> Result<(), AdapterError> {
        let (root, payload) = receipt(claim)?;
        for required in RUNTIME_FILES {
            if !claim.materialized_files.iter().any(|file| {
                file.resource_id == RES_PLUGIN && file.relative_path == Path::new(required)
            }) {
                return Err(invalid(
                    root,
                    format!("Required QwenWork resource is not package-owned: {required}"),
                ));
            }
        }
        if !payload.installation_started && exists(root)? {
            return Err(invalid(
                root,
                "QwenWork plugin already exists without CLI ownership; for a Tokenless script installation, run its scripts/uninstall.sh first, then retry anolisa adapter enable tokenless qwenwork",
            ));
        }
        // A new package file may collide with an unrelated desktop-created file.
        for file in &claim.materialized_files {
            let path = root.join(&file.relative_path);
            check_parents(root, &path)?;
            if exists(&path)?
                && !payload
                    .previous_files
                    .iter()
                    .any(|old| old.relative_path == file.relative_path)
            {
                return Err(invalid(
                    &path,
                    "Refusing to overwrite an unowned QwenWork file",
                ));
            }
        }
        Ok(())
    }

    fn apply_enable(
        &self,
        claim: &mut AdapterClaim,
        prepared: &PreparedEnable,
        ctx: &DriverCtx,
        progress: &mut dyn EnableProgress,
    ) -> Result<(), AdapterError> {
        ensure_current_home(claim, ctx)?;
        self.validate_prepared_enable(claim)?;
        verify_owned(claim, true)?;
        // Publish copy intent before any files, preserving both revisions until completion.
        if let DriverPayload::QwenWork(payload) = &mut claim.driver_payload {
            payload.installation_started = true;
        }
        progress.persist_claim(claim)?;
        copy_materialized_resource(claim, RES_PLUGIN, &claim.resource_root, ctx.ops)?;
        if let DriverPayload::QwenWork(payload) = &mut claim.driver_payload {
            payload.previous_files.clear();
        }
        claim.status = ClaimStatus::Enabled;
        // Older installed component manifests have no desktop lifecycle notices.
        for when in [NoticeWhen::PostEnable, NoticeWhen::PostDisable] {
            claim.notices.push(AdapterNotice {
                when,
                level: NoticeLevel::Warning,
                text: RESTART.to_string(),
                command: None,
            });
        }
        if let PreparedEnable::QwenWork { warnings } = prepared {
            claim
                .notices
                .extend(warnings.iter().map(|text| AdapterNotice {
                    when: NoticeWhen::PostEnable,
                    level: NoticeLevel::Warning,
                    text: text.clone(),
                    command: None,
                }));
        }
        Ok(())
    }

    fn status(
        &self,
        claim: &AdapterClaim,
        ctx: &DriverCtx,
    ) -> Result<AdapterStatusReport, AdapterError> {
        ensure_current_home(claim, ctx)?;
        let detected = self.detect(&HostEnv {
            user_home: ctx.user_home.clone(),
        });
        let (root, _) = receipt(claim)?;
        let present = root.is_dir();
        Ok(AdapterStatusReport {
            summary: if claim.status == ClaimStatus::CleanupFailed {
                AdapterSummary::CleanupFailed
            } else if !detected.detected || !present {
                AdapterSummary::Degraded
            } else {
                AdapterSummary::Unknown
            },
            conditions: vec![
                AdapterCondition {
                    kind: AdapterConditionKind::FrameworkDetected,
                    status: bool_status(detected.detected),
                    reason: Some(detected.reason),
                    resource: None,
                },
                AdapterCondition {
                    kind: AdapterConditionKind::TreePresent,
                    status: bool_status(present),
                    reason: Some(root.display().to_string()),
                    resource: None,
                },
                AdapterCondition {
                    kind: AdapterConditionKind::PluginResourcesLoaded,
                    status: ConditionStatus::Unknown,
                    reason: Some(RESTART.to_string()),
                    resource: None,
                },
                AdapterCondition {
                    kind: AdapterConditionKind::ActivationEnabled,
                    status: ConditionStatus::Unknown,
                    reason: Some("Desktop activation is not inspected or changed".to_string()),
                    resource: None,
                },
                AdapterCondition {
                    kind: AdapterConditionKind::VerificationSupported,
                    status: ConditionStatus::False,
                    reason: Some(
                        "Only application presence and receipt-owned files can be verified"
                            .to_string(),
                    ),
                    resource: None,
                },
            ],
        })
    }

    fn disable(
        &self,
        claim: &AdapterClaim,
        ctx: &DriverCtx,
    ) -> Result<DisableReport, AdapterError> {
        ensure_current_home(claim, ctx)?;
        let (root, payload) = receipt(claim)?;
        let mut report = DisableReport {
            cleanup_complete: true,
            messages: vec![RESTART.to_string()],
        };
        if !payload.installation_started {
            return Ok(report);
        }
        let mut directories = BTreeSet::new();
        for (relative, versions) in owned_versions(claim)? {
            let path = root.join(&relative);
            let checked =
                check_parents(root, &path).and_then(|()| matches_versions(&path, &versions, true));
            if let Err(error) = checked {
                report.cleanup_complete = false;
                report.messages.push(error.to_string());
                continue;
            }
            if !ctx.dry_run {
                ctx.ops.remove_path(&path)?;
            }
            let mut parent = path.parent();
            while let Some(dir) = parent.filter(|dir| dir.starts_with(root)) {
                directories.insert(dir.to_path_buf());
                parent = dir.parent();
            }
        }
        if !ctx.dry_run {
            let mut directories: Vec<_> = directories.into_iter().collect();
            directories.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
            for dir in directories {
                if dir.is_dir()
                    && std::fs::read_dir(&dir)
                        .map_err(|source| AdapterError::Io {
                            path: dir.clone(),
                            source,
                        })?
                        .next()
                        .is_none()
                {
                    ctx.ops.remove_path(&dir)?;
                }
            }
        }
        Ok(report)
    }
}

fn invalid(path: &Path, reason: impl Into<String>) -> AdapterError {
    AdapterError::BundleInvalid {
        root: path.to_path_buf(),
        reason: reason.into(),
    }
}

fn read_json(path: &Path) -> Result<serde_json::Value, AdapterError> {
    let bytes = std::fs::read(path).map_err(|source| AdapterError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|e| invalid(path, format!("Invalid JSON: {e}")))?;
    if !value.is_object() {
        return Err(invalid(path, "Expected a JSON object"));
    }
    Ok(value)
}

fn absolute(path: PathBuf, home: Option<&Path>) -> Option<PathBuf> {
    let path = if let Ok(suffix) = path.strip_prefix("~") {
        home?.join(suffix)
    } else {
        path
    };
    (path.is_absolute() && !path.components().any(|p| matches!(p, Component::ParentDir)))
        .then_some(path)
}

fn data_home(home: Option<&Path>) -> Option<PathBuf> {
    let path = std::env::var_os("TOKENLESS_QWENWORK_HOME")
        .map(PathBuf::from)
        .or_else(|| home.map(|h| h.join(".qwenworkcn")))?;
    let path = absolute(path, home)?;
    // Canonicalize only the trusted parent; the data root itself must not be a symlink.
    Some(canonicalize_nearest_existing(path.parent()?)?.join(path.file_name()?))
}

fn application(home: Option<&Path>) -> Result<(PathBuf, String), String> {
    if !cfg!(target_os = "macos") {
        return Err(
            "QwenWorkCN sideloading supports macOS native Bash and workspace Shell".to_string(),
        );
    }
    let candidates = match std::env::var_os("TOKENLESS_QWENWORK_APP") {
        Some(value) => vec![
            absolute(PathBuf::from(value), home)
                .ok_or("TOKENLESS_QWENWORK_APP must be absolute without '..'")?,
        ],
        None => std::iter::once(PathBuf::from("/Applications/QwenWorkCN.app"))
            .chain(home.map(|h| h.join("Applications/QwenWorkCN.app")))
            .collect(),
    };
    let app = candidates
        .iter()
        .find(|p| p.is_dir())
        .ok_or_else(|| format!("QwenWorkCN.app not found; checked {candidates:?}"))?;
    let version = application_version(&app.join("Contents/Info.plist"))?;
    Ok((app.clone(), version))
}

fn application_version(path: &Path) -> Result<String, String> {
    let info = plist::Value::from_file(path)
        .map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
    let dictionary = info
        .as_dictionary()
        .ok_or("Expected application plist dictionary")?;
    if dictionary
        .get("CFBundleIdentifier")
        .and_then(plist::Value::as_string)
        != Some("cn.qwenwork.desktop.mac")
    {
        return Err(format!(
            "Unexpected bundle identifier in {}; expected cn.qwenwork.desktop.mac",
            path.display()
        ));
    }
    dictionary
        .get("CFBundleShortVersionString")
        .and_then(plist::Value::as_string)
        .filter(|v| !v.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("Missing application version in {}", path.display()))
}

fn destination(bundle: &AdapterBundle, ctx: &DriverCtx) -> Result<PathBuf, AdapterError> {
    let home = data_home(ctx.user_home.as_deref()).ok_or_else(|| {
        invalid(
            &bundle.resource_root,
            "QwenWork requires an absolute data directory and user home",
        )
    })?;
    let plugin = bundle
        .plugin_id
        .as_deref()
        .ok_or_else(|| invalid(&bundle.resource_root, "QwenWork plugin id missing"))?;
    validate_plugin_id(plugin)?;
    let target = home.join("plugins-custom").join(plugin);
    check_parents(&home, &target.join("probe"))?;
    Ok(target)
}

fn preflight(ctx: &DriverCtx) -> Result<Vec<String>, AdapterError> {
    let (_, version) =
        application(ctx.user_home.as_deref()).map_err(|e| invalid(&ctx.resource_root, e))?;
    let mut warnings = Vec::new();
    if !matches!(version.as_str(), "1.2.5" | "1.3.2") {
        warnings.push(format!("QwenWorkCN {version} is outside the 1.2.5 native Bash / 1.3.2 workspace Shell baselines; compatibility is unverified."));
    }
    for name in ["bash", "tokenless"] {
        if binary(name, ctx).is_none() {
            return Err(invalid(
                &ctx.resource_root,
                format!("{name} binary not found; install it before enabling QwenWork"),
            ));
        }
    }
    let python = binary("python3", ctx)
        .ok_or_else(|| invalid(&ctx.resource_root, "Python 3.9+ is required for QwenWork"))?;
    let output = ctx.ops.run_framework_cli(FrameworkCommand {
        program: python.to_string_lossy().into_owned(),
        args: vec!["-I".to_string(), "-B".to_string(), "-c".to_string(), "import sys; print('%d.%d' % sys.version_info[:2]); sys.exit(sys.version_info < (3, 9))".to_string()],
        stdin: None, env_set: Vec::new(), env_remove: Vec::new(), path_prepend: Vec::new(), timeout: Duration::from_secs(5),
    })?;
    if !output.success() {
        return Err(invalid(
            &ctx.resource_root,
            "Python 3.9+ is required for QwenWork; version probe failed",
        ));
    }
    if binary("rtk", ctx).is_none() {
        warnings.push("RTK not found; command rewriting is unavailable, response compression remains available.".to_string());
    }
    Ok(warnings)
}

fn binary(name: &str, ctx: &DriverCtx) -> Option<PathBuf> {
    if let Some(found) = find_binary_in_path(name) {
        return Some(found);
    }
    let mut candidates = vec![ctx.layout.bin_dir.join(name)];
    if let Some(home) = &ctx.user_home {
        for dir in [
            ".local/bin",
            ".local/lib/anolisa/libexec/tokenless",
            ".local/libexec/anolisa/tokenless",
        ] {
            candidates.push(home.join(dir).join(name));
        }
    }
    for dir in [
        "/usr/local/bin",
        "/usr/local/libexec/anolisa/tokenless",
        "/usr/bin",
        "/usr/libexec/anolisa/tokenless",
        "/usr/lib/anolisa/tokenless",
    ] {
        candidates.push(Path::new(dir).join(name));
    }
    candidates
        .into_iter()
        .find(|p| p.is_file() && is_executable(p))
}

fn receipt(claim: &AdapterClaim) -> Result<(&Path, &QwenWorkClaim), AdapterError> {
    if let DriverPayload::QwenWork(payload) = &claim.driver_payload
        && let Some(resource) = claim.resource(&payload.plugin_resource)
        && let ClaimResourceKind::ExternalPath { path } = &resource.kind
        && let Some(plugin) = &claim.plugin_id
    {
        validate_plugin_id(plugin)?;
        if payload.plugin_resource == RES_PLUGIN
            && path.file_name().is_some_and(|v| v == plugin.as_str())
            && path
                .parent()
                .and_then(Path::file_name)
                .is_some_and(|v| v == "plugins-custom")
        {
            return Ok((path, payload));
        }
    }
    Err(invalid(
        &claim.resource_root,
        "Invalid QwenWork plugin receipt",
    ))
}

fn ensure_current_home(claim: &AdapterClaim, ctx: &DriverCtx) -> Result<(), AdapterError> {
    let (root, _) = receipt(claim)?;
    let current = data_home(ctx.user_home.as_deref())
        .ok_or_else(|| invalid(root, "Cannot resolve QwenWork data directory"))?;
    if root.parent().and_then(Path::parent) != Some(current.as_path()) {
        return Err(invalid(
            root,
            "TOKENLESS_QWENWORK_HOME differs from receipt; restore the original setting before status, disable, or re-enable",
        ));
    }
    check_parents(&current, &root.join("probe"))
}

fn exists(path: &Path) -> Result<bool, AdapterError> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(AdapterError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn check_parents(root: &Path, path: &Path) -> Result<(), AdapterError> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| invalid(path, "QwenWork path escaped plugin root"))?;
    let mut current = root.to_path_buf();
    for part in std::iter::once(None).chain(
        relative
            .parent()
            .into_iter()
            .flat_map(Path::components)
            .map(Some),
    ) {
        if let Some(part) = part {
            if !matches!(part, Component::Normal(_)) {
                return Err(invalid(path, "Invalid relative QwenWork path"));
            }
            current.push(part);
        }
        if exists(&current)?
            && !std::fs::symlink_metadata(&current)
                .map_err(|source| AdapterError::Io {
                    path: current.clone(),
                    source,
                })?
                .is_dir()
        {
            return Err(invalid(
                &current,
                "Refusing a symlink or non-directory in the QwenWork managed path",
            ));
        }
    }
    Ok(())
}

fn owned_versions(
    claim: &AdapterClaim,
) -> Result<BTreeMap<PathBuf, Vec<&MaterializedFile>>, AdapterError> {
    let (root, payload) = receipt(claim)?;
    let mut files = BTreeMap::<PathBuf, Vec<&MaterializedFile>>::new();
    for file in payload
        .previous_files
        .iter()
        .chain(&claim.materialized_files)
    {
        if file.resource_id != RES_PLUGIN
            || file.relative_path.as_os_str().is_empty()
            || !file
                .relative_path
                .components()
                .all(|p| matches!(p, Component::Normal(_)))
            || file.kind != ManagedFileKind::File
            || file.sha256.is_none()
        {
            return Err(invalid(root, "Invalid QwenWork managed file entry"));
        }
        files
            .entry(file.relative_path.clone())
            .or_default()
            .push(file);
    }
    Ok(files)
}

fn matches_versions(
    path: &Path,
    versions: &[&MaterializedFile],
    allow_absent: bool,
) -> Result<(), AdapterError> {
    if !exists(path)? {
        return if allow_absent {
            Ok(())
        } else {
            Err(invalid(path, "Managed QwenWork file is missing"))
        };
    }
    let metadata = std::fs::symlink_metadata(path).map_err(|source| AdapterError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if !metadata.is_file() {
        return Err(invalid(path, "Preserving changed QwenWork file type"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.nlink() != 1 {
            return Err(invalid(path, "Preserving hard-linked QwenWork file"));
        }
    }
    let bytes = std::fs::read(path).map_err(|source| AdapterError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let digest = format!("{:x}", Sha256::digest(bytes));
    if !versions
        .iter()
        .any(|file| file.sha256.as_deref() == Some(&digest))
    {
        return Err(invalid(
            path,
            "Preserving modified QwenWork file; restore or remove the conflict before retrying",
        ));
    }
    Ok(())
}

fn verify_owned(claim: &AdapterClaim, allow_absent: bool) -> Result<(), AdapterError> {
    let (root, payload) = receipt(claim)?;
    if !payload.installation_started {
        if exists(root)? {
            return Err(invalid(
                root,
                "QwenWork copy never started; refusing an existing plugin directory",
            ));
        }
        return Ok(());
    }
    for (relative, versions) in owned_versions(claim)? {
        let path = root.join(relative);
        check_parents(root, &path)?;
        matches_versions(&path, &versions, allow_absent)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
