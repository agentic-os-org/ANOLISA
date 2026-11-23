use std::path::Path;
use std::process::Stdio;

use serde::Deserialize;

use super::{account, Error, Result, COSH_PATH};

const PROOF_PATH: &str = "/usr/share/cosh/login-capability.json";
const PREFLIGHT_PATH: &str = "/usr/libexec/cosh/login-preflight";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Proof {
    pub schema: u32,
    pub login_record_schema: u32,
    pub stable_entry: String,
    pub package: String,
    pub minimum_compatible_package_version: String,
    pub replacement_preflight_protocol: u32,
    pub replacement_preflight: String,
    pub final_uninstall_guard: bool,
}

impl Proof {
    pub fn validate(&self, version: &str) -> Result<()> {
        let minimum = semver::Version::parse(&self.minimum_compatible_package_version)
            .map_err(|_| Error::new("invalid login capability minimum version"))?;
        let current =
            semver::Version::parse(version).map_err(|_| Error::new("invalid package version"))?;
        if self.schema != 1
            || self.login_record_schema != 1
            || self.stable_entry != COSH_PATH
            || self.package != "cosh-ng"
            || self.replacement_preflight_protocol != 1
            || self.replacement_preflight != PREFLIGHT_PATH
            || !self.final_uninstall_guard
            || current < minimum
        {
            return Err(Error::new(
                "login set is unavailable: controlled package lifecycle capability is incompatible",
            ));
        }
        Ok(())
    }
}

pub(super) fn verify() -> Result<()> {
    let bytes = account::read_trusted(Path::new(PROOF_PATH), 8192).map_err(|_| {
        Error::new(
            "login set is unavailable: controlled package lifecycle proof is missing or unsafe",
        )
    })?;
    let proof: Proof =
        serde_json::from_slice(&bytes).map_err(|_| Error::new("invalid login capability proof"))?;
    proof.validate(env!("CARGO_PKG_VERSION"))?;
    account::verify_package(PREFLIGHT_PATH, "cosh-ng")?;
    let status = account::command(PREFLIGHT_PATH)?
        .args([
            "--verify-installed",
            "--entry",
            COSH_PATH,
            "--record-version",
            "1",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(Error::io)?;
    if status.success() {
        Ok(())
    } else {
        Err(Error::new(
            "login set is unavailable: controlled package lifecycle preflight failed",
        ))
    }
}
