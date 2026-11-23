/** Framework-free language-level Hook Source runtime over aw-hook-source/v1. */
import { randomUUID } from "node:crypto";
import { Admission } from "./admission.mjs";
import { Instance } from "./client.mjs";
import {
  defaultExtractInput,
  defaultExtractText,
  findPrototypeDescriptor,
  isAsyncGeneratorFunction,
  jsonEqual,
  readStream,
  snapshotInputArg,
  snapshotStreamChunk,
  toolExecuted,
  normalizeJson,
  restoreEntries,
  snapshotToolInput,
  validateIdentity,
} from "./source-helpers.mjs";
import { AwError } from "./transport.mjs";

import { SOURCE_PROTOCOL, DEFAULT_SOURCE_EVENTS, sourceCapabilities, confirmSourceRoutes } from "./source-profile.mjs";
export { SOURCE_PROTOCOL, DEFAULT_SOURCE_EVENTS, sourceCapabilities, confirmSourceRoutes } from "./source-profile.mjs";

export const DEFAULT_MAX_STREAM_ITEMS = 1024;
const WRAPPED = Symbol("aw.source.wrapped");

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

export class SourceSession {
  constructor(
    source,
    { sessionId, events, timeoutMs = 5000, cwd, environment } = {},
  ) {
    this._source = source;
    this.sessionId = validateIdentity(sessionId ?? randomUUID());
    const declared = Object.keys(source.instance.options.capabilities.events);
    const selected = events === undefined ? declared : [...new Set(events)];
    if (
      !Array.isArray(selected) ||
      !selected.length ||
      selected.some((event) => !declared.includes(event))
    )
      throw new AwError("admission_event_unavailable");
    this.events = Object.freeze(selected);
    this.admission = new Admission(source.instance, {
      events: [...this.events],
      timeoutMs,
      cwd,
      environment,
    });
    this.closed = false;
    this._instrumentations = [];
    this._activeStreams = new Set();
    this._activeCalls = new Set();
    this._taskClosed = false;
    this._shutdown = new AbortController();
    if (source._closed) throw new AwError("source_closed");
    source._sessions.add(this);
  }

  get task() {
    return this.admission.task;
  }

  get fallback() {
    return this.admission.fallback;
  }

  get signal() {
    return this._shutdown.signal;
  }

  _observeAfter() {
    return (
      !this.fallback &&
      this.task?.routes.hooks.some((hook) => hook.event === "tool.after")
    );
  }

  _requireOpen() {
    if (this.closed) throw new AwError("source_session_closed");
  }

  async _runAdmittedCall(fn) {
    this._requireOpen();
    const { promise, resolve } = Promise.withResolvers();
    this._activeCalls.add(promise);
    try {
      return await fn();
    } finally {
      this._activeCalls.delete(promise);
      resolve();
    }
  }

  _verifyRouteCoverage() {
    const task = this.admission.task;
    if (this.fallback || !task) return;
    for (const hook of task.routes?.hooks ?? []) {
      if (!this.events.includes(hook.event))
        throw new AwError("source_route_coverage_incomplete");
      if (hook.replace_input || hook.replace_result || hook.skill_check)
        throw new AwError("source_route_effect_unsupported");
    }
  }

  _combineSignal(signal) {
    if (!signal || signal === this.signal) return this.signal;
    return AbortSignal.any([signal, this.signal]);
  }

  async prepare(signal) {
    this._requireOpen();
    signal?.throwIfAborted();
    await this.admission.prepare(this._combineSignal(signal));
    this._verifyRouteCoverage();
    this._requireOpen();
    signal?.throwIfAborted();
  }

  async _lifecycle(event, fields, signal) {
    if (!this.events.includes(event))
      throw new AwError("semantic_event_unavailable");
    const combined = this._combineSignal(signal);
    await this.prepare(combined);
    return this._runAdmittedCall(async () => {
      const decision = await this.task.emit(
        event,
        { session_id: this.sessionId, ...fields },
        combined,
      );
      if (decision.decision === "block") return decision;
      this._requireOpen();
      signal?.throwIfAborted();
      return decision;
    });
  }

