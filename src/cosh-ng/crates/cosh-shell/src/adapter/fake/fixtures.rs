pub(super) struct FakeToolResult {
    pub(super) request: String,
    pub(super) status: Option<String>,
}

pub(super) fn fake_long_tool_output() -> String {
    (1..=24)
        .map(|idx| format!("line {idx}: fake tool output for details view"))
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn extract_fake_pending_answer(input: &str) -> Option<String> {
    input
        .lines()
        .find_map(|line| line.strip_prefix("User answer: "))
        .map(str::trim)
        .filter(|answer| !answer.is_empty())
        .map(ToString::to_string)
}

pub(super) fn extract_fake_tool_result(input: &str) -> Option<FakeToolResult> {
    let prefix = if input.starts_with("Tool result for request ") {
        "Tool result for request "
    } else if input.starts_with("Tool result for approved request ") {
        "Tool result for approved request "
    } else {
        return None;
    };
    let request = input
        .lines()
        .next()
        .and_then(|line| line.strip_prefix(prefix))
        .map(str::trim)
        .filter(|request| !request.is_empty())
        .map(ToString::to_string)?;
    let status = input
        .lines()
        .find_map(|line| line.strip_prefix("Status: "))
        .map(str::trim)
        .filter(|status| !status.is_empty())
        .map(ToString::to_string);
    Some(FakeToolResult { request, status })
}

pub(super) fn extract_fake_approval_result(input: &str) -> Option<String> {
    if !input.starts_with("Approval result for request ") {
        return None;
    }
    input
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("Approval result for request "))
        .map(str::trim)
        .filter(|request| !request.is_empty())
        .map(ToString::to_string)
}
