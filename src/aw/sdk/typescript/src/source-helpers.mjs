/** Pure JSON normalization, argument isolation and descriptor helpers for sources.mjs. */
import { addAbortListener } from "node:events";
import { types } from "node:util";
import { AwError } from "./transport.mjs";

const MAX_DEPTH = 32;

export function validateIdentity(value) {
  if (
    typeof value !== "string" ||
    value.trim().length === 0 ||
    Buffer.byteLength(value, "utf8") > 512
  )
    throw new AwError("source_invalid_identity");
  return value;
}

export function setDataProperty(target, key, value) {
  Object.defineProperty(target, key, {
    value,
    enumerable: true,
    writable: true,
    configurable: true,
  });
}

export function normalizeJson(value, depth = 0, seen = new Set()) {
  if (depth > MAX_DEPTH) throw new AwError("source_invalid_payload");
  if (value === null || value === undefined) return null;
  const kind = typeof value;
  if (kind === "boolean" || kind === "string") return value;
  if (kind === "number") {
    if (!Number.isFinite(value)) throw new AwError("source_invalid_payload");
    return value;
  }
  if (kind === "bigint") return value.toString();
  if (value instanceof Uint8Array) return Buffer.from(value).toString("utf8");
  if (value instanceof Date) return value.toISOString();
  if (value instanceof URL) return value.toString();
  if (Array.isArray(value)) {
    if (seen.has(value)) throw new AwError("source_invalid_payload");
    seen.add(value);
    try {
      return value.map((item) => normalizeJson(item, depth + 1, seen));
    } finally {
      seen.delete(value);
    }
  }
  if (kind === "object") {
    if (seen.has(value)) throw new AwError("source_invalid_payload");
    seen.add(value);
    try {
      const result = {};
      for (const key of Object.keys(value)) {
        const item = value[key];
        if (item !== undefined)
          setDataProperty(result, key, normalizeJson(item, depth + 1, seen));
      }
      return result;
    } finally {
      seen.delete(value);
    }
  }
  return String(value);
}

export function isPlainObject(value) {
  if (value === null || typeof value !== "object" || Array.isArray(value))
    return false;
  const proto = Object.getPrototypeOf(value);
  return proto === Object.prototype || proto === null;
}

export const UNCLONEABLE = Symbol("uncloneable");

function cloneJsonValue(value, allowUndefined, depth = 0, seen = new Set()) {
  if (depth > MAX_DEPTH) return UNCLONEABLE;
  if (value === null) return null;
  if (value === undefined) return allowUndefined ? undefined : UNCLONEABLE;
  const kind = typeof value;
  if (kind === "boolean" || kind === "string") return value;
  if (kind === "number") return Number.isFinite(value) ? value : UNCLONEABLE;
  if (kind !== "object" || types.isProxy(value)) return UNCLONEABLE;
  if (seen.has(value)) return UNCLONEABLE;
  const array = Array.isArray(value);
  const proto = Object.getPrototypeOf(value);
  if (array ? proto !== Array.prototype : !isPlainObject(value))
    return UNCLONEABLE;
  const copy = array ? [] : Object.create(proto);
  const keys = Reflect.ownKeys(value);
  if (array && (keys.pop() !== "length" || keys.length !== value.length))
    return UNCLONEABLE;
  seen.add(value);
  try {
    for (const [index, key] of keys.entries()) {
      if (typeof key !== "string" || (array && key !== String(index)))
        return UNCLONEABLE;
      const property = Object.getOwnPropertyDescriptor(value, key);
      if (!property?.enumerable || !Object.hasOwn(property, "value"))
        return UNCLONEABLE;
      const item = cloneJsonValue(property.value, allowUndefined, depth + 1, seen);
      if (item === UNCLONEABLE) return UNCLONEABLE;
      setDataProperty(copy, key, item);
    }
    return copy;
  } finally {
    seen.delete(value);
  }
}

export function clonePlainJson(value) {
  return cloneJsonValue(value, true);
}

export function cloneStrictPlainJson(value) {
  return cloneJsonValue(value, false);
}

export function jsonEqual(a, b) {
  if (Object.is(a, b)) return true;
  if (
    a === null ||
    b === null ||
    typeof a !== "object" ||
    typeof b !== "object"
  )
    return false;
  if (Array.isArray(a) !== Array.isArray(b)) return false;
  if (Array.isArray(a))
    return a.length === b.length && a.every((item, i) => jsonEqual(item, b[i]));
  const keysA = Object.keys(a);
  const keysB = Object.keys(b);
  if (keysA.length !== keysB.length) return false;
  return keysA.every(
    (key) => Object.hasOwn(b, key) && jsonEqual(a[key], b[key]),
  );
}

export function defaultExtractText(...args) {
  for (const arg of args) {
    if (typeof arg === "string") return arg;
    if (isPlainObject(arg)) {
      for (const key of ["text", "prompt", "input"]) {
        if (typeof arg[key] === "string") return arg[key];
      }
    }
  }
  throw new AwError("source_input_text_required");
}

