//! Cargo/Rust build backend.

use std::path::PathBuf;
use std::process::{Command, ExitStatus};

use super::super::{Artifact, ArtifactType, BuildBackend, BuildError, BuildProfile, BuildSpec};

/// Build backend for Cargo projects.
pub struct CargoBuilder;

impl BuildBackend for CargoBuilder {
    fn name(&self) -> &str {
        "cargo"
    }

    fn build(&self, spec: &BuildSpec) -> Result<Vec<Artifact>, BuildError> {
        let profile_dir = match spec.profile {
            BuildProfile::Release => "release",
            BuildProfile::Debug => "debug",
        };

        let manifest_path = manifest_path(spec);
        let target_dir = target_dir(spec);
        let mut cmd = Command::new("cargo");
        cmd.arg("build")
            .arg("--manifest-path")
            .arg(&manifest_path)
            .arg("--target-dir")
            .arg(&target_dir);
        if matches!(spec.profile, BuildProfile::Release) {
            cmd.arg("--release");
        }
        let status = run_cargo(&mut cmd, &spec.component_name)?;
        if !status.success() {
            return Err(BuildError::Failed {
                component: spec.component_name.clone(),
                reason: format!("cargo build exited with {status}"),
            });
        }

        let artifacts: Vec<Artifact> = spec
            .targets
            .iter()
            .map(|target| Artifact {
                path: target_dir.join(profile_dir).join(target),
                artifact_type: ArtifactType::Binary,
            })
            .collect();

        for artifact in &artifacts {
            if !artifact.path.is_file() {
                return Err(BuildError::Failed {
                    component: spec.component_name.clone(),
                    reason: format!(
                        "cargo build succeeded but expected artifact '{}' is missing",
                        artifact.path.display(),
                    ),
                });
            }
        }

        Ok(artifacts)
    }

    fn clean(&self, spec: &BuildSpec) -> Result<(), BuildError> {
        let manifest_path = manifest_path(spec);
        let target_dir = target_dir(spec);
        let mut cmd = Command::new("cargo");
        cmd.arg("clean")
            .arg("--manifest-path")
            .arg(&manifest_path)
            .arg("--target-dir")
            .arg(&target_dir);
        let status = run_cargo(&mut cmd, &spec.component_name)?;
        if !status.success() {
            return Err(BuildError::Failed {
                component: spec.component_name.clone(),
                reason: format!("cargo clean exited with {status}"),
            });
        }
        Ok(())
    }
}

fn manifest_path(spec: &BuildSpec) -> PathBuf {
    spec.source_dir.join("Cargo.toml")
}

fn target_dir(spec: &BuildSpec) -> PathBuf {
    spec.source_dir.join("target")
}

fn run_cargo(cmd: &mut Command, component: &str) -> Result<ExitStatus, BuildError> {
    cmd.status().map_err(|source| {
        if source.kind() == std::io::ErrorKind::NotFound {
            BuildError::ToolchainMissing("cargo".to_string())
        } else {
            BuildError::Failed {
                component: component.to_string(),
                reason: format!("failed to execute cargo: {source}"),
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    fn write_fixture_crate(root: &Path) {
        fs::write(
            root.join("Cargo.toml"),
            r#"[package]
name = "hello-fixture"
version = "0.1.0"
edition = "2024"

[[bin]]
name = "hello-fixture"
path = "src/main.rs"
"#,
        )
        .unwrap();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
    }

    #[test]
    fn build_runs_cargo_and_returns_existing_artifact() {
        let tmp = tempfile::tempdir().unwrap();
        write_fixture_crate(tmp.path());
        let spec = BuildSpec {
            component_name: "hello-fixture".to_string(),
            source_dir: tmp.path().to_path_buf(),
            output_dir: tmp.path().join("out"),
            targets: vec!["hello-fixture".to_string()],
            profile: BuildProfile::Debug,
        };

        let artifacts = CargoBuilder.build(&spec).expect("build succeeds");

        assert_eq!(artifacts.len(), 1);
        assert!(artifacts[0].path.is_file());
    }

    #[test]
    fn clean_delegates_to_cargo_clean() {
        let tmp = tempfile::tempdir().unwrap();
        write_fixture_crate(tmp.path());
        let spec = BuildSpec {
            component_name: "hello-fixture".to_string(),
            source_dir: tmp.path().to_path_buf(),
            output_dir: tmp.path().join("out"),
            targets: vec!["hello-fixture".to_string()],
            profile: BuildProfile::Debug,
        };

        CargoBuilder.build(&spec).expect("build succeeds");
        assert!(target_dir(&spec).exists());
        CargoBuilder.clean(&spec).expect("clean succeeds");

        assert!(!target_dir(&spec).join("debug").exists());
    }
}
