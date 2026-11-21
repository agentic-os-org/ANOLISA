/**
 * Opt-in non-invasive Node.js preload instrumentation over LanguageSource.
 *
 * Load with `node --import @anolisa/aw-sdk/sources/preload app.mjs` and set
 * `AW_AUTO_INSTRUMENT=1` with `AW_AUTO_INSTRUMENT_CONFIG` (file path) or
 * `AW_AUTO_INSTRUMENT_SPEC` (JSON string). Importing without `AW_AUTO_INSTRUMENT=1`
 * performs no global patching.
 */
import { readFileSync } from "node:fs";
import * as moduleApi from "node:module";
import path from "node:path";
import { pathToFileURL } from "node:url";
import {
  DEFAULT_MAX_STREAM_ITEMS,
  DEFAULT_SOURCE_EVENTS,
  LanguageSource,
} from "./sources.mjs";
import { isPlainObject, validateIdentity } from "./source-helpers.mjs";
import { AwError } from "./transport.mjs";

const DEFAULT_MAX_BODY_BYTES = 1024 * 1024;
const REGISTRY_SYM = Symbol.for("aw.sdk.preload.registry");
const PROXY_SCHEME = "aw-instrument:";
const TOP_KEYS = new Set([
  "endpoint",
  "target",
  "availability",
  "sessionId",
  "events",
  "replaceInput",
  "replaceResult",
  "withholdResult",
  "timeoutMs",
  "cwd",
  "targets",
]);
const FETCH_KEYS = new Set(["kind", "name", "maxBodyBytes"]);
const MODULE_KEYS = new Set([
  "kind",
  "module",
  "exportName",
  "role",
  "name",
  "bufferStream",
  "maxStreamItems",
]);
const SUPPORTED_EVENTS = new Set(["input.submit", "tool.before", "tool.after"]);
const IDENT_RE = /^[A-Za-z_$][A-Za-z0-9_$]*$/;

export function validateAutoSpec(raw) {
  if (!isPlainObject(raw)) throw new AwError("source_auto_config_invalid");
  for (const key of Object.keys(raw)) {
    if (!TOP_KEYS.has(key)) throw new AwError("source_auto_config_invalid");
  }
  const { endpoint, target } = raw;
  if (typeof endpoint !== "string" || !endpoint.trim())
    throw new AwError("source_auto_config_invalid");
  if (typeof target !== "string" || !target.trim())
    throw new AwError("source_auto_config_invalid");
  const availability = raw.availability ?? "required";
  if (!["required", "native_fallback"].includes(availability))
    throw new AwError("source_auto_config_invalid");
  const events = raw.events ?? [...DEFAULT_SOURCE_EVENTS];
  if (
    !Array.isArray(events) ||
    !events.length ||
    events.some((e) => typeof e !== "string" || !SUPPORTED_EVENTS.has(e))
  )
    throw new AwError("source_auto_config_invalid");
  for (const flag of ["replaceInput", "replaceResult", "withholdResult"]) {
    if (raw[flag] !== undefined && typeof raw[flag] !== "boolean")
      throw new AwError("source_auto_config_invalid");
  }
  const timeoutMs = raw.timeoutMs ?? 5000;
  if (!Number.isInteger(timeoutMs) || timeoutMs < 100 || timeoutMs > 60000)
    throw new AwError("source_auto_config_invalid");
  if (raw.sessionId !== undefined) validateIdentity(raw.sessionId);
  if (raw.cwd !== undefined && (typeof raw.cwd !== "string" || !raw.cwd.trim()))
    throw new AwError("source_auto_config_invalid");
  if (!Array.isArray(raw.targets) || !raw.targets.length)
    throw new AwError("source_auto_config_invalid");

  const targets = [];
  let hasFetch = false;
  const seenMod = new Set();
  for (const entry of raw.targets) {
    if (!isPlainObject(entry)) throw new AwError("source_auto_config_invalid");
    if (entry.kind === "fetch") {
      for (const key of Object.keys(entry)) {
        if (!FETCH_KEYS.has(key))
          throw new AwError("source_auto_config_invalid");
      }
      if (hasFetch) throw new AwError("source_auto_config_invalid");
      hasFetch = true;
      const name = validateIdentity(entry.name ?? "fetch");
      const maxBodyBytes = entry.maxBodyBytes ?? DEFAULT_MAX_BODY_BYTES;
      if (!Number.isInteger(maxBodyBytes) || maxBodyBytes < 1)
        throw new AwError("source_auto_config_invalid");
      targets.push({ kind: "fetch", name, maxBodyBytes });
    } else if (entry.kind === "module") {
      for (const key of Object.keys(entry)) {
        if (!MODULE_KEYS.has(key))
          throw new AwError("source_auto_config_invalid");
      }
      const mod = entry.module;
      const exportName = entry.exportName;
      const role = entry.role ?? "tool";
      if (
        typeof mod !== "string" ||
        !mod.trim() ||
        mod.includes("*") ||
        typeof exportName !== "string" ||
        (exportName !== "default" && !IDENT_RE.test(exportName)) ||
        !["tool", "input"].includes(role)
      )
        throw new AwError("source_auto_config_invalid");
      const resolvedUrl = mod.startsWith("file://")
        ? mod
        : path.isAbsolute(mod)
          ? pathToFileURL(mod).href
          : mod;
      const key = `${resolvedUrl}#${exportName}`;
      if (seenMod.has(key)) throw new AwError("source_auto_config_invalid");
      seenMod.add(key);
      const name = validateIdentity(entry.name ?? exportName);
      const bufferStream = entry.bufferStream ?? false;
      const maxStreamItems = entry.maxStreamItems ?? DEFAULT_MAX_STREAM_ITEMS;
      if (
        typeof bufferStream !== "boolean" ||
        !Number.isInteger(maxStreamItems) ||
        maxStreamItems < 1
      )
        throw new AwError("source_auto_config_invalid");
      targets.push({
        kind: "module",
        module: mod,
        matchUrl: resolvedUrl,
        exportName,
        role,
        name,
        bufferStream,
        maxStreamItems,
      });
    } else {
      throw new AwError("source_auto_config_invalid");
    }
  }
  return {
    endpoint,
    target,
    availability,
    sessionId: raw.sessionId,
    events: [...new Set(events)],
    replaceInput: raw.replaceInput ?? false,
    replaceResult: raw.replaceResult ?? false,
    withholdResult: raw.withholdResult ?? false,
    timeoutMs,
    cwd: raw.cwd,
    targets,
  };
}

