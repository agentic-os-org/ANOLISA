/** Execute an admitted profile call and release it when its work settles. */
import { randomUUID } from "node:crypto";
import { addAbortListener } from "node:events";
import { types } from "node:util";
import {
  fieldValue,
  parseFieldPath,
  readField,
  replaceField,
  writeField,
} from "./profile-eval.mjs";
import { SourceDenied, DEFAULT_MAX_STREAM_ITEMS } from "./sources.mjs";
import {
  DEFAULT_MAX_STREAM_BYTES,
  applyStreamResult,
  applyToolResult,
  isolateStrictValue,
  jsonEncodedBytes,
  streamCollector,
  routeHasStrictAfter,
} from "./source-mutation.mjs";
import {
  isAbortReason,
  jsonEqual,
  snapshotToolInput,
  toolExecuted,
  validateIdentity,
} from "./source-helpers.mjs";
import { AwError } from "./transport.mjs";

function beforeOptions(context) {
  return { sessionId: context.sessionId, callId: context.callId };
}

// Keep both the wire value and its native representation through all Hook waits.
function checkedField(binding, context) {
  const raw = fieldValue(binding, context);
  const value = readField(binding, context, raw);
  const json = binding.codec === "json";
  const object = json && value !== null && typeof value === "object";
  const { root, parts } = parseFieldPath(binding.from);
  if (
    root === "receiver" &&
    (object || (parts.length && context.target.call !== "sync"))
  )
    throw new AwError("source_input_isolation_required");
  function prepare() {
    const current = fieldValue(binding, context);
    const matches = json
      ? jsonEqual(value, readField(binding, context, current))
      : Object.is(raw, current);
    if (!matches) throw new AwError("source_input_mutated");
    if (root === "arg" && (object || parts.length))
      context.args = writeField(binding, context, json ? value : raw, "arg");
  }
  return { value, prepare };
}

function* before(context) {
  const {
    hooks,
    target: { fields },
  } = context;
  const options = { sessionId: context.sessionId };
  if (hooks.has("session.start")) yield ["sessionStart", options];
  if (hooks.has("input.submit")) {
    const prompt = checkedField(fields.prompt, context);
    context.preparePrompt = prompt.prepare;
    yield ["submit", prompt.value, { ...options, turnId: context.turnId }];
  }
  if (hooks.has("tool.before") || hooks.has("tool.after")) {
    context.name = validateIdentity(readField(fields.tool_name, context));
    context.callId = validateIdentity(
      readField(fields.call_id, context) ?? randomUUID(),
    );
    const binding = fields.input;
    const checked = hooks.has("tool.before")
      ? checkedField(binding, context)
      : undefined;
    const projected = checked ? checked.value : readField(binding, context);
    const toolInput = (value) =>
      snapshotToolInput(typeof value === "string" ? { text: value } : value);
    context.input = binding ? toolInput(projected) : {};
    if (checked) {
      const { decision, input } = yield [
        "beforeTool",
        context.name,
        context.input,
        beforeOptions(context),
      ];
      context.prepareInput = () => {
        checked.prepare();
        if (Object.hasOwn(decision, "updated_input")) {
          context.args = replaceField(binding, context, input, "arg");
          if (!jsonEqual(input, toolInput(readField(binding, context))))
            throw new AwError("source_input_replace_mismatch");
          context.input = input;
        }
      };
    }
  }
}

function* after(context, result, stream) {
  const {
    hooks,
    target: { fields },
  } = context;
  const { sessionId, turnId } = context;
  let delivered = result;
  if (hooks.has("tool.after")) {
    if (!stream && context.strictAfter)
      result = delivered = isolateStrictValue(result);
    const projected = readField(fields.result, { ...context, result });
    const decision = yield [
      "afterTool",
      context.name,
      context.input,
      projected,
      beforeOptions(context),
    ];
    if (Object.hasOwn(decision, "replaced_result")) {
      const options = {
        applyResult: (original, revision) =>
          replaceField(
            fields.result,
            { ...context, result: original },
            revision,
            "result",
          ),
      };
      delivered = stream
        ? applyStreamResult(result, decision.replaced_result, options)
        : applyToolResult(result, decision.replaced_result, options);
    }
  }
  if (hooks.has("turn.stop")) {
    const decision = yield ["turnStop", turnId, "completed", { sessionId }];
    if (decision.decision === "block")
      throw new SourceDenied("turn.stop", { sessionId, turnId });
  }
  if (hooks.has("session.end"))
    yield ["sessionEnd", "completed", { sessionId }];
  return delivered;
}

