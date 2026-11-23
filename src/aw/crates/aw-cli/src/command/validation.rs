//! Validate configuration and policy scope with the public human or JSON report.

use crate::output::{Outcome, ValidationError};
use std::{fs::File, io::Read, os::unix::fs::OpenOptionsExt};

fn read(path: &str, input: Option<&'static str>) -> Result<Vec<u8>, ValidationError> {
    let io = |source| ValidationError::Io { source, input };
    let file = File::options()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .map_err(io)?;
    if !file.metadata().map_err(io)?.is_file() {
        return Err(io(std::io::Error::other(
            "configuration must be a regular file",
        )));
    }
    let mut bytes = Vec::new();
    file.take(aw_config::MAX_DOCUMENT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(io)?;
    Ok(bytes)
}

fn validate(options: &super::Validate) -> Result<Outcome, ValidationError> {
    let configuration = |source| ValidationError::Configuration {
        source,
        input: None,
    };
    let validator = aw_config::Validator::new().map_err(configuration)?;
    let load = |path: &str, input| {
        validator
            .parse(&read(path, input)?)
            .map_err(|source| ValidationError::Configuration { source, input })
    };
    let candidate = load(
        &options.config,
        options.against.as_ref().map(|_| "candidate"),
    )?;
    if let Some(path) = &options.against {
        let baseline = load(path, Some("baseline"))?;
        baseline
            .validate_policy_update(&candidate)
            .map_err(configuration)?;
    }
    Ok(Outcome::ConfigurationChecked {
        policy_update_scope: options.against.is_some(),
        projected_out_steps: crate::output::projection::collect(&candidate)
            .map_err(ValidationError::Projection)?,
    })
}

pub(super) fn run(options: super::Validate) -> crate::cli::Result<crate::cli::Exit> {
    let result = validate(&options);
    crate::output::complete_validation(options.output, result)
}
