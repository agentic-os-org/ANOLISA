/** Correlate native model frames and control dispatch without trusting caller Hook input. */
import { AsyncLocalStorage } from "node:async_hooks";
import { randomUUID } from "node:crypto";
import { isDeepStrictEqual } from "node:util";

export class Identities {
  constructor(scope) {
    this.scope = scope;
    this.active = new Map();
    this.dispatch = new AsyncLocalStorage();
    this.pending = new Set();
    this.session = undefined;
  }

  produce(message) {
    if (message.type !== "assistant") return;
    this.scope.checkExecution();
    if (!message.session_id || (this.session && this.session !== message.session_id))
      throw new Error("AW native Query session identity changed");
    this.session = message.session_id;
    for (const block of message.message?.content ?? []) {
      if (block.type !== "tool_use") continue;
      if (typeof block.id !== "string" || !block.id || this.active.has(block.id) ||
          typeof block.name !== "string" || !block.name || !block.input ||
          Array.isArray(block.input) || typeof block.input !== "object" ||
          this.active.size >= this.scope.invocationLimit)
        throw new Error("AW rejected invalid or duplicate native model invocation");
      const entry = { id: randomUUID(), native: block.id, name: block.name,
        input: structuredClone(block.input), parent: message.parent_tool_use_id,
        callbacks: new Set(), phase: "issued", pending: new Set() };
      this.active.set(block.id, entry);
      entry.admitted = this.scope.register(entry.id, entry.name, entry.input);
      entry.admitted.catch(() => { this.scope.failed = true; });
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

  async claim(input, id, callback) {
    const ticket = this.dispatch.getStore();
    const entry = ticket?.entry;
    if (!entry || ticket.kind !== "hook_callback" || id !== entry.native ||
        input.tool_use_id !== id || input.session_id !== this.session ||
        input.tool_name !== entry.name || !isDeepStrictEqual(input.tool_input, entry.input) ||
        entry.callbacks.has(callback) || this.active.get(id) !== entry)
      throw new Error("AW rejected stale, repeated or changed native Hook input");
    this.scope.checkExecution();
    await entry.admitted;
    entry.callbacks.add(callback);
    return entry;
  }

  payload(entry, input) {
    return { ...structuredClone(input), tool_use_id: entry.id,
      aw_native_tool_use_id: entry.native, aw_parent_tool_use_id: entry.parent ?? null };
  }

  async retire(native) {
    const entry = this.active.get(native);
    if (!entry) return;
    await entry.admitted;
    await Promise.allSettled([...entry.pending]);
    await this.scope.finish(entry.id);
    this.active.delete(native);
  }
}
