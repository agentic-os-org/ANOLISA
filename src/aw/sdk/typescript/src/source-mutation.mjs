/** Errors, input/result rewrites and strict stream helpers for sources.mjs. */
import {
  UNCLONEABLE,
  cloneStrictPlainJson,
  defaultExtractInput,
  isAsyncGeneratorFunction,
  isPlainObject,
  jsonEqual,
  normalizeJson,
  readStream,
  snapshotInputArg,
  snapshotStreamChunk,
  snapshotToolInput,
  toolExecuted,
  validateIdentity,
} from "./source-helpers.mjs";
import { AwError } from "./transport.mjs";

export const DEFAULT_MAX_STREAM_BYTES = 1024 * 1024;

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

export function isSyncGeneratorFunction(fn) {
  return fn?.constructor?.name === "GeneratorFunction";
}

export function isLazyResult(value) {
  if (!value || typeof value !== "object") return false;
  if (typeof value[Symbol.asyncIterator] === "function") return true;
  if (typeof value.next === "function") return true;
  if (
    !Array.isArray(value) &&
    !ArrayBuffer.isView(value) &&
    !(value instanceof Map) &&
    !(value instanceof Set) &&
    !(value instanceof String) &&
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

export function jsonEncodedBytes(value) {
  return Buffer.byteLength(JSON.stringify(value), "utf8");
}

function cloneBufferedChunks(chunks) {
  return chunks.map((item) => {
    const copy = cloneStrictPlainJson(item);
    if (copy === UNCLONEABLE)
      throw new AwError("source_result_isolation_required");
    return copy;
  });
}

export function prepareAuditedResult(
  value,
  { extractResult, maxBytes, isolateForExtract = false, isStream = false } = {},
) {
  let raw = value;
  if (extractResult) {
    let source = value;
    if (isolateForExtract) {
      if (isStream) {
        source = cloneBufferedChunks(value);
      } else {
        const detached = cloneStrictPlainJson(value);
        if (detached !== UNCLONEABLE) source = detached;
      }
    }
    raw = extractResult(source);
  }
  const projected = normalizeJson(raw);
  if (maxBytes !== undefined && jsonEncodedBytes(projected) > maxBytes)
    throw new AwError("source_stream_result_too_large");
  return projected;
}

export function isolateStrictValue(
  value,
  { snapshotResult, requirePlainJson = false, customExtract = false } = {},
) {
  if (isLazyResult(value)) {
    closeLazyResult(value);
    throw new AwError("source_stream_withhold_requires_buffering");
  }
  if (value === undefined && !requirePlainJson) return undefined;
  const plainCopy = cloneStrictPlainJson(value);
  if (plainCopy !== UNCLONEABLE) return plainCopy;
  if (snapshotResult) {
    const copied = snapshotResult(value);
    if (isLazyResult(copied)) {
      closeLazyResult(copied);
      throw new AwError("source_stream_withhold_requires_buffering");
    }
    if (typeof value === "object" && value !== null && copied === value)
      throw new AwError("source_result_isolation_required");
    const copiedPlain = cloneStrictPlainJson(copied);
    if (copiedPlain !== UNCLONEABLE) return copiedPlain;
    if (requirePlainJson || !customExtract)
      throw new AwError("source_result_isolation_required");
    return copied;
  }
  throw new AwError("source_result_isolation_required");
}

function bufferSyncIterable(
  iterable,
  { maxStreamItems, maxStreamBytes, snapshotResult, customExtract },
) {
  const checked = [];
  let bufferedBytes = 2;
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
        requirePlainJson: true,
        customExtract,
      });
      bufferedBytes += jsonEncodedBytes(isolated) + (checked.length ? 1 : 0);
      if (bufferedBytes > maxStreamBytes)
        throw new AwError("source_stream_result_too_large");
      checked.push(isolated);
    }
    if (bufferedBytes > maxStreamBytes)
      throw new AwError("source_stream_result_too_large");
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
    callArgs.every((arg) => cloneStrictPlainJson(arg) !== UNCLONEABLE)
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
    cloneStrictPlainJson(replaced) !== UNCLONEABLE
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
        { maxBytes: maxStreamBytes, isolateForExtract: true, isStream: true },
      );
      const deliver = Object.hasOwn(decision, "replaced_result")
        ? applyStreamResult(checkedItems, decision.replaced_result, {
            applyResult,
            customExtract,
          })
        : cloneBufferedChunks(checkedItems);
      signal?.throwIfAborted();
      return deliver[Symbol.iterator]();
    }
    const isolated = isolateStrictValue(result, {
      snapshotResult,
      requirePlainJson: false,
      customExtract,
    });
    const decision = await observeResult(isolated, callId, snapshot, signal, {
      isolateForExtract: true,
    });
    if (Object.hasOwn(decision, "replaced_result")) {
      return applyToolResult(isolated, decision.replaced_result, {
        applyResult,
        customExtract,
      });
    }
    const delivered = cloneStrictPlainJson(isolated);
    return delivered !== UNCLONEABLE ? delivered : isolated;
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
  let bufferedBytes = 2;
  let afterDecision;
  let dispatched = false;
  function finish(close) {
    return session._runAdmittedCall(async () => {
      await close();
      if (strictAfter && bufferedBytes > maxStreamBytes)
        throw new AwError("source_stream_result_too_large");
      if (session._observeAfter()) {
        afterDecision = await observeResult(
          items,
          callId,
          snapshot,
          signal,
          strictAfter
            ? { maxBytes: maxStreamBytes, isolateForExtract: true, isStream: true }
            : undefined,
        );
      }
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
          requirePlainJson: true,
          customExtract,
        });
        bufferedBytes += jsonEncodedBytes(isolated) + (items.length ? 1 : 0);
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
          : cloneBufferedChunks(items);
      for (const out of deliver) {
        session._requireOpen();
        signal?.throwIfAborted();
        yield out;
      }
    }
  } catch (error) {
    throw dispatched ? toolExecuted(error) : error;
  }
}