  async sessionStart({ signal } = {}) {
    return this._lifecycle("session.start", {}, signal);
  }

  async turnStop(turnId, reason, { attemptId, signal } = {}) {
    const fields = { turn_id: validateIdentity(turnId), reason: validateIdentity(reason) };
    if (attemptId !== undefined) fields.attempt_id = validateIdentity(attemptId);
    return this._lifecycle("turn.stop", fields, signal);
  }

  async sessionEnd(reason, { signal } = {}) {
    return this._lifecycle("session.end", { reason: validateIdentity(reason) }, signal);
  }

  async submit(text, { turnId, signal } = {}) {
    this._requireOpen();
    signal?.throwIfAborted();
    if (!this.events.includes("input.submit"))
      throw new AwError("semantic_event_unavailable");
    if (typeof text !== "string")
      throw new AwError("source_input_text_required");
    const resolvedTurn = validateIdentity(turnId ?? randomUUID());
    const combined = this._combineSignal(signal);
    await this.prepare(combined);
    return this._runAdmittedCall(async () => {
      const decision = await this.task.emit(
        "input.submit",
        { session_id: this.sessionId, turn_id: resolvedTurn, text },
        combined,
      );
      if (decision.decision === "block")
        throw new SourceDenied("input.submit", {
          sessionId: this.sessionId,
          turnId: resolvedTurn,
        });
      this._requireOpen();
      signal?.throwIfAborted();
      return decision;
    });
  }

  async beforeTool(name, input, { callId, signal } = {}) {
    this._requireOpen();
    signal?.throwIfAborted();
    const toolName = validateIdentity(name);
    const resolvedCall = validateIdentity(callId ?? randomUUID());
    const snapshot = snapshotToolInput(input);
    const combined = this._combineSignal(signal);
    await this.prepare(combined);
    if (this.events.includes("tool.before")) {
      return this._runAdmittedCall(async () => {
        const decision = await this.task.emit(
          "tool.before",
          {
            session_id: this.sessionId,
            call_id: resolvedCall,
            name: toolName,
            input: snapshot,
          },
          combined,
        );
        if (decision.decision === "block")
          throw new SourceDenied("tool.before", {
            sessionId: this.sessionId,
            callId: resolvedCall,
            toolName,
          });
        this._requireOpen();
        signal?.throwIfAborted();
        return { callId: resolvedCall, input: snapshot, decision };
      });
    }
    this._requireOpen();
    signal?.throwIfAborted();
    return {
      callId: resolvedCall,
      input: snapshot,
      decision: { decision: "continue", coverage: "not_configured" },
    };
  }

  async afterTool(name, input, result, { callId, signal } = {}) {
    try {
      this._requireOpen();
      if (!this.events.includes("tool.after"))
        return { decision: "continue", coverage: "not_configured" };
      await this.prepare(signal);
      return await this._runAdmittedCall(() =>
        this._afterAdmittedTool(name, input, result, { callId, signal }),
      );
    } catch (error) {
      throw toolExecuted(error);
    }
  }

  async _afterAdmittedTool(name, input, result, { callId, signal }) {
    try {
      if (this._taskClosed) throw new AwError("source_session_closed");
      if (!this._observeAfter())
        return {
          decision: "continue",
          coverage: this.fallback ? "unavailable" : "not_configured",
        };
      const toolName = validateIdentity(name);
      const resolvedCall = validateIdentity(callId);
      const snapshot = snapshotToolInput(input);
      return await this.task.emit(
        "tool.after",
        {
          session_id: this.sessionId,
          call_id: resolvedCall,
          name: toolName,
          input: snapshot,
          result: normalizeJson(result),
        },
        signal,
      );
    } catch (error) {
      throw toolExecuted(error);
    }
  }

  wrapInputStream(fn, options = {}) {
    return this.wrapInput(fn, { ...options, stream: true });
  }

