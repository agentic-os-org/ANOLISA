/** Native MCP dispatch owns a child invocation; schemas and result envelopes stay native. */
import { AsyncLocalStorage } from "node:async_hooks";
import { randomUUID } from "node:crypto";
import { isDeepStrictEqual } from "node:util";
import { invoke } from "./callback.mjs";

const servers = new WeakMap();
const current = new AsyncLocalStorage();

export function managedMcpServer(sdk, options) {
  const names = new Set();
  const definitions = new Map();
  const tools = (options.tools ?? []).map(definition => {
    if (!definition.name || names.has(definition.name)) throw new Error("Duplicate managed MCP tool");
    names.add(definition.name);
    const identity = {};
    definitions.set(definition.name, identity);
    return { ...definition, handler: (...args) => {
      const ticket = current.getStore();
      if (!ticket || ticket.definition !== identity)
        throw new Error("AW MCP handler lacks native dispatch identity");
      ticket.resident.scope.checkExecution();
      if (ticket.entered) throw new Error("AW MCP handler invocation was already consumed");
      ticket.entered = true;
      if (!isDeepStrictEqual(args[0], ticket.input))
        throw new Error("AW MCP schema changed the approved tool arguments");
      return ticket.resident.track(() => {
        ticket.resident.scope.checkExecution();
        if (ticket.signal?.aborted) throw new Error("AW MCP dispatch was cancelled before handler entry");
        return definition.handler(...args);
      }, ticket.entry);
    } };
  });
  const server = sdk.createSdkMcpServer({ ...options, tools });
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
  const request = frame.request;
  const message = request.message;
  if ((request.subtype ?? request.type) !== "mcp_message" || message?.method !== "tools/call")
    return await original(frame, signal);
  const server = resident.mcp.get(request.server_name);
  const name = message.params?.name;
  const input = message.params?.arguments ?? {};
  if (!server?.names.has(name) || !input || Array.isArray(input) || typeof input !== "object")
    throw new Error("AW rejected unknown MCP server/tool dispatch");
  resident.scope.checkExecution();
  const id = randomUUID();
  const tool = `mcp__${request.server_name}__${name}`;
  const entry = { pending: new Set() };
  const ticket = { resident, entry, definition: server.definitions.get(name), signal,
    input: structuredClone(input), entered: false };
  const payload = { session_id: resident.identities.session ?? `query-${resident.id}`,
    tool_use_id: id, tool_name: tool, tool_input: structuredClone(input), cwd: resident.cwd,
    aw_native_request_id: frame.request_id, aw_parent_query_id: resident.id,
    mcp_context: { server_name: request.server_name, tool_name: name } };
  await resident.scope.register(id, tool, input);
  try {
    for (const hook of resident.config.hooks.filter(hook => hook.event === "tool.before")) {
      let result;
      try {
        result = await invoke({ ...resident.config, scope: resident.scope }, hook,
          { ...payload, hook_event_name: "PreToolUse" }, signal);
      } catch (error) {
        if (hook.on_error === "block") throw error;
      }
      if (result?.hookSpecificOutput?.permissionDecision === "deny")
        throw new Error(result.hookSpecificOutput.permissionDecisionReason);
    }
    const response = await current.run(ticket, () => original(frame, signal));
    await Promise.allSettled([...entry.pending]);
    for (const hook of resident.config.hooks.filter(hook => hook.event === "tool.after")) {
      try {
        await invoke({ ...resident.config, scope: resident.scope }, hook,
          { ...payload, hook_event_name: "PostToolUse", tool_response: response.mcp_response }, signal);
      } catch (error) {
        if (hook.on_error === "block") { resident.fail(error); throw error; }
      }
    }
    return response;
  } finally {
    // SDK cancellation settles its MCP waiter before an application handler's finally.
    await Promise.allSettled([...entry.pending]);
    await resident.scope.finish(id);
  }
}
