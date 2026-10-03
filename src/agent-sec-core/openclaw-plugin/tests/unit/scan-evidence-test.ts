import assert from "node:assert/strict";
import { it } from "node:test";
import {
  classifyCodePolicy,
  readScanEvidence,
  POLICY_TOOL_CALL_ID,
} from "../e2e/pilot/scan-evidence.mjs";

const evidence = {
  schemaVersion: 1,
  run_id: "target",
  tool_call_id: POLICY_TOOL_CALL_ID,
  policy: "ask",
  outcome: "scan-result",
  verdict: "deny",
  decision: "requireApproval",
};
const state = {
  evidence,
  requireApproval: true,
  approvalFound: true,
  blocked: false,
  toolExecuted: false,
  preResolveToolExecuted: false,
  pendingApprovals: 0,
};
const failed = {
  ...evidence,
  outcome: "scanner-failed",
  reason: "invalid-response",
  decision: "allow",
};

it("correlates plugin evidence and ignores heartbeat and wrapper evidence", () => {
  const log = (record: unknown) =>
    `timestamp [scan-code] ${JSON.stringify(record)}\n`;
  const text =
    log({ ...evidence, run_id: "heartbeat" }) +
    log({ ...evidence, tool_call_id: "other" }) +
    '[wrapper] {"verdict":"deny"}\n' +
    log(evidence);
  assert.deepEqual(readScanEvidence(text, "target"), evidence);
  assert.equal(readScanEvidence(text, "missing"), undefined);
  assert.throws(
    () => readScanEvidence(text + log(failed), "target"),
    /ambiguous/,
  );
});

it("passes verified approval or supported fail-closed behavior", () => {
  assert.equal(classifyCodePolicy(state), "approval-verified");
  assert.equal(
    classifyCodePolicy({ ...state, approvalFound: false, blocked: true }),
    "fail-closed-verified",
  );
});

it("fails valid denials without enforcement, including execution before approval", () => {
  for (const change of [
    { approvalFound: false },
    { toolExecuted: true },
    { preResolveToolExecuted: true },
    { evidence: { ...evidence, decision: "allow" } },
  ]) {
    assert.throws(
      () => classifyCodePolicy({ ...state, ...change }),
      /not enforced/,
    );
  }
});

it("requires positive failure evidence and execution for a fail-open pass", () => {
  assert.equal(
    classifyCodePolicy({
      ...state,
      evidence: failed,
      approvalFound: false,
      toolExecuted: true,
    }),
    "fail-open-verified",
  );
  for (const change of [
    { toolExecuted: false },
    { approvalFound: true },
    { blocked: true },
    { pendingApprovals: 1 },
  ]) {
    assert.throws(
      () =>
        classifyCodePolicy({
          ...state,
          evidence: failed,
          approvalFound: false,
          toolExecuted: true,
          ...change,
        }),
      /not verified/,
    );
  }
  for (const bad of [
    undefined,
    { ...failed, outcome: "hook-error" },
    { ...failed, reason: "unknown" },
  ]) {
    assert.throws(() =>
      classifyCodePolicy({
        ...state,
        evidence: bad,
        approvalFound: false,
        toolExecuted: true,
      }),
    );
  }
});
