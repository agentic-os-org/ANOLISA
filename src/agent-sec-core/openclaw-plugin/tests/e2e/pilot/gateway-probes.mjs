import fs from "node:fs/promises";
import path from "node:path";

import {
  DEFAULT_GATEWAY_TURN_TIMEOUT_MS,
  PLUGIN_ID,
  POLICY_CODE_DENY_COMMAND,
  POLICY_CODE_DENY_MARKER,
  POLICY_CODE_DENY_OUTPUT,
  POLICY_PROMPT_DENY_MARKER,
  POLICY_PROMPT_DENY_TEXT,
  POLICY_PROMPT_REACHED_MODEL_TEXT,
  MOCK_MODEL_ID,
  MOCK_MODEL_PROVIDER_ID,
  isOpenClawVersionLessThan,
  readJsonLines,
  readTextIfExists,
  sleep,
  slugify,
} from "./common.mjs";
import { summarizeMockModelRequests, waitForMockModelToolTurn } from "./mock-model.mjs";

const CONFIG_HOT_RELOAD_SETTLE_MS = 3_000;
const POLICY_CONFIG_HOT_RELOAD_IMPLEMENTATION_VERSION = "2026.5.2";
const POLICY_CONFIG_VERIFIED_HOT_RELOAD_VERSION = "2026.5.7";
const POLICY_CONFIG_PATHS = {
  promptScanBlock: "plugins.entries.agent-sec.config.promptScanBlock",
  codeScanRequireApproval: "plugins.entries.agent-sec.config.codeScanRequireApproval",
};

export async function runGatewayTrafficProbe({
  assertProcessStillRunning,
  callGatewayRpc,
  dataDir,
  gatewayToken,
  gatewayUrl,
  logsDir,
  mockModel,
  processRef,
  runtimeInspect,
}) {
  // This is the positive end-to-end lane: prompt scan, code scan, tool execution,
  // and observability must all happen through a real Gateway session.
  const runId = `agent-sec-pilot-gateway-${Date.now()}`;
  const sessionKey = `agent:main:dashboard:${runId}`;
  const observabilityPath = path.join(dataDir, "observability.jsonl");
  const gatewayLogPaths = [
    path.join(logsDir, "openclaw-gateway.stdout.log"),
    path.join(logsDir, "openclaw-gateway.stderr.log"),
  ];
  const probe = {
    mode: "openclaw-gateway-session-send",
    sessionKey,
    runId,
    modelRef: `${MOCK_MODEL_PROVIDER_ID}/${MOCK_MODEL_ID}`,
    rpc: {},
    logs: {
      gatewayStdout: gatewayLogPaths[0],
      gatewayStderr: gatewayLogPaths[1],
      mockModelRequests: mockModel.requestsLog,
      observability: observabilityPath,
    },
  };
  const observabilityRequirement = resolveObservabilityRequirement(runtimeInspect);
  probe.observabilityCompatibility = observabilityRequirement;

  probe.rpc.createSession = unwrapGatewayPayload(
    await callGatewayRpc("gateway-sessions-create", "sessions.create", {
      key: sessionKey,
      agentId: "main",
      label: "AgentSec Pilot Gateway Traffic",
    }, {
      gatewayToken,
      gatewayUrl,
    }),
  );
  probe.rpc.send = unwrapGatewayPayload(
    await callGatewayRpc(
      "gateway-sessions-send-safe-exec",
      "sessions.send",
      {
        key: sessionKey,
        idempotencyKey: runId,
        message:
          "agent-sec pilot model-driven safe exec: call the exec tool with exactly `printf agent-sec-pilot-safe`, then summarize the result.",
        timeoutMs: DEFAULT_GATEWAY_TURN_TIMEOUT_MS,
      },
      {
        gatewayToken,
        gatewayUrl,
        timeoutMs: DEFAULT_GATEWAY_TURN_TIMEOUT_MS + 30_000,
      },
    ),
  );
  const waitPayload = unwrapGatewayPayload(
    await callGatewayRpc(
      "gateway-agent-wait-safe-exec",
      "agent.wait",
      {
        runId,
        timeoutMs: DEFAULT_GATEWAY_TURN_TIMEOUT_MS,
      },
      {
        gatewayToken,
        gatewayUrl,
        timeoutMs: DEFAULT_GATEWAY_TURN_TIMEOUT_MS + 30_000,
      },
    ),
  );
  probe.rpc.wait = waitPayload;

  const modelRequests = await waitForMockModelToolTurn(mockModel, 45_000);
  probe.modelRequests = summarizeMockModelRequests(modelRequests);
  assertProcessStillRunning(processRef);
  probe.gatewayLogSignals = await waitForGatewayLogSignals(gatewayLogPaths, 45_000);
  probe.observability = await waitForObservabilityHooks({
    expectedToolCommand: "printf agent-sec-pilot-safe",
    expectedToolOutput: "agent-sec-pilot-safe",
    observabilityPath,
    requiredHooks: observabilityRequirement.requiredHooks,
    requireFinalResponse: observabilityRequirement.conversation.required,
    requireModelCalls: observabilityRequirement.modelCalls.required,
    runId,
    timeoutMs: 45_000,
  });

  const probeFile = path.join(logsDir, "gateway-traffic-probe.json");
  await fs.writeFile(probeFile, `${JSON.stringify(probe, null, 2)}\n`);
  probe.resultFile = probeFile;
  return probe;
}

