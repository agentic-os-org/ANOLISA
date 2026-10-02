import fs from "node:fs/promises";
import http from "node:http";
import path from "node:path";

import {
  MOCK_MODEL_ID,
  MOCK_MODEL_PROVIDER_ID,
  POLICY_CODE_DENY_COMMAND,
  POLICY_CODE_DENY_MARKER,
  POLICY_CODE_DONE_TEXT,
  POLICY_PROMPT_DENY_MARKER,
  POLICY_PROMPT_REACHED_MODEL_TEXT,
  PII_GATEWAY_MARKER,
  PII_GATEWAY_REPLY,
  sleep,
} from "./common.mjs";
import { formatError } from "./errors.mjs";

export async function startMockModelServer({ logsDir, registerServer }) {
  // The mock implements just enough of OpenAI chat completions for OpenClaw to
  // run a real model/tool turn. Requests are logged so probes can assert whether
  // a prompt reached the model or was blocked before model invocation.
  const requests = [];
  const requestsLog = path.join(logsDir, "mock-model-requests.jsonl");
  const server = http.createServer(async (req, res) => {
    try {
      if (req.method === "GET" && req.url === "/v1/models") {
        writeJson(res, 200, {
          object: "list",
          data: [{ id: MOCK_MODEL_ID, object: "model", owned_by: "agent-sec-pilot" }],
        });
        return;
      }

      if (req.method === "POST" && req.url === "/v1/chat/completions") {
        const body = await readRequestJson(req);
        const entry = {
          receivedAt: new Date().toISOString(),
          method: req.method,
          url: req.url,
          body,
        };
        requests.push(entry);
        await fs.appendFile(requestsLog, `${JSON.stringify(entry)}\n`);
        respondMockChatCompletion(res, body);
        return;
      }

      writeJson(res, 404, { error: { message: `not found: ${req.method} ${req.url}` } });
    } catch (error) {
      writeJson(res, 500, { error: { message: formatError(error) } });
    }
  });

  await new Promise((resolve, reject) => {
    server.listen(0, "127.0.0.1", resolve);
    server.once("error", reject);
  });
  const address = server.address();
  if (!address || typeof address !== "object") {
    throw new Error("mock model server did not expose a TCP address");
  }

  const ref = {
    name: "mock-model-server",
    baseUrl: `http://127.0.0.1:${address.port}`,
    requests,
    requestsLog,
    server,
  };
  try {
    registerServer?.(ref);
  } catch (error) {
    await closeHttpServer(server);
    throw error;
  }
  return ref;
}

export async function configureGatewayPilotModel({ env, mockModel, pluginRoot, runRequiredStep }) {
  // Configure OpenClaw to use the local mock model through the normal config
  // surface. This keeps the test model-driven instead of calling hooks directly.
  const modelRef = `${MOCK_MODEL_PROVIDER_ID}/${MOCK_MODEL_ID}`;
  const providerConfig = {
    baseUrl: `${mockModel.baseUrl}/v1`,
    api: "openai-completions",
    apiKey: "agent-sec-pilot-key",
    request: {
      allowPrivateNetwork: true,
    },
    models: [
      {
        id: MOCK_MODEL_ID,
        name: "AgentSec Pilot Tool Model",
        api: "openai-completions",
        reasoning: false,
        input: ["text"],
        // OpenClaw 2026.6.11+ performs a context-overflow precheck before the
        // provider is called. Keep the mock model large enough for Gateway's
        // normal system/tool prompt so the test reaches the plugin hooks.
        contextWindow: 65536,
        maxTokens: 1024,
        cost: {
          input: 0,
          output: 0,
          cacheRead: 0,
          cacheWrite: 0,
        },
        compat: {
          supportsStrictMode: false,
          supportsUsageInStreaming: false,
        },
      },
    ],
  };

  await runRequiredStep(
    "openclaw-config-pilot-model-provider",
    "openclaw",
    [
      "config",
      "set",
      `models.providers.${MOCK_MODEL_PROVIDER_ID}`,
      JSON.stringify(providerConfig),
      "--strict-json",
    ],
    { cwd: pluginRoot, env },
  );
  await runRequiredStep(
    "openclaw-config-pilot-default-model",
    "openclaw",
    ["config", "set", "agents.defaults.model.primary", JSON.stringify(modelRef), "--strict-json"],
    { cwd: pluginRoot, env },
  );
  await runRequiredStep(
    "openclaw-config-pilot-model-allowlist",
    "openclaw",
    [
      "config",
      "set",
      "agents.defaults.models",
      JSON.stringify({ [modelRef]: {} }),
      "--strict-json",
    ],
    { cwd: pluginRoot, env },
  );
  await runRequiredStep(
    "openclaw-config-pilot-tools-profile",
    "openclaw",
    ["config", "set", "tools.profile", JSON.stringify("full"), "--strict-json"],
    { cwd: pluginRoot, env },
  );
  await runRequiredStep(
    "openclaw-config-pilot-exec-host",
    "openclaw",
    ["config", "set", "tools.exec.host", JSON.stringify("gateway"), "--strict-json"],
    { cwd: pluginRoot, env },
  );
  await runRequiredStep(
    "openclaw-config-pilot-exec-security",
    "openclaw",
    ["config", "set", "tools.exec.security", JSON.stringify("full"), "--strict-json"],
    { cwd: pluginRoot, env },
  );
  await runRequiredStep(
    "openclaw-config-pilot-exec-ask",
    "openclaw",
    ["config", "set", "tools.exec.ask", JSON.stringify("off"), "--strict-json"],
    { cwd: pluginRoot, env },
  );
}

