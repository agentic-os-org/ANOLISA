import { Instance } from "@anolisa/aw-sdk";
import { qoderOptions } from "@anolisa/aw-sdk/qoder";
import { query } from "@qoder-ai/qoder-agent-sdk";

// Start AW independently and select its private Unix socket explicitly.
const instance = new Instance({
  endpoint: process.env.AW_SOCKET,
  target: "qoder",
  availability: "native_fallback",
  capabilities: {
    adapter: "qoder-sdk",
    version: "public-hooks/v1",
    entrypoint: "sdk-worker",
    events: {
      "input.submit": ["observe", "block"],
      "tool.before": ["observe", "block", "replace_input"],
      "tool.after": ["observe", "replace_result"],
    },
  },
  // This application routes every Query through qoderOptions below.
  install: async (routes) =>
    routes.hooks.every((hook) =>
      ["input.submit", "tool.before", "tool.after"].includes(hook.event),
    ),
});
let task, session;
try {
  await instance.connect();
  task = await instance.task();
  const attached = qoderOptions(task, { cwd: process.cwd(), maxTurns: 2 });
  session = attached.bind(
    query({
      prompt: "Describe the current project.",
      options: attached.options,
    }),
  );
  for await (const message of session)
    if (message.type === "result") console.log(message);
  if (attached.failure) throw attached.failure;
} finally {
  await session?.close();
  await task?.close();
  await instance.close();
}