export async function runGatewayPolicyMatrix({
  callGatewayRpc,
  cliLogPath,
  env,
  gatewayToken,
  gatewayUrl,
  getGatewayUrl = () => gatewayUrl,
  logsDir,
  mockModel,
  openclawVersion,
  pluginRoot,
  restartGateway,
  runRequiredStep,
}) {
  // The matrix validates policy outcomes from Gateway/session/model evidence,
  // not by scraping logs: model request deltas, session text, approvals, and CLI
  // call records must line up with each config state.
  const policyDebugLog = logsDir ? path.join(logsDir, "gateway-policy-debug.jsonl") : undefined;
  const matrix = {
    mode: "openclaw-gateway-policy-matrix",
    evidence: {
      cliCalls: cliLogPath,
      mockModelRequests: mockModel.requestsLog,
      policyDebug: policyDebugLog,
    },
    cases: [],
  };
  const policyConfigApplication = resolvePolicyConfigApplication(openclawVersion);
  matrix.policyConfigApplication = policyConfigApplication;
  matrix.livePolicyConfig = policyConfigApplication.livePolicyConfig;

  matrix.cases.push(
    await runPromptPolicyCase({
      callGatewayRpc,
      caseName: "promptScanBlock=false passes deny prompt to model",
      cliLogPath,
      env,
      gatewayToken,
      getGatewayUrl,
      mockModel,
      policyConfigApplication,
      pluginRoot,
      promptScanBlock: false,
      restartGateway,
      runRequiredStep,
    }),
  );
  matrix.cases.push(
    await runPromptPolicyCase({
      callGatewayRpc,
      caseName: "promptScanBlock=true handles deny prompt before model",
      cliLogPath,
      env,
      gatewayToken,
      getGatewayUrl,
      mockModel,
      policyConfigApplication,
      pluginRoot,
      promptScanBlock: true,
      restartGateway,
      runRequiredStep,
    }),
  );
  matrix.cases.push(
    await runCodeApprovalPolicyCase({
      callGatewayRpc,
      caseName: "codeScanRequireApproval=false allows deny scan without approval",
      cliLogPath,
      codeScanRequireApproval: false,
      env,
      gatewayToken,
      getGatewayUrl,
      mockModel,
      policyConfigApplication,
      policyDebugLog,
      pluginRoot,
      restartGateway,
      runRequiredStep,
    }),
  );
  matrix.cases.push(
    await runCodeApprovalPolicyCase({
      callGatewayRpc,
      caseName: "codeScanRequireApproval=true blocks denied tool before execution",
      cliLogPath,
      codeScanRequireApproval: true,
      env,
      gatewayToken,
      getGatewayUrl,
      mockModel,
      policyConfigApplication,
      policyDebugLog,
      pluginRoot,
      restartGateway,
      runRequiredStep,
    }),
  );

  return matrix;
}

function resolvePolicyConfigApplication(openclawVersion) {
  const verifiedHotReload = !isOpenClawVersionLessThan(
    openclawVersion,
    POLICY_CONFIG_VERIFIED_HOT_RELOAD_VERSION,
  );
  if (!verifiedHotReload) {
    return {
      mode: "gateway-restart",
      reason: "agentsec-does-not-verify-plugin-policy-live-hot-reload-before-2026.5.7",
      openclawVersion,
      implementationHotReloadVersion: POLICY_CONFIG_HOT_RELOAD_IMPLEMENTATION_VERSION,
      verifiedHotReloadVersion: POLICY_CONFIG_VERIFIED_HOT_RELOAD_VERSION,
      livePolicyConfig: {
        verifiedByAgentSec: false,
        reason: "not-covered-by-agentsec-verified-live-hot-reload-baseline",
        openclawVersion,
        implementationHotReloadVersion: POLICY_CONFIG_HOT_RELOAD_IMPLEMENTATION_VERSION,
        verifiedHotReloadVersion: POLICY_CONFIG_VERIFIED_HOT_RELOAD_VERSION,
      },
    };
  }
  return {
    mode: "hot-reload",
    reason: "agentsec-verified-plugin-policy-live-hot-reload-baseline",
    openclawVersion,
    implementationHotReloadVersion: POLICY_CONFIG_HOT_RELOAD_IMPLEMENTATION_VERSION,
    verifiedHotReloadVersion: POLICY_CONFIG_VERIFIED_HOT_RELOAD_VERSION,
    livePolicyConfig: {
      verifiedByAgentSec: true,
      openclawVersion,
      implementationHotReloadVersion: POLICY_CONFIG_HOT_RELOAD_IMPLEMENTATION_VERSION,
      verifiedHotReloadVersion: POLICY_CONFIG_VERIFIED_HOT_RELOAD_VERSION,
    },
  };
}

export function assertGatewayTrafficProbe(probe) {
  if (probe?.skipped) {
    return;
  }
  if (probe?.mode !== "openclaw-gateway-session-send") {
    throw new Error(`gateway traffic probe mode is ${JSON.stringify(probe?.mode)}`);
  }
  if (probe.rpc?.send?.runId !== probe.runId) {
    throw new Error(
      `sessions.send returned runId=${JSON.stringify(probe.rpc?.send?.runId)}, expected ${probe.runId}`,
    );
  }
  if (probe.rpc?.wait?.status !== "ok") {
    throw new Error(`agent.wait status is ${JSON.stringify(probe.rpc?.wait?.status)}, expected "ok"`);
  }
  if (!probe.gatewayLogSignals?.promptScanPass || !probe.gatewayLogSignals?.codeScanPass) {
    throw new Error(`gateway log signals incomplete: ${JSON.stringify(probe.gatewayLogSignals)}`);
  }
  if (!Array.isArray(probe.modelRequests) || probe.modelRequests.length < 2) {
    throw new Error("gateway traffic probe did not observe the model tool-result round trip");
  }
  const firstRequest = probe.modelRequests[0];
  const secondRequest = probe.modelRequests[1];
  if (!firstRequest?.tools?.includes("exec")) {
    throw new Error("first model request did not expose the exec tool");
  }
  if (!secondRequest?.hasToolResultMessage) {
    throw new Error("second model request did not include a tool result message");
  }
  const requiredHooks = probe.observability?.requirements?.hooks ?? [
    "after_agent_run",
    "after_llm_call",
    "after_tool_call",
    "before_agent_run",
    "before_llm_call",
    "before_tool_call",
  ];
  for (const hook of requiredHooks) {
    if (!probe.observability?.hooks?.includes(hook)) {
      throw new Error(`gateway traffic observability missing ${hook}`);
    }
  }
  const observabilityAssertions = probe.observability?.assertions ?? {};
  const failedObservabilityAssertions = Object.entries(observabilityAssertions)
    .filter(([, value]) => value !== true)
    .map(([key]) => key);
  if (failedObservabilityAssertions.length > 0) {
    throw new Error(
      `gateway traffic observability assertions failed: ${failedObservabilityAssertions.join(",")}`,
    );
  }
}

