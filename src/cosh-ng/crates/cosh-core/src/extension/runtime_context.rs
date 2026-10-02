//! Immutable, bounded extension context snapshots for one core generation.

use std::fs;
use std::path::Path;

use super::settings::ExtensionSettings;
use super::{
    EffectiveState, Extension, ExtensionDiagnostic, ExtensionHealth, ExtensionManager,
    ExtensionSourceKind,
};

const MAX_CONTEXT_FILE_BYTES: usize = 64 * 1024;
const MAX_CONTEXT_TOTAL_BYTES: usize = 256 * 1024;

/// Validated context payload captured before a core generation starts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExtensionContextSnapshot {
    rendered: String,
    diagnostics: Vec<ExtensionDiagnostic>,
}

impl ExtensionContextSnapshot {
    /// Validates active extensions and captures their context files once.
    pub fn build(manager: &mut ExtensionManager) -> Self {
        let settings = ExtensionSettings::new(manager.workspace_dir().to_path_buf());
        Self::build_with_settings(
            manager,
            settings.as_ref().map_err(|error| error.to_string()),
        )
    }

    pub(crate) fn build_with_settings(
        manager: &mut ExtensionManager,
        settings: Result<&ExtensionSettings, String>,
    ) -> Self {
        let mut sections = Vec::new();
        let mut diagnostics = Vec::new();
        let mut total_bytes = 0usize;
        let mut extensions = manager
            .list_mut()
            .iter_mut()
            .filter(|extension| extension.is_active)
            .collect::<Vec<_>>();
        extensions.sort_by(|left, right| left.name.cmp(&right.name));

        for extension in extensions {
            let extension_start_bytes = total_bytes;
            let link_trust_failure = matches!(extension.source, ExtensionSourceKind::Link)
                && !settings
                    .as_ref()
                    .map(|settings| settings.workspace_trusted())
                    .unwrap_or(false);
            if link_trust_failure {
                let diagnostic = ExtensionDiagnostic::new(
                    "extension_workspace_untrusted",
                    format!(
                        "linked extension {} is not loaded for an untrusted workspace",
                        extension.name
                    ),
                );
                fail_extension(extension, diagnostic.clone());
                diagnostics.push(diagnostic);
                continue;
            }
            let setting_failure = match &settings {
                Ok(settings) => settings
                    .validate_required(extension)
                    .err()
                    .map(|error| ExtensionDiagnostic::new(error.code(), error.to_string())),
                Err(error) if extension.settings.iter().any(|setting| setting.required) => Some(
                    ExtensionDiagnostic::new("extension_settings_path_unavailable", error.clone()),
                ),
                Err(_) => None,
            };
            if let Some(diagnostic) = setting_failure {
                fail_extension(extension, diagnostic.clone());
                diagnostics.push(diagnostic);
                continue;
            }

            let mut extension_sections = Vec::new();
            let mut required_failure = None;
            for contribution in &extension.contexts {
                match load_context(extension, &contribution.path, total_bytes) {
                    Ok(content) => {
                        total_bytes += content.len();
                        extension_sections.push(format!(
                            "<!-- cosh-extension-context begin id=\"{}\" source=\"{}\" -->\n{}\n<!-- cosh-extension-context end id=\"{}\" -->",
                            contribution.id,
                            source_label(extension.source),
                            content,
                            contribution.id
                        ));
                    }
                    Err(diagnostic) if contribution.required => {
                        required_failure = Some(diagnostic);
                        break;
                    }
                    Err(diagnostic) => {
                        extension.health = ExtensionHealth::Degraded;
                        extension.diagnostics.push(diagnostic.clone());
                        diagnostics.push(diagnostic);
                    }
                }
            }
            if let Some(diagnostic) = required_failure {
                total_bytes = extension_start_bytes;
                fail_extension(extension, diagnostic.clone());
                diagnostics.push(diagnostic);
                continue;
            }
            sections.extend(extension_sections);
        }

        Self {
            rendered: sections.join("\n\n"),
            diagnostics,
        }
    }

    /// Returns the immutable prompt section, if any contribution was captured.
    pub fn rendered(&self) -> Option<&str> {
        (!self.rendered.is_empty()).then_some(self.rendered.as_str())
    }

    /// Returns stable diagnostics produced while building the snapshot.
    pub fn diagnostics(&self) -> &[ExtensionDiagnostic] {
        &self.diagnostics
    }
}

