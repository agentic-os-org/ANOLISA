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

export function sourceCapabilities(events = DEFAULT_SOURCE_EVENTS) {
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
  return {
    adapter: "sdk",
    version: SOURCE_PROTOCOL,
    entrypoint: "application",
    events: Object.fromEntries(
      selected.map((event) => [event, [...SUPPORTED_EVENTS[event]]]),
    ),
  };
}

export function confirmSourceRoutes(capabilities, routes) {
  const events = capabilities?.events;
  const hooks = routes?.hooks;
  if (!events || typeof events !== "object" || !Array.isArray(hooks))
    return false;
  return hooks.every(
    (hook) =>
      hook &&
      typeof hook === "object" &&
      Object.hasOwn(events, hook.event) &&
      !hook.replace_input &&
      !hook.replace_result &&
      !hook.skill_check,
  );
}
