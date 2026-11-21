/** Canonical-URL ESM module preload hooks with dynamic dispatch for source-preload.mjs. */
import { randomBytes } from "node:crypto";
import * as moduleApi from "node:module";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { isAsyncGeneratorFunction } from "./source-helpers.mjs";
import { AwError } from "./transport.mjs";

const REGISTRY_SYM = Symbol.for("aw.sdk.preload.registry.v2");
const PROXY_SCHEME = "aw-instrument:";
const ORIG_SCHEME = "aw-instrument-orig:";

function getRegistry() {
  if (!globalThis[REGISTRY_SYM]) {
    const activeByUrl = new Map();
    const proxyStateByUrl = new Map();
    const delegationTokens = new Map();

    function wrapExport(realUrl, exportName, origNs) {
      if (!Object.prototype.hasOwnProperty.call(origNs, exportName))
        throw new AwError("source_auto_target_missing");
      const originalFn = origNs[exportName];
      if (typeof originalFn !== "function")
        throw new AwError("source_auto_target_unsupported");
      if (isAsyncGeneratorFunction(originalFn)) {
        return async function* awModuleStreamWrapper(...args) {
          const active = activeByUrl.get(realUrl);
          const spec = active?.handle?.active
            ? active.targetsByExport.get(exportName)
            : undefined;
          if (!spec) {
            yield* originalFn.apply(this, args);
            return;
          }
          const session = await active.handle._getSession();
          const wrapped =
            spec.role === "input"
              ? session.wrapInput(originalFn)
              : session.wrapTool(originalFn, {
                  name: spec.name,
                  bufferStream: spec.bufferStream,
                  maxStreamItems: spec.maxStreamItems,
                  maxStreamBytes: spec.maxStreamBytes,
                });
          yield* wrapped.apply(this, args);
        };
      }
      if (originalFn.constructor?.name === "AsyncFunction") {
        return async function awModuleAsyncWrapper(...args) {
          const active = activeByUrl.get(realUrl);
          const spec = active?.handle?.active
            ? active.targetsByExport.get(exportName)
            : undefined;
          if (!spec) return originalFn.apply(this, args);
          const session = await active.handle._getSession();
          const wrapped =
            spec.role === "input"
              ? session.wrapInput(originalFn)
              : session.wrapTool(originalFn, {
                  name: spec.name,
                  bufferStream: spec.bufferStream,
                  maxStreamItems: spec.maxStreamItems,
                  maxStreamBytes: spec.maxStreamBytes,
                });
          return wrapped.apply(this, args);
        };
      }
      throw new AwError("source_auto_target_sync_unsupported");
    }

    globalThis[REGISTRY_SYM] = {
      activeByUrl,
      proxyStateByUrl,
      delegationTokens,
      wrapExport,
    };
  }
  return globalThis[REGISTRY_SYM];
}

export function resolveCanonicalModuleUrl(mod, baseDir) {
  if (mod.startsWith("file://")) {
    try {
      return new URL(mod).href;
    } catch (error) {
      throw new AwError("source_auto_config_invalid", { cause: error });
    }
  }
  if (path.isAbsolute(mod)) {
    return pathToFileURL(path.resolve(mod)).href;
  }
  const anchorUrl = pathToFileURL(
    path.join(path.resolve(baseDir), "__aw_spec_anchor__.mjs"),
  );
  if (mod.startsWith("./") || mod.startsWith("../")) {
    try {
      return new URL(mod, anchorUrl).href;
    } catch (error) {
      throw new AwError("source_auto_config_invalid", { cause: error });
    }
  }
  try {
    const req = moduleApi.createRequire(anchorUrl.href);
    return pathToFileURL(req.resolve(mod)).href;
  } catch (error) {
    throw new AwError("source_auto_target_missing", { cause: error });
  }
}

function stripCommentsAndStrings(src) {
  let out = "";
  let i = 0;
  const n = src.length;
  while (i < n) {
    const ch = src[i];
    const next = src[i + 1];
    if (ch === "/" && next === "/") {
      i += 2;
      while (i < n && src[i] !== "\n") i++;
      out += "\n";
    } else if (ch === "/" && next === "*") {
      i += 2;
      while (i < n && !(src[i] === "*" && src[i + 1] === "/")) i++;
      i += 2;
      out += " ";
    } else if (ch === "'" || ch === '"' || ch === "`") {
      const quote = ch;
      i++;
      while (i < n && src[i] !== quote) {
        if (src[i] === "\\") i++;
        i++;
      }
      i++;
      out += " ";
    } else {
      out += ch;
      i++;
    }
  }
  return out;
}

function hasDefaultExport(loaded) {
  if (!loaded) return false;
  if (
    loaded.format === "commonjs" ||
    loaded.format === "json" ||
    loaded.format === "addon"
  )
    return true;
  if (loaded.source == null) return false;
  const text =
    typeof loaded.source === "string"
      ? loaded.source
      : Buffer.from(loaded.source).toString("utf8");
  const stripped = stripCommentsAndStrings(text);
  return (
    /\bexport\s+default\b/.test(stripped) ||
    /\bexport\s*\{[^}]*\bas\s+default\b/.test(stripped) ||
    /\bexport\s*\{\s*default\s*[,}]/.test(stripped)
  );
}

