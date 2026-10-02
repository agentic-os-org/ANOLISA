use crate::command::{classify_exit, first_program_token, ExitCodeCategory};
use crate::hooks::model::{HookInput, HookMatcher, HookTrigger};

pub(super) fn matches_command(matcher: &HookMatcher, input: &HookInput) -> bool {
    match matcher.trigger {
        HookTrigger::OnFail if !is_failure_exit(input) => return false,
        HookTrigger::OnSuccess if input.exit_code != 0 => return false,
        _ => {}
    }
    if let Some(ref codes) = matcher.exit_codes {
        if !codes.contains(&input.exit_code) {
            return false;
        }
    }
    if let Some(min_bytes) = matcher.min_output_bytes {
        if input.output_bytes < min_bytes {
            return false;
        }
    }
    let program = first_program_token(&input.command);
    if matcher.commands.iter().any(|cmd| cmd == program) {
        return true;
    }
    if matcher
        .command_patterns
        .iter()
        .any(|p| input.command.trim_start().starts_with(p))
    {
        return true;
    }
    if let Some(ref pattern) = matcher.command_regex {
        if input.command.contains(pattern) {
            return true;
        }
    }
    matcher.commands.is_empty()
        && matcher.command_patterns.is_empty()
        && matcher.command_regex.is_none()
}

fn is_failure_exit(input: &HookInput) -> bool {
    !matches!(
        classify_exit(input.exit_code, &input.command),
        ExitCodeCategory::Success
            | ExitCodeCategory::UserInterrupt
            | ExitCodeCategory::PipelineNormal
            | ExitCodeCategory::CommandSpecificNormal
    )
}