  wrapInput(
    fn,
    {
      extractText,
      extractTurnId,
      extractSignal,
      snapshotArg,
      stream: asStream,
    } = {},
  ) {
    if (typeof fn !== "function") throw new AwError("source_callable_required");
    const session = this;
    const textOf = extractText ?? defaultExtractText;
    const streaming = asStream === true || isAsyncGeneratorFunction(fn);
    async function dispatch(receiver, args, signal) {
      const callArgs = args.map((arg) => snapshotInputArg(arg, snapshotArg));
      const text = textOf(...callArgs);
      const turnId = extractTurnId?.(...callArgs);
      await session.submit(text, { turnId, signal });
      session._requireOpen();
      signal?.throwIfAborted();
      if (textOf(...callArgs) !== text)
        throw new AwError("source_input_mutated");
      const invoke = () => fn.apply(receiver, callArgs);
      return streaming ? invoke() : session._runAdmittedCall(invoke);
    }
    if (streaming) {
      async function* wrappedStream(...args) {
        const signal = extractSignal?.(...args);
        yield* readStream(
          (stop) => dispatch(this, args, stop),
          session.signal,
          signal,
          session._activeStreams,
        );
      }
      wrappedStream[WRAPPED] = true;
      return wrappedStream;
    }
    async function wrapped(...args) {
      return dispatch(this, args, extractSignal?.(...args));
    }
    wrapped[WRAPPED] = true;
    return wrapped;
  }

  wrapToolStream(fn, options = {}) {
    return this.wrapTool(fn, { ...options, stream: true });
  }

  wrapTool(
    fn,
    {
      name,
      extractInput,
      extractCallId,
      extractResult,
      extractSignal,
      snapshotArg,
      stream: asStream,
      maxStreamItems = DEFAULT_MAX_STREAM_ITEMS,
    } = {},
  ) {
    if (typeof fn !== "function") throw new AwError("source_callable_required");
    if (!Number.isInteger(maxStreamItems) || maxStreamItems < 1)
      throw new AwError("source_invalid_stream_limit");
    const session = this;
    const toolName = validateIdentity(name ?? fn.name);
    const inputOf = extractInput ?? defaultExtractInput;
    const streaming = asStream === true || isAsyncGeneratorFunction(fn);

    function requireCheckedInput(callArgs, snapshot) {
      if (!jsonEqual(snapshotToolInput(inputOf(...callArgs)), snapshot))
        throw new AwError("source_input_mutated");
    }

    async function admitCall(args) {
      const signal = extractSignal?.(...args);
      const callArgs = args.map((arg) => snapshotInputArg(arg, snapshotArg));
      const rawInput = inputOf(...callArgs);
      const callId = extractCallId?.(...callArgs);
      const { callId: resolvedCall, input: snapshot } =
        await session.beforeTool(toolName, rawInput, { callId, signal });
      session._requireOpen();
      signal?.throwIfAborted();
      requireCheckedInput(callArgs, snapshot);
      return { resolvedCall, snapshot, signal, callArgs };
    }

    async function observeResult(result, callId, snapshot, signal) {
      try {
        const payload = extractResult ? extractResult(result) : result;
        await session._afterAdmittedTool(toolName, snapshot, payload, {
          callId,
          signal,
        });
      } catch (error) {
        throw toolExecuted(error);
      }
    }

    if (streaming) {
      async function* wrappedStream(...args) {
        const { resolvedCall, snapshot, signal, callArgs } =
          await admitCall(args);
        const items = session._observeAfter() ? [] : null;
        let dispatched = false;
        // Transfer ownership before the native stream finishes closing.
        function finish(close) {
          return session._runAdmittedCall(async () => {
            await close();
            if (session._observeAfter())
              await observeResult(items, resolvedCall, snapshot, signal);
          });
        }
        const stream = readStream(
          () => {
            requireCheckedInput(callArgs, snapshot);
            dispatched = true;
            return fn.apply(this, callArgs);
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
            yield item;
          }
        } catch (error) {
          throw dispatched ? toolExecuted(error) : error;
        }
      }
      wrappedStream[WRAPPED] = true;
      return wrappedStream;
    }
    async function wrapped(...args) {
      const { resolvedCall, snapshot, signal, callArgs } =
        await admitCall(args);
      session._requireOpen();
      signal?.throwIfAborted();
      requireCheckedInput(callArgs, snapshot);
      return session._runAdmittedCall(async () => {
        const result = await fn.apply(this, callArgs);
        if (session._observeAfter())
          await observeResult(result, resolvedCall, snapshot, signal);
        return result;
      });
    }
    wrapped[WRAPPED] = true;
    return wrapped;
  }

