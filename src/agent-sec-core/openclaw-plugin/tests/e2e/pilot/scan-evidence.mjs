// Match the plugin's consumed result, not the CLI wrapper's produced output.
export const POLICY_TOOL_CALL_ID = "call_agent_sec_pilot_exec";
export const normalizeToolCallId = (value) =>
  typeof value === "string" ? value.replace(/[^a-zA-Z0-9]/gu, "") : "";

export function readScanEvidence(text, runId) {
  const marker = "[scan-code] ";
  const matching = [];
  for (const line of text.split("\n")) {
    const index = line.indexOf(marker);
    if (index < 0) continue;
    let record;
    try {
      record = JSON.parse(line.slice(index + marker.length));
    } catch {
      continue;
    }
    if (
      record.run_id === runId &&
      normalizeToolCallId(record.tool_call_id) ===
        normalizeToolCallId(POLICY_TOOL_CALL_ID)
    ) {
      matching.push(record);
    }
  }
  if (matching.length > 1)
    throw new Error(`ambiguous scanner evidence for run ${runId}`);
  return matching[0];
}

export function classifyCodePolicy({
  evidence,
  requireApproval,
  approvalFound,
  blocked,
  toolExecuted,
  preResolveToolExecuted,
  pendingApprovals,
}) {
  if (
    evidence?.schemaVersion !== 1 ||
    evidence.policy !== (requireApproval ? "ask" : "observe")
  ) {
    throw new Error("missing or incompatible plugin scanner evidence");
  }
  if (
    evidence.outcome === "scanner-failed" &&
    ["cli-error", "invalid-response", "scanner-error"].includes(
      evidence.reason,
    ) &&
    evidence.decision === "allow"
  ) {
    if (approvalFound || blocked || pendingApprovals || !toolExecuted) {
      throw new Error(
        "scanner failed but fail-open execution was not verified",
      );
    }
    return "fail-open-verified";
  }
  if (evidence.outcome !== "scan-result" || evidence.verdict !== "deny") {
    throw new Error("expected a valid denial or explicit scanner failure");
  }
  if (requireApproval) {
    if (
      evidence.decision !== "requireApproval" ||
      (!approvalFound && !blocked) ||
      toolExecuted ||
      preResolveToolExecuted ||
      pendingApprovals
    ) {
      throw new Error(
        "valid denial received but approval/blocking was not enforced",
      );
    }
    return approvalFound ? "approval-verified" : "fail-closed-verified";
  }
  if (
    evidence.decision !== "allow" ||
    approvalFound ||
    blocked ||
    pendingApprovals ||
    !toolExecuted
  ) {
    throw new Error("observe policy execution was not verified");
  }
  return "observe-verified";
}