export function assertPolicyMatrix(matrix) {
  if (matrix?.skipped) {
    return;
  }
  const cases = Array.isArray(matrix?.cases) ? matrix.cases : [];
  const expectedCases = [
    "promptScanBlock=false passes deny prompt to model",
    "promptScanBlock=true handles deny prompt before model",
    "codeScanRequireApproval=false allows deny scan without approval",
    "codeScanRequireApproval=true blocks denied tool before execution",
  ];
  for (const expectedCase of expectedCases) {
    const actual = cases.find((item) => item?.name === expectedCase);
    if (!actual) {
      throw new Error(`policy matrix missing case: ${expectedCase}`);
    }
    if (actual.passed !== true) {
      throw new Error(`policy matrix case failed: ${expectedCase}`);
    }
  }
}

async function runPromptPolicyCase({
  callGatewayRpc,
  caseName,
  cliLogPath,
  env,
  gatewayToken,
  getGatewayUrl,
  mockModel,
  policyConfigApplication,
  pluginRoot,
  promptScanBlock,
  restartGateway,
  runRequiredStep,
}) {
  // promptScanBlock=false should still scan and return deny, but allow the turn
  // to reach the model. promptScanBlock=true should stop before model request.
  const configReload = await applyAgentSecPolicyConfig({
    callGatewayRpc,
    caseName,
    codeScanRequireApproval: true,
    env,
    gatewayToken,
    gatewayUrl: getGatewayUrl(),
    getGatewayUrl,
    policyConfigApplication,
    pluginRoot,
    promptScanBlock,
    restartGateway,
    runRequiredStep,
  });

  const cliCallStart = await countJsonLines(cliLogPath);
  const modelRequestStart = mockModel.requests.length;
  const turn = await runGatewayPolicyTurn({
    callGatewayRpc,
    caseName,
    gatewayToken,
    gatewayUrl: getGatewayUrl(),
    message: POLICY_PROMPT_DENY_TEXT,
  });
  turn.wait = unwrapGatewayPayload(
    await callGatewayRpc(
      `${caseName}-wait`,
      "agent.wait",
      { runId: turn.runId, timeoutMs: DEFAULT_GATEWAY_TURN_TIMEOUT_MS },
      {
        gatewayToken,
        gatewayUrl: getGatewayUrl(),
        timeoutMs: DEFAULT_GATEWAY_TURN_TIMEOUT_MS + 30_000,
      },
    ),
  );
  const records = await waitForSessionRecords(turn.readRecords, 15_000);
  const cliCalls = await readJsonLinesSince(cliLogPath, cliCallStart);
  const promptCall = findCliCall(cliCalls, {
    subcommand: "scan-prompt",
    inputIncludes: POLICY_PROMPT_DENY_MARKER,
  });
  const modelRequests = mockModel.requests.slice(modelRequestStart);
  const matchedPolicyModelRequests = modelRequests.filter((request) =>
    mockModelRequestContainsText(request, POLICY_PROMPT_DENY_MARKER),
  );
  const reachedModel = matchedPolicyModelRequests.length > 0;
  const blockedTextFound = sessionContainsText(records, "[prompt-scan] 检测到安全风险");
  const modelReplyFound = sessionContainsText(records, POLICY_PROMPT_REACHED_MODEL_TEXT);

  const policyCase = {
    name: caseName,
    config: { promptScanBlock },
    configReload,
    cli: summarizePolicyCliCall(promptCall),
    gateway: {
      runId: turn.runId,
      sessionFile: turn.sessionFile,
      waitStatus: turn.wait?.status,
    },
    allModelRequestDelta: modelRequests.length,
    matchedPolicyRequestDelta: matchedPolicyModelRequests.length,
    assertions: {
      scanPromptDeny: promptCall?.stdoutJson?.verdict === "deny",
      reachedModel,
      blockedTextFound,
      modelReplyFound,
    },
  };

  if (promptCall?.stdoutJson?.verdict !== "deny") {
    throw new Error(`${caseName}: expected scan-prompt deny call`);
  }
  assertGatewayWaitDidNotTimeout(caseName, turn);
  if (promptScanBlock) {
    if (reachedModel) {
      throw new Error(`${caseName}: model received a request even though promptScanBlock=true`);
    }
    if (!blockedTextFound) {
      throw new Error(`${caseName}: session did not contain the prompt-scan block text`);
    }
  } else {
    if (!reachedModel) {
      throw new Error(`${caseName}: model did not receive the prompt when promptScanBlock=false`);
    }
    if (!modelReplyFound) {
      throw new Error(`${caseName}: session did not contain the model reply`);
    }
  }

  policyCase.passed = true;
  return policyCase;
}

