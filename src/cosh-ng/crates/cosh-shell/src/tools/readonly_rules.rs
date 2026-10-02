mod evaluator;
mod runtime_config;
mod specs;
mod validators;

use evaluator::{config_disables_command, evaluate, evaluate_runtime};

pub use evaluator::{is_bounded_positive_count, is_safe_readonly_path};
pub use runtime_config::{
    ReadonlyRuleKey, RuntimeGenericSpec, RuntimeReadonlyConfig, RuntimeReadonlySpec,
    RuntimeSubcommandSpec, RuntimeValidator,
};
use specs::READONLY_SPECS;

pub use specs::PathMode;

// ── Evaluator ──

pub fn is_readonly_command(tokens: &[String]) -> bool {
    let cmd = tokens.first().map(String::as_str).unwrap_or("");
    READONLY_SPECS
        .iter()
        .find(|spec| spec.command == cmd)
        .map(|spec| evaluate(&spec.validator, tokens))
        .unwrap_or(false)
}

pub fn is_readonly_command_with_config(tokens: &[String], config: &RuntimeReadonlyConfig) -> bool {
    let Some(command) = tokens.first().map(String::as_str) else {
        return false;
    };
    let subcommand = tokens.get(1).map(String::as_str);

    if config_disables_command(config, command, subcommand) {
        return false;
    }

    if let Some(spec) = config.overrides.iter().find(|spec| spec.command == command) {
        return evaluate_runtime(&spec.validator, tokens);
    }

    is_readonly_command(tokens)
}

#[cfg(test)]
mod tests;