fn load_context(
    extension: &Extension,
    path: &Path,
    total_bytes: usize,
) -> Result<String, ExtensionDiagnostic> {
    let package_root = extension.path.canonicalize().map_err(|error| {
        ExtensionDiagnostic::new(
            "extension_context_path_unreadable",
            format!(
                "failed to resolve extension root {}: {error}",
                extension.path.display()
            ),
        )
    })?;
    let canonical = path.canonicalize().map_err(|error| {
        ExtensionDiagnostic::new(
            "extension_context_unreadable",
            format!("failed to resolve context {}: {error}", path.display()),
        )
    })?;
    if !canonical.starts_with(&package_root) {
        return Err(ExtensionDiagnostic::new(
            "extension_context_path_escape",
            format!(
                "context path escapes extension package {}: {}",
                extension.name,
                path.display()
            ),
        ));
    }
    let bytes = fs::read(&canonical).map_err(|error| {
        ExtensionDiagnostic::new(
            "extension_context_unreadable",
            format!("failed to read context {}: {error}", canonical.display()),
        )
    })?;
    if bytes.len() > MAX_CONTEXT_FILE_BYTES {
        return Err(ExtensionDiagnostic::new(
            "extension_context_file_too_large",
            format!(
                "context {} exceeds the {} byte limit",
                canonical.display(),
                MAX_CONTEXT_FILE_BYTES
            ),
        ));
    }
    if total_bytes.saturating_add(bytes.len()) > MAX_CONTEXT_TOTAL_BYTES {
        return Err(ExtensionDiagnostic::new(
            "extension_context_total_too_large",
            format!(
                "extension context exceeds the {} byte generation limit",
                MAX_CONTEXT_TOTAL_BYTES
            ),
        ));
    }
    let content = String::from_utf8(bytes).map_err(|_| {
        ExtensionDiagnostic::new(
            "extension_context_not_utf8",
            format!("context is not valid UTF-8: {}", canonical.display()),
        )
    })?;
    Ok(content)
}

fn fail_extension(extension: &mut Extension, diagnostic: ExtensionDiagnostic) {
    extension.is_active = false;
    extension.effective_state = EffectiveState::Disabled;
    extension.health = ExtensionHealth::Broken;
    extension.diagnostics.push(diagnostic);
}

