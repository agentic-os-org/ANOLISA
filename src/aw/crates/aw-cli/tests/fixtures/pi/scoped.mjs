/** Preserve the native callback result through a mismatched final compression step. */
import { writeFileSync } from "node:fs";
import { join } from "node:path";

const nativeResult = {
  content: [{ type: "text", text: "native result", metadata: { retained: true } }],
  details: { exitCode: 17, source: "native" },
  usage: { units: 3 },
  terminate: false,
  isError: process.env.PI_SCOPE_ERROR === "1",
};

export async function createSession({ sdk }) {
  const session = new sdk.AgentSession();
  session.agent.beforeToolCall = async () => {
    if (process.env.PI_NATIVE_DENY === "1")
      return { block: true, reason: "native permission denied" };
  };
  session.agent.afterToolCall = async () => structuredClone(nativeResult);
  return { session };
}

export async function run(session) {
  const context = {
    toolCall: { id: "call-1", name: "test" },
    args: { command: "fixture" },
    result: { content: [{ type: "text", text: "original" }], details: {} },
    isError: false,
  };
  const denied = await session.agent.beforeToolCall(context);
  let result;
  if (!denied?.block) {
    writeFileSync(join(process.env.FAKE_ROOT, "side-effect"), "yes");
    result = await session.agent.afterToolCall(context);
  }
  console.log(JSON.stringify({ denied: denied?.block === true, result, expected: nativeResult }));
}
