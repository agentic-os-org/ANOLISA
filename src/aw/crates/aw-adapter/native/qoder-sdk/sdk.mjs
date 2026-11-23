/** Public Qoder Hooks use the canonical payload and effect adapters. */
import { randomUUID } from "node:crypto";
import { invoke, callbackTimeoutMs } from "./callback.mjs";
import { bindSkillQuery, noteSkillRead, resolveSkill } from "./skills.mjs";

export const qoderProfile = Object.freeze({
  adapter: "qoder-sdk",
  version: "public-hooks/v1",
  entrypoint: "sdk-worker",
});

export function qoderCapabilities(inputEnabled, sessionStartEnabled = false) {
  return {
    ...qoderProfile,
    events: {
      ...(sessionStartEnabled ? { "session.start": ["observe"] } : {}),
      ...(inputEnabled ? { "input.submit": ["observe", "block"] } : {}),
      "tool.before": ["observe", "block"],
      "tool.after": ["observe"],
    },
  };
}

const deny = (reason) => ({
  hookSpecificOutput: {
    hookEventName: "PreToolUse",
    permissionDecision: "deny",
    permissionDecisionReason: reason,
  },
});

/** Prepare options before native Query creation; bind the returned Query immediately. */
export function qoderOptions(task, options = {}) {
  if (options[Symbol.for("aw.sdk.hooks")])
    throw new Error("AW Hooks already installed");
  const hooks = Object.fromEntries(
    Object.entries(options.hooks ?? {}).map(([event, groups]) => [
      event,
      groups.map((group) => ({ ...group, hooks: [...group.hooks] })),
    ]),
  );
  const controller = { query: undefined, failure: undefined };
  const skill = task.routes.hooks.some((hook) => hook.skill_check);
  for (const [event, names] of [
    ["session.start", ["SessionStart"]],
    ["input.submit", ["UserPromptSubmit"]],
    ["tool.before", ["PreToolUse"]],
    ["tool.after", ["PostToolUse", "PostToolUseFailure"]],
  ]) {
    const subscribed = task.routes.hooks.filter((hook) => hook.event === event);
    if (!subscribed.length) continue;
    const callback = async (input, id, context = {}) => {
      const signal = context.signal;
      signal?.throwIfAborted();
      if (event === "session.start" && input?.source === "compact") return {};
      let payload,
        started = false,
        routes = [];
      try {
        await task.prepare?.(signal);
        if (task.fallback) return {};
        routes = task.routes.hooks.filter((hook) => hook.event === event);
        payload = structuredClone(input);
        if (
          id !== undefined &&
          (event === "tool.before" || event === "tool.after") &&
          id !== payload.tool_use_id
        )
          throw new Error("native_tool_identity_mismatch");
        if (event === "input.submit") payload.aw_turn_id = randomUUID();
        if (controller.query) noteSkillRead(payload, controller.query);
        let response = {};
        const verify = skill
          ? await resolveSkill(payload, options, controller.query, signal)
          : undefined;
        for (const hook of routes) {
          started = true;
          const result = await invoke({ scope: task }, hook, payload, signal);
          if (
            result.decision === "block" ||
            result.hookSpecificOutput?.permissionDecision === "deny"
          )
            return result;
          if (result.hookSpecificOutput) response = result;
        }
        if (!task.fallback) await verify?.();
        return task.fallback ? {} : response;
      } catch (error) {
        if (started) await task.abortEvent(routes[0], payload);
        signal?.throwIfAborted();
        if (event === "session.start") {
          process.stderr.write(`AW session.start failed: ${error.message}\n`);
          return {};
        }
        controller.failure ??= error;
        if (event === "input.submit")
          return { decision: "block", reason: "AW input check failed" };
        if (event === "tool.before") return deny("AW tool check failed");
        // A policy/Provider failure remains a native stop. Transport loss was
        // already converted to a neutral reply by the task availability contract.
        const interrupted = controller.query?.interrupt();
        interrupted?.catch(() => {});
        return { continue: false, stopReason: "AW result check failed" };
      }
    };
    for (const name of names)
      (hooks[name] ??= []).push({
        matcher: "*",
        hooks: [callback],
        timeout:
          subscribed.reduce((ms, hook) => ms + callbackTimeoutMs(hook), 1000) /
          1000,
      });
  }
  const result = { ...options, hooks };
  Object.defineProperty(result, Symbol.for("aw.sdk.hooks"), { value: true });
  return {
    options: result,
    bind(query) {
      controller.query = bindSkillQuery(query, skill);
      return query;
    },
    get failure() {
      return controller.failure;
    },
  };
}