function runSync(steps, dispatch) {
  let step = steps.next();
  while (!step.done) step = steps.next(dispatch(step.value));
  return step.value;
}

async function runAsync(steps, dispatch) {
  let step = steps.next();
  while (!step.done) step = steps.next(await dispatch(step.value));
  return step.value;
}

function findSignal(receiver, args, target, resolver) {
  if (resolver) return resolver({ receiver, args, target });
  for (const arg of args) {
    if (types.isProxy(arg)) continue;
    if (arg instanceof AbortSignal) return arg;
    if (arg != null && typeof arg === "object") {
      const signal = Object.getOwnPropertyDescriptor(arg, "signal")?.value;
      if (!types.isProxy(signal) && signal instanceof AbortSignal)
        return signal;
    }
  }
}

function iteratorOf(value, async) {
  const acquire = async
    ? (value?.[Symbol.asyncIterator] ?? value?.[Symbol.iterator])
    : value?.[Symbol.iterator];
  const iterator = acquire ? acquire.call(value) : value;
  if (typeof iterator?.next !== "function")
    throw new AwError("source_callable_required");
  return iterator;
}

function iteratorStep(iterator, method, args) {
  if (typeof iterator[method] === "function")
    return Reflect.apply(iterator[method], iterator, args);
  if (method === "return") return { done: true, value: args[0] };
  throw args[0];
}

function profileIterator(async, run) {
  return {
    [async ? Symbol.asyncIterator : Symbol.iterator]() {
      return this;
    },
    next(...args) {
      return run("next", args);
    },
    return(...args) {
      return run("return", args);
    },
    throw(...args) {
      return run("throw", args);
    },
  };
}

// Retain call-time context, but admit async iterators only when first read.
export function lazyAsyncIterator(start) {
  let iterator;
  async function step(method, args) {
    const open = start;
    start = undefined;
    if (open && method === "next") iterator = iteratorOf(open(), true);
    if (!iterator && method === "next") return { value: undefined, done: true };
    return iteratorStep(iterator ?? {}, method, args);
  }
  return profileIterator(true, step);
}

function invokeStep(steps, dispatch, step) {
  let reply;
  try {
    reply = dispatch(step.value);
  } catch (error) {
    return steps.throw(error);
  }
  return steps.next(reply);
}

async function awaitStep(steps, dispatch, step) {
  let reply;
  try {
    reply = await dispatch(step.value);
  } catch (error) {
    return steps.throw(error);
  }
  return steps.next(reply);
}

