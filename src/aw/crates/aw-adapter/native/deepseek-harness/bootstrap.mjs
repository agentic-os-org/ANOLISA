/** Install the managed profile's verified gates before Cordis loads. */
import { readFileSync } from "node:fs";
import { removeOwnPreload } from "../shared/js-runtime/module-patch.mjs";

const config = JSON.parse(readFileSync(process.env.AW_NATIVE_CONFIG, "utf8"));
if (config.result_gate) await import("./result-preload.mjs");
if (config.hooks.some(hook => hook.event === "security.violation"))
  await import("./final-preload.mjs");
removeOwnPreload(import.meta.url);