  instrument(target, { inputMethods = [], toolMethods = [] } = {}) {
    this._requireOpen();
    if (!target || (typeof target !== "object" && typeof target !== "function"))
      throw new AwError("source_target_required");
    const inputSpec = Array.isArray(inputMethods)
      ? Object.fromEntries(inputMethods.map((name) => [name, {}]))
      : { ...inputMethods };
    const toolSpec = Array.isArray(toolMethods)
      ? Object.fromEntries(toolMethods.map((name) => [name, {}]))
      : Object.fromEntries(
          Object.entries(toolMethods).map(([attr, spec]) => [
            attr,
            typeof spec === "string" ? { name: spec } : { ...spec },
          ]),
        );
    for (const name of Object.keys(inputSpec)) {
      if (Object.hasOwn(toolSpec, name))
        throw new AwError("source_method_conflict");
    }
    const entries = [];
    try {
      for (const [name, opts] of Object.entries(inputSpec))
        entries.push(this._patchOne(target, name, this.wrapInput, opts));
      for (const [name, opts] of Object.entries(toolSpec))
        entries.push(
          this._patchOne(target, name, this.wrapTool, { name, ...opts }),
        );
    } catch (error) {
      try {
        restoreEntries(entries);
      } catch {}
      throw error;
    }
    let restored = false;
    const handle = {
      get restored() {
        return restored;
      },
      restore() {
        if (restored) return;
        restored = true;
        restoreEntries(entries);
      },
    };
    if (typeof Symbol.dispose === "symbol")
      handle[Symbol.dispose] = () => handle.restore();
    this._instrumentations.push(handle);
    return handle;
  }

  _patchOne(target, name, factory, opts) {
    if (!(name in target)) throw new AwError("source_attribute_missing");
    const hadOwn = Object.hasOwn(target, name);
    const descriptor = hadOwn
      ? Object.getOwnPropertyDescriptor(target, name)
      : findPrototypeDescriptor(target, name);
    if (!descriptor || descriptor.get || descriptor.set)
      throw new AwError("source_not_patchable");
    if (hadOwn && (!descriptor.writable || !descriptor.configurable))
      throw new AwError("source_not_patchable");
    if (!hadOwn && !Object.isExtensible(target))
      throw new AwError("source_not_patchable");
    const original = descriptor.value;
    if (typeof original !== "function")
      throw new AwError("source_callable_required");
    if (original[WRAPPED]) throw new AwError("source_already_instrumented");
    const wrapper = factory.call(this, original, opts);
    try {
      Object.defineProperty(target, name, {
        value: wrapper,
        writable: true,
        configurable: true,
        enumerable: hadOwn ? Boolean(descriptor.enumerable) : false,
      });
    } catch (error) {
      throw new AwError("source_not_patchable", { cause: error });
    }
    if (target[name] !== wrapper) throw new AwError("source_not_patchable");
    return { target, name, hadOwn, descriptor, wrapper };
  }

  async close(options = {}) {
    if (options === null || typeof options !== "object")
      throw new AwError("invalid_attach_timeout");
    const { timeoutMs = this.admission.options.timeoutMs } = options;
    if (!Number.isInteger(timeoutMs) || timeoutMs < 100 || timeoutMs > 60000)
      throw new AwError("invalid_attach_timeout");
    this.closed = true;
    this._shutdown.abort(new AwError("source_session_closed"));
    this._closing ??= this._drain();
    let timer;
    try {
      await Promise.race([
        this._closing,
        new Promise((_, reject) => {
          timer = setTimeout(
            () => reject(new AwError("source_close_pending")),
            timeoutMs,
          );
        }),
      ]);
    } finally {
      clearTimeout(timer);
    }
  }