async function runCodeApprovalPolicyCase({
  callGatewayRpc,
  caseName,
  cliLogPath,
  codeScanRequireApproval,
  env,
  gatewayToken,
  getGatewayUrl,
  mockModel,
  policyConfigApplication,
  policyDebugLog,
  pluginRoot,
  restartGateway,
  runRequiredStep,
}) {
  // codeScanRequireApproval controls whether a deny scan requires operator
  // approval before execution. Newer OpenClaw builds may fail closed when their
  // approval runtime cannot acquire approval scope in a fresh test gateway, so
  // the stable assertion is: deny scan happened and the tool never executed.
  const configReload = await applyAgentSecPolicyConfig({
    callGatewayRpc,
    caseName,
    codeScanRequireApproval,
    env,
    gatewayToken,
    gatewayUrl: getGatewayUrl(),
    getGatewayUrl,
    policyConfigApplication,
    pluginRoot,
    promptScanBlock: true,
    restartGateway,
    runRequiredStep,
  });

  {
    const expectedPolicy = codeScanRequireApproval ? "ask" : "observe";
    const approvalPolls = [];
    const cliCallStart = await countJsonLines(cliLogPath);
    const modelRequestStart = mockModel.requests.length;
    const activeGatewayUrl = getGatewayUrl();
    const gatewayLogName =
      configReload.mode === "gateway-restart" && configReload.skipped !== true
        ? `openclaw-gateway-policy-${slugify(caseName)}`
        : "openclaw-gateway";
    const gatewayLogPaths = policyDebugLog
      ? [
          path.join(path.dirname(policyDebugLog), `${slugify(gatewayLogName)}.stdout.log`),
          path.join(path.dirname(policyDebugLog), `${slugify(gatewayLogName)}.stderr.log`),
        ]
      : [];
    const turn = await runGatewayPolicyTurn({
      callGatewayRpc,
      caseName,
      gatewayToken,
      gatewayUrl: activeGatewayUrl,
      message:
        `[${POLICY_CODE_DENY_MARKER}] Call the exec tool with exactly \`${POLICY_CODE_DENY_COMMAND}\`, then summarize the result.`,
    });

    let codeScanLog;
    if (codeScanRequireApproval) {
      codeScanLog = (
        await waitForGatewayLogSignals(gatewayLogPaths, DEFAULT_GATEWAY_TURN_TIMEOUT_MS, {
          scanCodeDeny: {
            command: POLICY_CODE_DENY_COMMAND,
            policy: expectedPolicy,
          },
        })
      ).scanCodeDeny;
    }
    let approval;
    let approvalResolve;
    let preResolveToolExecuted = false;
    if (codeScanRequireApproval) {
      approval = await waitForPluginApprovalOrUndefined({
        callGatewayRpc,
        descriptionIncludes: POLICY_CODE_DENY_COMMAND,
        gatewayToken,
        gatewayUrl: activeGatewayUrl,
        observations: approvalPolls,
        timeoutMs: 5_000,
      });
      if (approval) {
        const recordsBeforeResolve = await turn.readRecords();
        preResolveToolExecuted = sessionHasSuccessfulToolOutput(
          recordsBeforeResolve,
          POLICY_CODE_DENY_OUTPUT,
        );
        approvalResolve = unwrapGatewayPayload(
          await callGatewayRpc(
            `${caseName}-approval-deny`,
            "plugin.approval.resolve",
            { id: approval.id, decision: "deny" },
            { gatewayToken, gatewayUrl: activeGatewayUrl },
          ),
        );
      }
    }

    turn.wait = unwrapGatewayPayload(
      await callGatewayRpc(
        `${caseName}-wait`,
        "agent.wait",
        { runId: turn.runId, timeoutMs: DEFAULT_GATEWAY_TURN_TIMEOUT_MS },
        {
          gatewayToken,
          gatewayUrl: activeGatewayUrl,
          timeoutMs: DEFAULT_GATEWAY_TURN_TIMEOUT_MS + 30_000,
        },
      ),
    );

    const records = await waitForSessionRecords(turn.readRecords, 15_000);
    const cliCalls = await readJsonLinesSince(cliLogPath, cliCallStart);
    const codeCall = findCliCall(cliCalls, {
      subcommand: "scan-code",
      inputIncludes: POLICY_CODE_DENY_COMMAND,
    });
    if (!codeScanLog) {
      codeScanLog = (
        await waitForGatewayLogSignals(gatewayLogPaths, 15_000, {
          scanCodeDeny: {
            command: POLICY_CODE_DENY_COMMAND,
            policy: expectedPolicy,
          },
        })
      ).scanCodeDeny;
    }
    const modelRequests = mockModel.requests.slice(modelRequestStart);
    const matchedPolicyModelRequests = modelRequests.filter((request) =>
      mockModelRequestContainsText(request, POLICY_CODE_DENY_MARKER),
    );
    const toolExecuted = sessionHasSuccessfulToolOutput(records, POLICY_CODE_DENY_OUTPUT);
    const approvalRequiredErrorFound = sessionContainsText(records, "Plugin approval required");
    const approvalTimedOutFound = sessionContainsText(records, "Approval timed out");
    const approvalUnavailableErrorFound = sessionContainsText(
      records,
      "Plugin approval unavailable",
    );
    const pendingApprovalSnapshot = await listPluginApprovalSnapshot({
      callGatewayRpc,
      descriptionIncludes: POLICY_CODE_DENY_COMMAND,
      gatewayToken,
      gatewayUrl: activeGatewayUrl,
    });
    const pendingApprovalsAfterWait = pendingApprovalSnapshot.matching;

    const policyCase = {
      name: caseName,
      config: { codeScanRequireApproval },
      configReload,
      cli: summarizePolicyCliCall(codeCall),
      gateway: {
        runId: turn.runId,
        sessionFile: turn.sessionFile,
        waitStatus: turn.wait?.status,
      },
      allModelRequestDelta: modelRequests.length,
      matchedPolicyRequestDelta: matchedPolicyModelRequests.length,
      gatewayCodeScan: codeScanLog,
      approval: approval
        ? {
            id: approval.id,
            pluginId: approval.request?.pluginId,
            title: approval.request?.title,
            toolName: approval.request?.toolName,
            resolvedAs: approvalResolve?.decision ?? "deny",
          }
        : undefined,
      approvalDelivery: approval
        ? "gateway-approval"
        : approvalRequiredErrorFound
          ? "fail-closed-session-error"
          : approvalTimedOutFound
            ? "fail-closed-approval-timeout"
            : approvalUnavailableErrorFound
              ? "fail-closed-approval-unavailable"
              : "missing",
      approvalPolling: approvalPolls,
      postWaitApprovalList: summarizeApprovalSnapshot(pendingApprovalSnapshot),
      sessionSignals: {
        approvalRequiredErrorFound,
        approvalTimedOutFound,
        approvalUnavailableErrorFound,
        toolResultErrors: summarizeToolResultErrors(records),
      },
      assertions: {
        scanCodeDeny: codeScanLog?.verdict === "deny",
        cliScanCodeDeny: codeCall?.stdoutJson?.verdict === "deny",
        approvalFound: Boolean(approval),
        approvalRequiredErrorFound,
        approvalTimedOutFound,
        approvalUnavailableErrorFound,
        preResolveToolExecuted,
        toolExecuted,
        pendingApprovalsAfterWait: pendingApprovalsAfterWait.length,
      },
    };

    await appendPolicyDebug(policyDebugLog, {
      type: "code-approval-policy-case",
      observedAt: new Date().toISOString(),
      caseName,
      policyCase,
    });

    if (codeScanLog?.verdict !== "deny") {
      throw new Error(`${caseName}: expected gateway scan-code deny log`);
    }
    assertGatewayWaitDidNotTimeout(caseName, turn);
    if (codeScanRequireApproval) {
      if (
        !approval &&
        !approvalRequiredErrorFound &&
        !approvalTimedOutFound &&
        !approvalUnavailableErrorFound
      ) {
        throw new Error(
          `${caseName}: expected plugin approval, fail-closed session error, approval timeout, or approval-unavailable fail-closed result`,
        );
      }
      if (approval && approval.request?.pluginId !== PLUGIN_ID) {
        throw new Error(`${caseName}: approval pluginId was ${approval.request?.pluginId}`);
      }
      if (preResolveToolExecuted || toolExecuted) {
        throw new Error(`${caseName}: tool executed despite denied plugin approval`);
      }
    } else {
      if (pendingApprovalsAfterWait.length > 0) {
        throw new Error(`${caseName}: approval was created even though codeScanRequireApproval=false`);
      }
      if (!toolExecuted) {
        throw new Error(`${caseName}: tool did not execute when codeScanRequireApproval=false`);
      }
    }

    policyCase.passed = true;
    return policyCase;
  }
}

