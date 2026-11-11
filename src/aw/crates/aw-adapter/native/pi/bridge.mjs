/** Compose AW with Pi's installed callbacks, preserving native permission and result semantics. */
import { run } from "./transport.mjs";
import { modelProducer } from "./producer.mjs";
import { randomUUID } from "node:crypto";
import { isDeepStrictEqual } from "node:util";
import { readFileSync, writeFileSync, renameSync, unlinkSync } from "node:fs";

export function configuration(config = JSON.parse(readFileSync(process.env.AW_NATIVE_CONFIG, "utf8"))) {
  for (const key of ["executable", "binding", "ready_path", "ready_token"]) {
    if (typeof config[key] !== "string" || !config[key])
      throw new Error(`Missing AW ${key}`);
  }
  if (!Array.isArray(config.hooks))
    throw new Error("AW hooks must be an array");
  for (const hook of config.hooks) {
    if (
      !["input.submit", "tool.before", "tool.after"].includes(hook.event) ||
      typeof hook.step !== "string" ||
      !hook.step ||
      !["report", "block", "withhold_result"].includes(hook.on_error) ||
      (hook.on_error === "withhold_result" && hook.event !== "tool.after") ||
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
  let directive;
  try { directive = stdout.length ? JSON.parse(stdout.toString("utf8")) : {}; }
  catch { throw new Error("invalid_pi_directive_json"); }
  if (
    !directive ||
    typeof directive !== "object" ||
    Array.isArray(directive) ||
    Object.keys(directive).some(
      (key) => !["block", "reason", "aw_replacement", "aw_withheld", "aw_updated_input"].includes(key),
    ) ||
    (directive.block !== undefined &&
      typeof directive.block !== "boolean") ||
    (directive.reason !== undefined &&
      typeof directive.reason !== "string") ||
    (hook.event === "tool.after" && directive.block === true)
  ) {
    throw new Error("unsupported_pi_directive");
  }
  if (Object.hasOwn(directive, "aw_updated_input") &&
      (hook.event !== "tool.before" || Object.keys(directive).length !== 1 ||
       !directive.aw_updated_input || typeof directive.aw_updated_input !== "object" ||
       Array.isArray(directive.aw_updated_input))) throw new Error("invalid_pi_input_replacement");
  if (Object.hasOwn(directive, "aw_withheld") &&
      (hook.event !== "tool.after" || hook.on_error !== "withhold_result" ||
       directive.aw_withheld !== true || Object.keys(directive).length !== 1))
    throw new Error("unauthorized_pi_withholding");
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
export function installSessionBridge(session, scope = null, policy = configuration()) {
  const config = { ...configuration(policy), scope };
  const agent = session.agent;
  if (!agent || typeof session.sessionManager?.getSessionId !== "function") {
    throw new Error("AW requires a Pi Coding AgentSession");
  }
  if (agent[installed])
    throw new Error("AW Pi bridge already installed on this Agent");
  const previousBefore = agent.beforeToolCall;
  const previousAfter = agent.afterToolCall;
  const active = new Map();
  const checkedResults = new Map();
  const producer = scope ? modelProducer(scope) : null;
  const beforeRoutes = config.hooks.filter(
    (hook) => hook.event === "tool.before",
  );
  const afterRoutes = config.hooks.filter(
    (hook) => hook.event === "tool.after",
  );
  const strict = afterRoutes.some(hook => hook.on_error === "withhold_result");
  const previousEvents = agent.processEvents;
  if (strict && typeof previousEvents !== "function")
    throw new Error("AW strict Pi output requires the native event dispatch seam");
  const evaluate = async (routes, payload, signal) => {
    let withhold = false;
    for (const hook of routes) {
      try {
        const directive = await invoke(config, hook, payload, signal);
        if (directive.aw_updated_input) return { input: directive.aw_updated_input };
        if (directive.aw_withheld) { withhold = true; continue; }
        if (Object.hasOwn(directive, "aw_replacement"))
          return withhold ? { withhold: true } : { replacement: directive.aw_replacement };
        if (directive.block)
          return {
            block: true,
            reason: directive.reason ?? "Blocked by AW policy",
          };
      } catch (error) {
        process.stderr.write(
          `AW ${hook.event} step ${hook.step} failed: ${error.message}; on_error=${hook.on_error}\n`,
        );
        // A report-configured fault keeps the unchecked native result and
        // lets the session continue: identity, cleanup and cancellation
        // failures latch scope.failed on their own before reaching here.
        if (
          config.scope &&
          error.message !== "callback_aborted" &&
          !config.scope.failed &&
          hook.event === "tool.after" &&
          hook.on_error === "report"
        ) {
          continue;
        }
        if (config.scope) {
          config.scope.failed = true;
          agent.abort();
          throw new Error("AW managed task stopped after a callback failure");
        }
        if (hook.event === "tool.after" && hook.on_error === "withhold_result")
          throw new Error("AW post-tool check failed; result withheld by policy");
        if (hook.event === "tool.before" && hook.on_error === "block") {
          return {
            block: true,
            reason:
              "AW hook failed; configured failure action blocked the tool",
          };
        }
      }
    }
    return withhold ? { withhold: true } : undefined;
  };
  let before = async function (context, signal) {
    config.scope?.checkExecution();
    producer?.claim(context);
    const rewrite = beforeRoutes.some(hook => hook.replace_input);
    let original = rewrite ? undefined : await previousBefore?.call(agent, context, signal);
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
    if (denied?.block) {
      if (config.scope) await config.scope.finish(identity.invocation_id);
      return denied;
    }
    if (denied?.input) {
      for (const key of Object.keys(context.args)) delete context.args[key];
      Object.assign(context.args, structuredClone(denied.input));
      identity.tool_call.input = structuredClone(denied.input);
    }
    if (rewrite) {
      const checked = structuredClone(context.args);
      original = await previousBefore?.call(agent, context, signal);
      if (original?.block || !isDeepStrictEqual(context.args, checked)) {
        if (config.scope) await config.scope.finish(identity.invocation_id);
        return original?.block ? original : { block: true, reason: "Native callback changed checked arguments" };
      }
    }
    if (config.scope) await config.scope.validate();
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
      if (strict) throw new WithheldResult();
      return await previousAfter?.call(agent, context, signal);
    }
    let original;
    try { original = await previousAfter?.call(agent, context, signal); }
    catch (error) {
      if (strict) throw new Error("AW native after callback failed; result withheld by policy");
      throw error;
    }
    let result = { ...context.result };
    for (const key of ["content", "details", "usage", "terminate"]) {
      result[key] = original?.[key] ?? result[key];
    }
    const isError = original?.isError ?? context.isError;
    try {
      if (strict) {
        // Tools and native callbacks may retain and mutate their result objects
        // while the Provider or invocation cleanup is awaiting I/O.
        try { result = structuredClone(result); }
        catch { throw new Error("AW could not snapshot tool result; result withheld by policy"); }
      }
      const directive = await evaluate(
        afterRoutes,
        {
          ...identity,
          event: "tool.after",
          tool_response: { ...result, isError },
        },
        signal,
      );
      // Pi's native error path discards the entire unchecked result envelope.
      if (directive?.withhold) throw new WithheldResult();
      if (strict) {
        if (directive?.replacement)
          result.content[0].text = directive.replacement.content[0].text;
      } else if (directive?.replacement) {
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
    if (strict) {
      checkedResults.set(context.toolCall.id, result);
      // A truthy override makes Pi allocate a fresh final result envelope.
      return { ...result, isError };
    }
    return original;
  };
  if (config.scope) {
    const guarded = (callback) => async (context, signal) => {
      try { return await callback(context, signal); }
      catch (error) {
        if (!(error instanceof WithheldResult)) { config.scope.failed = true; agent.abort(); }
        throw error;
      }
    };
    before = guarded(before);
    after = guarded(after);
  }
  let managed = true;
  const events = strict ? async function (event) {
    if (managed && event.type === "tool_execution_update") return;
    if (managed && event.type === "tool_execution_end") {
      const result = checkedResults.get(event.toolCallId);
      checkedResults.delete(event.toolCallId);
      if (result) {
        // Pi merges overrides using ?? and keeps extra fields from execute().
        // Replace those aliases on its fresh envelope before any subscriber or
        // model message can observe it, including null/absent and extra fields.
        for (const key of Reflect.ownKeys(event.result)) delete event.result[key];
        Object.defineProperties(event.result, Object.getOwnPropertyDescriptors(result));
      }
    }
    return await previousEvents.call(this, event);
  } : null;
  // ponytail: descriptors last for this Agent; rebind with a fresh factory result.
  for (const [key, callback] of [
    ["beforeToolCall", before],
    ["afterToolCall", after],
    ...(strict ? [["processEvents", events]] : []),
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
    async submit(text, turn, signal, skill) {
      const hooks = config.hooks.filter(hook => hook.event === "input.submit");
      if (!hooks.length) return;
      const payload = { ...skill, event: "input.submit", text, aw_turn_id: turn,
        session_id: identifier(session.sessionManager.getSessionId(), "session id") };
      // Reject local encoding errors before consuming the service sequence.
      if (Buffer.byteLength(JSON.stringify(payload)) > 1024 * 1024)
        throw new Error("native_input_limit");
      if (config.scope) payload.aw_turn_id = await scope.nextSubmission();
      try {
        for (const hook of hooks) {
          let directive;
          try { directive = await invoke(config, hook, payload, signal); }
          catch (error) {
            scope.checkExecution();
            if (signal.aborted) throw new Error("AW input check cancelled");
            process.stderr.write(`AW input.submit/${hook.step} failed: ${error.message}; on_error=${hook.on_error}\n`);
            if (hook.on_error === "block") throw new Error("AW input check failed; submission blocked");
            continue;
          }
          scope.checkExecution();
          if (signal.aborted) throw new Error("AW input check cancelled");
          if (directive.block) throw new Error(directive.reason ?? "Input blocked by AW policy");
        }
      } finally {
        if (config.scope) {
          // Cleanup retains its own bounded transport after cancellation fences execution.
          try {
            const reply = await scope.command("invoke", {
              scope: scope.handle, native_input: JSON.stringify(payload),
            }, 6000, { ...hooks[0], close_event: true });
            if (reply.code !== 0 || reply.stdout.length)
              throw new Error("AW input event cleanup failed");
          } catch (error) {
            scope.failed = true;
            throw error;
          }
        }
      }
    },
    model: (stream) => producer.wrap(stream),
    ready() {
      if (agent.beforeToolCall !== before || agent.afterToolCall !== after)
        throw new Error("Pi callbacks replaced before readiness");
      publishReady(config);
    },
    async drain() {
      try { if (config.scope) await config.scope.drain(); }
      finally { active.clear(); checkedResults.clear(); producer?.clear(); }
    },
    dispose() {
      managed = false;
      if (agent.beforeToolCall === before)
        agent.beforeToolCall = previousBefore;
      if (agent.afterToolCall === after) agent.afterToolCall = previousAfter;
      if (strict && agent.processEvents === events) agent.processEvents = previousEvents;
      delete agent[installed];
      active.clear();
      checkedResults.clear();
      unsubscribe();
    },
  };
}
class WithheldResult extends Error {
  constructor() { super("AW post-tool check failed; result withheld by policy"); }
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
