use std::collections::{HashMap, HashSet};

#[derive(Debug, Default)]
pub(crate) struct ProviderToolState {
    commands: HashMap<ProviderToolKey, RuntimeProviderToolCommand>,
    pending_shell_commands: Vec<PendingProviderShellCommand>,
    shell_tool_ids: HashSet<ProviderToolKey>,
    control_permission_shell_tool_ids: HashSet<ProviderToolKey>,
    outputs: HashMap<ProviderToolKey, String>,
    stderr: HashMap<ProviderToolKey, String>,
    rendered_shell_transcript_commands: HashSet<ProviderToolKey>,
    rendered_shell_transcript_outputs: HashSet<ProviderToolKey>,
    delivered_host_executed_shell_results: HashSet<String>,
    foreground_shell_commands: HashSet<String>,
    /// Tool calls the core marked with the machine-readable hook verdict
    /// (#2156): the only source the approval bridge may use to tell a hook
    /// rejection apart from an executed command.
    hook_blocked_results: HashSet<ProviderToolKey>,
}

impl ProviderToolState {
    pub(crate) fn record_command_from_input(
        &mut self,
        run_id: &str,
        tool_id: &str,
        tool_input: &serde_json::Value,
    ) -> bool {
        let Some(command) = provider_tool_command(tool_input) else {
            return false;
        };
        self.record_command(run_id, tool_id, command);
        true
    }

    pub(crate) fn record_shell_command_from_tool_call(
        &mut self,
        run_id: &str,
        tool_id: &str,
        input: &str,
    ) -> bool {
        self.shell_tool_ids
            .insert(ProviderToolKey::new(run_id, tool_id));
        let command = provider_tool_command_from_text(input);
        let Some(command) = command else {
            return false;
        };
        self.record_command(run_id, tool_id, command);
        true
    }

    pub(crate) fn record_pending_shell_command(&mut self, run_id: &str, command: &str) -> bool {
        if command.is_empty() || command.contains('\0') {
            return false;
        }
        self.pending_shell_commands
            .push(PendingProviderShellCommand {
                run_id: run_id.to_string(),
                command: command.to_string(),
            });
        true
    }

    fn record_command(&mut self, run_id: &str, tool_id: &str, command: String) {
        let key = ProviderToolKey::new(run_id, tool_id);
        self.shell_tool_ids.insert(key.clone());
        self.commands.insert(
            key,
            RuntimeProviderToolCommand {
                run_id: run_id.to_string(),
                tool_id: tool_id.to_string(),
                command,
            },
        );
    }

    pub(crate) fn command(
        &self,
        run_id: &str,
        tool_id: &str,
    ) -> Option<&RuntimeProviderToolCommand> {
        self.commands.get(&ProviderToolKey::new(run_id, tool_id))
    }

    pub(crate) fn is_shell_tool(&self, run_id: &str, tool_id: &str) -> bool {
        let key = ProviderToolKey::new(run_id, tool_id);
        self.shell_tool_ids.contains(&key) || self.commands.contains_key(&key)
    }

    pub(crate) fn mark_control_permission_shell_tool(&mut self, run_id: &str, tool_id: &str) {
        let key = ProviderToolKey::new(run_id, tool_id);
        self.shell_tool_ids.insert(key.clone());
        self.control_permission_shell_tool_ids.insert(key);
    }

    pub(crate) fn is_control_permission_shell_tool(&self, run_id: &str, tool_id: &str) -> bool {
        self.control_permission_shell_tool_ids
            .contains(&ProviderToolKey::new(run_id, tool_id))
    }

    pub(crate) fn record_output_delta(
        &mut self,
        run_id: &str,
        tool_id: &str,
        stream: &str,
        text: &str,
    ) {
        self.adopt_pending_shell_command(run_id, tool_id);
        let key = ProviderToolKey::new(run_id, tool_id);
        self.outputs.entry(key.clone()).or_default().push_str(text);
        if stream == "stderr" {
            self.stderr.insert(key, text.to_string());
        }
    }

    fn adopt_pending_shell_command(&mut self, run_id: &str, tool_id: &str) {
        if self
            .commands
            .contains_key(&ProviderToolKey::new(run_id, tool_id))
        {
            return;
        }
        let Some(index) = self
            .pending_shell_commands
            .iter()
            .position(|pending| pending.run_id == run_id)
        else {
            return;
        };
        let pending = self.pending_shell_commands.remove(index);
        self.record_command(&pending.run_id, tool_id, pending.command);
    }