async function applyAgentSecPolicyConfig({
  callGatewayRpc,
  caseName,
  codeScanRequireApproval,
  env,
  gatewayToken,
  gatewayUrl,
  getGatewayUrl = () => gatewayUrl,
  policyConfigApplication,
  pluginRoot,
  promptScanBlock,
  restartGateway,
  runRequiredStep,
}) {
  const configPath = env?.OPENCLAW_CONFIG_PATH;
  if (!configPath) {
    throw new Error(`${caseName}: OPENCLAW_CONFIG_PATH is required for hot-reload synchronization`);
  }
  const configBefore = await readOpenClawConfig(configPath);
  const desiredPolicyValues = [
    {
      path: POLICY_CONFIG_PATHS.promptScanBlock,
      value: promptScanBlock,
    },
    {
      path: POLICY_CONFIG_PATHS.codeScanRequireApproval,
      value: codeScanRequireApproval,
    },
  ];
  const changedPaths = desiredPolicyValues
    .filter((item) => !jsonValuesEqual(readConfigPath(configBefore, item.path), item.value))
    .map((item) => item.path);

  await runRequiredStep(
    `${caseName}-config-promptScanBlock`,
    "openclaw",
    [
      "config",
      "set",
      POLICY_CONFIG_PATHS.promptScanBlock,
      JSON.stringify(promptScanBlock),
      "--strict-json",
    ],
    { cwd: pluginRoot, env },
  );
  await runRequiredStep(
    `${caseName}-config-codeScanRequireApproval`,
    "openclaw",
    [
      "config",
      "set",
      POLICY_CONFIG_PATHS.codeScanRequireApproval,
      JSON.stringify(codeScanRequireApproval),
      "--strict-json",
    ],
    { cwd: pluginRoot, env },
  );
  if (changedPaths.length === 0) {
    return {
      changedPaths,
      gatewayUrl: getGatewayUrl(),
      mode: policyConfigApplication.mode,
      skipped: true,
      reason: "policy config already matched requested values",
    };
  }
  if (policyConfigApplication.mode === "gateway-restart") {
    if (typeof restartGateway !== "function") {
      throw new Error(
        `${caseName}: restartGateway callback is required for policy config restart mode`,
      );
    }
    const startedAtMs = Date.now();
    await restartGateway(`policy-${slugify(caseName)}`);
    const activeGatewayUrl = getGatewayUrl();
    return {
      changedPaths,
      gatewayUrl: activeGatewayUrl,
      mode: policyConfigApplication.mode,
      reason: policyConfigApplication.reason,
      restartMs: Date.now() - startedAtMs,
      gatewayReady: await waitForGatewayReadyAfterConfig({
        callGatewayRpc,
        caseName,
        gatewayToken,
        gatewayUrl: activeGatewayUrl,
      }),
    };
  }
  const settle = await waitForGatewayConfigSettle({ changedPaths });
  const activeGatewayUrl = getGatewayUrl();
  return {
    ...settle,
    gatewayUrl: activeGatewayUrl,
    mode: policyConfigApplication.mode,
    gatewayReady: await waitForGatewayReadyAfterConfig({
      callGatewayRpc,
      caseName,
      gatewayToken,
      gatewayUrl: activeGatewayUrl,
    }),
  };
}

async function waitForGatewayConfigSettle({ changedPaths }) {
  if (changedPaths.length === 0) {
    return {
      changedPaths,
      skipped: true,
      reason: "policy config already matched requested values",
    };
  }

  // OpenClaw currently has no stable reload-ack protocol. Avoid coupling this
  // e2e to gateway log wording; the following policy assertions prove whether
  // the settled runtime actually picked up the requested config.
  await sleep(CONFIG_HOT_RELOAD_SETTLE_MS);
  return {
    changedPaths,
    settleMs: CONFIG_HOT_RELOAD_SETTLE_MS,
  };
}

async function waitForGatewayReadyAfterConfig({
  callGatewayRpc,
  caseName,
  gatewayToken,
  gatewayUrl,
}) {
  const timeoutMs = 60_000;
  const startedAtMs = Date.now();
  const deadline = startedAtMs + timeoutMs;
  let attempt = 0;
  let lastError;
  while (Date.now() < deadline) {
    attempt += 1;
    try {
      const health = await callGatewayRpc(
        `${caseName}-gateway-ready-${attempt}`,
        "health",
        {},
        { gatewayToken, gatewayUrl, maxAttempts: 1, retryDelayBaseMs: 0, timeoutMs: 5_000 },
      );
      // A successful Gateway RPC proves the restarted or hot-reloaded runtime is
      // accepting operator traffic again. The policy cases below prove config use.
      await sleep(500);
      return {
        attempts: attempt,
        waitMs: Date.now() - startedAtMs,
        health,
      };
    } catch (error) {
      lastError = error;
      await sleep(1_000);
    }
  }
  throw new Error(
    `${caseName}: gateway did not become ready after config change within ${timeoutMs}ms: ${String(lastError?.message ?? lastError)}`,
  );
}