function instrumentStream(context, dispatch, open, release, async) {
  const { signal } = context;
  const { stream } = context.target;
  const maxStreamItems = stream?.max_items ?? DEFAULT_MAX_STREAM_ITEMS;
  const maxStreamBytes = stream?.max_bytes ?? DEFAULT_MAX_STREAM_BYTES;
  const collector = streamCollector({
    observeAfter: context.hooks.has("tool.after"),
    strictAfter: context.strictAfter,
    bufferStream: stream?.buffer,
    maxStreamItems,
    maxStreamBytes,
  });
  let native = async ? undefined : iteratorOf(open(), false);
  let nativeDone = false;
  let closed = false;
  let delivery;
  let offset = 0;
  let completion;
  let pending = Promise.resolve();
  let cancellation;

  function close(cleanupError) {
    closed = true;
    cancellation?.[Symbol.dispose]();
    release(cleanupError);
  }

  function* step(method, args) {
    if (
      closed ||
      (!native && method !== "next") ||
      (delivery && method !== "next")
    ) {
      close();
      if (method === "next") signal?.throwIfAborted();
      if (method === "throw") throw args[0];
      return { done: true, value: method === "return" ? args[0] : undefined };
    }
    try {
      if (method === "next") signal?.throwIfAborted();
      if (!delivery) {
        native ??= iteratorOf(yield open, async);
        let item = yield () => iteratorStep(native, method, args);
        nativeDone = item.done;
        if (signal?.aborted && method === "return" && !nativeDone)
          throw new AwError("source_cleanup_unconfirmed");
        while (!nativeDone) {
          signal?.throwIfAborted();
          collector.collect(item.value);
          if (!collector.buffered) return item;
          item = yield () => iteratorStep(native, "next", []);
          nativeDone = item.done;
        }
        completion = item;
        if (
          method === "next" ||
          (collector.buffered && collector.items.length)
        ) {
          collector.finish();
          const delivered = yield () =>
            async
              ? runAsync(after(context, collector.items, true), dispatch)
              : runSync(after(context, collector.items, true), dispatch);
          signal?.throwIfAborted();
          if (collector.buffered) {
            if (
              delivered.length > maxStreamItems ||
              jsonEncodedBytes(delivered) > maxStreamBytes
            )
              throw new AwError("source_stream_result_too_large");
            delivery = delivered;
          }
        }
      }
      if (delivery && offset < delivery.length)
        return { done: false, value: delivery[offset++] };
      close();
      return completion;
    } catch (error) {
      let cleanupError;
      try {
        if (native && !nativeDone) {
          if (method === "return") throw error;
          const result = yield () => iteratorStep(native, "return", []);
          if (!result.done) throw new AwError("source_cleanup_unconfirmed");
        }
      } catch (cause) {
        if (isAbortReason(cause, [signal])) throw cause;
        cleanupError = toolExecuted(
          new AwError("source_cleanup_unconfirmed", { cause }),
        );
        throw cleanupError;
      } finally {
        close(cleanupError);
      }
      throw native ? toolExecuted(error) : error;
    }
  }

  function run(method, args) {
    const steps = step(method, args);
    if (!async) {
      let item = steps.next();
      while (!item.done) item = invokeStep(steps, (action) => action(), item);
      return item.value;
    }
    const result = pending.then(async () => {
      let item = steps.next();
      while (!item.done)
        item = await awaitStep(steps, (action) => action(), item);
      return item.value;
    });
    pending = result.catch(() => {});
    return result;
  }

  cancellation =
    signal &&
    addAbortListener(signal, () => {
      try {
        const result = run("return", []);
        if (async) result.catch(() => {});
      } catch {
        /* The next read reports cancellation. */
      }
    });
  return profileIterator(async, run);
}

export function runProfileCall({
  target,
  receiver,
  args,
  options,
  binding,
  callNative,
  release,
}) {
  try {
    const context = { target, receiver, args };
    context.signal = findSignal(receiver, args, target, options.resolveSignal);
    context.signal?.throwIfAborted();
    const extracted = readField(target.fields.session_id, context);
    const local = extracted ?? options.resolveSessionId?.(context);
    const store =
      local == null ? options.sessionStorage?.getStore() : undefined;
    context.sessionId = validateIdentity(
      local ??
        (typeof store === "string" ? store : store?.sessionId) ??
        binding.sessionId,
    );
    context.turnId = validateIdentity(
      readField(target.fields.turn_id, context) ?? randomUUID(),
    );
    context.hooks = new Set(
      binding.routes.hooks
        .map(({ event }) => event)
        .filter((event) => target.hooks[event]),
    );
    context.strictAfter =
      context.hooks.has("tool.after") && routeHasStrictAfter(binding.routes);
    const dispatch = (operation, sync) => {
      context.signal?.throwIfAborted();
      return binding.dispatch(operation, {
        signal: context.signal,
        sync,
      });
    };
    const syncDispatch = (operation) => dispatch(operation, true);
    const open = () => {
      context.signal?.throwIfAborted();
      context.prepareInput?.();
      context.preparePrompt?.();
      return callNative(context.args);
    };
    if (target.call === "async_iter") {
      return instrumentStream(
        context,
        dispatch,
        async () => {
          await runAsync(before(context), dispatch);
          return open();
        },
        release,
        true,
      );
    }
    if (target.call === "async") {
      return (async () => {
        try {
          await runAsync(before(context), dispatch);
          const result = await open();
          try {
            context.signal?.throwIfAborted();
            return await runAsync(after(context, result, false), dispatch);
          } catch (error) {
            throw toolExecuted(error);
          }
        } finally {
          release();
        }
      })();
    }
    runSync(before(context), syncDispatch);
    if (target.call === "sync_iter")
      return instrumentStream(context, syncDispatch, open, release, false);
    const result = open();
    try {
      context.signal?.throwIfAborted();
      return runSync(after(context, result, false), syncDispatch);
    } catch (error) {
      throw toolExecuted(error);
    } finally {
      release();
    }
  } catch (error) {
    release();
    throw error;
  }
}
