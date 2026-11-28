//! Experimental, offline Agent Plugins 1.0 package loading.

mod containment;
mod manifest;
mod mcp;
mod skill;
mod store;

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use serde::{Serialize, de::DeserializeOwned};
use serde_json::value::RawValue;
use thiserror::Error;

use containment::{Kind, OpenError, Opened, PackageRoot};

pub use containment::check_package_resource_path;
pub use manifest::PluginManifest;
pub use mcp::{McpServer, McpTransport};
pub use skill::PackageSkill;
pub use store::{PackageStore, is_package_root_entry_visible};

/// Scope of a package validation failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DiagnosticSeverity {
    /// The package cannot be loaded.
    RejectPlugin,
    /// The fixed component location or configuration cannot be loaded.
    DisableComponent,
    /// The individual skill cannot be loaded.
    SkipSkill,
    /// The individual server cannot be loaded.
    SkipServer,
    /// Loading can proceed without the invalid field.
    Warn,
}

impl DiagnosticSeverity {
    /// Stable spelling used in CLI diagnostics.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RejectPlugin => "reject-plugin",
            Self::DisableComponent => "disable-component",
            Self::SkipSkill => "skip-skill",
            Self::SkipServer => "skip-server",
            Self::Warn => "warn",
        }
    }
}

/// Package component to which a diagnostic applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PluginComponent {
    /// The package manifest.
    Manifest,
    /// The skills directory or a discovered skill.
    Skills,
    /// The MCP configuration or one server entry.
    Mcp,
}

impl PluginComponent {
    /// Stable component name used in CLI diagnostics.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manifest => "manifest",
            Self::Skills => "skills",
            Self::Mcp => "mcp",
        }
    }
}

/// A validation finding with its package-relative location.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PluginDiagnostic {
    /// Failure scope.
    pub severity: DiagnosticSeverity,
    /// Affected component.
    pub component: PluginComponent,
    /// Logical package-relative path.
    pub path: String,
    /// RFC 6901 pointer, empty for the entire document.
    pub json_pointer: String,
    /// Section of the Agent Plugins specification.
    pub spec_ref: &'static str,
    /// Human-readable reason.
    pub message: &'static str,
}

impl PluginDiagnostic {
    fn new(
        severity: DiagnosticSeverity,
        component: PluginComponent,
        path: &str,
        keys: &[&str],
        spec_ref: &'static str,
        message: &'static str,
    ) -> Self {
        let mut json_pointer = String::new();
        for key in keys {
            json_pointer.push('/');
            json_pointer.push_str(&key.replace('~', "~0").replace('/', "~1"));
        }
        Self {
            severity,
            component,
            path: path.to_owned(),
            json_pointer,
            spec_ref,
            message,
        }
    }
}

/// Errors returned at resource and package registration boundaries.
#[derive(Debug, Error)]
pub enum PluginError {
    /// A resource path must be relative to its package.
    #[error("resource path must be relative to the plugin root: {0:?}")]
    AbsoluteResource(PathBuf),
    /// The resource resolves outside its owning root.
    #[error("path {path:?} escapes its root {root:?}")]
    OutsideRoot {
        /// Requested path.
        path: PathBuf,
        /// Containment boundary.
        root: PathBuf,
    },
    /// A filesystem operation failed while resolving a path.
    #[error("cannot resolve path {path:?}: {source}")]
    ResolvePath {
        /// Path being resolved.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: io::Error,
    },
    /// The identifier cannot be used as a package path segment.
    #[error("invalid package ID {0:?}: expected a single safe non-dot path segment")]
    InvalidPackageId(String),
    /// A package identifier was already registered.
    #[error(
        "package ID {id:?} is already registered for {existing:?}; cannot register {incoming:?}"
    )]
    DuplicatePackageId {
        /// Conflicting identifier.
        id: String,
        /// Root of the previously registered package.
        existing: PathBuf,
        /// Root of the incoming package.
        incoming: PathBuf,
    },
}

/// An accepted package with discovery results and pinned configuration bytes.
#[derive(Debug)]
pub struct LoadedPackage {
    /// Canonical package root.
    pub root: PathBuf,
    /// Validated manifest fields.
    pub manifest: PluginManifest,
    /// Valid direct child skills, ordered by logical directory name.
    pub skills: Vec<PackageSkill>,
    /// Valid server configurations, ordered by server name.
    pub servers: Vec<McpServer>,
    manifest_bytes: Arc<[u8]>,
    mcp_bytes: Option<Arc<[u8]>>,
    has_skills_dir: bool,
}

