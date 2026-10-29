/** Real pinned SDK parser with a memory transport and a stubbed AW service scope. */
import assert from "node:assert/strict";
import { pathToFileURL } from "node:url";
import { createRequire } from "node:module";
import { Resident } from "../../../../aw-adapter/native/qoder-sdk/resident.mjs";
import { admitMcp } from "../../../../aw-adapter/native/qoder-sdk/mcp.mjs";
import { invoke } from "../../../../aw-adapter/native/qoder-sdk/callback.mjs";
export const sdk = await import(pathToFileURL(process.argv[2]).href);
export const { z } = createRequire(pathToFileURL(process.argv[2]))("zod");
export const pause = () => new Promise(resolve => setImmediate(resolve));
export const deferred = () => {
  let resolve;
  const promise = new Promise(done => { resolve = done; });
  return { promise, resolve };
};

class Transport {
  messages = [];
  responses = new Map();
  changed = deferred();
  closed = false;
  hooks = [];
  requests = [];
  async initialize() {}
  isReady() { return !this.closed; }
  emit(message) { this.messages.push(message); this.changed.resolve(); }
  write(line) {
    const message = JSON.parse(line);
    if (message.type === "control_request") {
      this.requests.push(message);
      if (message.request.type === "initialize") {
        this.initialization = message.request;
        this.hooks = message.request.hooks;
      }
      if (message.request.type === "get_context_usage" && this.holdBarrier) {
        this.barrier = message;
        return;
      }
      if (message.request.type === "set_permission_mode" && this.holdPermission) {
        this.permissionRequest = message;
        return;
      }
      this.emit({ type: "control_response", response: { subtype: "success",
        request_id: message.request_id, response: { capabilities: ["msg_lifecycle_v1", "host_action"] } } });
    } else if (message.type === "control_response") {
      this.responses.set(message.response.request_id, message.response);
    }
  }
  async *readMessages() {
    while (!this.closed) {
      if (!this.messages.length) { await this.changed.promise; this.changed = deferred(); }
      if (this.failure) throw this.failure;
      while (this.messages.length) yield this.messages.shift();
    }
  }
  endInput() { this.inputEnds = (this.inputEnds ?? 0) + 1; }
  close() { this.closed = true; this.changed.resolve(); }
}

export async function fixture(t, callback = async () => ({}), extra = {}, policyHooks, configuration = {}, source) {
  const transport = new Transport();
  const resident = new Resident({ hooks: policyHooks ?? [], sdk: process.argv[2], ...configuration });
  const scope = resident.scope;
  scope.open = async () => { scope.handle = {}; };
  scope.invocationLimit = 4;
  scope.register = async (id, name, input) => { scope.invocations.set(id, { name, input }); };
  scope.finish = async id => { assert(scope.invocations.delete(id)); };
  scope.close = async () => { scope.closed = true; };
  const input = deferred();
  async function* prompt() { await input.promise; }
  const options = { transport: { create: () => transport },
    auth: { type: "accessToken", accessToken: "fixture-local-only" }, ...extra,
    hooks: { PreToolUse: [{ hooks: [callback] }], PostToolUse: [{ hooks: [callback] }] } };
  let composedOptions;
  if (policyHooks) {
    await resident.create(source ?? prompt(), options, (config, native) => {
      composedOptions = native;
      const hooks = Object.fromEntries(Object.entries(native.hooks).map(([event, groups]) => [event, [...groups]]));
      for (const hook of config.hooks) {
        const event = hook.event === "tool.before" ? "PreToolUse" : "PostToolUse";
        hooks[event].push({ hooks: [async (input, id, { signal }) => {
          assert.equal(id, input.tool_use_id);
          return await invoke(config, hook, input, signal);
        }] });
      }
      return { ...native, hooks };
    }, () => {});
  } else {
    resident.mcp = admitMcp(extra);
    resident.cwd = process.cwd();
    await scope.open();
    scope.begin();
    const query = sdk.query({ prompt: source ?? prompt(), options: resident.decorate(options) });
    resident.install(query);
    await query.initializationResult();
  }
  t.after(async () => { input.resolve(); await resident.close(); });
  const model = (id, input = { command: "fixture" }, name = "Bash", parent = null) => transport.emit({ type: "assistant",
    session_id: "session", uuid: `message-${id}`, parent_tool_use_id: parent,
    message: { role: "assistant", content: [{ type: "tool_use", id, name, input }] } });
  const hook = (id, request, event = "PreToolUse", input = { command: "fixture" },
    { name = "Bash", mcpContext, group = 0, session = "session", permissionMode = extra.permissionMode ?? "default" } = {}) => {
    const callbackId = transport.hooks[event][group].hookCallbackIds[0];
    transport.emit({ type: "control_request", request_id: request,
      request: { subtype: "hook_callback", callback_id: callbackId, tool_use_id: id,
        input: { hook_event_name: event, tool_use_id: id, tool_name: name, tool_input: input,
          permission_mode: permissionMode,
          ...(mcpContext ? { mcp_context: mcpContext } : {}),
          cwd: process.cwd(), session_id: session, tool_response: "result" } } });
  };
  const response = async request => {
    for (let attempt = 0; attempt < 1000; attempt++) {
      if (transport.responses.has(request)) return transport.responses.get(request);
      await pause();
    }
    throw new Error(`Native response timeout: ${request}`);
  };
  const parallelHooks = async (id, label, event, input, native = {}) => {
    const groups = transport.hooks[event];
    const requests = groups.map((_group, index) => `${label}-${index}`);
    groups.forEach((_group, group) => hook(id, requests[group], event, input, { ...native, group }));
    return await Promise.all(requests.map(response));
  };
  return { resident, scope, composedOptions, query: resident.facade(), transport, model, hook, response, parallelHooks };
}
