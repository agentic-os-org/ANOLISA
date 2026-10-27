/** Synthetic native transport exposes deterministic lifecycle barriers to the real AW service. */
import { query as nativeQuery } from "./fixture-sdk.mjs";
export { runFixture } from "./fixture-sdk.mjs";
export const state = { started: 0, stopped: 0, closing: null, query: null };
export async function startDaemon() {
  state.started++;
  return { async stop() { state.stopped++; } };
}
export function createSdkMcpServer(options) { return { type: "sdk", ...options }; }
export function query(input) {
  const query = nativeQuery(input);
  query.options = input.options;
  const close = query.close.bind(query);
  query.close = async () => { await state.closing?.(); await close(); };
  query.processControlRequest = async (frame, signal) => {
    const request = frame.request;
    const server = input.options.mcpServers[request.server_name];
    const tool = server.tools.find(tool => tool.name === request.message.params.name);
    return { mcp_response: await tool.handler(request.message.params.arguments, { signal }) };
  };
  const dispatch = query.handleControlRequest.bind(query);
  query.handleControlRequest = frame => frame.request.subtype === "mcp_message" ?
    query.processControlRequest(frame, new AbortController().signal) : dispatch(frame);
  state.query = query;
  return query;
}
