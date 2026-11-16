/** Run after independently starting AW with an sdk/application target named app. */
import { Instance } from "@anolisa/aw-sdk";
const capabilities = {
  adapter: "sdk",
  version: "aw-hook-source/v1",
  entrypoint: "application",
  events: { "tool.before": ["observe", "block"], "tool.after": ["observe"] },
};
const instance = new Instance({
  endpoint: process.env.AW_SOCKET,
  target: "app",
  availability: "required",
  capabilities,
  install: (routes) =>
    routes.hooks.every(
      (hook) =>
        Object.hasOwn(capabilities.events, hook.event) && !hook.replace_result,
    ),
});
let task;
try {
  await instance.connect();
  task = await instance.task();
  const invocation = {
    session_id: "example",
    call_id: "1",
    name: "echo",
    input: { text: "hello" },
  };
  const decision = await task.emit("tool.before", invocation);
  if (decision.decision === "block") throw new Error("AW denied this tool");
  const result = invocation.input.text;
  console.log(result);
  await task.emit("tool.after", { ...invocation, result });
} finally {
  await task?.close();
  await instance.close();
}
