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
      "tool.after": ["observe", "replace_result"],
    },
  };
}

const boundaries = {
  "session.start": ["SessionStart"],
  "input.submit": ["UserPromptSubmit"],
  "tool.before": ["PreToolUse"],
  "tool.after": ["PostToolUse", "PostToolUseFailure"],
};
const allEvents = Object.keys(boundaries);

function validRoutes(routes, events, skill) {
  if (!Array.isArray(routes?.hooks) || !routes.events) return false;
  for (const [name, route] of Object.entries(routes.events))
    if (
      !events.includes(name) ||
      (name !== "tool.after" && route?.replacement_step != null)
    )
      return false;
  for (const hook of routes.hooks) {
    const canBlock = hook?.event === "tool.before" || hook?.event === "input.submit";
    if (
      !hook ||
      !events.includes(hook.event) ||
      (hook.skill_check && !skill) ||
      hook.replace_input ||
      (hook.on_error !== "report" && !(canBlock && hook.on_error === "block")) ||
      (hook.replace_result !== undefined &&
        typeof hook.replace_result !== "boolean") ||
      (hook.replace_result && hook.event !== "tool.after")
    )
      return false;
  }
  const post = routes.hooks.filter((hook) => hook.event === "tool.after");
  const event = routes.events["tool.after"];
  if (!event) return post.length === 0;
  const steps = new Set(post.map((hook) => hook.step));
  if (
    !event.steps ||
    !post.length ||
    post.length !== steps.size ||
    Object.keys(event.steps).length !== steps.size ||
    post.some(
      (hook) =>
        typeof hook.step !== "string" ||
        !Object.hasOwn(event.steps, hook.step) ||
        event.steps[hook.step] !== hook.on_error,
    )
  )
    return false;
  const replacements = post.filter((hook) => hook.replace_result);
  return event.replacement_step == null
    ? replacements.length === 0
    : replacements.length === 1 &&
        replacements[0] === post.at(-1) &&
        replacements[0].step === event.replacement_step;
}

/** Confirm that public callbacks can install the negotiated routes. */
export function qoderInstallRoutes(routes, inputEnabled, sessionStartEnabled = false) {
  return validRoutes(
    routes,
    Object.keys(qoderCapabilities(inputEnabled, sessionStartEnabled).events),
    false,
  );
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
  const installsPost = task.routes.hooks.some(
    (hook) => hook.event === "tool.after",
  );
  if (!task.prepare && !validRoutes(task.routes, allEvents, skill))
    throw new Error("invalid_qoder_sdk_routes");
  for (const [event, names] of Object.entries(boundaries)) {
    const subscribed = task.routes.hooks.filter((hook) => hook.event === event);
    if (!subscribed.length) continue;
    const callback = (name) => async (input, id, context = {}) => {
      const signal = context.signal;
      signal?.throwIfAborted();
      if (event === "session.start" && input?.source === "compact") return {};
      let payload,
        started = false,
        routes = [];
      try {
        await task.prepare?.(signal);
        if (task.fallback) return {};
        const snapshot = task.routes;
        if (
          !validRoutes(snapshot, allEvents, skill) ||
          (!installsPost && snapshot.events["tool.after"]?.replacement_step != null)
        )
          throw new Error("invalid_qoder_sdk_routes");
        routes = snapshot.hooks.filter((hook) => hook.event === event);
        payload = structuredClone(input);
        if (payload.hook_event_name !== name)
          throw new Error("native_hook_event_mismatch");
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
          if (result.hookSpecificOutput) {
            if (
              event !== "tool.after" ||
              !result.hookSpecificOutput.updatedToolOutput
            )
              throw new Error("unsupported_qoder_sdk_directive");
            response = result;
          }
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
        // Request a native stop and surface the error without claiming result withholding.
        // Task availability has already handled transport loss.
        const interrupted = controller.query?.interrupt();
        interrupted?.catch(() => {});
        return { continue: false, stopReason: "AW result check failed" };
      }
    };
    for (const name of names)
      (hooks[name] ??= []).push({
        matcher: "*",
        hooks: [callback(name)],
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
