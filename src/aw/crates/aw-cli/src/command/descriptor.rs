//! Compile bounded deployment hints from the same configuration used by Service.

use aw_core::descriptor::{Availability, CompileOptions};
use clap::{builder::PossibleValuesParser, Args};
use std::io::Write;

#[derive(Args, Debug, PartialEq)]
pub(super) struct Options {
    /// Deployment configuration to bind by its exact bytes
    #[arg(long, value_name = "FILE")]
    config: String,
    /// Target name declared in the configuration
    #[arg(long, value_name = "TARGET")]
    agent: String,
    /// Installed SDK integration whose capability ceiling must admit the policy
    #[arg(long, value_parser = PossibleValuesParser::new(aw_adapter::deployment::profile_names()))]
    profile: String,
}

pub(super) fn run(options: Options) -> crate::cli::Result<crate::cli::Exit> {
    let bytes =
        aw_package::config_edit::read_configuration_bytes(std::path::Path::new(&options.config))?;
    let configuration = aw_config::Validator::new()?.parse(&bytes)?;
    let events = &configuration.as_value()["spec"]["events"];
    let input_enabled = events["input.submit"]["enabled"] == true;
    let profile = aw_adapter::deployment::profile(&options.profile, input_enabled)?;
    let required = profile.requires_service
        || aw_config::requirements::requires_service(&configuration, &options.agent)?;
    let descriptor = aw_core::descriptor::compile_with_options(
        &bytes,
        &options.agent,
        profile.capabilities,
        if required {
            Availability::Required
        } else {
            Availability::NativeFallback
        },
        CompileOptions {
            session_start_hint: profile.session_start_hint,
        },
    )?;
    let mut output = serde_json::to_vec_pretty(&descriptor)?;
    output.push(b'\n');
    std::io::stdout().lock().write_all(&output)?;
    Ok(crate::cli::Exit::Code(0))
}
