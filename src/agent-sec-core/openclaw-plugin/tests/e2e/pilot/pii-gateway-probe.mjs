import fs from "node:fs/promises";
import path from "node:path";

import { compareOpenClawVersions, PII_GATEWAY_MARKER, readJsonLines, sleep } from "./common.mjs";

// Synthetic values only. The Chinese field also exercises a credential shape
// independently of the host's sk-* masking rule.
const RAW_CREDENTIAL = "sk-3219AbCdEfGhIjKlMnOpQrStUvWxYz987654";
const DENY_CREDENTIAL = "PiIGateQ9m4v7R2x6c8";

export async function runPiiGatewayProbe({
  callGatewayRpc, cliLogPath, gatewayToken, gatewayUrl, logsDir, mockModel, openclawVersion,
}) {
  const modern = /^\d{4}\.\d+\.\d+(?:\+[^-]+)?$/u.test(openclawVersion) &&
    compareOpenClawVersions(openclawVersion, "2026.5.12") >= 0;
  const source = modern ? "model_input" : "user_input";
  const runPrefix = `${PII_GATEWAY_MARKER}-${Date.now()}`;
  const result = {
    mode: "real-gateway-chat-send", openclawVersion, source,
    inputHook: modern ? "before_agent_run" : "before_dispatch",
    evidence: { cliCalls: cliLogPath, modelRequests: mockModel.requestsLog }, cases: [],
  };
  const rows = [
    { id: "benign-first", session: "benign", text: "Hello. Reply briefly.", expected: "pass" },
    { id: "masked-subsequent", session: "benign", text: "sk-321…7654", expected: "pass" },
    { id: "raw-after-benign", session: "benign", text: RAW_CREDENTIAL },
    { id: "credential-first", session: "credential", text: RAW_CREDENTIAL },
    { id: "credential-subsequent", session: "credential", text: RAW_CREDENTIAL },
    { id: "block-before-provider", session: "deny", text: `密码=${DENY_CREDENTIAL}`, expected: "deny" },
  ];
  try {
    // Recent hosts otherwise schedule an unrelated title-model request after
    // the turn ends. Native labels keep the zero-provider-request oracle exact.
    if (compareOpenClawVersions(openclawVersion, "2026.9.2") >= 0) {
      for (const session of new Set(rows.map((row) => row.session))) {
        await callGatewayRpc(`pii-label-${session}`, "sessions.create", {
          key: `agent:main:dashboard:${runPrefix}-${session}`,
          agentId: "main", label: `PII regression ${session}`,
        }, { gatewayToken, gatewayUrl, timeoutMs: 30_000 });
      }
    }
    for (const row of rows) {
      const runId = `${runPrefix}-${row.id}`;
      const sessionKey = `agent:main:dashboard:${runPrefix}-${row.session}`;
      const marker = `${PII_GATEWAY_MARKER} ${row.id}`;
      const scanStart = (await readJsonLines(cliLogPath)).length;
      const requestStart = mockModel.requests.length;
      const testCase = { id: row.id, runId, sessionKey, message: `${marker}\n${row.text}` };
      result.cases.push(testCase);
      const send = unwrap(await callGatewayRpc(`${row.id}-chat-send`, "chat.send", {
        sessionKey, idempotencyKey: runId, message: testCase.message,
      }, { gatewayToken, gatewayUrl, timeoutMs: 30_000 }));
      testCase.send = send;
      const actualRunId = send?.runId ?? runId;
      const wait = unwrap(await callGatewayRpc(`${row.id}-agent-wait`, "agent.wait", {
        runId: actualRunId, timeoutMs: 60_000,
      }, { gatewayToken, gatewayUrl, timeoutMs: 65_000, maxAttempts: 1 }));
      testCase.wait = wait;
      const history = unwrap(await callGatewayRpc(`${row.id}-chat-history`, "chat.history", {
        sessionKey, limit: 100,
      }, { gatewayToken, gatewayUrl, timeoutMs: 15_000 }));
      const calls = (await readJsonLines(cliLogPath)).slice(scanStart);
      const scans = calls.filter((call) => call.subcommand === "scan-pii" &&
        call.args?.[call.args.indexOf("--source") + 1] === source && call.input?.includes(marker));
      testCase.scan = scans.map((call) => ({
        input: call.input, args: call.args, verdict: call.stdoutJson?.verdict,
        summary: call.stdoutJson?.summary, override: call.override,
      }));
      // A completed run (or a persisted dispatch denial) establishes the no-send
      // boundary; the short settle only drains evidence writes after completion.
      const dispatchDenied = JSON.stringify(history).includes("[pii-checker]") &&
        JSON.stringify(history).includes("阻断");
      if ((!wait?.status || wait.status === "timeout") && !dispatchDenied) {
        throw new Error(`${row.id}: no terminal run or persisted PII denial`);
      }
      await sleep(300);
      const requests = mockModel.requests.slice(requestStart);
      const requestText = JSON.stringify(requests.map((request) => request.body));
      const scan = scans.at(-1);
      testCase.modelRequestCount = requests.length;
      testCase.modelContainsRawCredential = requestText.includes(RAW_CREDENTIAL);
      testCase.modelContainsDenyCredential = requestText.includes(DENY_CREDENTIAL);
      testCase.modelContainsMarker = requestText.includes(marker);
      if (scans.length !== 1 || scan.override !== false || scan.stdoutJson?.summary?.source !== source) {
        throw new Error(`${row.id}: expected exactly one real scan-pii call with source=${source}`);
      }
      if (row.expected && scan.stdoutJson.verdict !== row.expected) {
        throw new Error(`${row.id}: expected ${row.expected}, got ${scan.stdoutJson.verdict}`);
      }
      if (scan.stdoutJson.verdict === "deny") {
        if (requests.length !== 0) throw new Error(`${row.id}: denied input reached the provider`);
      } else {
        if (scan.stdoutJson.verdict !== "pass" || requests.length === 0 || !testCase.modelContainsMarker) {
          throw new Error(`${row.id}: passing input did not reach the provider`);
        }
        if (testCase.modelContainsRawCredential || testCase.modelContainsDenyCredential) {
          throw new Error(`${row.id}: raw credential reached the provider despite a non-deny verdict`);
        }
      }
      testCase.passed = true;
    }
    result.passed = true;
    return result;
  } finally {
    await fs.writeFile(path.join(logsDir, "pii-gateway-probe.json"), `${JSON.stringify(result, null, 2)}\n`);
  }
}

function unwrap(value) {
  return value?.ok === true && value.payload ? value.payload : value;
}
