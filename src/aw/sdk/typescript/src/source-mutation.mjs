/** Capabilities, errors, input/result rewrites and strict stream helpers for sources.mjs. */
import {
  isPlainObject,
  normalizeJson,
  readStream,
  snapshotInputArg,
  snapshotStreamChunk,
  snapshotToolInput,
  toolExecuted,
} from "./source-helpers.mjs";
import { AwError } from "./transport.mjs";

export const SOURCE_PROTOCOL = "aw-hook-source/v1";
export const DEFAULT_SOURCE_EVENTS = Object.freeze([
  "input.submit",
  "tool.before",
  "tool.after",
]);
const SUPPORTED_EVENTS = Object.freeze({
  "input.submit": ["observe", "block"],
  "tool.before": ["observe", "block"],
  "tool.after": ["observe"],
});

export class SourceDenied extends AwError {
  constructor(event, { sessionId, callId, turnId, toolName } = {}) {
    super("source_denied", { transport: false });
    this.name = "SourceDenied";
    this.event = event;
    this.sessionId = sessionId;
    this.callId = callId;
    this.turnId = turnId;
    this.toolName = toolName;
  }
}

export class SourceResultWithheld extends AwError {
  constructor(event = "tool.after", { sessionId, callId, toolName } = {}) {
    super("source_result_withheld", { transport: false });
    this.name = "SourceResultWithheld";
    this.event = event;
    this.sessionId = sessionId;
    this.callId = callId;
    this.toolName = toolName;
    this.toolExecuted = true;
  }
}

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

export function routeHasStrictAfter(routes) {
  const hooks = routes?.hooks;
  if (!Array.isArray(hooks)) return false;
  return hooks.some(
    (hook) =>
      hook &&
      hook.event === "tool.after" &&
      (Boolean(hook.replace_result) || hook.on_error === "withhold_result"),
  );
}

function isPlainJsonValue(value, depth = 0) {
  if (depth > 32) return false;
  if (value === null || typeof value === "boolean" || typeof value === "string")
    return true;
  if (typeof value === "number") return Number.isFinite(value);
  if (Array.isArray(value))
    return value.every((item) => isPlainJsonValue(item, depth + 1));
  if (isPlainObject(value))
    return Object.values(value).every((item) =>
      isPlainJsonValue(item, depth + 1),
    );
  return false;
}

export function applyToolInput(
  updatedInput,
  callArgs,
  { applyInput, customExtract, snapshotArg },
) {
  const revised = snapshotToolInput(updatedInput);
  if (applyInput) {
    const rebuilt = applyInput(structuredClone(revised), callArgs);
    if (!Array.isArray(rebuilt))
      throw new AwError("source_input_replace_unsupported");
    return rebuilt.map((arg) => snapshotInputArg(arg, snapshotArg));
  }
  if (customExtract) throw new AwError("source_input_replace_unsupported");
  if (callArgs.length === 1 && isPlainObject(callArgs[0]))
    return [structuredClone(revised)];
  const keys = Object.keys(revised);
  if (
    keys.length === 1 &&
    keys[0] === "args" &&
    Array.isArray(revised.args) &&
    callArgs.every((arg) => isPlainJsonValue(arg))
  )
    return structuredClone(revised.args);
  throw new AwError("source_input_replace_unsupported");
}

export function applyToolResult(
  original,
  replaced,
  { applyResult, customExtract },
) {
  if (applyResult) return applyResult(original, structuredClone(replaced));
  if (customExtract) throw new AwError("source_result_replace_unsupported");
  if (typeof original === "string" && typeof replaced === "string")
    return replaced;
  if (
    isPlainObject(original) &&
    isPlainObject(replaced) &&
    isPlainJsonValue(replaced)
  )
    return structuredClone(replaced);
  throw new AwError("source_result_replace_unsupported");
}

export function applyStreamResult(
  buffered,
  replaced,
  { applyResult, customExtract },
) {
  if (applyResult) {
    const rebuilt = applyResult(buffered, structuredClone(replaced));
    if (!Array.isArray(rebuilt))
      throw new AwError("source_result_replace_unsupported");
    return rebuilt;
  }
  if (customExtract) throw new AwError("source_result_replace_unsupported");
  if (Array.isArray(replaced)) return structuredClone(replaced);
  if (typeof replaced === "string") return [replaced];
  throw new AwError("source_result_replace_unsupported");
}

export async function* runToolStream({
  session,
  receiver,
  fn,
  callArgs,
  snapshot,
  callId,
  signal,
  requireCheckedInput,
  observeResult,
  extractResult,
  applyResult,
  maxStreamItems,
  bufferStream,
}) {
  const strictAfter = session._strictAfter();
  if (strictAfter && !bufferStream)
    throw new AwError("source_stream_withhold_requires_buffering");
  const items = session._observeAfter() ? [] : null;
  const rawItems = strictAfter ? [] : null;
  let afterDecision;
  let dispatched = false;
  function finish(close) {
    return session._runAdmittedCall(async () => {
      await close();
      if (session._observeAfter())
        afterDecision = await observeResult(items, callId, snapshot, signal);
    });
  }
  const stream = readStream(
    () => {
      requireCheckedInput(callArgs, snapshot);
      dispatched = true;
      return fn.apply(receiver, callArgs);
    },
    session.signal,
    signal,
    session._activeStreams,
    items ? finish : undefined,
  );
  try {
    for await (const item of stream) {
      if (items && session._observeAfter()) {
        if (items.length >= maxStreamItems)
          throw new AwError("source_stream_result_too_large");
        items.push(
          extractResult ? snapshotStreamChunk(item) : normalizeJson(item),
        );
      }
      if (strictAfter) rawItems.push(item);
      else yield item;
    }
    if (strictAfter) {
      const deliver =
        afterDecision && Object.hasOwn(afterDecision, "replaced_result")
          ? applyStreamResult(rawItems, afterDecision.replaced_result, {
              applyResult,
              customExtract: Boolean(extractResult),
            })
          : rawItems;
      for (const out of deliver) {
        session._requireOpen();
        yield out;
      }
    }
  } catch (error) {
    throw dispatched ? toolExecuted(error) : error;
  }
}
