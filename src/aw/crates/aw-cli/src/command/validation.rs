//! Keep canonical validation output separate from the retained legacy command.

use crate::output::{Outcome, ValidationError};
use std::{fs::File, io::Read, os::unix::fs::OpenOptionsExt};

fn read(path: &str) -> Result<Vec<u8>, ValidationError> {
    let file = File::options()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .map_err(ValidationError::Io)?;
    if !file.metadata().map_err(ValidationError::Io)?.is_file() {
        return Err(ValidationError::Io(std::io::Error::other(
            "configuration must be a regular file",
        )));
    }
    let mut bytes = Vec::new();
    file.take(aw_config::MAX_DOCUMENT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(ValidationError::Io)?;
    Ok(bytes)
}

fn validate(options: &super::Validate) -> Result<Outcome, ValidationError> {
    let validator = aw_config::Validator::new().map_err(ValidationError::Configuration)?;
    let candidate = validator
        .parse(&read(&options.config)?)
        .map_err(ValidationError::Configuration)?;
    if let Some(path) = &options.against {
        let baseline = validator
            .parse(&read(path)?)
            .map_err(ValidationError::Configuration)?;
        baseline
            .validate_policy_update(&candidate)
            .map_err(ValidationError::Configuration)?;
    }
    Ok(Outcome::ConfigurationChecked {
        policy_update_scope: options.against.is_some(),
    })
}

pub(super) fn run(options: super::ManagedValidate) -> crate::cli::Result<crate::cli::Exit> {
    crate::output::complete_validation(options.output, validate(&options.validation))
}