  async _drain() {
    const errors = [];
    while (this._activeStreams.size || this._activeCalls.size) {
      const streams = [...this._activeStreams];
      this._activeStreams.clear();
      const results = await Promise.allSettled(streams.map((close) => close()));
      for (const stream of results)
        if (stream.status === "rejected") errors.push(stream.reason);
      if (this._activeCalls.size) await Promise.all([...this._activeCalls]);
    }
    const handles = this._instrumentations.splice(0).reverse();
    for (const handle of handles) {
      try {
        handle.restore();
      } catch (error) {
        errors.push(error);
      }
    }
    this._taskClosed = true;
    try {
      await this.admission.close();
    } catch (error) {
      errors.push(error);
    }
    this._source._sessions.delete(this);
    if (errors.length) {
      if (this._source._closed) this._source._drainErrors ??= errors;
      throw errors[0];
    }
  }
}

export class LanguageSource {
  constructor(instance, { ownsInstance = false } = {}) {
    const caps = instance?.options?.capabilities;
    if (caps?.adapter !== "sdk" || caps?.version !== SOURCE_PROTOCOL)
      throw new AwError("semantic_profile_required");
    this.instance = instance;
    this._ownsInstance = ownsInstance;
    this._sessions = new Set();
    this._closed = false;
  }

  static create({
    endpoint,
    target,
    availability = "required",
    events = DEFAULT_SOURCE_EVENTS,
    cwd,
    environment,
    config,
    timeoutMs,
    reconnectMs,
  }) {
    const capabilities = sourceCapabilities(events);
    const instance = new Instance({
      endpoint,
      target,
      capabilities,
      availability,
      ...(cwd !== undefined ? { cwd } : {}),
      ...(environment !== undefined ? { environment } : {}),
      ...(config !== undefined ? { config } : {}),
      ...(timeoutMs !== undefined ? { timeoutMs } : {}),
      ...(reconnectMs !== undefined ? { reconnectMs } : {}),
      install: (routes) => confirmSourceRoutes(capabilities, routes),
    });
    return new LanguageSource(instance, { ownsInstance: true });
  }

  connect() {
    return this.instance.connect();
  }

  start() {
    this.instance.start();
  }

  listen(options) {
    this.instance.listen(options);
  }

  stop() {
    return this.instance.stop();
  }

  session(options) {
    return new SourceSession(this, options);
  }

  async withSession(options, callback) {
    const session = this.session(options);
    try {
      return await callback(session);
    } finally {
      await session.close();
    }
  }

  async close(options = {}) {
    if (options === null || typeof options !== "object")
      throw new AwError("invalid_attach_timeout");
    const { closeInstance = this._ownsInstance, timeoutMs } = options;
    if (
      timeoutMs !== undefined &&
      (!Number.isInteger(timeoutMs) || timeoutMs < 100 || timeoutMs > 60000)
    )
      throw new AwError("invalid_attach_timeout");
    this._closed = true;
    const errors = [];
    const results = await Promise.allSettled(
      [...this._sessions].map((active) =>
        active.close(timeoutMs === undefined ? undefined : { timeoutMs }),
      ),
    );
    for (const result of results)
      if (result.status === "rejected") errors.push(result.reason);
    const isPending = (error) =>
      error instanceof AwError && error.code === "source_close_pending";
    if (this._sessions.size) {
      const pending = errors.find(isPending);
      if (pending) throw pending;
    } else {
      const remaining = errors.filter((error) => !isPending(error));
      if (closeInstance) {
        try {
          await this.instance.close();
        } catch (error) {
          remaining.push(error);
        }
      }
      errors.length = 0;
      errors.push(...remaining);
    }
    if (errors.length) throw errors[0];
    if (this._drainErrors) throw this._drainErrors[0];
  }
}
