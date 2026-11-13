/** Pin the published agent-loop commit boundary before any Agent code loads. */
import { createHash } from "node:crypto";
import { registerHooks } from "node:module";

const expected = "cf57e3ecd487ac5c2553455aaa5cef2c6fb41e6bd61be5aa455675d1d7193033";
const anchor = "const result = slot.needsPost ? await ctx.tools[TOOL_RUNTIME_SCHEDULER].finalize(slot.exec, slot.result) : ctx.tools[TOOL_RUNTIME_SCHEDULER].finish(slot.exec, slot.result);";
const gate = Symbol.for("aw.deepseek.finalGate");
const patched = Symbol.for("aw.deepseek.finalPatched");

globalThis[gate] = async () => {
  throw Error("AW final result gate is not ready");
};
registerHooks({
  load(url, context, nextLoad) {
    const loaded = nextLoad(url, context);
    if (!url.endsWith("/@deepseek-ai/dsh-agent-loop/lib/index.js")) return loaded;
    const source = loaded.source?.toString();
    if (
      !source ||
      createHash("sha256").update(source).digest("hex") !== expected ||
      source.split(anchor).length !== 2
    ) throw Error("AW final result gate does not support this DSH agent-loop build");
    globalThis[patched] = true;
    return {
      ...loaded,
      source: source.replace(
        anchor,
        `${anchor.replace("const result", "let result")}\n\t\t\tresult = await globalThis[Symbol.for("aw.deepseek.finalGate")](slot.exec, result);`,
      ),
    };
  },
});

// NODE_OPTIONS is inherited by tools and Providers. Keep this loader in the DSH process only.
const option = `--import=${import.meta.url}`;
const options = process.env.NODE_OPTIONS;
if (options === option) {
  delete process.env.NODE_OPTIONS;
} else if (options?.endsWith(` ${option}`)) {
  process.env.NODE_OPTIONS = options.slice(0, -option.length - 1);
} else {
  throw Error("AW final result preload option is missing from NODE_OPTIONS");
}
