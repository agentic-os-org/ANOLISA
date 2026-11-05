/** Native MCP dispatch owns a child invocation; schemas and result envelopes stay native. */
import { AsyncLocalStorage } from "node:async_hooks";
import { randomUUID } from "node:crypto";
import { isDeepStrictEqual } from "node:util";
import { invoke } from "./callback.mjs";
import { postTools } from "./post.mjs";

const servers = new WeakMap();
const current = new AsyncLocalStorage();
const deferred = () => {
  let resolve, reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  promise.catch(() => {});
  return { promise, resolve, reject };
};

export function managedMcpServer(sdk, options) {
  const { parentIdentity = "query", ...nativeOptions } = options;
  if (!["query", "required"].includes(parentIdentity))
    throw new Error("Invalid managed MCP parentIdentity");
  // SDK 1.0.56 / Worker 1.1.78 omit model identity from mcp_message.
  // Reject the stronger contract before creating a native server or Query.
  if (parentIdentity === "required")
    throw new Error("Pinned Qoder MCP dispatch lacks a trusted model parent identity");
  const names = new Set();
  const definitions = new Map();
  const tools = (options.tools ?? []).map(definition => {
    if (!definition.name || names.has(definition.name)) throw new Error("Duplicate managed MCP tool");
    names.add(definition.name);
    const identity = {};
    const handler = definition.handler;
    if (typeof handler !== "function") throw new Error("Managed MCP tool requires a handler");
    definitions.set(definition.name, identity);
    return { ...definition, handler: (...args) => {
      const ticket = current.getStore();
      if (!ticket || ticket.definition !== identity)
        throw new Error("AW MCP handler lacks native dispatch identity");
      ticket.resident.scope.checkExecution(ticket.generation);
      if (ticket.entered) throw new Error("AW MCP handler invocation was already consumed");
      ticket.entered = true;
      if (!isDeepStrictEqual(args[0], ticket.input))
        throw new Error("AW MCP schema changed the approved tool arguments");
      // A schema may retain parsed references. Capture the consumer input before
      // tracking or credential acquisition yields to application code.
      const input = structuredClone(args[0]);
      return ticket.resident.track(async () => {
        await ticket.resident.scope.validate();
        ticket.resident.scope.checkExecution(ticket.generation);
        if (ticket.signal?.aborted) throw new Error("AW MCP dispatch was cancelled before handler entry");
        if (ticket.resident.executionOwner)
          return ticket.resident.executionOwner.run({ invocationId: ticket.id,
            toolName: ticket.tool, input }, (approved, { signal }) =>
            handler(approved.input, { ...args[1], signal }), { signal: ticket.signal });
        return handler(input, ...args.slice(1));
      }, ticket.entry);
    } };
  });
  const server = sdk.createSdkMcpServer({ ...nativeOptions, tools });
  servers.set(server, { names, definitions, claimed: false });
  return server;
}

// Native schema parsing precedes registered handlers. Check parsed input here
// while keeping Post on the native response after output validation.
function nativeServer(server) {
  const instance = server.instance;
  if (!instance?._registeredTools || typeof instance._registeredTools !== "object" ||
      typeof instance.executeToolHandler !== "function")
    throw new Error("AW native MCP lacks the registered-handler boundary");
  const execute = instance.executeToolHandler;
  const record = { native: true, instance };
  // Guard the actual executor: native dispatch can select a replacement after
  // tools/call admission, and input schema validation can also yield.
  Object.defineProperty(instance, "executeToolHandler", { value: function (definition, args, extra) {
    const ticket = current.getStore();
    if (!ticket) return execute.call(this, definition, args, extra);
    if (ticket.native !== record || ticket.definition !== definition || ticket.entered)
      throw new Error("AW native MCP handler identity changed");
    if (typeof definition.handler !== "function")
      throw new Error("AW native MCP requires a handler");
    ticket.entered = true;
    // Preserve native zero-input callback signatures and pin the selected handler
    // while asynchronous policy checks run.
    const selected = { ...definition };
    const input = selected.inputSchema ? structuredClone(args ?? {}) : {};
    ticket.work = ticket.resident.track(() => ticket.invoke(input, async () => ({ mcp_response: {
      jsonrpc: "2.0", id: ticket.messageId,
      result: await execute.call(this, selected, selected.inputSchema ? input : args, extra),
    } })));
    ticket.work.catch(error => ticket.handler.reject(error));
    return ticket.handler.promise;
  } });
  servers.set(server, record);
  return record;
}

export function admitMcp(options, nativeConfiguration = false) {
  const admitted = new Map();
  for (const [name, server] of Object.entries(options.mcpServers ?? {})) {
    if (server.type !== "sdk") continue;
    const record = servers.get(server) ?? (nativeConfiguration ? nativeServer(server) : undefined);
    if (!record || (record.claimed && !record.native) || !/^[A-Za-z0-9_-]+$/.test(name))
      throw new Error("AW in-process MCP requires a fresh managed server per Query");
    admitted.set(name, record);
  }
  for (const record of admitted.values()) if (!record.native) record.claimed = true;
  return admitted;
}

