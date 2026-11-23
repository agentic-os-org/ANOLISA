//! Operator-owned backend configuration; sandbox requests cannot override it.

use crate::DockerError;
use std::path::PathBuf;

/// Placement inputs fixed by the deployment, never by the sandbox request.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DockerConfig {
    /// Trusted docker CLI, invoked by absolute path with a cleared environment.
    pub cli: PathBuf,
    /// Fixed local Unix endpoint; personal Docker contexts are never read.
    pub host: String,
    /// Trusted absolute guard path; defaults to the installed sibling of the calling executable.
    pub guard: Option<PathBuf>,
    /// Operator-pinned image; a digest reference is strongly recommended.
    pub image: String,
    /// Optional memory limit in docker syntax, such as `512m`.
    pub memory: Option<String>,
    /// Optional CPU quota in docker syntax, such as `2.0`.
    pub cpus: Option<String>,
    /// Optional process limit; the executor applies a default when unset.
    pub pids_limit: Option<u32>,
}

impl DockerConfig {
    /// Checks the trusted CLI path and the docker-syntax option values.
    ///
    /// # Errors
    /// Rejects relative CLI paths, empty or malformed images and limits.
    pub fn validate(&self) -> Result<(), DockerError> {
        if self.guard.as_ref().is_some_and(|path| !path.is_absolute()) {
            return Err(DockerError::Unsupported(
                "guard requires an absolute path".into(),
            ));
        }
        if !self.cli.is_absolute() {
            return Err(DockerError::Unsupported(
                "docker CLI must be an explicit absolute path".into(),
            ));
        }
        let endpoint = self.host.strip_prefix("unix://").unwrap_or_default();
        if !std::path::Path::new(endpoint).is_absolute()
            || endpoint.contains(['\0', '\n', '\r'])
            || endpoint.split('/').any(|part| matches!(part, "." | ".."))
        {
            return Err(DockerError::Unsupported(
                "docker host must be unix:///absolute/path without traversal".into(),
            ));
        }
        if self.image.is_empty()
            || self.image.starts_with('-')
            || !self
                .image
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "._:/@-".contains(c))
        {
            return Err(DockerError::Unsupported(format!(
                "invalid pinned image reference: {}",
                self.image
            )));
        }
        if let Some(memory) = &self.memory {
            let digits = memory.trim_end_matches(['b', 'k', 'm', 'g', 'B', 'K', 'M', 'G']);
            if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                return Err(DockerError::Unsupported(format!(
                    "invalid memory limit: {memory}"
                )));
            }
        }
        if let Some(cpus) = &self.cpus {
            let mut parts = cpus.split('.');
            let whole = parts.next().unwrap_or_default();
            let fraction = parts.next();
            let valid = !whole.is_empty()
                && whole.bytes().all(|b| b.is_ascii_digit())
                && fraction.is_none_or(|f| !f.is_empty() && f.bytes().all(|b| b.is_ascii_digit()))
                && parts.next().is_none();
            if !valid {
                return Err(DockerError::Unsupported(format!(
                    "invalid CPU limit: {cpus}"
                )));
            }
        }
        if self.pids_limit == Some(0) {
            return Err(DockerError::Unsupported(
                "pids limit must be positive".into(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> DockerConfig {
        DockerConfig {
            cli: PathBuf::from("/usr/bin/docker"),
            host: "unix:///var/run/docker.sock".into(),
            guard: None,
            image: "registry.example.com/team/agent@sha256:abc123".into(),
            memory: None,
            cpus: None,
            pids_limit: None,
        }
    }

    #[test]
    fn accepts_pinned_references_and_limits() {
        let mut config = config();
        assert!(config.validate().is_ok());
        config.memory = Some("512m".into());
        config.cpus = Some("2.5".into());
        config.pids_limit = Some(256);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn rejects_untrusted_or_malformed_inputs() {
        let mut config = config();
        config.cli = PathBuf::from("docker");
        assert!(config.validate().is_err());
        config.cli = PathBuf::from("/usr/bin/docker");
        for image in ["", "-evil", "image;rm", "image name"] {
            config.image = image.into();
            assert!(config.validate().is_err(), "{image}");
        }
        config.image = "agent:1.0".into();
        for memory in ["m", "12x", "-1m"] {
            config.memory = Some(memory.into());
            assert!(config.validate().is_err(), "{memory}");
        }
        config.memory = None;
        for cpus in [".5", "1.", "1.2.3", "abc"] {
            config.cpus = Some(cpus.into());
            assert!(config.validate().is_err(), "{cpus}");
        }
        config.cpus = None;
        config.pids_limit = Some(0);
        assert!(config.validate().is_err());
    }
}
