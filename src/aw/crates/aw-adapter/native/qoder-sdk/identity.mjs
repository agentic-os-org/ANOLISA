/** Correlate native model frames and control dispatch without trusting caller Hook input. */
import { AsyncLocalStorage } from "node:async_hooks";
import { randomUUID } from "node:crypto";
import { isDeepStrictEqual } from "node:util";

export class Identities {
  constructor(scope, onFailure) {
    this.scope = scope;
    this.onFailure = onFailure;
    this.active = new Map();
    this.dispatch = new AsyncLocalStorage();
    this.pending = new Set();
    this.session = undefined;
  }

  produce(message) {
    const start = message.type === "stream_event" && message.event?.type === "content_block_start" &&
      message.event.content_block?.type === "tool_use" && message.parent_tool_use_id != null;
    if (message.type !== "assistant" && !start) return;
    this.scope.checkExecution();
    if (typeof message.session_id !== "string" || !message.session_id ||
        (this.session && this.session !== message.session_id))
      throw new Error("AW native Query session identity changed");
    this.session = message.session_id;
    for (const block of start ? [message.event.content_block] : message.message?.content ?? []) {
      if (block.type !== "tool_use") continue;
      let entry = this.active.get(block.id);
      if (typeof block.id !== "string" || !block.id ||
          typeof block.name !== "string" || !block.name ||
          (entry && (start || !entry.producer)) ||
          (!entry && this.active.size >= this.scope.invocationLimit))
        throw new Error("AW rejected invalid or duplicate native model invocation");
      const parent = message.parent_tool_use_id ?? null;
      if (!entry) entry = { id: randomUUID(), native: block.id, name: block.name,
        parent, parentEntry: this.active.get(parent), callbacks: new Set(),
        phase: "issued", pending: new Set() };
      if (entry.name !== block.name || entry.parent !== parent)
        throw new Error("AW rejected changed native child producer identity");
      this.checkParent(entry);
      if (start) {
        // Child Pre can precede the complete assistant. Reserve identity only;
        // input deltas and callback input never become the model producer.
        let resolve, reject;
        const promise = new Promise((done, fail) => { resolve = done; reject = fail; });
        const timer = setTimeout(() => this.onFailure(
          new Error("AW native child producer deadline exceeded")), 12000);
        entry.producer = { promise, resolve, reject, timer };
        promise.catch(() => {});
        this.active.set(block.id, entry);
        continue;
      }
      if (!block.input || Array.isArray(block.input) || typeof block.input !== "object")
        throw new Error("AW rejected invalid native model input");
      entry.input = structuredClone(block.input);
      this.active.set(block.id, entry);
      entry.admitted = this.scope.register(entry.id, entry.name, entry.input);
      entry.admitted.catch(error => this.onFailure(error));
      if (entry.producer) {
        clearTimeout(entry.producer.timer);
        entry.producer.resolve();
        entry.producer = undefined;
      }
    }
  }

  checkParent(entry) {
    if (entry.parent === null) return;
    const parent = this.active.get(entry.parent);
    if (!parent || parent !== entry.parentEntry || parent.name !== "Agent" ||
        parent.phase !== "allowed" || parent.terminal)
      throw new Error("AW native child lacks an active admitted parent Agent");
  }

  check(entry, authorize = false) {
    this.scope.checkExecution();
    if (this.active.get(entry.native) !== entry || entry.phase === "retiring" ||
        (authorize && entry.terminal))
      throw new Error("AW rejected a retired native tool identity");
    if (authorize) this.checkParent(entry);
  }

  async ready(entry, signal, authorize = false) {
    this.scope.checkExecution();
    let abort;
    const cancelled = new Promise((_, reject) => {
      abort = () => reject(new Error("AW native producer wait cancelled"));
      if (signal?.aborted) abort();
      else signal?.addEventListener("abort", abort, { once: true });
    });
    try {
      await Promise.race([(async () => {
        await entry.producer?.promise;
        await entry.admitted;
      })(), cancelled]);
    } finally { signal?.removeEventListener("abort", abort); }
    if (signal?.aborted) throw new Error("AW native producer wait cancelled");
    this.check(entry, authorize);
  }

  cancel(error) {
    for (const entry of this.active.values()) {
      if (!entry.producer) continue;
      clearTimeout(entry.producer.timer);
      entry.producer.reject(error);
    }
  }

  ticket(frame) {
    this.scope.checkExecution();
    if (!frame.request_id || this.pending.has(frame.request_id))
      throw new Error("AW rejected a duplicate native control request");
    const request = frame.request;
    const kind = request.subtype ?? request.type;
    const native = kind === "hook_callback" ? request.tool_use_id :
      kind === "can_use_tool" ? request.tool_use_id : undefined;
    const entry = native === undefined ? undefined : this.active.get(native);
    if (native !== undefined && !entry)
      throw new Error("AW rejected an unknown native tool identity");
    this.pending.add(frame.request_id);
    return { id: frame.request_id, kind, entry, request: structuredClone(request) };
  }

  async claim(input, id, callback, signal) {
    const ticket = this.dispatch.getStore();
    const entry = ticket?.entry;
    if (!entry || ticket.kind !== "hook_callback" || id !== entry.native ||
        input.tool_use_id !== id || input.session_id !== this.session ||
        input.tool_name !== entry.name ||
        entry.callbacks.has(callback) || this.active.get(id) !== entry)
      throw new Error("AW rejected stale, repeated or changed native Hook input");
    // Reserve before awaiting the producer or admission to reject concurrent replay.
    entry.callbacks.add(callback);
    await this.ready(entry, signal, input.hook_event_name === "PreToolUse");
    const expected = input.hook_event_name === "PreToolUse" ? entry.input :
      (entry.nativeInput ?? entry.input);
    if (!isDeepStrictEqual(input.tool_input, expected))
      throw new Error("AW rejected changed native Hook input");
    return entry;
  }

  payload(entry, input) {
    return { ...structuredClone(input), tool_use_id: entry.id,
      tool_input: structuredClone(entry.input),
      aw_native_tool_use_id: entry.native, aw_parent_tool_use_id: entry.parent ?? null };
  }

  terminal(native) {
    const entry = this.active.get(native);
    if (!entry || entry.terminal) return;
    // Result receipt fences new authorization while the native queue barrier and
    // original callbacks still retain this identity for Post and finalizer drain.
    entry.terminal = true;
    if (entry.producer) throw new Error("AW native child result preceded its producer");
    for (const child of this.active.values()) {
      if (child.parentEntry === entry && child.producer) {
        this.onFailure(new Error("AW native parent retired before its child producer"));
        break;
      }
    }
  }

  async retire(native) {
    const entry = this.active.get(native);
    if (!entry) return;
    this.terminal(native);
    entry.phase = "retiring";
    await entry.admitted;
    await Promise.allSettled([...entry.pending]);
    await this.beforeRetire?.(entry);
    await this.scope.finish(entry.id);
    this.active.delete(native);
  }
}
