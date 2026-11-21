/**
 * Opt-in non-invasive Node.js preload instrumentation over LanguageSource.
 *
 * Load with `node --import @anolisa/aw-sdk/sources/preload app.mjs` and set
 * `AW_AUTO_INSTRUMENT=1` with `AW_AUTO_INSTRUMENT_CONFIG` (file path) or
 * `AW_AUTO_INSTRUMENT_SPEC` (JSON string). Importing without `AW_AUTO_INSTRUMENT=1`
 * performs no global patching.
 */
import { readFileSync } from "node:fs";
import path from "node:path";
import {
  DEFAULT_MAX_STREAM_BYTES,
  DEFAULT_MAX_STREAM_ITEMS,
  DEFAULT_SOURCE_EVENTS,
  LanguageSource,
} from "./sources.mjs";
import { isPlainObject, validateIdentity } from "./source-helpers.mjs";
import { wrapFetch } from "./source-preload-fetch.mjs";
import {
  registerModuleHooks,
  resolveCanonicalModuleUrl,
} from "./source-preload-modules.mjs";
import { AwError } from "./transport.mjs";

const DEFAULT_MAX_BODY_BYTES = 1024 * 1024;
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
  "baseDir",
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
  "maxStreamBytes",
]);
const SUPPORTED_EVENTS = new Set(["input.submit", "tool.before", "tool.after"]);
const IDENT_RE = /^[A-Za-z_$][A-Za-z0-9_$]*$/;

export function validateAutoSpec(raw, { baseDir } = {}) {
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
  if (
    raw.baseDir !== undefined &&
    (typeof raw.baseDir !== "string" || !raw.baseDir.trim())
  )
    throw new AwError("source_auto_config_invalid");
  if (!Array.isArray(raw.targets) || !raw.targets.length)
    throw new AwError("source_auto_config_invalid");

  const resolvedBaseDir = path.resolve(
    raw.baseDir ?? baseDir ?? raw.cwd ?? process.cwd(),
  );
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
      const matchUrl = resolveCanonicalModuleUrl(mod, resolvedBaseDir);
      const key = `${matchUrl}#${exportName}`;
      if (seenMod.has(key)) throw new AwError("source_auto_config_invalid");
      seenMod.add(key);
      const name = validateIdentity(entry.name ?? exportName);
      const bufferStream = entry.bufferStream ?? false;
      const maxStreamItems = entry.maxStreamItems ?? DEFAULT_MAX_STREAM_ITEMS;
      const maxStreamBytes = entry.maxStreamBytes ?? DEFAULT_MAX_STREAM_BYTES;
      if (
        typeof bufferStream !== "boolean" ||
        !Number.isInteger(maxStreamItems) ||
        maxStreamItems < 1 ||
        !Number.isInteger(maxStreamBytes) ||
        maxStreamBytes < 1
      )
        throw new AwError("source_auto_config_invalid");
      targets.push({
        kind: "module",
        module: mod,
        matchUrl,
        exportName,
        role,
        name,
        bufferStream,
        maxStreamItems,
        maxStreamBytes,
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
    baseDir: resolvedBaseDir,
    targets,
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
    if (!this.active) throw new AwError("source_closed");
    if (!this._sessionPromise) {
      const pending = (async () => {
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
        try {
          await source.connect();
          if (!this.active) {
            await source.close();
            throw new AwError("source_closed");
          }
          this._source = source;
          return source.session({
            sessionId: this.spec.sessionId,
            timeoutMs: this.spec.timeoutMs,
          });
        } catch (error) {
          if (this._sessionPromise === pending)
            this._sessionPromise = undefined;
          await source.close().catch(() => {});
          throw error;
        }
      })();
      this._sessionPromise = pending;
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
  const baseDir = configPath
    ? path.dirname(path.resolve(configPath))
    : undefined;
  return validateAutoSpec(loaded, { baseDir });
}

export function installAutoInstrumentation(spec, options) {
  return new AutoPreload(validateAutoSpec(spec, options));
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
