/** Compose AW with Pi's installed callbacks, preserving native permission and result semantics. */
import { run } from "./transport.mjs";
import { modelProducer } from "./producer.mjs";
import { randomUUID } from "node:crypto";
import { isDeepStrictEqual } from "node:util";
import { readFileSync, writeFileSync, renameSync, unlinkSync } from "node:fs";

export function configuration() {
  const config = JSON.parse(readFileSync(process.env.AW_NATIVE_CONFIG, "utf8"));
  for (const key of ["executable", "binding", "ready_path", "ready_token"]) {
    if (typeof config[key] !== "string" || !config[key])
      throw new Error(`Missing AW ${key}`);
  }
  if (!Array.isArray(config.hooks))
    throw new Error("AW hooks must be an array");
  for (const hook of config.hooks) {
    if (
      !["tool.before", "tool.after"].includes(hook.event) ||
      typeof hook.step !== "string" ||
      !hook.step ||
      !["report", "block"].includes(hook.on_error) ||
      !Number.isInteger(hook.budget_ms) ||
      hook.budget_ms < 1 ||
      hook.budget_ms > 55000
    ) {
      throw new Error("Invalid AW Pi hook route");
    }
  }
  const after = config.hooks.filter((hook) => hook.event === "tool.after");
  if (
    config.replacement_step != null &&
    (typeof config.replacement_step !== "string" ||
      !config.replacement_step ||
      after.at(-1)?.step !== config.replacement_step)
  )
    throw new Error(
      "Pi replacement step must be the last configured after step",
    );
  return config;
}

function identifier(value, label) {
  if (
    typeof value !== "string" ||
    !value.trim() ||
    Buffer.byteLength(value) > 512
  ) {
    throw new Error(`Pi requires nonempty ${label} up to 512 bytes`);
  }
  return value;
}

async function invoke(config, hook, payload, signal) {
  const input = Buffer.from(JSON.stringify(payload));
  if (input.length > 1024 * 1024) throw new Error("native_input_limit");
  if (signal?.aborted) throw new Error("callback_aborted");
  const response = config.scope
    ? await config.scope.invoke(hook, payload, signal)
    : await run(config.executable, ["hook", "--adapter", "pi", "--binding", config.binding,
        "--event", hook.event, "--step", hook.step, "--on-error", hook.on_error],
        input, hook.budget_ms + 3000, signal);
  const { code, stdout } = response;
  if (code !== 0) throw new Error(`callback_exit_${code}`);
  const directive = stdout.length
    ? JSON.parse(stdout.toString("utf8"))
    : {};
  if (
    !directive ||
    typeof directive !== "object" ||
    Array.isArray(directive) ||
    Object.keys(directive).some(
      (key) => !["block", "reason", "aw_replacement"].includes(key),
    ) ||
    (directive.block !== undefined &&
      typeof directive.block !== "boolean") ||
    (directive.reason !== undefined &&
      typeof directive.reason !== "string") ||
    (hook.event === "tool.after" && directive.block === true)
  ) {
    throw new Error("unsupported_pi_directive");
  }
  if (Object.hasOwn(directive, "aw_replacement")) {
    if (
      hook.event !== "tool.after" ||
      hook.step !== config.replacement_step
    ) {
      throw new Error("unauthorized_pi_replacement");
    }
    validateReplacement(payload.tool_response, directive.aw_replacement);
  }
  return directive;
}

// A projection may change one model-visible text slot, never the native result envelope.
function validateReplacement(result, replacement) {
  if (
    !result ||
    !replacement ||
    typeof replacement !== "object" ||
    Array.isArray(replacement) ||
    !Array.isArray(result.content) ||
    result.content.length !== 1 ||
    result.content[0]?.type !== "text" ||
    !Array.isArray(replacement.content) ||
    replacement.content.length !== 1 ||
    replacement.content[0]?.type !== "text"
  ) {
    throw new Error("invalid_pi_replacement");
  }
  const text = replacement.content[0].text;
  if (
    typeof text !== "string" ||
    !text ||
    Buffer.byteLength(text) > 1024 * 1024
  ) {
    throw new Error("invalid_pi_replacement_text");
  }
  const expected = JSON.parse(JSON.stringify(result));
  expected.content[0].text = text;
  if (!isDeepStrictEqual(expected, replacement))
    throw new Error("pi_replacement_changed_native_metadata");
}