async function readOpenClawConfig(configPath) {
  const text = await readTextIfExists(configPath);
  if (!text.trim()) {
    return {};
  }
  try {
    return JSON.parse(text);
  } catch (error) {
    throw new Error(`failed to parse OpenClaw config at ${configPath}: ${error.message}`);
  }
}

function readConfigPath(config, pathValue) {
  let current = config;
  for (const segment of pathValue.split(".")) {
    if (!current || typeof current !== "object" || !(segment in current)) {
      return undefined;
    }
    current = current[segment];
  }
  return current;
}

function jsonValuesEqual(left, right) {
  return JSON.stringify(left) === JSON.stringify(right);
}

function mockModelRequestContainsText(request, text) {
  return JSON.stringify(request?.body ?? {}).includes(text);
}

function assertGatewayWaitDidNotTimeout(caseName, turn) {
  const status = turn.wait?.status;
  // Plugin-denied tool calls can end the turn with status=error; only a missing
  // or timed-out wait result means the Gateway lane failed to reach a terminal state.
  if (!status || status === "timeout") {
    throw new Error(
      `${caseName}: gateway agent.wait did not complete; status=${String(status ?? "missing")} runId=${turn.runId}`,
    );
  }
}

async function runGatewayPolicyTurn({ callGatewayRpc, caseName, gatewayToken, gatewayUrl, message }) {
  const runId = `agent-sec-policy-${slugify(caseName)}-${Date.now()}`;
  const sessionKey = `agent:main:dashboard:${runId}`;
  const createSession = unwrapGatewayPayload(
    await callGatewayRpc(
      `${caseName}-sessions-create`,
      "sessions.create",
      {
        key: sessionKey,
        agentId: "main",
        label: `AgentSec Policy Matrix ${caseName}`,
      },
      { gatewayToken, gatewayUrl },
    ),
  );
  const send = unwrapGatewayPayload(
    await callGatewayRpc(
      `${caseName}-sessions-send`,
      "sessions.send",
      {
        key: sessionKey,
        idempotencyKey: runId,
        message,
        timeoutMs: DEFAULT_GATEWAY_TURN_TIMEOUT_MS,
      },
      {
        gatewayToken,
        gatewayUrl,
        timeoutMs: DEFAULT_GATEWAY_TURN_TIMEOUT_MS + 30_000,
      },
    ),
  );
  return {
    createSession,
    runId,
    send,
    sessionFile: createSession?.entry?.sessionFile,
    sessionKey,
    readRecords: () => readSessionRecords({ callGatewayRpc, sessionKey, gatewayToken, gatewayUrl }),
  };
}

async function waitForPluginApprovalOrUndefined({
  callGatewayRpc,
  descriptionIncludes,
  gatewayToken,
  gatewayUrl,
  observations,
  timeoutMs,
}) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const snapshot = await listPluginApprovalSnapshot({
        callGatewayRpc,
        descriptionIncludes,
        gatewayToken,
        gatewayUrl,
        timeoutMs: 2_000,
      });
      observations?.push(summarizeApprovalSnapshot(snapshot));
      if (snapshot.matching.length > 0) {
        return snapshot.matching[0];
      }
    } catch (error) {
      observations?.push({
        observedAt: new Date().toISOString(),
        error: {
          name: error?.name,
          message: String(error?.message ?? error),
        },
      });
    }
    await sleep(500);
  }
  return undefined;
}

async function listMatchingPluginApprovals({
  callGatewayRpc,
  descriptionIncludes,
  gatewayToken,
  gatewayUrl,
}) {
  const snapshot = await listPluginApprovalSnapshot({
    callGatewayRpc,
    descriptionIncludes,
    gatewayToken,
    gatewayUrl,
  });
  return snapshot.matching;
}

async function listPluginApprovalSnapshot({
  callGatewayRpc,
  descriptionIncludes,
  gatewayToken,
  gatewayUrl,
  timeoutMs = 10_000,
}) {
  const approvals = unwrapGatewayPayload(
    await callGatewayRpc(
      `plugin-approval-list-${Date.now()}`,
      "plugin.approval.list",
      {},
      { gatewayToken, gatewayUrl, timeoutMs },
    ),
  );
  const approvalList = Array.isArray(approvals) ? approvals : [];
  const matching = approvalList.filter((approval) => {
    const request = approval?.request ?? {};
    return (
      request.pluginId === PLUGIN_ID &&
      request.title === "Code Scanner Security Warning" &&
      request.toolName === "exec" &&
      typeof request.description === "string" &&
      request.description.includes(descriptionIncludes)
    );
  });
  return {
    observedAt: new Date().toISOString(),
    payloadType: Array.isArray(approvals) ? "array" : typeof approvals,
    total: approvalList.length,
    matching,
    approvals: approvalList,
  };
}

async function countJsonLines(file) {
  return (await readJsonLines(file)).length;
}

async function readJsonLinesSince(file, start) {
  return (await readJsonLines(file)).slice(start);
}

function findCliCall(calls, { inputIncludes, subcommand }) {
  return calls.find(
    (call) =>
      call?.subcommand === subcommand &&
      typeof call?.input === "string" &&
      call.input.includes(inputIncludes),
  );
}

function summarizePolicyCliCall(call) {
  if (!call) {
    return undefined;
  }
  return {
    subcommand: call.subcommand,
    input: call.input,
    override: call.override,
    exitCode: call.exitCode,
    verdict: call.stdoutJson?.verdict,
    findings: Array.isArray(call.stdoutJson?.findings) ? call.stdoutJson.findings.length : undefined,
  };
}

async function appendPolicyDebug(file, entry) {
  if (!file) return;
  await fs.appendFile(file, `${JSON.stringify(entry)}\n`);
}

function summarizeApprovalSnapshot(snapshot) {
  return {
    observedAt: snapshot.observedAt,
    payloadType: snapshot.payloadType,
    total: snapshot.total,
    matching: snapshot.matching.length,
    approvals: snapshot.approvals.map(summarizeApproval),
  };
}

function summarizeApproval(approval) {
  const request = approval?.request ?? {};
  const description =
    typeof request.description === "string" ? request.description : "";
  return {
    id: approval?.id,
    status: approval?.status,
    decision: approval?.decision,
    request: {
      pluginId: request.pluginId,
      title: request.title,
      toolName: request.toolName,
      descriptionIncludesPolicyCommand: description.includes(POLICY_CODE_DENY_COMMAND),
      descriptionPreview: description.slice(0, 300),
    },
  };
}

