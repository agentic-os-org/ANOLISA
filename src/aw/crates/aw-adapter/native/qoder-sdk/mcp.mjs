/** Native MCP dispatch owns a child invocation; schemas and result envelopes stay native. */
import { AsyncLocalStorage } from "node:async_hooks";
import { randomUUID } from "node:crypto";
import { isDeepStrictEqual } from "node:util";
import { invoke } from "./callback.mjs";
import { postTools } from "./post.mjs";

const servers = new WeakMap();
const current = new AsyncLocalStorage();

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
      return ticket.resident.track(() => {
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

export function admitMcp(options) {
  const admitted = new Map();
  for (const [name, server] of Object.entries(options.mcpServers ?? {})) {
    if (server.type !== "sdk") continue;
    const record = servers.get(server);
    if (!record || record.claimed || !/^[A-Za-z0-9_-]+$/.test(name))
      throw new Error("AW in-process MCP requires a fresh managed server per Query");
    admitted.set(name, record);
  }
  for (const record of admitted.values()) record.claimed = true;
  return admitted;
}

export function delegatesMcp(resident, input) {
  const context = input.mcp_context;
  return !!context && resident.mcp.get(context.server_name)?.names.has(context.tool_name);
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
  const input = request.message.params?.arguments ?? {};
  if (!server?.names.has(name) || !input || Array.isArray(input) || typeof input !== "object")
    throw new Error("AW rejected unknown MCP server/tool dispatch");
  resident.scope.checkExecution();
  const id = randomUUID();
  const tool = `mcp__${request.server_name}__${name}`;
  const generation = resident.scope.execution;
  const entry = { pending: new Set() };
  const ticket = { resident, entry, id, tool, generation, definition: server.definitions.get(name), signal,
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
    const response = structuredClone(await current.run(ticket, () => original(frame, signal)));
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
