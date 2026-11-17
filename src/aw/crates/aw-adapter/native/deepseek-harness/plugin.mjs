/** Native tool policy; framework permissions and structured values stay native. */
import { randomUUID } from "node:crypto";
import { readFileSync, writeFileSync, renameSync, unlinkSync } from "node:fs";
import { transport } from "./callback.mjs";

export const name = "aw-deepseek";
export const inject = ["tools"];

function identity(value, label) {
  if (
    typeof value !== "string" ||
    !value.trim() ||
    Buffer.byteLength(value) > 512
  )
    throw Error(`DeepSeek requires bounded ${label}`);
  return value;
}

export function apply(ctx) {
  const config = JSON.parse(readFileSync(process.env.AW_NATIVE_CONFIG, "utf8"));
  for (const key of ["executable", "binding", "ready_path", "ready_token"])
    identity(config[key], key);
  if (
    !Array.isArray(config.hooks) ||
    config.hooks.some(
      (hook) =>
        !["input.submit", "tool.before", "tool.after", "security.violation"].includes(hook.event) ||
        !hook.step ||
        !["block", "report"].includes(hook.on_error) ||
        !Number.isInteger(hook.budget_ms) ||
        hook.budget_ms < 1 ||
        hook.budget_ms > 55000,
    )
  )
    throw Error("Invalid AW DeepSeek routes");
  const before = config.hooks.filter((hook) => hook.event === "tool.before");
  const after = config.hooks.filter((hook) => hook.event === "tool.after");
  const submit = config.hooks.filter((hook) => hook.event === "input.submit");
  const final = config.hooks.filter((hook) => hook.event === "security.violation");
  const finalGate = Symbol.for("aw.deepseek.finalGate");
  if (final.length && globalThis[Symbol.for("aw.deepseek.finalPatched")] !== true)
    throw Error("AW final result gate was not installed before DSH Agent startup");
  if (
    config.replacement_step != null &&
    after.at(-1)?.step !== config.replacement_step
  )
    throw Error("AW replacement must be the final after step");
  const callback = transport(config);
  const active = new Map();
  const completed = new WeakMap();
  const disposers = [];
  const evaluate = async (routes, payload, signal) => {
    for (const hook of routes) {
      try {
        const directive = await callback.invoke(hook, payload, signal);
        if (directive.block || directive.aw_replacement)
          return directive;
      } catch (error) {
        process.stderr.write(
          `AW ${hook.event}/${hook.step}: ${error.message}; on_error=${hook.on_error}\n`,
        );
        if (hook.event !== "tool.after" && hook.on_error === "block")
          return {
            block: true,
            reason:
              "AW hook failed; configured failure action blocked the event",
          };
      }
    }
    return {};
  };
  if (submit.length)
    disposers.push(
      ctx.on(
        "agent/pre-step",
        async ({ agent, messages, turn, signal }, next) => {
          for (const message of messages) {
            if (
              !Array.isArray(message.content) ||
              message.content.some(
                (part) => part?.type !== "text" || typeof part.text !== "string",
              )
            ) {
              process.stderr.write("AW input.submit: unsupported content\n");
              return { kind: "reject" };
            }
            const directive = await evaluate(
              submit,
              {
                event: "input.submit",
                session_id: identity(agent.id, "session id"),
                aw_turn_id: identity(
                  `${turn}:${identity(message.id, "message id")}`,
                  "turn id",
                ),
                input_text: message.content.map((part) => part.text).join("\n"),
              },
              signal,
            );
            if (directive.block) {
              process.stderr.write(
                `AW input.submit: ${directive.reason ?? "Blocked by AW policy"}\n`,
              );
              return { kind: "reject" };
            }
          }
          return next();
        },
        { prepend: true },
      ),
    );
  disposers.push(
    ctx.on(
      "tools/pre-execute",
      async (exec, next) => {
        if (active.size >= 512 || active.has(exec.token))
          throw Error("AW execution capacity exceeded");
        const record = {
          payload: {
            invocation_id: randomUUID(),
            session_id: identity(exec.agent?.id, "session id"),
            nested: exec.parent !== undefined,
            tool_call: {
              id: identity(exec.callId, "call id"),
              name: identity(exec.name, "tool name"),
              input: exec.arguments,
            },
          },
          denied: "AW preparation did not complete",
        };
        active.set(exec.token, record);
        const directive = await evaluate(
          before,
          { ...record.payload, event: "tool.before" },
          exec.signal,
        );
        record.denied = directive.block
          ? (directive.reason ?? "Blocked by AW policy")
          : undefined;
        if (record.denied !== undefined)
          return { kind: "deny", reason: record.denied };
        return await next();
      },
      { prepend: true },
    ),
  );
  // Guards are synchronous and monotonic. They consume the async decision and
  // also deny an outer plugin's allow that bypassed AW's pre-execute listener.
  disposers.push(
    ctx.tools.guard(
      (exec) =>
        active.get(exec.token)?.denied ??
        (active.has(exec.token) ? undefined : "AW preparation was bypassed"),
    ),
  );
  disposers.push(
    ctx.on(
      "tools/post-execute",
      async (exec, result, next) => {
        const decision = await next();
        const record = active.get(exec.token);
        if (!record) return decision;
        // A value replacement requires native schema/render processing before its
        // text is known. Never compress that unmaterialized value or nested PTC data.
        if (Object.hasOwn(decision, "value")) return decision;
        const effective =
          decision.kind === "block"
            ? { content: decision.feedback, isError: true }
            : {
                ...result,
                ...(decision.content === undefined
                  ? {}
                  : { content: decision.content }),
              };
        const directive = await evaluate(
          after,
          { ...record.payload, event: "tool.after", tool_response: effective },
          exec.signal,
        );
        if (directive.aw_replacement && decision.kind === "accept")
          return { ...decision, content: directive.aw_replacement.content };
        return decision;
      },
      { prepend: true },
    ),
  );
  // shortcut: observers see raw results, upgrade when DSH has an awaited final Hook.
  disposers.push(ctx.on("tools/result", (exec, result) => {
    const record = active.get(exec.token);
    active.delete(exec.token);
    if (final.length && record) completed.set(result, record);
  }));
  if (final.length) {
    globalThis[finalGate] = async (exec, result) => {
      const record = completed.get(result);
      completed.delete(result);
      const denied = (reason) => ({
        isError: true,
        content: [{ type: "text", text: "AW withheld the final tool result" }],
        error: { message: reason },
      });
      if (!record) return denied("AW final result identity was not verified");
      try {
        const directive = await evaluate(final, {
          ...record.payload,
          event: "security.violation",
          tool_response: result,
        }, exec.signal);
        return directive.block ? denied("AW final result security check blocked the result") : result;
      } catch (error) {
        process.stderr.write(`AW final result gate: ${error.message}\n`);
        return denied("AW final result security check failed");
      }
    };
  }
  ctx.effect(() => () => {
    for (const dispose of disposers.reverse()) dispose();
    active.clear();
    if (final.length) globalThis[finalGate] = async () => {
      throw Error("AW final result gate was disposed");
    };
    callback.dispose();
  });
  const temporary = `${config.ready_path}.${process.pid}.tmp`;
  try {
    writeFileSync(
      temporary,
      JSON.stringify({
        version: 1,
        adapter: "deepseek-harness",
        pid: process.pid,
        token: config.ready_token,
        hooks: config.hooks.length,
      }),
      { mode: 0o600, flag: "wx" },
    );
    renameSync(temporary, config.ready_path);
  } finally {
    try {
      unlinkSync(temporary);
    } catch (error) {
      if (error.code !== "ENOENT") throw error;
    }
  }
  // The Agent loop waits for this service before accepting tool work.
  ctx.provide("awDeepseek", {});
}