export async function waitForMockModelToolTurn(mockModel, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const requests = mockModel.requests.slice();
    const hasToolCallRequest = requests.some((request) => requestBodyHasExecTool(request.body));
    const hasToolResultRequest = requests.some((request) =>
      (Array.isArray(request.body?.messages) ? request.body.messages : []).some(
        (message) => message?.role === "tool",
      ),
    );
    if (requests.length >= 2 && hasToolCallRequest && hasToolResultRequest) {
      return requests;
    }
    await sleep(250);
  }
  throw new Error(`mock model did not observe a two-call tool turn; log=${mockModel.requestsLog}`);
}

export function summarizeMockModelRequests(requests) {
  return requests.map((request, index) => {
    const body = request.body ?? {};
    const messages = Array.isArray(body.messages) ? body.messages : [];
    return {
      index,
      receivedAt: request.receivedAt,
      model: body.model,
      stream: body.stream,
      tools: (Array.isArray(body.tools) ? body.tools : []).map(
        (tool) => tool?.function?.name ?? tool?.name,
      ),
      hasToolResultMessage: messages.some((message) => message?.role === "tool"),
      messageRoles: messages.map((message) => message?.role).filter(Boolean),
    };
  });
}

async function readRequestJson(req) {
  const chunks = [];
  for await (const chunk of req) {
    chunks.push(Buffer.from(chunk));
  }
  const text = Buffer.concat(chunks).toString("utf8");
  return text ? JSON.parse(text) : {};
}

function closeHttpServer(server) {
  return new Promise((resolve, reject) => {
    server.close((error) => {
      if (error) {
        reject(error);
        return;
      }
      resolve();
    });
  });
}

function respondMockChatCompletion(res, body) {
  const messages = Array.isArray(body?.messages) ? body.messages : [];
  const hasToolResult = messages.some((message) => message?.role === "tool");
  const scenario = resolveMockScenario(messages);
  const created = Math.floor(Date.now() / 1000);
  const id = `chatcmpl-agent-sec-pilot-${created}`;
  const model = typeof body?.model === "string" ? body.model : MOCK_MODEL_ID;

  if (body?.stream !== true) {
    writeJson(res, 200, buildNonStreamingMockCompletion({ hasToolResult, id, model, created, scenario }));
    return;
  }

  // Most Gateway runs use streaming completions. The chunks below mimic the
  // minimal role/content/tool_call shapes OpenClaw expects from a provider.
  res.writeHead(200, {
    "Content-Type": "text/event-stream; charset=utf-8",
    "Cache-Control": "no-cache",
    Connection: "keep-alive",
  });
  for (const chunk of buildStreamingMockChunks({ hasToolResult, id, model, created, scenario })) {
    res.write(`data: ${JSON.stringify(chunk)}\n\n`);
  }
  res.write("data: [DONE]\n\n");
  res.end();
}

function resolveMockScenario(messages) {
  // The user's test prompt selects a scenario; no hidden state is used, so each
  // matrix case remains reproducible after Gateway restarts.
  const userText = messages
    .filter((message) => message?.role === "user")
    .map((message) => collectMessageText(message.content))
    .join("\n");
  if (userText.includes(PII_GATEWAY_MARKER)) return "pii";
  if (userText.includes(POLICY_PROMPT_DENY_MARKER)) {
    return "policy-prompt";
  }
  if (userText.includes(POLICY_CODE_DENY_MARKER)) {
    return "policy-code";
  }
  return "safe-exec";
}

