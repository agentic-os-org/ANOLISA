/** Register AW on each Query before initialization; the caller owns its daemon and queries. */
import { readFileSync, writeFileSync, renameSync, unlinkSync } from "node:fs";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { isDeepStrictEqual } from "node:util";
import { invoke } from "./callback.mjs";
import { Reload } from "./reload.mjs";

const deny = (reason) => ({ hookSpecificOutput: { hookEventName: "PreToolUse",
  permissionDecision: "deny", permissionDecisionReason: reason } });
const copy = (value) => JSON.parse(JSON.stringify(value));
const daemons = new WeakSet();

function configuration() {
  const config = JSON.parse(readFileSync(process.env.AW_QODER_SDK_CONFIG, "utf8"));
  for (const key of ["sdk", "executable", "binding", "cwd", "ready_path", "ready_token"])
    if (typeof config[key] !== "string" || !config[key]) throw new Error(`Missing AW ${key}`);
  if (!Array.isArray(config.hooks)) throw new Error("Missing AW hooks");
  for (const hook of config.hooks)
    if (!["tool.before", "tool.after"].includes(hook.event) ||
        typeof hook.step !== "string" || !hook.step ||
        !["block", "report"].includes(hook.on_error) || hook.replace_result !== false ||
        !Number.isInteger(hook.budget_ms) || hook.budget_ms < 1 || hook.budget_ms > 55000)
      throw new Error("Invalid AW Qoder SDK hook route");
  return config;
}

function runtime(options) {
  if (options.pathToQoderCLIExecutable != null || options.executable !== undefined ||
      options.executableArgs !== undefined || options.spawnQoderCLIProcess !== undefined)
    throw new Error("AW Qoder SDK rejects runtime executable overrides");
  if (process.env.QODERCLI_PATH)
    throw new Error("QODERCLI_PATH overrides the pinned Qoder SDK runtime");
  for (const environment of [process.env, options.env ?? {}])
    if (/^(1|true|yes|on)$/i.test((environment.QODER_HEADLESS_FAST_HOOKS ?? "").trim()))
      throw new Error("QODER_HEADLESS_FAST_HOOKS disables native hooks");
}

/** Start the pinned daemon after admission; the caller still owns stop(). */
export async function startDaemon(options) {
  const config = configuration();
  runtime(options);
  const sdk = await import(pathToFileURL(config.sdk).href);
  const daemon = await sdk.startDaemon(options);
  daemons.add(daemon);
  return daemon;
}

function compose(config, options) {
  if (!Array.isArray(options.settingSources) || options.settingSources.length ||
      options.settings != null || options.plugins?.length ||
      Object.keys(options.extraArgs ?? {}).length ||
      options.transport != null || options.permissionPromptToolName != null ||
      options.hooks?.PermissionRequest?.length)
    throw new Error("AW Qoder SDK requires settingSources=[] and no settings, plugins, runtime or permission-hook overrides");
  runtime(options);
  if (options.daemon !== undefined && !daemons.has(options.daemon))
    throw new Error("AW Qoder SDK requires a daemon created by its startDaemon factory");
  if (resolve(options.cwd ?? process.cwd()) !== config.cwd)
    throw new Error("Qoder Query cwd differs from the AW binding");
  const hooks = Object.fromEntries(Object.entries(options.hooks ?? {}).map(([event, groups]) =>
    [event, groups.map((group) => ({ ...group, hooks: group.hooks.map((callback) =>
      async (payload, id, context) => {
        const result = await callback(copy(payload), id, context);
        if (event === "PreToolUse" && result?.hookSpecificOutput?.updatedInput !== undefined)
          return deny("Tool argument rewriting is unsupported with AW");
        return result;
      }) }))]));
  for (const [event, nativeEvents] of [
    ["tool.before", ["PreToolUse"]], ["tool.after", ["PostToolUse", "PostToolUseFailure"]],
  ]) {
    const routes = config.hooks.filter((hook) => hook.event === event);
    if (!routes.length) continue;
    const callback = async (input, id, { signal }) => {
      const payload = copy(input);
      if (id !== undefined && id !== payload.tool_use_id)
        return event === "tool.before" ? deny("AW tool identity mismatch") : {};
      for (const hook of routes) {
        try {
          const result = await invoke(config, hook, payload, signal);
          if (result.hookSpecificOutput) return result;
        } catch (error) {
          process.stderr.write(`AW ${event}/${hook.step} failed: ${error.message}; on_error=${hook.on_error}\n`);
          if (event === "tool.before" && hook.on_error === "block")
            return deny("AW hook failed; configured failure action blocked the tool");
        }
      }
      return {};
    };
    for (const native of nativeEvents)
      (hooks[native] ??= []).push({ matcher: "*", hooks: [callback],
        timeout: routes.reduce((ms, hook) => ms + hook.budget_ms + 3000, 1000) / 1000 });
  }
  const canUseTool = options.canUseTool && (async (name, input, context) => {
    const result = await options.canUseTool(name, copy(input), context);
    if (result?.updatedInput !== undefined && !isDeepStrictEqual(result.updatedInput, input))
      return { behavior: "deny", message: "Tool argument rewriting is unsupported with AW" };
    return result;
  });
  return { ...options, hooks, ...(canUseTool ? { canUseTool } : {}) };
}

function ready(config) {
  const temporary = `${config.ready_path}.${process.pid}.tmp`;
  try {
    writeFileSync(temporary, JSON.stringify({ version: 1, adapter: "qoder-sdk",
      pid: process.pid, token: config.ready_token, hooks: config.hooks.length }),
    { mode: 0o600, flag: "wx" });
    renameSync(temporary, config.ready_path);
  } finally {
    try { unlinkSync(temporary); } catch (error) { if (error.code !== "ENOENT") throw error; }
  }
}

/** Return the native Query after its initialization handshake accepts AW callbacks. */
export async function createQuery({ prompt, options }) {
  const config = configuration();
  const reload = config.reloadable ? new Reload(config) : undefined;
  const composed = compose(config, options);
  const sdk = await import(pathToFileURL(config.sdk).href);
  const query = sdk.query({ prompt: reload ? reload.input(prompt) : prompt,
    options: reload ? reload.decorate(composed) : composed });
  try {
    const initialization = await query.initializationResult();
    const managed = reload ? await reload.attach(query, initialization) : query;
    ready(config);
    return managed;
  } catch (error) {
    if (reload?.admitted) await reload.close();
    else {
      if (reload) { reload.closing = true; reload.initialized.resolve(); }
      await query.return();
    }
    throw error;
  }
}
