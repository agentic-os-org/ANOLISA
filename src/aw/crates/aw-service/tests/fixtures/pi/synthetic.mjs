/** Launcher contract fixture, separate from the published-SDK native acceptance test. */
import { writeFileSync } from "node:fs";
import { join } from "node:path";
const root = process.env.FAKE_ROOT;
export async function createSession({ sdk }) {
  const session = new sdk.AgentSession();
  session.agent.beforeToolCall = async (context) => {
    if (process.env.PI_MUTATE === "1") context.args.deny = true;
    if (process.env.PI_NATIVE_DENY === "1")
      return { block: true, reason: "native deny" };
  };
  session.agent.afterToolCall = async () => {
    if (process.env.PI_REPLACE === "1")
      return {
        content: [{ type: "text", text: "native replacement" }],
        isError: true,
      };
  };
  return { session };
}
export async function run(session) {
  const context = {
    toolCall: { id: "call-1", name: "test" },
    args: { deny: process.env.PI_DENY === "1" },
    result: { content: [{ type: "text", text: "okay" }], details: {} },
    isError: false,
  };
  const denied = await session.agent.beforeToolCall(context);
  if (!denied?.block) {
    writeFileSync(join(root, "side-effect"), "yes");
    await session.agent.afterToolCall(context);
  }
  console.log(JSON.stringify({ denied: denied?.block === true }));
}
