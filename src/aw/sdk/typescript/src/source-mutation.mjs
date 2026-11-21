/** Capabilities, errors, input/result rewrites and strict stream helpers for sources.mjs. */
import { types } from "node:util";
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
export const DEFAULT_MAX_STREAM_BYTES = 1024 * 1024;
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
  if (typeof value !== "object" || types.isProxy(value)) return false;
  if (Array.isArray(value)) {
    if (Object.getPrototypeOf(value) !== Array.prototype) return false;
    return value.every((item, i) => {
      const d = Object.getOwnPropertyDescriptor(value, i);
      return Boolean(d?.enumerable && Object.hasOwn(d, "value")) && isPlainJsonValue(item, depth + 1);
    });
  }
  if (isPlainObject(value)) {
    return Reflect.ownKeys(value).every((key) => {
      if (typeof key !== "string") return false;
      const d = Object.getOwnPropertyDescriptor(value, key);
      return Boolean(d?.enumerable && Object.hasOwn(d, "value")) && isPlainJsonValue(d.value, depth + 1);
    });
  }
  return false;
}

export function isSyncGeneratorFunction(fn) {
  return fn?.constructor?.name === "GeneratorFunction";
}

export function isLazyResult(value) {
  if (!value || typeof value !== "object") return false;
  if (typeof value[Symbol.asyncIterator] === "function") return true;
  if (typeof value.next === "function") return true;
  if (
    !Array.isArray(value) &&
    !(value instanceof Uint8Array) &&
    typeof value[Symbol.iterator] === "function"
  )
    return true;
  return false;
}

function closeLazyResult(value) {
  try {
    if (typeof value?.return === "function") value.return();
  } catch {}
}

function jsonEncodedBytes(value) {
  return Buffer.byteLength(JSON.stringify(value), "utf8");
}

export function isolateStrictValue(
  value,
  { snapshotResult, customExtract = false } = {},
) {
  if (isLazyResult(value)) {
    closeLazyResult(value);
    throw new AwError("source_stream_withhold_requires_buffering");
  }
  if (value === undefined) return undefined;
  if (isPlainJsonValue(value)) return structuredClone(value);
  if (snapshotResult) {
    const copied = snapshotResult(value);
    if (copied === value) throw new AwError("source_result_isolation_required");
    return copied;
  }
  if (customExtract) {
    try {
      const copied = structuredClone(value);
      if (typeof value === "object" && value !== null && copied === value)
        throw new AwError("source_result_isolation_required");
      return copied;
    } catch (error) {
      if (error instanceof AwError) throw error;
      throw new AwError("source_result_isolation_required", { cause: error });
    }
  }
  throw new AwError("source_result_isolation_required");
}

