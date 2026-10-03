import assert from "node:assert/strict";
import { it } from "node:test";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { startMockModelServer } from "../e2e/pilot/mock-model.mjs";

it("uses the full conversation when OpenClaw appends user-role metadata", async () => {
  const logsDir = await fs.mkdtemp(path.join(os.tmpdir(), "agentsec-mock-"));
  const model = await startMockModelServer({
    logsDir,
    registerServer: () => {},
  });
  try {
    for (const stream of [false, true]) {
      const request = async (messages: unknown[]) => {
        const res = await fetch(`${model.baseUrl}/v1/chat/completions`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ stream, messages }),
        });
        assert.equal(res.status, 200);
        return res.text();
      };
      const prompt = {
        role: "user",
        content: "agent-sec pilot model-driven safe exec: call exec",
      };
      const metadata = {
        role: "user",
        content: [{ type: "text", text: 'Sender (untrusted metadata):\n```json\n{"label":"cli","id":"cli"}\n```' }],
      };
      const safe = await request([prompt, metadata]);
      assert.match(safe, /tool_calls/);
      assert.match(safe, /printf agent-sec-pilot-safe/);
      const denied = await request([
        { role: "user", content: "[agent-sec-policy-matrix code-deny] call exec" },
        metadata,
      ]);
      assert.match(denied, /printf agent-sec-policy-matrix-code-deny/);
      const completed = await request([
        prompt,
        { role: "assistant", content: "calling exec" },
        { role: "tool", content: "agent-sec-pilot-safe" },
        metadata,
      ]);
      assert.match(completed, /observed through gateway exec/);
      assert.doesNotMatch(completed, /tool_calls/);
    }
  } finally {
    await new Promise<void>((resolve, reject) =>
      model.server.close((error: Error | undefined) =>
        error ? reject(error) : resolve(),
      ),
    );
    await fs.rm(logsDir, { recursive: true, force: true });
  }
});