function snapshotFetchArgs(input, init) {
  let url;
  let method = "GET";
  const headers = {};
  let body = null;
  if (typeof input === "string" || input instanceof URL) {
    url = String(input);
  } else if (input && typeof input === "object" && typeof input.url === "string") {
    url = input.url;
    method = input.method ?? method;
    if (input.headers && typeof input.headers.forEach === "function") {
      input.headers.forEach((value, key) => {
        headers[key] = value;
      });
    }
  } else {
    throw new AwError("source_input_snapshot_required");
  }
  if (init && typeof init === "object") {
    if (init.method !== undefined) method = String(init.method);
    if (init.headers) {
      const h = new Headers(init.headers);
      h.forEach((value, key) => {
        headers[key] = value;
      });
    }
    if (typeof init.body === "string") body = init.body;
    else if (init.body instanceof Uint8Array)
      body = Buffer.from(init.body).toString("utf8");
    else if (init.body !== undefined && init.body !== null)
      throw new AwError("source_input_snapshot_required");
  }
  return { url, method: method.toUpperCase(), headers, body };
}

function rebuildFetchArgs(updated, originalInit) {
  if (typeof updated.url !== "string" || !updated.url)
    throw new AwError("source_input_replace_unsupported");
  const nextInit = { ...(originalInit && typeof originalInit === "object" ? originalInit : {}) };
  if (typeof updated.method === "string") nextInit.method = updated.method;
  if (isPlainObject(updated.headers)) nextInit.headers = { ...updated.headers };
  if (updated.body === null || updated.body === undefined) delete nextInit.body;
  else if (typeof updated.body === "string") nextInit.body = updated.body;
  else throw new AwError("source_input_replace_unsupported");
  return [updated.url, nextInit];
}

