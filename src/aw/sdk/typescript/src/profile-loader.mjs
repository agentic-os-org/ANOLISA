/** ESM facades keep the original module URL and its relative imports intact. */
import { register } from "node:module";
import { parentPort, workerData } from "node:worker_threads";
import { fileURLToPath } from "node:url";
import { isAbsolute, relative } from "node:path";

// es-module-lexer 3.0.5, JS build, included for offline SDK installs.
import { parse } from "./vendor/es-module-lexer/lexer.mjs";

export function insidePackage(url, root) {
  if (!url?.startsWith("file:")) return false;
  try {
    const path = relative(root, fileURLToPath(url));
    return (
      !!path && path !== ".." && !path.startsWith("../") && !isAbsolute(path)
    );
  } catch {
    return false;
  }
}

export function createProfileHooks({
  id,
  root,
  parentURL,
  modules,
  enabled,
  bridge,
}) {
  const prefix = `aw-profile:${id}:`;
  const original = `${prefix}original:`;
  const locate = `${prefix}locate:`;
  const targets = new Map(modules);
  const urls = new Map();
  const facades = new Map();
  const backingSources = new Map();

  function resolve(specifier, context, nextResolve) {
    if (specifier.startsWith(original))
      return nextResolve(
        decodeURIComponent(specifier.slice(original.length)),
        context,
      );
    const locating = specifier.startsWith(locate);
    const result = locating
      ? nextResolve(specifier.slice(locate.length), { ...context, parentURL })
      : nextResolve(specifier, context);
    if (
      !locating &&
      (!Atomics.load(enabled, 0) ||
        context.conditions.includes("require") ||
        insidePackage(context.parentURL, root))
    )
      return result;

    function bind(found) {
      if (!insidePackage(found.url, root) || found.format === "commonjs")
        return found;
      if (locating) {
        if (!urls.has(found.url)) urls.set(found.url, new Set());
        urls.get(found.url).add(specifier.slice(locate.length));
        return found;
      }
      const modules = urls.has(found.url)
        ? [...urls.get(found.url)]
        : [specifier];
      if (!modules.every((name) => targets.has(name))) return found;
      const url = prefix + encodeURIComponent(found.url);
      const names = modules.flatMap((name) => targets.get(name));
      facades.set(url, {
        url: found.url,
        format: found.format,
        names,
        modules,
      });
      return { url, format: "module", shortCircuit: true };
    }
    // Module.register and registerHooks share the same resolution rules.
    return result.then ? result.then(bind) : bind(result);
  }

  function load(url, context, nextLoad) {
    if (backingSources.has(url)) {
      const loaded = backingSources.get(url);
      backingSources.delete(url);
      return loaded;
    }
    const facade = facades.get(url);
    if (!facade) return nextLoad(url, context);
    const loaded = nextLoad(facade.url, { ...context, format: facade.format });
    function bind(result) {
      // Inspect the exact source the backing module will execute, without
      // evaluating a second copy of the application or running its loader twice.
      backingSources.set(facade.url, { ...result, shortCircuit: true });
      const source =
        typeof result.source === "string"
          ? result.source
          : new TextDecoder().decode(result.source);
      const [, exports] = parse(source);
      return facadeModule(
        facade,
        exports.some(({ name, start, typeOnly }) => {
          if (!name || typeOnly) return false;
          // The lexer decodes quoted export names and keeps bare identifiers raw.
          if (source[start] !== '"' && source[start] !== "'")
            name = name.replace(
              /\\u(?:\{([\da-fA-F]+)\}|([\da-fA-F]{4}))/g,
              (_, point, unit) =>
                String.fromCodePoint(parseInt(point ?? unit, 16)),
            );
          return name === "default";
        }),
      );
    }
    return loaded.then ? loaded.then(bind) : bind(loaded);
  }

  function facadeModule(facade, hasDefault) {
    const backing = JSON.stringify(original + encodeURIComponent(facade.url));
    const bindings = [...new Set(facade.names)];
    const declarations = bindings.map(
      (name, i) =>
        `let e${i} = ns[${JSON.stringify(name)}]; export { e${i} as ${name} };`,
    );
    const setters = bindings.map(
      (name, i) => `${JSON.stringify(name)}: (value) => { e${i} = value; }`,
    );
    return {
      format: "module",
      shortCircuit: true,
      source: [
        `import * as ns from ${backing}; export * from ${backing};`,
        `import { bindProfileExports } from ${JSON.stringify(bridge)};`,
        ...(hasDefault && !bindings.includes("default")
          ? [`export { default } from ${backing};`]
          : []),
        ...declarations,
        `bindProfileExports(${JSON.stringify(id)}, ${JSON.stringify(facade.modules)}, ns, { ${setters.join(", ")} });`,
      ].join("\n"),
    };
  }

  return { resolve, load };
}

let hooks;
export function initialize(options) {
  hooks = createProfileHooks(options);
}
export function resolve(...args) {
  return hooks.resolve(...args);
}
export function load(...args) {
  return hooks.load(...args);
}

// Older Node releases provide the asynchronous loader here. Resolve existing
// ESM seams for probe in an isolated worker, leaving the application's loader alone.
if (workerData?.profileResolution && parentPort) {
  const { options, names } = workerData.profileResolution;
  register(import.meta.url, { data: options });
  parentPort.postMessage(resolveProfileModules(names, options));
}

export function resolveProfileModules(names, { id, root }) {
  const result = [];
  for (const name of names) {
    try {
      const url = import.meta.resolve(`aw-profile:${id}:locate:${name}`);
      if (insidePackage(url, root)) result.push([name, url]);
    } catch (error) {
      if (
        ![
          "ERR_MODULE_NOT_FOUND",
          "ERR_PACKAGE_PATH_NOT_EXPORTED",
          "ERR_UNSUPPORTED_DIR_IMPORT",
        ].includes(error.code)
      )
        throw error;
    }
  }
  return result;
}