function summarizeToolResultErrors(records) {
  return records
    .filter((record) => {
      const message = record?.message ?? {};
      return (
        record?.type === "message" &&
        message.role === "toolResult" &&
        (message.isError === true || message?.details?.status === "error")
      );
    })
    .map((record) => {
      const message = record.message ?? {};
      return {
        isError: message.isError,
        detailsStatus: message?.details?.status,
        detailsTool: message?.details?.tool,
        detailsError: message?.details?.error,
        preview: JSON.stringify(message?.content ?? message?.details ?? "").slice(0, 500),
      };
    });
}

async function waitForSessionRecords(readRecords, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  let records = [];
  while (Date.now() < deadline) {
    records = await readRecords();
    if (records.length > 0) {
      return records;
    }
    await sleep(250);
  }
  return records;
}

export async function readSessionRecords({ callGatewayRpc, sessionKey, gatewayToken, gatewayUrl }) {
  // sessions.get works with both legacy JSONL and OpenClaw 2.0 SQLite storage.
  // Keep the assertion record shape independent of the host's on-disk format.
  const payload = unwrapGatewayPayload(await callGatewayRpc(
    "policy-session-records",
    "sessions.get",
    { key: sessionKey, limit: 200 },
    { gatewayToken, gatewayUrl },
  ));
  if (!Array.isArray(payload?.messages)) {
    throw new Error("sessions.get did not return a messages array");
  }
  return payload.messages.map((message) => ({ type: "message", message }));
}

function sessionContainsText(records, expected) {
  return records.some((record) => JSON.stringify(record).includes(expected));
}

function sessionHasSuccessfulToolOutput(records, expectedOutput) {
  return records.some((record) => {
    const message = record?.message;
    if (record?.type !== "message" || message?.role !== "toolResult" || message?.isError === true) {
      return false;
    }
    if (message?.details?.aggregated === expectedOutput) {
      return true;
    }
    return (Array.isArray(message?.content) ? message.content : []).some(
      (part) => typeof part?.text === "string" && part.text.includes(expectedOutput),
    );
  });
}

function unwrapGatewayPayload(value) {
  if (value && typeof value === "object" && value.ok === true && value.payload) {
    return value.payload;
  }
  return value;
}

async function waitForGatewayLogSignals(logPaths, timeoutMs, expected = {}) {
  // Logs are supplemental here: they prove the installed plugin emitted pass
  // diagnostics in the happy-path traffic probe, while policy cases use RPC/session evidence.
  const deadline = Date.now() + timeoutMs;
  let text = "";
  while (Date.now() < deadline) {
    text = (await Promise.all(logPaths.map((file) => readTextIfExists(file)))).join("\n");
    const signals = {
      promptScanPass: /\[prompt-scan\] pass/u.test(text),
      codeScanPass: /\[scan-code\].*pass/u.test(text),
    };
    if (expected.scanCodeDeny) {
      const { command, policy } = expected.scanCodeDeny;
      if (
        text.includes(`[scan-code] DENY (policy=${policy})`) &&
        text.includes(`Command: ${command}`)
      ) {
        return {
          ...signals,
          scanCodeDeny: {
            observedAt: new Date().toISOString(),
            verdict: "deny",
            policy,
            command,
            logFiles: logPaths,
          },
        };
      }
    }
    if (!expected.scanCodeDeny && signals.promptScanPass && signals.codeScanPass) {
      return signals;
    }
    await sleep(500);
  }
  text = (await Promise.all(logPaths.map((file) => readTextIfExists(file)))).join("\n");
  if (expected.scanCodeDeny) {
    const { command, policy } = expected.scanCodeDeny;
    throw new Error(
      `gateway logs did not contain scan-code DENY policy=${policy} command=${JSON.stringify(command)}; files=${logPaths.join(", ")} tail=${text.slice(-2000)}`,
    );
  }
  throw new Error(
    `gateway logs did not contain prompt-scan/code-scan pass signals; tail=${text.slice(-2000)}`,
  );
}

async function waitForObservabilityHooks({
  expectedToolCommand,
  expectedToolOutput,
  observabilityPath,
  requireFinalResponse,
  requiredHooks,
  requireModelCalls,
  runId,
  timeoutMs,
}) {
  // Observability records are keyed by runId so older records in the same data
  // dir cannot make the current Gateway turn pass accidentally.
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const records = (await readJsonLines(observabilityPath)).filter((record) => {
      const metadata = record?.metadata ?? {};
      return metadata.runId === runId;
    });
    const evidence = summarizeObservabilityEvidence({
      expectedToolCommand,
      expectedToolOutput,
      observabilityPath,
      records,
      requireFinalResponse,
      requiredHooks,
      requireModelCalls,
    });
    if (isObservabilityEvidenceComplete(evidence)) {
      return evidence;
    }
    await sleep(500);
  }

  const records = (await readJsonLines(observabilityPath)).filter((record) => {
    const metadata = record?.metadata ?? {};
    return metadata.runId === runId;
  });
  const evidence = summarizeObservabilityEvidence({
    expectedToolCommand,
    expectedToolOutput,
    observabilityPath,
    records,
    requireFinalResponse,
    requiredHooks,
    requireModelCalls,
  });
  const failedAssertions = Object.entries(evidence.assertions)
    .filter(([, value]) => value !== true)
    .map(([key]) => key);
  throw new Error(
    `observability evidence incomplete for run ${runId}: failed=${failedAssertions.join(",") || "<none>"} ` +
      `hooks=${evidence.hooks.join(",") || "<none>"} path=${observabilityPath}`,
  );
}

