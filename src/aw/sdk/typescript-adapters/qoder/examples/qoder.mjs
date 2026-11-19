import { createQoderBridge } from "@anolisa/aw-sdk-qoder";
import { query } from "@qoder-ai/qoder-agent-sdk";

// Keep the application's native daemon/authentication, options and Hooks.
const aw = await createQoderBridge({
  configFile: process.env.AW_CONFIG,
  query,
  availability: "native_fallback",
});
try {
  const session = aw.query({
    prompt: "Describe the current project.",
    options: { cwd: process.cwd(), maxTurns: 2 },
  });
  for await (const message of session)
    if (message.type === "result") console.log(message);
} finally {
  await aw.close();
}