export function defaultExtractInput(...args) {
  if (args.length === 1 && isPlainObject(args[0]))
    return normalizeJson(args[0]);
  return { args: normalizeJson(args) };
}

export function snapshotToolInput(raw) {
  const normalized = normalizeJson(raw);
  if (!isPlainObject(normalized))
    throw new AwError("source_tool_input_object_required");
  return normalized;
}

export function snapshotInputArg(value, snapshotArg) {
  if (value === null || !["object", "function"].includes(typeof value))
    return value;
  if (value instanceof AbortSignal) return value;
  const jsonCopy = clonePlainJson(value);
  if (jsonCopy !== UNCLONEABLE) return jsonCopy;
  if (snapshotArg) {
    const copy = snapshotArg(value);
    if (copy !== value) return copy;
  }
  throw new AwError("source_input_snapshot_required");
}

export function snapshotStreamChunk(value) {
  const jsonCopy = clonePlainJson(value);
  return jsonCopy === UNCLONEABLE ? value : jsonCopy;
}

export function isAsyncGeneratorFunction(fn) {
  return fn?.constructor?.name === "AsyncGeneratorFunction";
}

export function findPrototypeDescriptor(target, name) {
  let proto = Object.getPrototypeOf(target);
  while (proto !== null) {
    const desc = Object.getOwnPropertyDescriptor(proto, name);
    if (desc) return desc;
    proto = Object.getPrototypeOf(proto);
  }
  return undefined;
}

function withSignal(pending, signal, onSettled) {
  return new Promise((resolve, reject) => {
    let detached = false;
    const listener = addAbortListener(signal, () => {
      detached = true;
      reject(signal.reason);
    });
    function settle(deliver, value) {
      listener[Symbol.dispose]();
      if (detached) return;
      onSettled?.();
      deliver(value);
    }
    pending.then(
      (value) => settle(resolve, value),
      (error) => settle(reject, error),
    );
  });
}

export function isAbortReason(reason, signals) {
  return signals.some(
    (stop) =>
      stop?.aborted &&
      (Object.is(stop.reason, reason) ||
        (reason?.name === "AbortError" && Object.is(stop.reason, reason.cause))),
  );
}

export async function* readStream(open, sessionSignal, callerSignal, active, onComplete) {
  const signal = callerSignal
    ? AbortSignal.any([callerSignal, sessionSignal])
    : sessionSignal;
  const signals = [signal, sessionSignal, callerSignal];
  signal.throwIfAborted();
  const opening = Promise.resolve().then(async () => {
    signal.throwIfAborted();
    const iterable = await open(signal);
    const iterator = iterable?.[Symbol.asyncIterator]?.();
    if (typeof iterator?.next !== "function")
      throw new AwError("source_stream_required");
    return iterator;
  });
  let openingSettled = false;
  let pending;
  let closing;
  function close() {
    closing ??= (async () => {
      const [opened] = await Promise.allSettled([opening]);
      if (opened.status === "rejected") {
        if (!openingSettled && !isAbortReason(opened.reason, signals))
          throw opened.reason;
        return;
      }
      const iterator = opened.value;
      // return() can queue behind next(). Keep the Task until both settle.
      const results = await Promise.allSettled([
        Promise.resolve().then(() => iterator.return?.()),
        pending,
      ]);
      for (const result of results) {
        if (result.status === "rejected" && !isAbortReason(result.reason, signals))
          throw result.reason;
      }
    })().then(() => active.delete(close));
    // An aborted consumer can no longer observe native cleanup failures. Keep the
    // rejected close promise for the session drain.
    return closing;
  }
  active.add(close);
  const listener = addAbortListener(signal, () => {
    close().catch(() => {});
  });
  try {
    const iterator = await withSignal(opening, signal, () => {
      openingSettled = true;
    });
    while (true) {
      signal.throwIfAborted();
      pending = Promise.resolve(iterator.next());
      const step = await withSignal(pending, signal, () => {
        pending = undefined;
      });
      signal.throwIfAborted();
      if (step.done) {
        if (onComplete) await onComplete(close);
        return;
      }
      yield step.value;
    }
  } finally {
    listener[Symbol.dispose]();
    if (signal.aborted) close().catch(() => {});
    else {
      try {
        await close();
      } finally {
        active.delete(close);
      }
    }
  }
}

export function toolExecuted(error) {
  try {
    error.toolExecuted = true;
    return error;
  } catch {
    const failure = new AwError("source_result_processing_failed", {
      cause: error,
    });
    failure.toolExecuted = true;
    return failure;
  }
}

export function restoreEntries(entries) {
  let firstError;
  for (let i = entries.length - 1; i >= 0; i--) {
    const { target, name, hadOwn, descriptor, wrapper } = entries[i];
    try {
      if (target[name] !== wrapper) continue;
      if (hadOwn) Object.defineProperty(target, name, descriptor);
      else if (!delete target[name]) throw new AwError("source_restore_failed");
    } catch (error) {
      firstError ??= error;
    }
  }
  if (firstError !== undefined) throw firstError;
}
