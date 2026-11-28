//! Compile bounded deployment hints from the same configuration used by Service.

use aw_adapter::deployment::APPLICATION_HOOKS;
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
    profile: Option<String>,
}

pub(super) fn run(options: Options) -> crate::cli::Result<crate::cli::Exit> {
    let bytes =
        aw_package::config_edit::read_configuration_bytes(std::path::Path::new(&options.config))?;
    let configuration = aw_config::Validator::new()?.parse(&bytes)?;
    let agent = &configuration.as_value()["spec"]["agents"][&options.agent];
    let profiles = aw_config::instrumentation::profiles(&options.agent, agent)?;
    let profile = options.profile.as_deref();
    let descriptor = if !profiles.is_empty() {
        if profile.is_some_and(|name| name != APPLICATION_HOOKS) {
            return Err(
                "configured instrumentation profiles cannot use a native deployment profile".into(),
            );
        }
        crate::cli::profile_launch::compile(&bytes, &options.agent, &profiles)?
    } else {
        let name = profile.ok_or("--profile is required for targets without builtin profiles")?;
        let requirements = aw_config::requirements::project(&configuration, &options.agent)?;
        let input_enabled = requirements
            .iter()
            .any(|event| event.event == "input.submit");
        let profile = aw_adapter::deployment::profile(name, input_enabled)?;
        let required = profile.requires_service || requirements.iter().any(|event| event.required);
        let availability = if required {
            Availability::Required
        } else {
            Availability::NativeFallback
        };
        if name == APPLICATION_HOOKS {
            crate::cli::profile_launch::compile_for_service(
                &bytes,
                &options.agent,
                profile.capabilities,
                availability,
            )?
        } else {
            aw_core::descriptor::compile_with_options(
                &bytes,
                &options.agent,
                profile.capabilities,
                availability,
                CompileOptions {
                    session_start_hint: profile.session_start_hint,
                },
            )?
        }
    };
    let mut output = serde_json::to_vec_pretty(&descriptor)?;
    output.push(b'\n');
    std::io::stdout().lock().write_all(&output)?;
    Ok(crate::cli::Exit::Code(0))
}