export function delegatesMcp(resident, input) {
  const context = input.mcp_context;
  const server = context && resident.mcp.get(context.server_name);
  return !!server && (server.native || server.names.has(context.tool_name));
}

export async function dispatchMcp(resident, frame, signal, original) {
  let request = frame.request;
  const message = request.message;
  if ((request.subtype ?? request.type) !== "mcp_message" || message?.method !== "tools/call")
    return await original(frame, signal);
  const native = resident.identities.dispatch.getStore();
  if (!native || native.kind !== "mcp_message" || native.id !== frame.request_id ||
      !isDeepStrictEqual(native.request, request))
    throw new Error("AW MCP call lacks trusted native control dispatch");
  request = structuredClone(native.request);
  frame = { ...frame, request };
  const server = resident.mcp.get(request.server_name);
  const name = request.message.params?.name;
  if (server?.native) {
    const definition = server.instance._registeredTools[name];
    if (!definition || typeof definition.handler !== "function")
      throw new Error("AW rejected unknown native MCP tool");
    const ticket = { resident, native: server, definition, entered: false, messageId: message.id,
      handler: deferred(), response: deferred() };
    ticket.invoke = (input, handler) => invokeMcp(resident, frame, signal, server, name, input, handler, ticket);
    try {
      const response = await current.run(ticket, () => original(frame, signal));
      ticket.response.resolve(response);
      if (ticket.work) await ticket.work;
      else {
        // Only native rejections before handler entry lack an admitted invocation.
        const envelope = response?.mcp_response;
        if (ticket.entered || !(envelope?.error || envelope?.result?.isError))
          throw new Error("AW native MCP dispatch skipped handler admission");
        return await invokeMcp(resident, frame, signal, server, name,
          request.message.params?.arguments ?? {}, async () => response);
      }
      return response;
    } catch (error) {
      ticket.response.reject(error);
      throw error;
    }
  }

  const input = request.message.params?.arguments ?? {};
  if (!server?.names.has(name)) throw new Error("AW rejected unknown MCP server/tool dispatch");
  return await invokeMcp(resident, frame, signal, server, name, input, () => original(frame, signal));
}

async function invokeMcp(resident, frame, signal, server, name, input, original, nativeTicket) {
  const request = frame.request;
  if (!input || Array.isArray(input) || typeof input !== "object")
    throw new Error("AW rejected invalid MCP handler input");
  resident.scope.checkExecution();
  const id = randomUUID();
  const tool = `mcp__${request.server_name}__${name}`;
  const generation = resident.scope.execution;
  const entry = { pending: new Set() };
  const ticket = { resident, entry, id, tool, generation, definition: server.definitions?.get(name), signal,
    input: structuredClone(input), entered: false };
  const payload = { session_id: resident.identities.session ?? `query-${resident.id}`,
    tool_use_id: id, tool_name: tool, tool_input: structuredClone(input), cwd: resident.cwd,
    aw_native_request_id: frame.request_id, aw_parent_query_id: resident.id,
    aw_mcp_context: { server: request.server_name, tool: name, subject: tool,
      parent_tool_call_id: null, parent_query_id: resident.id,
      generation, identity_scope: "query" },
    mcp_context: { server_name: request.server_name, tool_name: name } };
  await resident.scope.register(id, tool, input);
  try {
    for (const hook of resident.config.hooks.filter(hook => hook.event === "tool.before")) {
      let result;
      try {
        result = await invoke({ ...resident.config, scope: resident.scope }, hook,
          { ...structuredClone(payload), hook_event_name: "PreToolUse" }, signal);
      } catch (error) {
        if (hook.on_error === "block") throw error;
      }
      if (result?.hookSpecificOutput?.permissionDecision === "deny")
        throw new Error(result.hookSpecificOutput.permissionDecisionReason);
    }
    resident.scope.checkExecution(generation);
    if (signal?.aborted) throw new Error("AW MCP dispatch was cancelled before execution");
    let response;
    if (nativeTicket) {
      try {
        const result = await current.run(ticket, () => original());
        nativeTicket.handler.resolve(result.mcp_response.result);
      } catch (error) { nativeTicket.handler.reject(error); }
      // Native output-schema validation and exception conversion happen after
      // the handler returns. Only that final envelope is eligible for Post.
      response = structuredClone(await nativeTicket.response.promise);
    } else response = structuredClone(await current.run(ticket, () => original()));
    await Promise.allSettled([...entry.pending]);
    resident.scope.checkExecution(generation);
    if (signal?.aborted) throw new Error("AW MCP dispatch was cancelled before delivery");
    await postTools(resident, { ...structuredClone(payload), hook_event_name: "PostToolUse",
      tool_response: structuredClone(response.mcp_response) }, signal);
    return response;
  } finally {
    // SDK cancellation settles its MCP waiter before an application handler's finally.
    await Promise.allSettled([...entry.pending]);
    await resident.scope.finish(id);
  }
}