    pub(crate) fn stderr(&self, run_id: &str, tool_id: &str) -> Option<&str> {
        self.stderr
            .get(&ProviderToolKey::new(run_id, tool_id))
            .map(String::as_str)
    }

    pub(crate) fn output_text(&self, run_id: &str, tool_id: &str) -> Option<String> {
        self.outputs
            .get(&ProviderToolKey::new(run_id, tool_id))
            .cloned()
    }

    pub(crate) fn interactive_failure_command(
        &self,
        run_id: &str,
        tool_id: &str,
        status: &str,
    ) -> Option<&RuntimeProviderToolCommand> {
        if !matches!(status, "error" | "failed" | "interrupted") {
            return None;
        }
        let stderr = self.stderr(run_id, tool_id)?;
        if !looks_interactive_tool_failure(stderr) {
            return None;
        }
        self.command(run_id, tool_id)
    }

    pub(crate) fn claim_shell_transcript_command(&mut self, run_id: &str, tool_id: &str) -> bool {
        self.rendered_shell_transcript_commands
            .insert(ProviderToolKey::new(run_id, tool_id))
    }

    pub(crate) fn mark_shell_transcript_output(&mut self, run_id: &str, tool_id: &str) {
        self.rendered_shell_transcript_outputs
            .insert(ProviderToolKey::new(run_id, tool_id));
    }

    pub(crate) fn mark_hook_blocked_result(&mut self, run_id: &str, tool_id: &str) {
        self.hook_blocked_results
            .insert(ProviderToolKey::new(run_id, tool_id));
    }

    pub(crate) fn hook_result_is_blocked(&self, run_id: &str, tool_id: &str) -> bool {
        self.hook_blocked_results
            .contains(&ProviderToolKey::new(run_id, tool_id))
    }

    pub(crate) fn mark_shell_transcript_seen(&mut self, run_id: &str, tool_id: &str) {
        let key = ProviderToolKey::new(run_id, tool_id);
        self.rendered_shell_transcript_commands.insert(key.clone());
        self.rendered_shell_transcript_outputs.insert(key);
    }

    pub(crate) fn shell_transcript_output_seen(&self, run_id: &str, tool_id: &str) -> bool {
        self.rendered_shell_transcript_outputs
            .contains(&ProviderToolKey::new(run_id, tool_id))
    }

    pub(crate) fn shell_transcript_seen(&self, run_id: &str, tool_id: &str) -> bool {
        let key = ProviderToolKey::new(run_id, tool_id);
        self.rendered_shell_transcript_commands.contains(&key)
            || self.rendered_shell_transcript_outputs.contains(&key)
    }

    pub(crate) fn mark_foreground_shell_command(&mut self, command: &str) -> bool {
        let Some(command) = shell_command_key(command) else {
            return false;
        };
        self.foreground_shell_commands.insert(command)
    }

    pub(crate) fn foreground_shell_command_seen(&self, command: &str) -> bool {
        shell_command_key(command)
            .is_some_and(|command| self.foreground_shell_commands.contains(&command))
    }

    pub(crate) fn claim_host_executed_shell_result(
        &mut self,
        run_id: &str,
        request_id: &str,
        tool_use_id: Option<&str>,
    ) -> Option<HostExecutedShellResultClaim> {
        let key = host_executed_shell_result_key(run_id, request_id, tool_use_id);
        if self
            .delivered_host_executed_shell_results
            .insert(key.clone())
        {
            Some(HostExecutedShellResultClaim { key })
        } else {
            None
        }
    }

    pub(crate) fn host_executed_shell_result_delivered(
        &self,
        run_id: &str,
        request_id: &str,
        tool_use_id: Option<&str>,
    ) -> bool {
        self.delivered_host_executed_shell_results
            .contains(&host_executed_shell_result_key(
                run_id,
                request_id,
                tool_use_id,
            ))
    }