/** Install after createAgentSession returns; resolve durable session identity at each preparation. */
export function installSessionBridge(session, scope = null) {
  const config = { ...configuration(), scope };
  const agent = session.agent;
  if (!agent || typeof session.sessionManager?.getSessionId !== "function") {
    throw new Error("AW requires a Pi Coding AgentSession");
  }
  if (agent[installed])
    throw new Error("AW Pi bridge already installed on this Agent");
  const previousBefore = agent.beforeToolCall;
  const previousAfter = agent.afterToolCall;
  const active = new Map();
  const producer = scope ? modelProducer(scope) : null;
  const beforeRoutes = config.hooks.filter(
    (hook) => hook.event === "tool.before",
  );
  const afterRoutes = config.hooks.filter(
    (hook) => hook.event === "tool.after",
  );
  const evaluate = async (routes, payload, signal) => {
    for (const hook of routes) {
      try {
        const directive = await invoke(config, hook, payload, signal);
        if (Object.hasOwn(directive, "aw_replacement"))
          return { replacement: directive.aw_replacement };
        if (directive.block)
          return {
            block: true,
            reason: directive.reason ?? "Blocked by AW policy",
          };
      } catch (error) {
        process.stderr.write(
          `AW ${hook.event} step ${hook.step} failed: ${error.message}; on_error=${hook.on_error}\n`,
        );
        if (config.scope) {
          config.scope.failed = true;
          agent.abort();
          throw new Error("AW managed task stopped after a callback failure");
        }
        if (hook.event === "tool.before" && hook.on_error === "block") {
          return {
            block: true,
            reason:
              "AW hook failed; configured failure action blocked the tool",
          };
        }
      }
    }
    return undefined;
  };
  let before = async function (context, signal) {
    config.scope?.checkExecution();
    producer?.claim(context);
    const original = await previousBefore?.call(agent, context, signal);
    if (original?.block) return original;
    const call = identifier(context.toolCall.id, "tool call id");
    const identity = {
      invocation_id: scope ? call : randomUUID(),
      session_id: identifier(
        session.sessionManager.getSessionId(),
        "session id",
      ),
      tool_call: {
        id: call,
        name: identifier(context.toolCall.name, "tool name"),
        input: JSON.parse(JSON.stringify(context.args)),
      },
    };
    if (active.has(call))
      throw new Error("Duplicate in-flight Pi tool call id");
    if (active.size >= (config.scope?.invocationLimit ?? 512))
      throw new Error("AW Pi in-flight tool capacity exceeded");
    if (config.scope)
      await config.scope.register(identity.invocation_id, identity.tool_call.name, identity.tool_call.input);
    const denied = await evaluate(
      beforeRoutes,
      { ...identity, event: "tool.before" },
      signal,
    );
    if (denied) {
      if (config.scope) await config.scope.finish(identity.invocation_id);
      return denied;
    }
    active.set(call, { identity, object: context.toolCall });
    return original;
  };
  let after = async function (context, signal) {
    config.scope?.checkExecution();
    const entry = active.get(context.toolCall.id);
    const identity = entry?.identity;
    if (config.scope && entry?.object !== context.toolCall)
      throw new Error("AW rejected a stale Pi completion object");
    if (!identity && config.scope) {
      config.scope.failed = true;
      agent.abort();
      throw new Error("AW managed completion lacks preparation identity");
    }
    if (!identity) {
      process.stderr.write(
        "AW tool.after skipped: completed call lacks preparation identity\n",
      );
      return await previousAfter?.call(agent, context, signal);
    }
    const original = await previousAfter?.call(agent, context, signal);
    const result = { ...context.result };
    for (const key of ["content", "details", "usage", "terminate"]) {
      result[key] = original?.[key] ?? result[key];
    }
    const isError = original?.isError ?? context.isError;
    try {
      const directive = await evaluate(
        afterRoutes,
        {
          ...identity,
          event: "tool.after",
          tool_response: { ...result, isError },
        },
        signal,
      );
      if (directive?.replacement) {
        return {
          ...(original ?? {}),
          content: [
            {
              ...result.content[0],
              text: directive.replacement.content[0].text,
            },
          ],
        };
      }
    } finally {
      if (config.scope) await config.scope.finish(identity.invocation_id);
      active.delete(context.toolCall.id);
    }
    return original;
  };
  if (config.scope) {
    const guarded = (callback) => async (context, signal) => {
      try { return await callback(context, signal); }
      catch (error) { config.scope.failed = true; agent.abort(); throw error; }
    };
    before = guarded(before);
    after = guarded(after);
  }
  let managed = true;
  // ponytail: descriptors last for this Agent; rebind with a fresh factory result.
  for (const [key, callback] of [
    ["beforeToolCall", before],
    ["afterToolCall", after],
  ]) {
    let current = callback;
    Object.defineProperty(agent, key, {
      configurable: false,
      enumerable: true,
      get: () => current,
      set(value) {
        if (managed)
          throw new Error(
            `AW owns ${key}; configure callbacks in createSession`,
          );
        current = value;
      },
    });
  }
  agent[installed] = true;
  // Immediate abort and native errors may skip afterToolCall; the completion event still closes state.
  const unsubscribe = agent.subscribe((event) => {
    if (event.type === "tool_execution_end") {
      active.delete(event.toolCallId);
      producer?.retire(event.toolCallId);
    }
  });
  return {
    model: (stream) => producer.wrap(stream),
    ready() {
      if (agent.beforeToolCall !== before || agent.afterToolCall !== after)
        throw new Error("Pi callbacks replaced before readiness");
      publishReady(config);
    },
    async drain() {
      try { if (config.scope) await config.scope.drain(); }
      finally { active.clear(); producer?.clear(); }
    },
    dispose() {
      managed = false;
      if (agent.beforeToolCall === before)
        agent.beforeToolCall = previousBefore;
      if (agent.afterToolCall === after) agent.afterToolCall = previousAfter;
      delete agent[installed];
      active.clear();
      unsubscribe();
    },
  };
}
const installed = Symbol("aw.pi.installed");


export function publishReady(config = configuration()) {
      const temporary = `${config.ready_path}.${process.pid}.tmp`;
      try {
        writeFileSync(
          temporary,
          JSON.stringify({
            version: 1,
            adapter: "pi",
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
}