function makeProxyUrl(gen, realUrl) {
  return `${PROXY_SCHEME}${gen}/${encodeURIComponent(realUrl)}`;
}

function parseProxyUrl(url) {
  const rest = url.slice(PROXY_SCHEME.length);
  const slash = rest.indexOf("/");
  if (slash < 1) return null;
  return {
    gen: Number(rest.slice(0, slash)),
    realUrl: decodeURIComponent(rest.slice(slash + 1)),
  };
}

export function registerModuleHooks(handle, moduleTargets) {
  if (typeof moduleApi.registerHooks !== "function")
    throw new AwError("source_preload_module_hook_unavailable");
  const registry = getRegistry();
  const byUrl = new Map();
  for (const item of moduleTargets) {
    let map = byUrl.get(item.matchUrl);
    if (!map) {
      map = new Map();
      byUrl.set(item.matchUrl, map);
    }
    map.set(item.exportName, item);
  }

  for (const realUrl of byUrl.keys()) {
    if (registry.activeByUrl.has(realUrl))
      throw new AwError("source_already_instrumented");
  }

  const ownedTokens = [];
  const genByUrl = new Map();
  let hooks;
  const rollback = () => {
    hooks?.deregister();
    for (const realUrl of byUrl.keys()) {
      const current = registry.activeByUrl.get(realUrl);
      if (current?.handle === handle) registry.activeByUrl.delete(realUrl);
    }
    for (const token of ownedTokens) registry.delegationTokens.delete(token);
  };
  try {
    for (const [realUrl, targetsByExport] of byUrl.entries()) {
      const prev = registry.proxyStateByUrl.get(realUrl);
      let gen = 0;
      let wrappedExports;
      if (prev) {
        const missingFromPrev = [...targetsByExport.keys()].some(
          (name) => !prev.wrappedExports.has(name),
        );
        gen = missingFromPrev ? prev.gen + 1 : prev.gen;
        wrappedExports = new Set([
          ...prev.wrappedExports,
          ...targetsByExport.keys(),
        ]);
      } else {
        wrappedExports = new Set(targetsByExport.keys());
      }
      genByUrl.set(realUrl, gen);
      registry.proxyStateByUrl.set(realUrl, { gen, wrappedExports });
      registry.activeByUrl.set(realUrl, { handle, targetsByExport });
    }

    hooks = moduleApi.registerHooks({
    resolve(specifier, context, nextResolve) {
      if (specifier.startsWith(ORIG_SCHEME)) {
        const rest = specifier.slice(ORIG_SCHEME.length);
        const slash = rest.indexOf("/");
        if (slash < 1) throw new AwError("source_auto_delegation_denied");
        const token = rest.slice(0, slash);
        const targetUrl = decodeURIComponent(rest.slice(slash + 1));
        const deleg = registry.delegationTokens.get(token);
        if (
          !deleg ||
          deleg.proxyUrl !== context.parentURL ||
          deleg.realUrl !== targetUrl
        )
          throw new AwError("source_auto_delegation_denied");
        return { url: deleg.realUrl, shortCircuit: true };
      }
      const resolved = nextResolve(specifier, context);
      if (!handle.active) return resolved;
      if (!byUrl.has(resolved.url)) return resolved;
      return {
        url: makeProxyUrl(genByUrl.get(resolved.url), resolved.url),
        shortCircuit: true,
      };
    },
    load(url, context, nextLoad) {
      if (!url.startsWith(PROXY_SCHEME)) return nextLoad(url, context);
      const parsed = parseProxyUrl(url);
      if (!parsed || !byUrl.has(parsed.realUrl)) return nextLoad(url, context);
      const { realUrl } = parsed;
      const targetsByExport = byUrl.get(realUrl);
      const loaded = nextLoad(realUrl, { ...context, format: undefined });
      const origHasDefault = hasDefaultExport(loaded);
      const token = randomBytes(16).toString("hex");
      registry.delegationTokens.set(token, { proxyUrl: url, realUrl });
      ownedTokens.push(token);
      const origSpecifier = `${ORIG_SCHEME}${token}/${encodeURIComponent(realUrl)}`;
      const lines = [
        `import * as _orig from ${JSON.stringify(origSpecifier)};`,
        `export * from ${JSON.stringify(origSpecifier)};`,
        `const _reg = globalThis[Symbol.for("aw.sdk.preload.registry.v2")];`,
      ];
      for (const exportName of targetsByExport.keys()) {
        const expr = `_reg.wrapExport(${JSON.stringify(realUrl)}, ${JSON.stringify(exportName)}, _orig)`;
        if (exportName === "default") lines.push(`export default ${expr};`);
        else lines.push(`export const ${exportName} = ${expr};`);
      }
      if (!targetsByExport.has("default") && origHasDefault) {
        lines.push(`export default _orig.default;`);
      }
      return {
        format: "module",
        source: `${lines.join("\n")}\n`,
        shortCircuit: true,
      };
    },
    });
  } catch (error) {
    rollback();
    throw error;
  }

  return rollback;
}