fn source_label(source: ExtensionSourceKind) -> &'static str {
    match source {
        ExtensionSourceKind::PathCopy => "path-copy",
        ExtensionSourceKind::Link => "link",
        ExtensionSourceKind::GitHttps => "git-https",
        ExtensionSourceKind::Legacy => "legacy",
        ExtensionSourceKind::System => "system",
        ExtensionSourceKind::Conflict => "conflict",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extension::EXTENSION_CONFIG_FILENAME;
    use serde_json::json;

    fn manager_with_extension(
        root: &Path,
        name: &str,
        contexts: serde_json::Value,
    ) -> ExtensionManager {
        let user = root.join("extensions");
        let system = root.join("system");
        let package = user.join(name);
        fs::create_dir_all(&package).unwrap();
        fs::create_dir_all(&system).unwrap();
        fs::write(
            package.join(EXTENSION_CONFIG_FILENAME),
            serde_json::to_vec_pretty(&json!({
                "schemaVersion": 1,
                "name": name,
                "version": "1.0.0",
                "compatibility": {"cosh": ">=0.12.0"},
                "contextFiles": contexts,
            }))
            .unwrap(),
        )
        .unwrap();
        let mut manager = ExtensionManager::new_isolated_with_state(
            root.join("workspace"),
            Some(user),
            Some(system),
            root.join("state"),
        );
        manager.refresh();
        manager
    }

    #[test]
    fn snapshot_preserves_manifest_order_and_provenance() {
        let temporary = tempfile::tempdir().unwrap();
        let package = temporary.path().join("extensions/example.ops");
        fs::create_dir_all(&package).unwrap();
        fs::write(package.join("first.md"), "FIRST").unwrap();
        fs::write(package.join("second.md"), "SECOND").unwrap();
        let mut manager = manager_with_extension(
            temporary.path(),
            "example.ops",
            json!([
                {"id": "first", "path": "first.md", "required": true},
                {"id": "second", "path": "second.md"}
            ]),
        );

        let snapshot = ExtensionContextSnapshot::build(&mut manager);
        let rendered = snapshot.rendered().unwrap();
        assert!(rendered.contains("begin id=\"example.ops/context/first\" source=\"legacy\""));
        assert!(rendered.find("FIRST").unwrap() < rendered.find("SECOND").unwrap());
        assert!(snapshot.diagnostics().is_empty());
        assert!(manager.list()[0].is_active);
    }

    #[test]
    fn required_failure_disables_only_the_extension() {
        let temporary = tempfile::tempdir().unwrap();
        let mut manager = manager_with_extension(
            temporary.path(),
            "example.ops",
            json!([{"id": "missing", "path": "missing.md", "required": true}]),
        );
        let snapshot = ExtensionContextSnapshot::build(&mut manager);
        assert!(snapshot.rendered().is_none());
        assert_eq!(manager.list()[0].health, ExtensionHealth::Broken);
        assert!(!manager.list()[0].is_active);
        assert_eq!(
            snapshot.diagnostics()[0].code,
            "extension_context_unreadable"
        );
    }

    #[test]
    fn optional_invalid_utf8_degrades_without_injection() {
        let temporary = tempfile::tempdir().unwrap();
        let package = temporary.path().join("extensions/example.ops");
        fs::create_dir_all(&package).unwrap();
        fs::write(package.join("binary.md"), [0xff, 0xfe]).unwrap();
        let mut manager = manager_with_extension(
            temporary.path(),
            "example.ops",
            json!([{"id": "binary", "path": "binary.md"}]),
        );
        let snapshot = ExtensionContextSnapshot::build(&mut manager);
        assert!(snapshot.rendered().is_none());
        assert!(manager.list()[0].is_active);
        assert_eq!(manager.list()[0].health, ExtensionHealth::Degraded);
        assert_eq!(snapshot.diagnostics()[0].code, "extension_context_not_utf8");
    }

    #[test]
    fn file_and_generation_byte_limits_fail_closed() {
        let temporary = tempfile::tempdir().unwrap();
        let package = temporary.path().join("extensions/example.ops");
        fs::create_dir_all(&package).unwrap();
        fs::write(
            package.join("large.md"),
            vec![b'a'; MAX_CONTEXT_FILE_BYTES + 1],
        )
        .unwrap();
        let mut manager = manager_with_extension(
            temporary.path(),
            "example.ops",
            json!([{"id": "large", "path": "large.md", "required": true}]),
        );
        let snapshot = ExtensionContextSnapshot::build(&mut manager);
        assert_eq!(
            snapshot.diagnostics()[0].code,
            "extension_context_file_too_large"
        );

        let total_root = tempfile::tempdir().unwrap();
        let total_package = total_root.path().join("extensions/example.ops");
        fs::create_dir_all(&total_package).unwrap();
        let contexts = (0..5)
            .map(|index| {
                let filename = format!("{index}.md");
                fs::write(
                    total_package.join(&filename),
                    vec![b'a'; MAX_CONTEXT_FILE_BYTES],
                )
                .unwrap();
                json!({
                    "id": format!("part-{index}"),
                    "path": filename,
                    "required": true
                })
            })
            .collect::<Vec<_>>();
        let mut manager = manager_with_extension(total_root.path(), "example.ops", json!(contexts));
        let snapshot = ExtensionContextSnapshot::build(&mut manager);
        assert_eq!(
            snapshot.diagnostics()[0].code,
            "extension_context_total_too_large"
        );
        assert!(snapshot.rendered().is_none());
        assert!(!manager.list()[0].is_active);
    }

    #[cfg(unix)]
    #[test]
    fn changed_symlink_cannot_escape_after_manifest_validation() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().unwrap();
        let outside = temporary.path().join("outside.md");
        let package = temporary.path().join("extensions/example.ops");
        fs::create_dir_all(&package).unwrap();
        fs::write(package.join("inside.md"), "inside").unwrap();
        fs::write(&outside, "outside").unwrap();
        symlink(package.join("inside.md"), package.join("context.md")).unwrap();
        let mut manager = manager_with_extension(
            temporary.path(),
            "example.ops",
            json!([{"id": "safe", "path": "context.md", "required": true}]),
        );
        fs::remove_file(package.join("context.md")).unwrap();
        symlink(&outside, package.join("context.md")).unwrap();

        let snapshot = ExtensionContextSnapshot::build(&mut manager);
        assert_eq!(
            snapshot.diagnostics()[0].code,
            "extension_context_path_escape"
        );
        assert!(!manager.list()[0].is_active);
    }
}