function summarizeObservabilityEvidence({
  expectedToolCommand,
  expectedToolOutput,
  observabilityPath,
  records,
  requireFinalResponse,
  requiredHooks,
  requireModelCalls,
}) {
  const hooks = [...new Set(records.map((record) => record.hook).filter(Boolean))].sort();
  const countsByHook = countObservabilityHooks(records);
  const beforeToolRecords = records.filter((record) => record.hook === "before_tool_call");
  const afterToolRecords = records.filter((record) => record.hook === "after_tool_call");
  const beforeLlmRecords = records.filter((record) => record.hook === "before_llm_call");
  const afterLlmRecords = records.filter((record) => record.hook === "after_llm_call");
  const sessionIds = uniqueNonEmptyStrings(records.map((record) => record?.metadata?.sessionId));
  const beforeLlmCallIds = uniqueNonEmptyStrings(
    beforeLlmRecords.map((record) => record?.metadata?.callId),
  );
  const afterLlmCallIds = uniqueNonEmptyStrings(
    afterLlmRecords.map((record) => record?.metadata?.callId),
  );
  const sharedLlmCallIds = beforeLlmCallIds.filter((callId) => afterLlmCallIds.includes(callId));
  const beforeToolCallIds = uniqueNonEmptyStrings(
    beforeToolRecords.map((record) => record?.metadata?.toolCallId),
  );
  const afterToolCallIds = uniqueNonEmptyStrings(
    afterToolRecords.map((record) => record?.metadata?.toolCallId),
  );
  const sharedToolCallIds = beforeToolCallIds.filter((toolCallId) =>
    afterToolCallIds.includes(toolCallId),
  );
  const metricKeysByHook = summarizeMetricKeysByHook(records);
  const assertions = {
    requiredHooksPresent: requiredHooks.every((hook) => hooks.includes(hook)),
    scopedToRunAndSession: records.every((record) => {
      const metadata = record?.metadata ?? {};
      return typeof metadata.runId === "string" && typeof metadata.sessionId === "string";
    }),
    singleSessionObserved: sessionIds.length === 1,
    twoModelCallsObserved:
      !requireModelCalls ||
      ((countsByHook.before_llm_call ?? 0) >= 2 && (countsByHook.after_llm_call ?? 0) >= 2),
    modelCallIdsLinked: !requireModelCalls || sharedLlmCallIds.length >= 2,
    execBeforeToolRecorded: beforeToolRecords.some((record) => record?.metrics?.tool_name === "exec"),
    execCommandRecorded: beforeToolRecords.some((record) =>
      JSON.stringify(record?.metrics?.parameters ?? "").includes(expectedToolCommand),
    ),
    execAfterToolRecorded: afterToolRecords.length > 0,
    execOutputRecorded: afterToolRecords.some((record) =>
      JSON.stringify(record?.metrics?.result ?? "").includes(expectedToolOutput),
    ),
    toolCallIdLinked: sharedToolCallIds.length > 0,
    finalResponseRecorded:
      !requireFinalResponse ||
      records
        .filter((record) => record.hook === "after_agent_run")
        .some((record) => JSON.stringify(record?.metrics ?? "").includes(expectedToolOutput)),
  };

  return {
    path: observabilityPath,
    count: records.length,
    hooks,
    countsByHook,
    metricKeysByHook,
    sessionIds,
    modelCallIds: {
      beforeLlmCall: beforeLlmCallIds,
      afterLlmCall: afterLlmCallIds,
      shared: sharedLlmCallIds,
    },
    toolCallIds: {
      beforeToolCall: beforeToolCallIds,
      afterToolCall: afterToolCallIds,
      shared: sharedToolCallIds,
    },
    assertions,
    requirements: {
      finalResponse: requireFinalResponse,
      modelCalls: requireModelCalls,
      hooks: requiredHooks,
    },
  };
}

function resolveObservabilityRequirement(runtimeInspect) {
  const diagnostics = Array.isArray(runtimeInspect?.diagnostics) ? runtimeInspect.diagnostics : [];
  const blockedConversationHooks = diagnostics
    .map((diagnostic) => String(diagnostic?.message ?? ""))
    .filter((message) =>
      /typed hook "(llm_input|llm_output|agent_end)" blocked because non-bundled plugins must set plugins\.entries\.agent-sec\.hooks\.allowConversationAccess=true/u.test(
        message,
      ),
    );
  const ignoredModelCallHooks = diagnostics
    .map((diagnostic) => String(diagnostic?.message ?? ""))
    .filter((message) => /unknown typed hook "model_call_(started|ended)" ignored/u.test(message));
  const conversation = blockedConversationHooks.length > 0
    ? {
        required: false,
        reason: "openclaw-runtime-blocks-conversation-hooks-without-allow-conversation-access",
        diagnostics: blockedConversationHooks,
      }
    : {
        required: true,
        reason: "openclaw-runtime-allows-conversation-hooks",
      };
  const modelCalls = ignoredModelCallHooks.length > 0
    ? {
        required: false,
        reason: "openclaw-runtime-ignores-model-call-hooks",
        diagnostics: ignoredModelCallHooks,
      }
    : {
        required: true,
        reason: "openclaw-runtime-supports-model-call-hooks",
      };
  const requiredHooks = ["before_tool_call", "after_tool_call"];
  if (conversation.required) {
    requiredHooks.unshift("before_agent_run");
    requiredHooks.push("after_agent_run");
  }
  if (modelCalls.required) {
    requiredHooks.push("before_llm_call", "after_llm_call");
  }
  return {
    conversation,
    modelCalls,
    requiredHooks,
  };
}

function isObservabilityEvidenceComplete(evidence) {
  return Object.values(evidence.assertions).every((value) => value === true);
}

function countObservabilityHooks(records) {
  const counts = {};
  for (const record of records) {
    if (typeof record?.hook !== "string") {
      continue;
    }
    counts[record.hook] = (counts[record.hook] ?? 0) + 1;
  }
  return counts;
}

function summarizeMetricKeysByHook(records) {
  const keysByHook = {};
  for (const record of records) {
    if (typeof record?.hook !== "string") {
      continue;
    }
    const metricKeys = Object.keys(record?.metrics ?? {});
    const existing = keysByHook[record.hook] ?? new Set();
    for (const key of metricKeys) {
      existing.add(key);
    }
    keysByHook[record.hook] = existing;
  }
  return Object.fromEntries(
    Object.entries(keysByHook).map(([hook, keys]) => [hook, [...keys].sort()]),
  );
}

function uniqueNonEmptyStrings(values) {
  return [...new Set(values.filter((value) => typeof value === "string" && value.length > 0))];
}