function bufferSyncIterable(
  iterable,
  { maxStreamItems, maxStreamBytes, snapshotResult, customExtract },
) {
  const checked = [];
  let bufferedBytes = 0;
  const iterator =
    typeof iterable.next === "function"
      ? iterable
      : iterable[Symbol.iterator]();
  try {
    while (true) {
      const step = iterator.next();
      if (step.done) break;
      if (checked.length >= maxStreamItems)
        throw new AwError("source_stream_result_too_large");
      const isolated = isolateStrictValue(step.value, {
        snapshotResult,
        customExtract,
      });
      const encoded = customExtract ? normalizeJson(isolated) : isolated;
      bufferedBytes += jsonEncodedBytes(encoded);
      if (bufferedBytes > maxStreamBytes)
        throw new AwError("source_stream_result_too_large");
      checked.push(isolated);
    }
  } finally {
    closeLazyResult(iterator);
  }
  return checked;
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

export function validateStreamLimits(maxStreamItems, maxStreamBytes) {
  if (
    !Number.isInteger(maxStreamItems) ||
    maxStreamItems < 1 ||
    !Number.isInteger(maxStreamBytes) ||
    maxStreamBytes < 1
  )
    throw new AwError("source_invalid_stream_limit");
}

export async function deliverToolResult(session, result, opts) {
  if (session._strictAfter()) return deliverStrictToolResult(result, opts);
  if (!session._observeAfter()) return result;
  const decision = await opts.observeResult(
    result,
    opts.callId,
    opts.snapshot,
    opts.signal,
  );
  if (!Object.hasOwn(decision, "replaced_result")) return result;
  try {
    return applyToolResult(result, decision.replaced_result, {
      applyResult: opts.applyResult,
      customExtract: Boolean(opts.extractResult),
    });
  } catch (error) {
    throw toolExecuted(error);
  }
}

export async function deliverStrictToolResult(
  result,
  {
    observeResult,
    callId,
    snapshot,
    signal,
    extractResult,
    applyResult,
    snapshotResult,
    bufferStream,
    maxStreamItems,
    maxStreamBytes = DEFAULT_MAX_STREAM_BYTES,
  },
) {
  const customExtract = Boolean(extractResult);
  try {
    if (isLazyResult(result)) {
      if (
        typeof result[Symbol.asyncIterator] === "function" ||
        !bufferStream
      ) {
        closeLazyResult(result);
        throw new AwError("source_stream_withhold_requires_buffering");
      }
      const checkedItems = bufferSyncIterable(result, {
        maxStreamItems,
        maxStreamBytes,
        snapshotResult,
        customExtract,
      });
      const decision = await observeResult(
        checkedItems,
        callId,
        snapshot,
        signal,
      );
      const deliver = Object.hasOwn(decision, "replaced_result")
        ? applyStreamResult(checkedItems, decision.replaced_result, {
            applyResult,
            customExtract,
          })
        : checkedItems.map((item) =>
            isPlainJsonValue(item) ? structuredClone(item) : item,
          );
      return deliver[Symbol.iterator]();
    }
    const isolated = isolateStrictValue(result, {
      snapshotResult,
      customExtract,
    });
    const decision = await observeResult(isolated, callId, snapshot, signal);
    if (Object.hasOwn(decision, "replaced_result")) {
      return applyToolResult(isolated, decision.replaced_result, {
        applyResult,
        customExtract,
      });
    }
    return isPlainJsonValue(isolated) ? structuredClone(isolated) : isolated;
  } catch (error) {
    throw toolExecuted(error);
  }
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
  snapshotResult,
  maxStreamItems,
  maxStreamBytes = DEFAULT_MAX_STREAM_BYTES,
  bufferStream,
}) {
  const strictAfter = session._strictAfter();
  if (strictAfter && !bufferStream)
    throw new AwError("source_stream_withhold_requires_buffering");
  const customExtract = Boolean(extractResult);
  const items = session._observeAfter() ? [] : null;
  let bufferedBytes = 0;
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
      if (strictAfter) {
        if (items.length >= maxStreamItems)
          throw new AwError("source_stream_result_too_large");
        const isolated = isolateStrictValue(item, {
          snapshotResult,
          customExtract,
        });
        const encoded = customExtract ? normalizeJson(isolated) : isolated;
        bufferedBytes += jsonEncodedBytes(encoded);
        if (bufferedBytes > maxStreamBytes)
          throw new AwError("source_stream_result_too_large");
        items.push(isolated);
      } else {
        if (items && session._observeAfter()) {
          if (items.length >= maxStreamItems)
            throw new AwError("source_stream_result_too_large");
          items.push(
            extractResult ? snapshotStreamChunk(item) : normalizeJson(item),
          );
        }
        yield item;
      }
    }
    if (strictAfter) {
      const deliver =
        afterDecision && Object.hasOwn(afterDecision, "replaced_result")
          ? applyStreamResult(items, afterDecision.replaced_result, {
              applyResult,
              customExtract,
            })
          : items.map((out) =>
              isPlainJsonValue(out) ? structuredClone(out) : out,
            );
      for (const out of deliver) {
        session._requireOpen();
        yield out;
      }
    }
  } catch (error) {
    throw dispatched ? toolExecuted(error) : error;
  }
}
