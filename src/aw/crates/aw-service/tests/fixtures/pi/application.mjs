import { EventStream } from "@earendil-works/pi-ai/compat";
import { Type } from "typebox";
import { writeFileSync, mkdirSync, existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { spawnSync } from "node:child_process";
const root = process.env.PI_TEST_ROOT;
const toolName = process.env.PI_TOOL_NAME ?? "test";
const executedCommands = [];
const prettyJson = JSON.stringify(
  {
    rows: Array.from({ length: 40 }, (_, id) => ({
      id,
      label: `row-${id}`,
      enabled: true,
      description: "public SDK compression fixture",
    })),
  },
  null,
  2,
);
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const settings = {
  role: "assistant",
  api: "openai-responses",
  provider: "openai",
  model: "mock",
  timestamp: 0,
  usage: {
    input: 0,
    output: 0,
    cacheRead: 0,
    cacheWrite: 0,
    totalTokens: 0,
    cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
  },
};
let sideEffects = [];
let sdkModule;
let beforeCalls = 0;
let afterCalls = 0;
const modelToolResults = [];
const liveCounts = [];
const callbackMutations = [];
export async function createSession({ sdk, sessionManager }) {
  sdkModule = sdk;
  const agentDir = join(root, "agent");
  mkdirSync(agentDir, { recursive: true });
  const settingsManager = sdk.SettingsManager.inMemory({
    toolExecution: "parallel",
    defaultThinkingLevel: "off",
  });
  const loader = new sdk.DefaultResourceLoader({
    cwd: root,
    agentDir,
    settingsManager,
    noExtensions: true,
    noSkills: true,
    noPromptTemplates: true,
    noThemes: true,
    noContextFiles: true,
  });
  await loader.reload();
  const modelRuntime = await sdk.ModelRuntime.create({
    authPath: join(agentDir, "auth.json"),
    modelsPath: null,
    refreshOnCreate: false,
    allowModelNetwork: false,
  });
  const model = {
    id: "mock",
    name: "mock",
    provider: "openai",
    api: "openai-responses",
    baseUrl: "http://127.0.0.1:1",
    reasoning: false,
    input: ["text"],
    cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
    contextWindow: 10000,
    maxTokens: 1000,
  };
  const result = await sdk.createAgentSession({
    cwd: root,
    agentDir,
    modelRuntime,
    settingsManager,
    resourceLoader: loader,
    sessionManager:
      sessionManager ??
      (process.env.PI_FACTORY_MODE
        ? sdk.SessionManager.create(root, join(agentDir, "sessions"))
        : sdk.SessionManager.inMemory(root)),
    model,
    tools: [toolName],
    customTools: [
      {
        name: toolName,
        label: "test",
        description: "test",
        parameters: Type.Object({
          deny: Type.Boolean(),
          fail: Type.Boolean(),
          command: Type.Optional(Type.String()),
        }),
        async execute(id, args) {
          if (
            process.env.PI_WAIT === "1" &&
            !existsSync(join(root, "tool.before-waited"))
          )
            throw new Error("before did not wait");
          sideEffects.push(id);
          if (args.command) executedCommands.push(args.command);
          writeFileSync(
            join(root, "side-effects.json"),
            JSON.stringify(sideEffects),
          );
          if (process.env.PI_PARALLEL === "1")
            await sleep(id.endsWith("slow") ? 100 : 1);
          if (args.fail) throw new Error("tool failed");
          return {
            content:
              process.env.PI_MULTI === "1"
                ? [
                    { type: "text", text: "tool okay" },
                    { type: "text", text: "second block" },
                  ]
                : process.env.PI_IMAGE === "1"
                  ? [
                      {
                        type: "image",
                        mimeType: "image/png",
                        data: "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Wl6FDsAAAAASUVORK5CYII=",
                      },
                    ]
                  : [
                      {
                        type: "text",
                        text:
                          process.env.PI_PRETTY_JSON === "1"
                            ? prettyJson
                            : "tool okay",
                      },
                    ],
            details: { id },
          };
        },
      },
    ],
  });
  const original = result.session.agent.beforeToolCall;
  result.session.agent.beforeToolCall = async (context, signal) => {
    beforeCalls++;
    const value = await original(context, signal);
    if (process.env.PI_MUTATE === "1") context.args.deny = true;
    return value;
  };
  const originalAfter = result.session.agent.afterToolCall;
  result.session.agent.afterToolCall = async (context, signal) => {
    afterCalls++;
    const value = await originalAfter(context, signal);
    if (process.env.PI_NATIVE_AFTER_ERROR === "1")
      throw new Error("native after failed");
    const modified = { ...value };
    if (process.env.PI_METADATA === "1") {
      modified.details = { native: true, id: context.toolCall.id };
      modified.usage = settings.usage;
      modified.terminate = false;
      modified.isError = value?.isError ?? context.isError;
    }
    if (process.env.PI_REPLACE === "1") {
      modified.content = [{ type: "text", text: "replacement" }];
      modified.isError = true;
    }
    if (process.env.PI_CLEAR_ERROR === "1") modified.isError = false;
    if (process.env.PI_NULLISH === "1") {
      for (const key of ["content", "details", "usage", "terminate", "isError"])
        modified[key] = null;
    }
    return Object.keys(modified).length ? modified : value;
  };
  return result;
}
export async function run(session, { createSession }) {
  session.agent.toolExecution = "parallel";
  const rounds = process.env.PI_ROUNDS
    ? Number(process.env.PI_ROUNDS)
    : process.env.PI_TWO_SESSIONS === "1" ||
        process.env.PI_FACTORY_MODE ||
        process.env.PI_REUSE_ID === "1"
      ? 2
      : 1;
  const sessions = [];
  for (let round = 0; round < rounds; round++) {
    if (round) {
      const mode = process.env.PI_FACTORY_MODE;
      if (mode) {
        let manager;
        if (mode === "new") manager = sdkModule.SessionManager.inMemory(root);
        if (mode === "resume")
          manager = sdkModule.SessionManager.open(
            session.sessionManager.getSessionFile(),
          );
        if (mode === "fork") {
          const path = session.sessionManager.createBranchedSession(
            session.sessionManager.getLeafId(),
          );
          manager = sdkModule.SessionManager.open(path);
        }
        if (mode === "import")
          manager = sdkModule.SessionManager.forkFrom(
            session.sessionManager.getSessionFile(),
            root,
            join(root, "agent/imported"),
          );
        session = (await createSession({ sessionManager: manager })).session;
        session.agent.toolExecution = "parallel";
      } else if (process.env.PI_REUSE_ID !== "1")
        session.sessionManager.newSession();
    }
    const id = session.sessionManager.getSessionId();
    sessions.push(id);
    if (process.env.PI_LATE_CALLBACK) {
      const key = process.env.PI_LATE_CALLBACK;
      let rejected = false;
      try {
        session.agent[key] = async () => undefined;
      } catch (error) {
        if (!error.message.includes(`AW owns ${key}`)) throw error;
        rejected = true;
      }
      callbackMutations.push({ key, rejected });
    }
    let requested = false;
    let requestCount = 0;
    session.agent.streamFunction = (_model, context) => {
      if (requested)
        modelToolResults.push(
          ...context.messages
            .filter((message) => message.role === "toolResult")
            .slice(-1),
        );
      const stream = new EventStream(
        (e) => e.type === "done" || e.type === "error",
        (e) => (e.type === "done" ? e.message : e.error),
      );
      const calls = (
        process.env.PI_PARALLEL === "1"
          ? [`call-${round}-slow`, `call-${round}-fast`]
          : [
              process.env.PI_REUSE_ID === "1"
                ? "reused-native-id"
                : `call-${round}`,
            ]
      ).map((id) => ({
        type: "toolCall",
        id,
        name: toolName,
        arguments: {
          deny: process.env.PI_DENY === "1",
          fail: process.env.PI_FAIL === "1",
        },
      }));
      if (process.env.PI_COMMAND)
        calls[0].arguments.command = process.env.PI_COMMAND;
      let retry;
      if (
        requested &&
        requestCount === 1 &&
        process.env.PI_TOKENLESS_RETRY === "1"
      ) {
        const previous = context.messages
          .filter((message) => message.role === "toolResult")
          .at(-1);
        const text = previous?.content?.[0]?.text;
        const prefix = "[tokenless:rewrite] Retry this tool with arguments: ";
        if (!text?.startsWith(prefix))
          throw new Error("Tokenless retry guidance missing from model input");
        retry = {
          type: "toolCall",
          id: "retry-call",
          name: toolName,
          arguments: JSON.parse(text.slice(prefix.length)),
        };
      }
      const content = retry
        ? [retry]
        : requested
          ? [{ type: "text", text: "done" }]
          : calls;
      const message = {
        ...settings,
        content,
        stopReason: retry || !requested ? "toolUse" : "stop",
      };
      requested = true;
      requestCount++;
      stream.push({ type: "done", reason: message.stopReason, message });
      stream.end();
      return stream;
    };
    await session.agent.prompt("test");
    if (process.env.PI_CHECK_LIVE === "1") {
      const native = JSON.parse(
        readFileSync(process.env.AW_NATIVE_CONFIG, "utf8"),
      );
      const binding = JSON.parse(readFileSync(native.binding, "utf8"));
      const status = spawnSync(
        native.executable,
        ["status", "--socket", binding.socket],
        { encoding: "utf8", timeout: 5000 },
      );
      if (status.status !== 0)
        throw new Error(`live status failed: ${status.stderr}`);
      const counts = JSON.parse(status.stdout).result;
      liveCounts.push(counts.events);
      if (counts.events !== 0)
        throw new Error(`live AW events leaked after turn ${round}`);
    }
  }
  writeFileSync(
    join(root, "results.json"),
    JSON.stringify({
      sessions,
      sideEffects,
      messages: session.agent.state.messages,
      modelToolResults,
      beforeCalls,
      afterCalls,
      liveCounts,
      callbackMutations,
      executedCommands,
      prettyJson,
    }),
  );
  console.log(
    JSON.stringify({
      sessions,
      sideEffects,
      messages: session.agent.state.messages,
      modelToolResults,
      beforeCalls,
      afterCalls,
      liveCounts,
      callbackMutations,
      executedCommands,
      prettyJson,
    }),
  );
}