function collectMessageText(content) {
  if (typeof content === "string") return content;
  if (!Array.isArray(content)) return "";
  return content
    .map((part) => {
      if (typeof part === "string") return part;
      if (typeof part?.text === "string") return part.text;
      if (typeof part?.content === "string") return part.content;
      return "";
    })
    .join("\n");
}

function buildStreamingMockChunks({ hasToolResult, id, model, created, scenario }) {
  const base = {
    id,
    object: "chat.completion.chunk",
    created,
    model,
  };
  if (scenario === "policy-prompt" || scenario === "pii") {
    return [
      { ...base, choices: [{ index: 0, delta: { role: "assistant" } }] },
      {
        ...base,
        choices: [{
          index: 0,
          delta: { content: scenario === "pii" ? PII_GATEWAY_REPLY : POLICY_PROMPT_REACHED_MODEL_TEXT },
        }],
      },
      { ...base, choices: [{ index: 0, delta: {}, finish_reason: "stop" }] },
    ];
  }
  if (hasToolResult) {
    return [
      { ...base, choices: [{ index: 0, delta: { role: "assistant" } }] },
      {
        ...base,
        choices: [
          {
            index: 0,
            delta: {
              content:
                scenario === "policy-code"
                  ? POLICY_CODE_DONE_TEXT
                  : "agent-sec-pilot-safe observed through gateway exec",
            },
          },
        ],
      },
      { ...base, choices: [{ index: 0, delta: {}, finish_reason: "stop" }] },
    ];
  }
  // First model call asks OpenClaw to execute a real Gateway exec tool. The
  // second call, after tool result delivery, summarizes the observed result.
  return [
    { ...base, choices: [{ index: 0, delta: { role: "assistant" } }] },
    {
      ...base,
      choices: [
        {
          index: 0,
          delta: {
            tool_calls: [
              {
                index: 0,
                id: "call_agent_sec_pilot_exec",
                type: "function",
                function: {
                  name: "exec",
                  arguments: JSON.stringify({
                    command: scenario === "policy-code" ? POLICY_CODE_DENY_COMMAND : "printf agent-sec-pilot-safe",
                  }),
                },
              },
            ],
          },
          finish_reason: "tool_calls",
        },
      ],
    },
  ];
}

function buildNonStreamingMockCompletion({ hasToolResult, id, model, created, scenario }) {
  if (scenario === "policy-prompt" || scenario === "pii") {
    return {
      id,
      object: "chat.completion",
      created,
      model,
      choices: [
        {
          index: 0,
          message: {
            role: "assistant",
            content: scenario === "pii" ? PII_GATEWAY_REPLY : POLICY_PROMPT_REACHED_MODEL_TEXT,
          },
          finish_reason: "stop",
        },
      ],
    };
  }
  if (hasToolResult) {
    return {
      id,
      object: "chat.completion",
      created,
      model,
      choices: [
        {
          index: 0,
          message: {
            role: "assistant",
            content:
              scenario === "policy-code"
                ? POLICY_CODE_DONE_TEXT
                : "agent-sec-pilot-safe observed through gateway exec",
          },
          finish_reason: "stop",
        },
      ],
    };
  }
  return {
    id,
    object: "chat.completion",
    created,
    model,
    choices: [
      {
        index: 0,
        message: {
          role: "assistant",
          content: null,
          tool_calls: [
            {
              id: "call_agent_sec_pilot_exec",
              type: "function",
              function: {
                name: "exec",
                arguments: JSON.stringify({
                  command: scenario === "policy-code" ? POLICY_CODE_DENY_COMMAND : "printf agent-sec-pilot-safe",
                }),
              },
            },
          ],
        },
        finish_reason: "tool_calls",
      },
    ],
  };
}

function requestBodyHasExecTool(body) {
  return (Array.isArray(body?.tools) ? body.tools : []).some((tool) => {
    const name = tool?.function?.name ?? tool?.name;
    return name === "exec";
  });
}

function writeJson(res, statusCode, payload) {
  res.writeHead(statusCode, { "Content-Type": "application/json; charset=utf-8" });
  res.end(`${JSON.stringify(payload)}\n`);
}
