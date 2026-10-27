/** Register AW on each Query before initialization; the caller owns its daemon and queries. */
import { readFileSync, writeFileSync, renameSync, unlinkSync } from "node:fs";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { isDeepStrictEqual } from "node:util";
import { invoke } from "./callback.mjs";
import { Reload } from "./reload.mjs";
import { Resident, bounded, validateHooks } from "./resident.mjs";
import { managedDaemon, daemonOwner, credentialOptions } from "./daemon.mjs";
import { managedMcpServer } from "./mcp.mjs";
import { Scope } from "./scope.mjs";
import { pluginsAdmitted, runtimeOptions, sdkEntry, verifyNativeSettings } from "./native-options.mjs";
export { PRIVATE_EXECUTION_VERSION, executionEnvironment, hookEnvironment } from "./private.mjs";
export { NATIVE_COMMAND_WRAPPER_VERSION } from "./command-wrapper.mjs";

const deny = (reason) => ({ hookSpecificOutput: { hookEventName: "PreToolUse",
  permissionDecision: "deny", permissionDecisionReason: reason } });
const copy = (value) => JSON.parse(JSON.stringify(value));

function configuration() {
  const config = JSON.parse(readFileSync(process.env.AW_QODER_SDK_CONFIG, "utf8"));
  for (const key of ["sdk", "executable", "binding", "cwd", "ready_path", "ready_token"])
    if (typeof config[key] !== "string" || !config[key]) throw new Error(`Missing AW ${key}`);
  validateHooks(config);
  return config;
}

function runtime(config, options, operation) {
  runtimeOptions(config, options, operation);
  if (process.env.QODERCLI_PATH)
    throw new Error("QODERCLI_PATH overrides the pinned Qoder SDK runtime");
  for (const environment of [process.env, options.env ?? {}])
    if (/^(1|true|yes|on)$/i.test((environment.QODER_HEADLESS_FAST_HOOKS ?? "").trim()))
      throw new Error("QODER_HEADLESS_FAST_HOOKS disables native hooks");
}

/** Start the pinned daemon after admission; the caller still owns stop(). */
export async function startDaemon(options) {
  const config = configuration();
  const { awSite = config.default_site ?? "global", ...nativeOptions } = options;
  const entry = sdkEntry(config, awSite);
  const environment = { ...process.env, ...(options.env ?? {}) };
  options = nativeOptions;
  options = { ...options, env: { ...environment },
    ...(options.auth ? { auth: credentialOptions(options.auth) } : {}) };
  runtime(config, { ...options, awSite }, "daemon");
  const sdk = await import(pathToFileURL(entry).href);
  const daemon = await managedDaemon(sdk, options, { ...config, sdk: entry, sdk_site: awSite }, bounded, environment);
  if (config.resident_policy_reload) {
    try {
      await new Scope(config, "qoder-sdk").ready();
      ready(config);
    } catch (error) {
      try { await daemon.stop(); }
      catch (cleanup) { throw new AggregateError([error, cleanup], "AW daemon startup cleanup failed"); }
      throw error;
    }
  }
  return daemon;
}

/** Preserve SDK tool definitions while requiring one managed MCP server per Query. */
export async function createSdkMcpServer(options) {
  const config = configuration();
  if (!config.resident) throw new Error("Managed MCP requires an AW resident Query");
  const { awSite, ...nativeOptions } = options;
  const sdk = await import(pathToFileURL(sdkEntry(config, awSite)).href);
  return managedMcpServer(sdk, nativeOptions);
}

function compose(config, options) {
  if (!Array.isArray(options.settingSources) || options.settingSources.length ||
      options.settings != null || !pluginsAdmitted(options.plugins) ||
      Object.keys(options.extraArgs ?? {}).length ||
      options.transport != null || options.permissionPromptToolName != null ||
      options.hooks?.PermissionRequest?.length)
    throw new Error("AW Qoder SDK requires settingSources=[] and no settings, plugins, runtime or permission-hook overrides");
  runtime(config, options);
  daemonOwner(options.daemon, config);
  if (!config.resident && resolve(options.cwd ?? process.cwd()) !== config.cwd)
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
          if (config.resident && event === "tool.after" && hook.on_error === "block")
            throw error;
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
  // The pinned Worker expands skill arguments before shell interpolation. Its
  // native switch must disable that secondary executor even for inert assets.
  const settings = config.native_policy?.declarative_plugins ?
    { settings: { skills: { disableShellExecution: true } } } : {};
  return { ...options, ...settings, hooks, ...(canUseTool ? { canUseTool } : {}) };
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
  let config = configuration();
  if (config.native_policy?.declarative_plugins && !options.daemon)
    throw new Error("AW declarative profile requires a managed daemon");
  if (config.resident) {
    const resident = new Resident(config);
    return await resident.create(prompt, options, compose, ready);
  }
  if (options.awExecutionFactory !== undefined)
    throw new Error("Private execution requires an AW resident Query");
  if (options.awNativeExecutionFactory !== undefined)
    throw new Error("Native command execution requires an AW resident Query");
  const { awSite, ...nativeOptions } = options;
  const owner = daemonOwner(options.daemon, config);
  const site = awSite ?? owner?.site ?? config.default_site ?? "global";
  const entry = sdkEntry(config, site);
  if (owner && owner.sdk !== entry) throw new Error("AW Query site differs from its managed daemon");
  config = { ...config, sdk: entry, active_site: site };
  options = nativeOptions;
  const reload = config.reloadable ? new Reload(config) : undefined;
  const composed = compose(config, options);
  const sdk = await import(pathToFileURL(config.sdk).href);
  const query = sdk.query({ prompt: reload ? reload.input(prompt) : prompt,
    options: reload ? reload.decorate(composed) : composed });
  try {
    const initialization = await query.initializationResult();
    await verifyNativeSettings(config, query, bounded);
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