async function readBoundedText(response, maxBodyBytes) {
  const reader = response.body?.getReader?.();
  if (!reader) {
    const text = await response.text();
    if (Buffer.byteLength(text, "utf8") > maxBodyBytes)
      throw new AwError("source_result_too_large");
    return text;
  }
  const chunks = [];
  let total = 0;
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      total += value.byteLength;
      if (total > maxBodyBytes) {
        await reader.cancel();
        throw new AwError("source_result_too_large");
      }
      chunks.push(Buffer.from(value));
    }
  } finally {
    reader.releaseLock();
  }
  return Buffer.concat(chunks, total).toString("utf8");
}

function wrapFetch(handle, targetSpec) {
  const originalFetch = globalThis.fetch;
  if (typeof originalFetch !== "function")
    throw new AwError("source_attribute_missing");
  const wrapped = async function awWrappedFetch(input, init) {
    if (!handle.active) return originalFetch.call(this, input, init);
    const session = await handle._getSession();
    const snap = snapshotFetchArgs(input, init);
    const { callId, input: effective, decision } = await session.beforeTool(
      targetSpec.name,
      snap,
      { signal: init?.signal },
    );
    let callInput = input;
    let callInit = init;
    if (Object.hasOwn(decision, "updated_input")) {
      [callInput, callInit] = rebuildFetchArgs(decision.updated_input, init);
    }
    const response = await originalFetch.call(this, callInput, callInit);
    if (!session._observeAfter()) return response;
    const text = await readBoundedText(response, targetSpec.maxBodyBytes);
    const headerObj = {};
    response.headers.forEach((value, key) => {
      headerObj[key] = value;
    });
    const after = await session.afterTool(
      targetSpec.name,
      effective,
      { status: response.status, ok: response.ok, headers: headerObj, text },
      { callId, signal: init?.signal },
    );
    let outText = text;
    if (Object.hasOwn(after, "replaced_result")) {
      const rep = after.replaced_result;
      if (typeof rep === "string") outText = rep;
      else if (isPlainObject(rep) && typeof rep.text === "string")
        outText = rep.text;
      else throw new AwError("source_result_replace_unsupported");
    }
    const nextHeaders = new Headers(response.headers);
    nextHeaders.delete("content-length");
    return new Response(outText, {
      status: response.status,
      statusText: response.statusText,
      headers: nextHeaders,
    });
  };
  globalThis.fetch = wrapped;
  return () => {
    if (globalThis.fetch === wrapped) globalThis.fetch = originalFetch;
  };
}

function registerModuleHooks(handle, moduleTargets) {
  if (typeof moduleApi.registerHooks !== "function")
    throw new AwError("source_preload_module_hook_unavailable");
  const registry = (globalThis[REGISTRY_SYM] ??= new Map());
  const byUrl = new Map();
  for (const item of moduleTargets) {
    let list = byUrl.get(item.matchUrl);
    if (!list) {
      list = [];
      byUrl.set(item.matchUrl, list);
    }
    list.push(item);
  }
  const hookId = `aw-${Date.now()}-${Math.random().toString(36).slice(2)}`;
  registry.set(hookId, {
    wrap(realUrl, exportName, originalFn) {
      const targets = byUrl.get(realUrl) ?? [];
      const spec = targets.find((t) => t.exportName === exportName);
      if (!spec || typeof originalFn !== "function") return originalFn;
      return async function awModuleWrapper(...args) {
        if (!handle.active) return originalFn.apply(this, args);
        const session = await handle._getSession();
        const wrapped =
          spec.role === "input"
            ? session.wrapInput(originalFn)
            : session.wrapTool(originalFn, {
                name: spec.name,
                bufferStream: spec.bufferStream,
                maxStreamItems: spec.maxStreamItems,
              });
        return wrapped.apply(this, args);
      };
    },
  });

  const hooks = moduleApi.registerHooks({
    resolve(specifier, context, nextResolve) {
      if (specifier.includes("aw_original=1")) {
        const clean = specifier.replace(/[?&]aw_original=1$/, "");
        return nextResolve(clean, context);
      }
      const resolved = nextResolve(specifier, context);
      if (!handle.active) return resolved;
      const matched =
        byUrl.has(resolved.url) || byUrl.has(specifier);
      if (!matched) return resolved;
      if (!byUrl.has(resolved.url) && byUrl.has(specifier)) {
        byUrl.set(resolved.url, byUrl.get(specifier));
      }
      return {
        url: `${PROXY_SCHEME}${resolved.url}`,
        shortCircuit: true,
      };
    },
    load(url, context, nextLoad) {
      if (!url.startsWith(PROXY_SCHEME)) return nextLoad(url, context);
      const realUrl = url.slice(PROXY_SCHEME.length);
      const targets = byUrl.get(realUrl) ?? [];
      const origSpecifier = `${realUrl}?aw_original=1`;
      const lines = [
        `import * as _orig from ${JSON.stringify(origSpecifier)};`,
        `export * from ${JSON.stringify(origSpecifier)};`,
        `const _reg = globalThis[Symbol.for("aw.sdk.preload.registry")].get(${JSON.stringify(hookId)});`,
      ];
      for (const spec of targets) {
        const expr = `_reg ? _reg.wrap(${JSON.stringify(realUrl)}, ${JSON.stringify(spec.exportName)}, _orig[${JSON.stringify(spec.exportName)}]) : _orig[${JSON.stringify(spec.exportName)}]`;
        if (spec.exportName === "default") {
          lines.push(`export default ${expr};`);
        } else {
          lines.push(`export const ${spec.exportName} = ${expr};`);
        }
      }
      return {
        format: "module",
        source: `${lines.join("\n")}\n`,
        shortCircuit: true,
      };
    },
  });

  return () => {
    hooks.deregister();
    registry.delete(hookId);
  };
}

