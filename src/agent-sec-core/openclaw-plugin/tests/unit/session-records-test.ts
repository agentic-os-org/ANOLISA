import assert from "node:assert/strict";
import { it } from "node:test";
import { readSessionRecords } from "../e2e/pilot/gateway-probes.mjs";

it("reads policy evidence through the host API without a transcript file", async () => {
  const message = {
    role: "toolResult",
    isError: true,
    content: [{ type: "text", text: "Plugin approval required" }],
  };
  const records = await readSessionRecords({
    sessionKey: "agent:main:policy-test",
    gatewayToken: "test-token",
    gatewayUrl: "ws://127.0.0.1:1234",
    callGatewayRpc: async (_step: string, method: string, params: unknown, options: unknown) => {
      assert.equal(method, "sessions.get");
      assert.deepEqual(params, { key: "agent:main:policy-test", limit: 200 });
      assert.deepEqual(options, {
        gatewayToken: "test-token",
        gatewayUrl: "ws://127.0.0.1:1234",
      });
      return { ok: true, payload: { messages: [message] } };
    },
  });
  assert.deepEqual(records, [{ type: "message", message }]);
});

it("rejects malformed transcript responses instead of hiding missing evidence", async () => {
  await assert.rejects(
    readSessionRecords({ callGatewayRpc: async () => ({}) }),
    /sessions.get did not return a messages array/,
  );
});
