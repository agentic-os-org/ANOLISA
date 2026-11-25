use crate::input::InputClassifier;

use super::ShellHostConfig;

pub(super) fn configure(
    classifier: InputClassifier,
    config: &ShellHostConfig,
    bounded_bash_handoff: bool,
    slash_route_enabled: bool,
) -> InputClassifier {
    classifier
        .with_shell_passthrough(!config.integration.uses_markers())
        .with_bash_readline_history_privacy(
            bounded_bash_handoff && config.integration.uses_markers(),
        )
        .with_bash_slash_submission_guard(slash_route_enabled)
        .with_bash_pressure_guard(bounded_bash_handoff && config.integration.uses_markers())
        .with_bash_pressure_recovery_file(
            config
                .work_dir
                .join("terminal-recovery-request.ps0-pressure"),
        )
}