export class AutoPreload {
  constructor(spec) {
    this.spec = spec;
    this.active = true;
    this._restorers = [];
    this._sessionPromise = undefined;
    this._source = undefined;

    const fetchTarget = spec.targets.find((t) => t.kind === "fetch");
    const moduleTargets = spec.targets.filter((t) => t.kind === "module");
    try {
      if (fetchTarget) this._restorers.push(wrapFetch(this, fetchTarget));
      if (moduleTargets.length)
        this._restorers.push(registerModuleHooks(this, moduleTargets));
    } catch (error) {
      this._restoreSync();
      throw error;
    }
  }

  async _getSession() {
    if (!this._sessionPromise) {
      this._sessionPromise = (async () => {
        const source = LanguageSource.create({
          endpoint: this.spec.endpoint,
          target: this.spec.target,
          availability: this.spec.availability,
          events: this.spec.events,
          replaceInput: this.spec.replaceInput,
          replaceResult: this.spec.replaceResult,
          withholdResult: this.spec.withholdResult,
          cwd: this.spec.cwd,
        });
        await source.connect();
        this._source = source;
        return source.session({
          sessionId: this.spec.sessionId,
          timeoutMs: this.spec.timeoutMs,
        });
      })();
    }
    return this._sessionPromise;
  }

  _restoreSync() {
    this.active = false;
    const restorers = this._restorers.splice(0).reverse();
    for (const restore of restorers) restore();
  }

  async uninstall() {
    this._restoreSync();
    const pending = this._sessionPromise;
    this._sessionPromise = undefined;
    if (pending) await pending.catch(() => {});
    const source = this._source;
    this._source = undefined;
    if (source) await source.close();
  }
}

export function loadSpecFromEnv(env = process.env) {
  if (env.AW_AUTO_INSTRUMENT !== "1") return null;
  const rawJson = env.AW_AUTO_INSTRUMENT_SPEC;
  const configPath = env.AW_AUTO_INSTRUMENT_CONFIG;
  if (Boolean(rawJson) === Boolean(configPath))
    throw new AwError("source_auto_config_invalid");
  let loaded;
  try {
    loaded = JSON.parse(rawJson ?? readFileSync(configPath, "utf8"));
  } catch (error) {
    throw new AwError("source_auto_config_invalid", { cause: error });
  }
  return validateAutoSpec(loaded);
}

export function installAutoInstrumentation(spec) {
  return new AutoPreload(validateAutoSpec(spec));
}

export function installFromEnv(env = process.env) {
  const spec = loadSpecFromEnv(env);
  return spec ? new AutoPreload(spec) : null;
}

let activePreload = null;
export function getActivePreload() {
  return activePreload;
}

if (process.env.AW_AUTO_INSTRUMENT === "1") {
  activePreload = installFromEnv();
  if (activePreload) {
    process.once("beforeExit", () => {
      activePreload?.uninstall().catch(() => {});
    });
  }
}