    pub(crate) fn release_host_executed_shell_result(
        &mut self,
        claim: HostExecutedShellResultClaim,
    ) {
        self.delivered_host_executed_shell_results
            .remove(&claim.key);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ProviderToolKey {
    run_id: String,
    tool_id: String,
}

impl ProviderToolKey {
    fn new(run_id: &str, tool_id: &str) -> Self {
        Self {
            run_id: run_id.to_string(),
            tool_id: tool_id.to_string(),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct RuntimeProviderToolCommand {
    pub(crate) run_id: String,
    pub(crate) tool_id: String,
    pub(crate) command: String,
}

#[derive(Debug, Clone)]
struct PendingProviderShellCommand {
    run_id: String,
    command: String,
}

#[derive(Debug)]
pub(crate) struct HostExecutedShellResultClaim {
    key: String,
}

fn provider_tool_command(tool_input: &serde_json::Value) -> Option<String> {
    tool_input
        .get("command")
        .and_then(|value| value.as_str())
        .filter(|command| !command.is_empty() && !command.contains('\0'))
        .map(ToString::to_string)
}

fn provider_tool_command_from_text(input: &str) -> Option<String> {
    let input = input.trim();
    if input.is_empty() || input.contains('\0') {
        return None;
    }
    serde_json::from_str::<serde_json::Value>(input)
        .ok()
        .and_then(|value| provider_tool_command(&value))
        .or_else(|| Some(input.to_string()))
}

fn shell_command_key(command: &str) -> Option<String> {
    let command = command.trim();
    if command.is_empty() || command.contains('\0') {
        None
    } else {
        Some(command.to_string())
    }
}

fn looks_interactive_tool_failure(stderr: &str) -> bool {
    let stderr = stderr.to_ascii_lowercase();
    [
        "a terminal is required",
        "no tty present",
        "not a tty",
        "password is required",
        "a password is required",
        "requires a terminal",
        "requires tty",
    ]
    .iter()
    .any(|needle| stderr.contains(needle))
}

fn host_executed_shell_result_key(
    run_id: &str,
    request_id: &str,
    tool_use_id: Option<&str>,
) -> String {
    let run = format!("{}:{run_id}", run_id.len());
    match tool_use_id {
        Some(tool_use_id) => format!("run:{run}:tool:{tool_use_id}"),
        None => format!("run:{run}:request:{request_id}"),
    }
}

#[cfg(test)]
mod tests {
    use super::ProviderToolState;

    #[test]
    fn provider_tool_state_guards_duplicate_host_executed_shell_results() {
        let mut state = ProviderToolState::default();

        let claim = state
            .claim_host_executed_shell_result("run-1", "req-1", Some("call-1"))
            .expect("claim");
        assert!(state
            .claim_host_executed_shell_result("run-1", "req-1", Some("call-1"))
            .is_none());
        assert!(state.host_executed_shell_result_delivered("run-1", "req-1", Some("call-1")));
        assert!(!state.host_executed_shell_result_delivered("run-1", "req-1", Some("call-2")));
        assert!(state
            .claim_host_executed_shell_result("run-2", "req-1", Some("call-1"))
            .is_some());

        state.release_host_executed_shell_result(claim);
        assert!(!state.host_executed_shell_result_delivered("run-1", "req-1", Some("call-1")));
        assert!(state
            .claim_host_executed_shell_result("run-1", "req-1", Some("call-1"))
            .is_some());
    }

    #[test]
    fn provider_tool_state_records_command_and_interactive_failure() {
        let mut state = ProviderToolState::default();

        assert!(state.record_command_from_input(
            "run-1",
            "tool-1",
            &serde_json::json!({ "command": "sudo systemctl status sshd" }),
        ));
        state.record_output_delta(
            "run-1",
            "tool-1",
            "stderr",
            "sudo: a terminal is required\n",
        );

        let command = state
            .interactive_failure_command("run-1", "tool-1", "error")
            .expect("interactive failure command");
        assert_eq!(command.run_id, "run-1");
        assert_eq!(command.tool_id, "tool-1");
        assert_eq!(command.command, "sudo systemctl status sshd");
    }

    #[test]
    fn provider_tool_state_links_streamed_shell_output_to_pending_command() {
        let mut state = ProviderToolState::default();

        assert!(state.record_pending_shell_command("run-1", "df -h"));
        state.record_output_delta("run-1", "toolu-1", "stdout", "Filesystem\n");

        let command = state.command("run-1", "toolu-1").expect("command");
        assert_eq!(command.run_id, "run-1");
        assert_eq!(command.tool_id, "toolu-1");
        assert_eq!(command.command, "df -h");
    }

    #[test]
    fn provider_tool_state_does_not_reuse_stderr_across_runs() {
        let mut state = ProviderToolState::default();

        assert!(state.record_command_from_input(
            "run-1",
            "tool-1",
            &serde_json::json!({ "command": "sudo first" }),
        ));
        state.record_output_delta(
            "run-1",
            "tool-1",
            "stderr",
            "sudo: a terminal is required\n",
        );
        assert!(state.record_command_from_input(
            "run-2",
            "tool-1",
            &serde_json::json!({ "command": "printf safe" }),
        ));

        assert!(state
            .interactive_failure_command("run-2", "tool-1", "error")
            .is_none());
    }
}
