/** Capability declarations for application-owned semantic Hooks. */
import { AwError } from "./transport.mjs";

export const SOURCE_PROTOCOL = "aw-hook-source/v1";
export const DEFAULT_SOURCE_EVENTS = Object.freeze([
  "input.submit",
  "tool.before",
  "tool.after",
]);
const SUPPORTED_EVENTS = Object.freeze({
  "session.start": ["observe"],
  "turn.stop": ["observe", "block"],
  "session.end": ["observe"],
  "input.submit": ["observe", "block"],
  "tool.before": ["observe", "block"],
  "tool.after": ["observe"],
});

export function sourceCapabilities(
  events = DEFAULT_SOURCE_EVENTS,
  { replaceInput = false, replaceResult = false, withholdResult = false } = {},
) {
  if (!Array.isArray(events)) throw new AwError("semantic_event_unavailable");
  const selected = [...new Set(events)];
  if (
    !selected.length ||
    selected.some(
      (event) =>
        typeof event !== "string" || !Object.hasOwn(SUPPORTED_EVENTS, event),
    )
  )
    throw new AwError("semantic_event_unavailable");
  if (replaceInput && !selected.includes("tool.before"))
    throw new AwError("semantic_event_unavailable");
  if ((replaceResult || withholdResult) && !selected.includes("tool.after"))
    throw new AwError("semantic_event_unavailable");
  const declared = Object.fromEntries(
    selected.map((event) => [event, [...SUPPORTED_EVENTS[event]]]),
  );
  if (replaceInput) declared["tool.before"].push("replace_input");
  if (replaceResult) declared["tool.after"].push("replace_result");
  const caps = {
    adapter: "sdk",
    version: SOURCE_PROTOCOL,
    entrypoint: "application",
    events: declared,
  };
  if (withholdResult)
    caps.failure_actions = { "tool.after": ["withhold_result"] };
  return caps;
}

export function confirmSourceRoutes(capabilities, routes) {
  const events = capabilities?.events;
  const failureActions = capabilities?.failure_actions ?? {};
  const hooks = routes?.hooks;
  if (
    !events ||
    typeof events !== "object" ||
    !failureActions ||
    typeof failureActions !== "object" ||
    !Array.isArray(hooks)
  )
    return false;
  return hooks.every((hook) => {
    if (!hook || typeof hook !== "object" || hook.skill_check) return false;
    if (!Object.hasOwn(events, hook.event)) return false;
    const allowedEffects = Array.isArray(events[hook.event])
      ? events[hook.event]
      : [];
    if (hook.replace_input && !allowedEffects.includes("replace_input"))
      return false;
    if (hook.replace_result && !allowedEffects.includes("replace_result"))
      return false;
    if (hook.on_error === "withhold_result") {
      const allowedActions = Array.isArray(failureActions[hook.event])
        ? failureActions[hook.event]
        : [];
      if (!allowedActions.includes("withhold_result")) return false;
    }
    return true;
  });
}