export function buildToolWrapper(
  session,
  fn,
  {
    name,
    extractInput,
    applyInput,
    extractCallId,
    extractResult,
    applyResult,
    extractSignal,
    snapshotArg,
    snapshotResult,
    stream: asStream,
    maxStreamItems,
    maxStreamBytes = DEFAULT_MAX_STREAM_BYTES,
    bufferStream = false,
  },
  wrappedSymbol,
) {
  if (typeof fn !== "function") throw new AwError("source_callable_required");
  validateStreamLimits(maxStreamItems, maxStreamBytes);
  const toolName = validateIdentity(name ?? fn.name);
  const inputOf = extractInput ?? defaultExtractInput;
  const streaming = asStream === true || isAsyncGeneratorFunction(fn);
  const syncGen = isSyncGeneratorFunction(fn);

  function requireCheckedInput(callArgs, snapshot) {
    if (!jsonEqual(snapshotToolInput(inputOf(...callArgs)), snapshot))
      throw new AwError("source_input_mutated");
  }

  async function admitCall(args) {
    const signal = extractSignal?.(...args);
    let callArgs = args.map((arg) => snapshotInputArg(arg, snapshotArg));
    const rawInput = inputOf(...callArgs);
    const originalSnapshot = snapshotToolInput(rawInput);
    const callId = extractCallId?.(...callArgs);
    const {
      callId: resolvedCall,
      input: snapshot,
      decision,
    } = await session.beforeTool(toolName, rawInput, { callId, signal });
    session._requireOpen();
    signal?.throwIfAborted();
    requireCheckedInput(callArgs, originalSnapshot);
    if (Object.hasOwn(decision, "updated_input")) {
      callArgs = applyToolInput(decision.updated_input, callArgs, {
        applyInput,
        customExtract: Boolean(extractInput),
        snapshotArg,
      });
      if (!jsonEqual(snapshotToolInput(inputOf(...callArgs)), snapshot))
        throw new AwError("source_input_replace_mismatch");
    }
    return { resolvedCall, snapshot, signal, callArgs };
  }

  async function observeResult(
    result,
    callId,
    snapshot,
    signal,
    { maxBytes, isolateForExtract = false, isStream = false } = {},
  ) {
    try {
      const projected = prepareAuditedResult(result, {
        extractResult,
        maxBytes,
        isolateForExtract,
        isStream,
      });
      return await session._afterAdmittedTool(toolName, snapshot, projected, {
        callId,
        signal,
        normalized: true,
      });
    } catch (error) {
      throw toolExecuted(error);
    }
  }

  if (streaming) {
    async function* wrappedStream(...args) {
      const { resolvedCall, snapshot, signal, callArgs } =
        await admitCall(args);
      yield* runToolStream({
        session,
        receiver: this,
        fn,
        callArgs,
        snapshot,
        callId: resolvedCall,
        signal,
        requireCheckedInput,
        observeResult,
        extractResult,
        applyResult,
        snapshotResult,
        maxStreamItems,
        maxStreamBytes,
        bufferStream,
      });
    }
    wrappedStream[wrappedSymbol] = true;
    return wrappedStream;
  }
  async function wrapped(...args) {
    const { resolvedCall, snapshot, signal, callArgs } = await admitCall(args);
    session._requireOpen();
    signal?.throwIfAborted();
    requireCheckedInput(callArgs, snapshot);
    if (syncGen && session._strictAfter() && !bufferStream)
      throw new AwError("source_stream_withhold_requires_buffering");
    return session._runAdmittedCall(async () => {
      const result = await fn.apply(this, callArgs);
      return deliverToolResult(session, result, {
        observeResult,
        callId: resolvedCall,
        snapshot,
        signal,
        extractResult,
        applyResult,
        snapshotResult,
        bufferStream,
        maxStreamItems,
        maxStreamBytes,
      });
    });
  }
  wrapped[wrappedSymbol] = true;
  return wrapped;
}
