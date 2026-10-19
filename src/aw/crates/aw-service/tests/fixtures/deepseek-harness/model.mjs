/** Real headless loop with a deterministic in-process model and observable tools. */
import { appendFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { LlmAdapter } from "@deepseek-ai/dsh-llm";
import { defineTool } from "@deepseek-ai/dsh-tools";

const root = process.env.AW_DSH_TEST_ROOT;
const mode = process.env.AW_DSH_TEST_CASE;
const log = (name, value) =>
  appendFileSync(join(root, name), JSON.stringify(value) + "\n");
const pretty = JSON.stringify(
  {
    items: Array.from({ length: 80 }, (_, i) => ({
      id: i,
      status: "ready",
      values: [1, 2, 3],
    })),
  },
  null,
  4,
);

function* calls(specs) {
  for (const [index, spec] of specs.entries()) {
    yield { type: "block-start", index, blockType: "tool-call" };
    yield {
      type: "block-end",
      index,
      block: {
        type: "tool-call",
        id: `call-${index}`,
        name: spec.name ?? "execute_shell_command",
        arguments: JSON.stringify(spec.args),
      },
    };
  }
  yield { type: "usage", usage: { inputTokens: 5, outputTokens: 5 } };
  yield { type: "finish", reason: { kind: "tool-calls" } };
}

class Model extends LlmAdapter {
  requests = 0;
  async *stream(options) {
    log("model.jsonl", options.messages);
    this.requests++;
    if (this.requests === 1) {
      if (mode === "parallel")
        yield* calls([
          { args: { command: "first" } },
          { args: { command: "second" } },
        ]);
      else
        yield* calls([
          {
            args: {
              command: mode.startsWith("retry-")
                ? "git status"
                : mode === "security-deny"
                  ? "forbidden"
                  : "printf json",
            },
          },
        ]);
      return;
    }
    if (mode.startsWith("retry-") && this.requests === 2) {
      const serialized = JSON.stringify(options.messages);
      const marker = "[tokenless:rewrite] Retry this tool with arguments: ";
      // Read the native final result directly; the next request below is a new call.
      const previous = globalThis.awLastResult;
      const text = previous?.content?.[0]?.text ?? "";
      const at = text.indexOf(marker);
      if (at < 0 || !serialized.includes("tokenless:rewrite"))
        throw Error("retry did not reach model");
      const args = JSON.parse(text.slice(at + marker.length));
      yield* calls([{ args }]);
      return;
    }
    yield { type: "block-start", index: 0, blockType: "text" };
    yield {
      type: "block-end",
      index: 0,
      block: { type: "text", text: "AW_NATIVE_DONE" },
    };
    yield { type: "usage", usage: { inputTokens: 5, outputTokens: 2 } };
    yield { type: "finish", reason: { kind: "stop" } };
  }
}

export const name = "aw-native-fixture";
export const inject = ["llm", "tools", "awDeepseek"];
export function apply(ctx) {
  ctx.llm.registerAdapter(["deepseek-official"], new Model());
  let started = 0;
  let release;
  const bothStarted = new Promise((resolve) => {
    release = resolve;
  });
  ctx.tools.register(
    defineTool({
      name: "execute_shell_command",
      description: "deterministic AW acceptance tool",
      parameters: { command: { type: "string", required: true } },
      output: {
        schema: { type: "string" },
        render: (_args, value) => [{ type: "text", text: value }],
        presentationMeta: () => ({ preserved: true }),
      },
      ...(mode === "finalizer"
        ? {
            finalizeContent: (_exec, result) => [
              ...result.content,
              { type: "text", text: "native finalizer" },
            ],
          }
        : {}),
      isConcurrencySafe: () => true,
      async execute(args, exec) {
        log("executed.jsonl", { id: exec.callId, command: args.command });
        if (mode === "parallel") {
          if (++started === 2) release();
          await Promise.race([
            bothStarted,
            new Promise((_, reject) => {
              const timer = setTimeout(
                () => reject(Error("native parallel barrier timed out")),
                5000,
              );
              bothStarted.then(() => clearTimeout(timer));
            }),
          ]);
        }
        if (mode === "tool-error") throw Error("native tool failed");
        if (mode === "cancel") {
          writeFileSync(join(root, "tool-started"), String(process.pid));
          await new Promise((resolve) => {
            if (exec.signal.aborted) resolve();
            else exec.signal.addEventListener("abort", resolve, { once: true });
          });
        }
        if (mode === "nested-deny") {
          const nested = await ctx.tools.execute({
            name: "aw_inner",
            callId: "nested",
            rootCallId: exec.rootCallId,
            parent: exec.token,
            agent: exec.agent,
            arguments: { command: "forbidden" },
            signal: exec.signal,
          });
          if (!nested.isError) throw Error("nested denial was lost");
        }
        return pretty;
      },
    }),
  );
  ctx.tools.register(
    defineTool({
      name: "aw_inner",
      description: "nested denial witness",
      parameters: { command: { type: "string", required: true } },
      output: {
        schema: { type: "string" },
        render: (_args, value) => [{ type: "text", text: value }],
      },
      async execute() {
        log("inner-executed.jsonl", true);
        return "must not execute";
      },
    }),
  );
  if (["native-deny", "native-bypass", "native-override"].includes(mode)) {
    ctx.on(
      "tools/pre-execute",
      async (_exec, next) => {
        if (mode === "native-deny")
          return { kind: "deny", reason: "native permission denied" };
        if (mode === "native-override") await next();
        return { kind: "allow" };
      },
      { prepend: true },
    );
  }
  if (["post-content", "post-value", "post-block"].includes(mode)) {
    // Installed inside AW's post listener: AW must honor the native decision.
    ctx.on("tools/post-execute", async (_exec, _result, next) => {
      await next();
      if (mode === "post-value")
        return { kind: "accept", value: "native canonical value" };
      if (mode === "post-block")
        return {
          kind: "block",
          feedback: [{ type: "text", text: "native post denial" }],
        };
      return {
        kind: "accept",
        content: [
          { type: "text", text: pretty.replace('"items"', '"native_policy"') },
        ],
      };
    });
  }
  ctx.on("tools/result", (exec, result) => {
    globalThis.awLastResult = result;
    log("results.jsonl", {
      id: exec.callId,
      nested: exec.parent !== undefined,
      result,
    });
  });
  ctx.effect(() => () => writeFileSync(join(root, "disposed"), "yes"));
}
