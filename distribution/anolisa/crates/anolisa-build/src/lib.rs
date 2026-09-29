//! Build orchestration library.
//! Handles toolchain resolution, parallel build execution, and artifact staging.

pub mod backends;

/// Trait for build system backends.
pub trait BuildBackend {
    fn name(&self) -> &str;
    fn build(&self, spec: &BuildSpec) -> Result<Vec<Artifact>, BuildError>;
    fn clean(&self, spec: &BuildSpec) -> Result<(), BuildError>;
}

/// Specification for a build task.
#[derive(Debug, Clone)]
pub struct BuildSpec {
    pub component_name: String,
    pub source_dir: std::path::PathBuf,
    pub output_dir: std::path::PathBuf,
    pub targets: Vec<String>,
    pub profile: BuildProfile,
}

#[derive(Debug, Clone, Copy)]
pub enum BuildProfile {
    Release,
    Debug,
}

/// A build artifact ready for installation.
#[derive(Debug, Clone)]
pub struct Artifact {
    pub path: std::path::PathBuf,
    pub artifact_type: ArtifactType,
}

#[derive(Debug, Clone)]
pub enum ArtifactType {
    Binary,
    Library,
    Data,
    Config,
}

#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    #[error("build failed for {component}: {reason}")]
    Failed { component: String, reason: String },
    #[error("toolchain not found: {0}")]
    ToolchainMissing(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}
