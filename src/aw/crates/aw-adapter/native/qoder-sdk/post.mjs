/** Required Post failures close the native Query before any callback result is released. */
import { invoke } from "./callback.mjs";

export async function postTools(resident, payload, signal) {
  for (const hook of resident.config.hooks.filter(hook => hook.event === "tool.after")) {
    try {
      await invoke({ ...resident.config, scope: resident.scope }, hook, payload, signal);
    } catch (error) {
      process.stderr.write(`AW tool.after/${hook.step} failed: ${error.message}; on_error=${hook.on_error}\n`);
      if (hook.on_error === "withhold_result") await resident.withhold(error);
    }
  }
}