impl LoadedPackage {
    /// Original bytes of the manifest accepted at load time.
    pub fn manifest_bytes(&self) -> &Arc<[u8]> {
        &self.manifest_bytes
    }

    /// Original bytes of a top-level valid MCP configuration, if present.
    ///
    /// Individual server diagnostics do not change the pinned bytes.
    pub fn mcp_bytes(&self) -> Option<&Arc<[u8]>> {
        self.mcp_bytes.as_ref()
    }
}

/// Result of static package validation, including diagnostics for accepted packages.
#[derive(Debug)]
pub struct PackageValidation {
    /// Loaded package, present when its root and manifest were accepted.
    pub package: Option<LoadedPackage>,
    /// Findings in discovery order: manifest, skills, then MCP.
    pub diagnostics: Vec<PluginDiagnostic>,
}

impl PackageValidation {
    /// Whether the package's root and manifest were accepted.
    pub fn accepted(&self) -> bool {
        self.package.is_some()
    }
}

/// Validate a package without fetching schemas or executing any server.
///
/// When supplied, `plugin_data` is the physical data directory used to check
/// containment of MCP working directories rooted at `${PLUGIN_DATA}`.
pub fn load_package(root: &Path, plugin_data: Option<&Path>) -> PackageValidation {
    let mut diagnostics = Vec::new();
    let package = load(root, plugin_data, &mut diagnostics);
    PackageValidation {
        package,
        diagnostics,
    }
}

fn load(
    root: &Path,
    plugin_data: Option<&Path>,
    diagnostics: &mut Vec<PluginDiagnostic>,
) -> Option<LoadedPackage> {
    let root = match PackageRoot::open(root) {
        Ok(root) => root,
        Err(err) => {
            let (spec_ref, message) = match err.kind() {
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory => {
                    ("§5.1", "plugin root must be a directory")
                }
                _ => ("§4.1", "cannot resolve plugin root"),
            };
            diagnostics.push(PluginDiagnostic::new(
                DiagnosticSeverity::RejectPlugin,
                PluginComponent::Manifest,
                "plugin.json",
                &[],
                spec_ref,
                message,
            ));
            return None;
        }
    };

    let (manifest, manifest_bytes) = manifest::load(&root, diagnostics)?;
    let skills = skill::discover(&root, diagnostics);
    let mcp = mcp::discover(&root, plugin_data, diagnostics);
    let has_skills_dir = skills.is_some();
    Some(LoadedPackage {
        root: root.path().to_path_buf(),
        manifest,
        skills: skills.unwrap_or_default(),
        servers: mcp.servers,
        manifest_bytes,
        mcp_bytes: mcp.bytes,
        has_skills_dir,
    })
}

fn fixed_location(
    root: &PackageRoot,
    name: &str,
    component: PluginComponent,
    kind: Kind,
    diagnostics: &mut Vec<PluginDiagnostic>,
) -> Option<Rc<Opened>> {
    let (spec_ref, message) = match root.open_entry(Path::new(name), kind) {
        Ok(entry) => return Some(entry),
        Err(OpenError::Absent) => return None,
        Err(OpenError::Containment(_)) => ("§4.1", "component location fails containment"),
        Err(OpenError::Unreadable(_)) => ("§6.2", "cannot inspect component location"),
        Err(OpenError::WrongKind) => (
            "§6.2",
            "component location has wrong kind or unreadable target",
        ),
    };
    diagnostics.push(PluginDiagnostic::new(
        DiagnosticSeverity::DisableComponent,
        component,
        name,
        &[],
        spec_ref,
        message,
    ));
    None
}

fn read_json(bytes: &[u8]) -> Option<&RawValue> {
    let json = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    serde_json::from_str(std::str::from_utf8(json).ok()?).ok()
}

// Raw members keep opaque numbers out of the typed field validators. The map
// also preserves last-wins behavior for repeated keys in a JSON object.
type JsonObject<'a> = BTreeMap<String, &'a RawValue>;

fn json_object(value: &RawValue) -> Option<JsonObject<'_>> {
    if !value.get().starts_with('{') {
        return None;
    }
    serde_json::from_str(value.get()).ok()
}

fn json_as<T: DeserializeOwned>(value: &RawValue) -> Option<T> {
    serde_json::from_str(value.get()).ok()
}

fn json_string_map(value: &RawValue) -> Option<BTreeMap<String, String>> {
    json_object(value)?
        .into_iter()
        .map(|(key, value)| json_as(value).map(|value| (key, value)))
        .collect()
}
