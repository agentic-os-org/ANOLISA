/** Pin the published agent-loop commit boundary before any Agent code loads. */
import { installVerifiedPatch, removeOwnPreload } from "../shared/js-runtime/module-patch.mjs";

const expected = "cf57e3ecd487ac5c2553455aaa5cef2c6fb41e6bd61be5aa455675d1d7193033";
const anchor = "const result = slot.needsPost ? await ctx.tools[TOOL_RUNTIME_SCHEDULER].finalize(slot.exec, slot.result) : ctx.tools[TOOL_RUNTIME_SCHEDULER].finish(slot.exec, slot.result);";
const gate = Symbol.for("aw.deepseek.finalGate");
const patched = Symbol.for("aw.deepseek.finalPatched");

globalThis[gate] = async () => {
  throw Error("AW final result gate is not ready");
};
installVerifiedPatch({
  target: "/node_modules/@deepseek-ai/dsh-agent-loop/lib/index.js",
  digest: expected,
  anchor,
  replacement: `${anchor.replace("const result", "let result")}\n\t\t\tresult = await globalThis[Symbol.for("aw.deepseek.finalGate")](slot.exec, result);`,
  onPatched: () => { globalThis[patched] = true; },
});

// NODE_OPTIONS is inherited by tools and Providers. Keep this loader in the DSH process only.
removeOwnPreload(import.meta.url);
