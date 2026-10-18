/** Deterministic callback contract fixture; native headless acceptance is separate. */
import { readFileSync } from "node:fs";
import { pathToFileURL } from "node:url";

const args = process.argv.slice(2);
const patch = JSON.parse(
  readFileSync(args[args.lastIndexOf("--patch") + 1], "utf8"),
);
if (!patch[1].inject.includes("awDeepseek"))
  throw Error("missing runner dependency");
const plugin = await import(pathToFileURL(patch[0].insert[0].name));
const listeners = new Map();
const guards = new Set();
const effects = [];
let ready = false;
const ctx = {
  effect(setup) {
    effects.push(setup());
  },
  on(event, callback, options) {
    const callbacks = listeners.get(event) ?? [];
    callbacks[options?.prepend ? "unshift" : "push"](callback);
    listeners.set(event, callbacks);
    return () => callbacks.splice(callbacks.indexOf(callback), 1);
  },
  tools: {
    guard(callback) {
      guards.add(callback);
      return () => guards.delete(callback);
    },
  },
  provide(name) {
    if (name !== "awDeepseek") throw Error(name);
    ready = true;
  },
};
plugin.apply(ctx);
if (!ready) throw Error("unready runner");
const mode = process.env.DSH_TEST_MODE ?? "allow";
const waterfall = async (event, args, last) => {
  const callbacks = [...(listeners.get(event) ?? [])];
  const next = () => (callbacks.shift() ?? last)(...args, next);
  return await next();
};
const controller = new AbortController();
const exec = {
  token: Symbol(),
  agent: { id: "session" },
  callId: "call",
  name: "bash",
  arguments: { command: mode === "deny" ? "forbidden" : "echo okay" },
  signal: controller.signal,
};
const pre =
  mode === "bypass"
    ? { kind: "allow" }
    : await waterfall("tools/pre-execute", [exec], () => ({ kind: "allow" }));
const reason =
  pre.kind === "deny"
    ? pre.reason
    : [...guards].map((guard) => guard(exec)).find(Boolean);
const result = {
  content: [{ type: "text", text: reason ?? "original" }],
  isError: Boolean(reason),
  ...(!reason ? { value: "canonical", meta: { kept: true } } : {}),
};
const decision = await waterfall("tools/post-execute", [exec, result], () => ({
  kind: "accept",
}));
const final = {
  ...result,
  ...(decision.content ? { content: decision.content } : {}),
};
for (const callback of listeners.get("tools/result") ?? [])
  callback(exec, final);
console.log(JSON.stringify({ executed: !reason, result: final }));
for (const dispose of effects.reverse()) dispose();
if (
  guards.size ||
  [...listeners.values()].some((callbacks) => callbacks.length)
)
  throw Error("hooks leaked after disposal");
