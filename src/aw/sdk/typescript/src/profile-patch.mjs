/** Local descriptors, package resolution, and module loader bindings for ProfileSource. */
import { createHash, randomUUID } from "node:crypto";
import { existsSync, readFileSync, realpathSync, statSync } from "node:fs";
import { Session as Inspector } from "node:inspector";
import Module, { createRequire, register } from "node:module";
import { dirname, join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { once } from "node:events";
import { types } from "node:util";
import { Worker } from "node:worker_threads";
import {
  createProfileHooks,
  insidePackage,
  resolveProfileModules,
} from "./profile-loader.mjs";
import { bindGuard, guards, wrap, forwardExport } from "./profile-callable.mjs";
import { versionMatches } from "./profile-eval.mjs";
import { AwError } from "./transport.mjs";

const NATIVE_CODE = /\{\s*\[native code\]\s*\}$/;
const active = new Map();
let originalLoad;
let dispatchLoad;

export function functionCodeSha256(fn) {
  const source = Function.prototype.toString
    .call(guards.get(fn)?.native ?? fn)
    .replaceAll("\r\n", "\n")
    .trim();
  if (NATIVE_CODE.test(source))
    throw new AwError("source_probe_fingerprint_mismatch");
  return createHash("sha256").update(source).digest("hex");
}

function verifyFingerprint(target, fn) {
  if (target.code_sha256 && functionCodeSha256(fn) !== target.code_sha256)
    throw new AwError("source_probe_fingerprint_mismatch");
}

function applicationURL(from) {
  if (from instanceof URL) return from.href;
  if (from?.startsWith("file:")) return new URL(from).href;
  const path = resolve(from ?? process.cwd());
  return pathToFileURL(
    existsSync(path) && statSync(path).isDirectory()
      ? join(path, "index.mjs")
      : path,
  ).href;
}

function resolveRequire(require, specifier) {
  try {
    return require.resolve(specifier);
  } catch (error) {
    if (
      ["MODULE_NOT_FOUND", "ERR_PACKAGE_PATH_NOT_EXPORTED"].includes(error.code)
    )
      return undefined;
    throw error;
  }
}

function readPackage(path) {
  if (!existsSync(path)) return undefined;
  try {
    return JSON.parse(readFileSync(path, "utf8"));
  } catch (cause) {
    throw new AwError("source_probe_package_mismatch", { cause });
  }
}

function findPackage(require, name) {
  const manifest = resolveRequire(require, name + "/package.json");
  const entry = resolveRequire(require, name);
  const candidates = [];
  if (manifest) candidates.push(manifest);
  if (entry?.startsWith("/")) {
    for (let dir = dirname(entry); dir !== dirname(dir); dir = dirname(dir))
      candidates.push(join(dir, "package.json"));
  }
  for (const path of require.resolve.paths(name) ?? [])
    candidates.push(join(path, name, "package.json"));
  for (const path of candidates) {
    const metadata = readPackage(path);
    if (metadata?.name === name)
      return { root: realpathSync(dirname(path)), metadata };
  }
  throw new AwError("source_probe_package_missing");
}

// Node exposes a CJS cache. For ESM the in-process debugger can enumerate
// parsed file URLs without importing modules or running package initializers.
function parsedFiles() {
  const urls = new Set();
  const inspector = new Inspector();
  inspector.connect();
  try {
    inspector.on("Debugger.scriptParsed", ({ params }) => urls.add(params.url));
    let failure;
    inspector.post("Debugger.enable", (error) => {
      failure = error;
    });
    if (failure) throw failure;
  } finally {
    inspector.disconnect();
  }
  return urls;
}

function loaderWorker(options, names) {
  const worker = new Worker(new URL("./profile-loader.mjs", import.meta.url), {
    execArgv: [],
    // Do not run the application's preloads while resolving fingerprints.
    env: {},
    workerData: { profileResolution: { options, names } },
  });
  return once(worker, "message")
    .then(([value]) => value)
    .finally(() => worker.terminate());
}

// Only accessors installed by this source may be read during seam discovery.
const guardGetters = new WeakSet();

function descriptorValue(descriptor) {
  return guardGetters.has(descriptor?.get)
    ? descriptor.get()
    : descriptor?.value;
}

function ownValue(owner, property) {
  if (owner == null) return undefined;
  return descriptorValue(Object.getOwnPropertyDescriptor(owner, property));
}

function findSeam(exports, target, cjs) {
  const path = target.qualname.split(".");
  if (
    cjs &&
    path.length === 1 &&
    path[0] === "default" &&
    typeof exports.value === "function"
  ) {
    return { owner: exports, key: "value" };
  }
  const key = path.pop();
  return { owner: path.reduce(ownValue, cjs ? exports.value : exports), key };
}

function inspectSeam(exports, target, cjs) {
  const { owner, key } = findSeam(exports, target, cjs);
  const descriptor =
    owner != null ? Object.getOwnPropertyDescriptor(owner, key) : undefined;
  if (typeof descriptorValue(descriptor) !== "function")
    throw new AwError("source_verify_target_missing", { message: target.id });
  return { owner, key, descriptor };
}

function addDispatcher() {
  if (dispatchLoad) return;
  originalLoad = Module._load;
  const previous = originalLoad;
  function load(request, parent, isMain) {
    const value = Reflect.apply(previous, this, arguments);
    // Another loader can retain a reference to a detached dispatcher.
    if (dispatchLoad !== load || request.startsWith("node:")) return value;
    let filename;
    for (const patches of active.values()) {
      if (!patches.mayLoad(request)) continue;
      if (!filename) {
        try {
          filename = Module._resolveFilename(request, parent, isMain);
        } catch {
          // Earlier loader customizations can return virtual CommonJS values.
          return value;
        }
      }
      patches.bindCJS(filename);
    }
    return Module._cache[filename]?.exports ?? value;
  }
  Module._load = dispatchLoad = load;
}

export function bindProfileExports(id, names, namespace, setters) {
  active.get(id)?.bindESM(names, namespace, setters);
}

function guardProperty(value, target, invoke, replacing) {
  if (typeof value !== "function")
    throw new AwError("source_verify_target_missing");
  const guard = guards.get(value);
  if (guard?.target === target) return value;
  if (guard && !guard.binding)
    throw new AwError("source_verify_duplicate_binding");
  if (
    !guard &&
    replacing &&
    NATIVE_CODE.test(Function.prototype.toString.call(value))
  )
    throw new AwError("source_verify_opaque_replacement");

  // Bind the destination guard so admission sees the effective receiver and args.
  const binding = guard?.binding;
  const original = binding?.original ?? value;
  verifyFingerprint(target, original);
  const wrapped = wrap(original, target, invoke);
  return binding
    ? Reflect.apply(bindGuard, wrapped, binding.parameters)
    : wrapped;
}

function restoreAll(actions) {
  let failure;
  for (const restore of actions.reverse()) {
    try {
      restore();
    } catch (error) {
      failure ??= error;
    }
  }
  return failure;
}

export class ProfilePatches {
  constructor(profile, options) {
    this.id = randomUUID();
    this.parentURL = applicationURL(options.resolveFrom);
    const require = createRequire(this.parentURL);
    const { root, metadata } = findPackage(require, profile.package.name);
    if (!versionMatches(metadata.version, profile.package.versions))
      throw new AwError("source_probe_package_mismatch");
    this.root = root;
    this.package = profile.package.name;
    this.packageVersion = metadata.version;
    this.modules = new Map();
    this.restores = [];
    this.bound = new WeakMap();
    this.properties = new Set();
    this.collected = new FinalizationRegistry((ref) =>
      this.properties.delete(ref),
    );
    this.enabled = new Int32Array(new SharedArrayBuffer(4));
    for (const target of profile.targets) {
      if (!this.modules.has(target.module)) {
        const found = resolveRequire(require, target.module);
        const path =
          found?.startsWith("/") &&
          insidePackage(pathToFileURL(found).href, root)
            ? found
            : undefined;
        this.modules.set(target.module, {
          name: target.module,
          path,
          targets: [],
        });
      }
      this.modules.get(target.module).targets.push(target);
    }
    this.loaderOptions = {
      id: this.id,
      root,
      parentURL: this.parentURL,
      bridge: import.meta.url,
      enabled: this.enabled,
      modules: [...this.modules].map(([name, { targets }]) => [
        name,
        targets
          .filter(({ qualname }) => !qualname.includes("."))
          .map(({ qualname }) => qualname),
      ]),
    };
  }

  async probe() {
    const names = [...this.modules.values()]
      .filter(({ targets }) => targets.some((target) => target.code_sha256))
      .map(({ name }) => name);
    if (!names.length) return;
    const loaded = parsedFiles();
    if ([...loaded].some((url) => insidePackage(url, this.root))) {
      const resolve = () => resolveProfileModules(names, this.loaderOptions);
      let urls;
      if (Module.registerHooks) {
        const hook = Module.registerHooks({
          resolve: createProfileHooks(this.loaderOptions).resolve,
        });
        try {
          urls = resolve();
        } finally {
          hook.deregister();
        }
      } else {
        urls = await loaderWorker(this.loaderOptions, names);
      }
      for (const [name, url] of urls) this.modules.get(name).importURL = url;
    }
    for (const file of this.modules.values()) {
      const fingerprinted = file.targets.filter(
        ({ code_sha256 }) => code_sha256,
      );
      if (!fingerprinted.length) continue;
      const cached = Module._cache[file.path];
      if (cached?.loaded && !types.isModuleNamespaceObject(cached.exports)) {
        for (const target of fingerprinted) {
          const descriptor = inspectSeam(
            { value: cached.exports },
            target,
            true,
          ).descriptor;
          verifyFingerprint(target, descriptorValue(descriptor));
        }
      }
      const requireURL = file.path && pathToFileURL(file.path).href;
      const url = file.importURL ?? requireURL;
      if (
        loaded.has(url) &&
        (url !== requireURL ||
          !cached ||
          types.isModuleNamespaceObject(cached.exports))
      ) {
        const namespace = await import(url);
        for (const target of fingerprinted) {
          const { descriptor } = inspectSeam(namespace, target, false);
          verifyFingerprint(target, descriptorValue(descriptor));
        }
      }
    }
  }

  install(invoke, onFailure) {
    if ([...active.values()].some((other) => other.root === this.root))
      throw new AwError("source_verify_duplicate_binding");
    this.invoke = invoke;
    this.onFailure = onFailure;
    active.set(this.id, this);
    addDispatcher();
    const loader = createProfileHooks(this.loaderOptions);
    try {
      if (Module.registerHooks) {
        this.hook = Module.registerHooks({
          resolve: loader.resolve,
          load: loader.load,
        });
      } else {
        const url = new URL("./profile-loader.mjs", import.meta.url);
        url.searchParams.set("source", this.id);
        register(url, { data: this.loaderOptions });
      }
      for (const [name, url] of resolveProfileModules(
        this.modules.keys(),
        this.loaderOptions,
      ))
        this.modules.get(name).importURL = url;
      Atomics.store(this.enabled, 0, 1);
    } catch (error) {
      this.restore();
      throw error;
    }
  }

  mayLoad(request) {
    return (
      request === this.package ||
      request.startsWith(this.package + "/") ||
      request.startsWith(".") ||
      request.startsWith(this.root + "/")
    );
  }

  bindCJS(filename, verifying = false) {
    const files = [...this.modules.values()].filter(
      (file) => file.path === filename,
    );
    const cached = Module._cache[filename];
    if (!files.length || !cached?.loaded) return;
    try {
      if (types.isModuleNamespaceObject(cached.exports)) {
        // require(esm) returns a raw namespace even when the import facade exists.
        this.bindESM(
          files.map((file) => file.name),
          cached.exports,
        );
        return;
      }
      for (const { targets } of files) {
        for (const target of targets) {
          const path =
            target.qualname === "default" &&
            typeof cached.exports === "function"
              ? []
              : target.qualname.split(".");
          this.patch(
            cached,
            "exports",
            Object.getOwnPropertyDescriptor(cached, "exports"),
            target,
            path,
          );
        }
      }
    } catch (error) {
      if (verifying) throw error;
      this.onFailure(error);
    }
  }

  bindESM(names, namespace, setters) {
    const wrappedExports = new Set();
    try {
      for (const name of names) {
        const file = this.modules.get(name);
        for (const target of file.targets) {
          const { owner, key, descriptor } = inspectSeam(
            namespace,
            target,
            false,
          );
          if (owner === namespace && setters) {
            if (wrappedExports.has(key))
              throw new AwError("source_verify_duplicate_binding");
            wrappedExports.add(key);
            verifyFingerprint(target, descriptor.value);
            const guarded = wrap(
              descriptor.value,
              target,
              this.invoke,
              () => namespace[key],
            );
            setters[key](guarded);
            this.restores.push(() => {
              const native = namespace[key];
              setters[key](
                typeof native === "function"
                  ? forwardExport(namespace, key)
                  : native,
              );
            });
          } else {
            this.patch(owner, key, descriptor, target);
          }
        }
        if (setters) file.bound = true;
      }
    } catch (error) {
      if (!setters) throw error;
      this.onFailure(error);
    }
  }

  patch(owner, key, descriptor, target, rest = [], undo) {
    owner = guards.get(owner)?.native ?? owner;
    const source = this;
    function descend(value, target, [key, ...tail], undo) {
      const parent = guards.get(value)?.native ?? value;
      const child =
        parent != null && Object.getOwnPropertyDescriptor(parent, key);
      source.patch(parent, key, child, target, tail, undo);
    }
    const known = this.bound.get(owner)?.get(key);
    if (known) {
      if (!known.installed())
        throw new AwError("source_verify_duplicate_binding");
      known.add(target, rest, undo);
      return;
    }
    if (!descriptor || !Object.hasOwn(descriptor, "value"))
      throw new AwError("source_verify_target_missing", { message: target.id });
    // A read-only intermediate binding already keeps its owner stable.
    if (rest.length && !descriptor.writable) {
      descend(descriptor.value, target, rest, undo);
      return;
    }

    const paths = new Map();
    let leaf;
    let native = descriptor.value;
    let exposed = native;
    function add(target, rest, undo) {
      if (paths.has(target)) return;
      const previous = exposed;
      if (rest.length) descend(native, target, rest, undo);
      else {
        if (leaf) throw new AwError("source_verify_duplicate_binding");
        exposed = guardProperty(native, target, source.invoke, !!undo);
        native = guards.get(exposed).native;
        leaf = target;
      }
      paths.set(target, rest);
      undo?.push(() => {
        paths.delete(target);
        if (!rest.length) {
          leaf = undefined;
          native = exposed = previous;
        }
      });
    }
    function write(value, undo) {
      if (exposed === value) return;
      for (const [target, rest] of paths)
        if (rest.length) descend(value, target, rest, undo);
      exposed = leaf ? guardProperty(value, leaf, source.invoke, true) : value;
      native = leaf ? guards.get(exposed).native : value;
    }
    function set(value) {
      const undo = [];
      try {
        if (this === owner) return write(value, undo);
        const slot = source.bound.get(this)?.get(key);
        if (slot?.installed()) return slot.write(value, undo);
        const own = Object.getOwnPropertyDescriptor(this, key);
        if (own ? !own.writable : !Object.isExtensible(this)) return;
        const data = own ?? {
          configurable: true,
          enumerable: true,
          writable: true,
        };
        for (const [target, rest] of paths)
          source.patch(this, key, { ...data, value }, target, rest, undo);
      } catch (error) {
        source.onFailure(restoreAll(undo) ?? error);
      }
    }
    add(target, rest, undo);
    const get = () => exposed;
    const installed = () => {
      const current = Object.getOwnPropertyDescriptor(owner, key);
      return descriptor.writable
        ? current?.get === get
        : current?.value === exposed;
    };
    const guarded = descriptor.writable
      ? {
          get,
          set,
          enumerable: descriptor.enumerable,
          configurable: descriptor.configurable,
        }
      : { ...descriptor, value: exposed };
    const previous = undo && Object.getOwnPropertyDescriptor(owner, key);
    if (!Reflect.defineProperty(owner, key, guarded))
      throw new AwError("source_verify_unwritable_export", {
        message: target.id,
      });
    if (descriptor.writable) guardGetters.add(get);
    const restore = () => {
      if (
        installed() &&
        !Reflect.defineProperty(owner, key, { ...descriptor, value: native })
      )
        throw new AwError("source_cleanup_unconfirmed");
    };
    // Only the owner keeps its patch alive; dead instances need no restoration.
    const record = { add, installed, restore, write };
    if (!this.bound.has(owner)) this.bound.set(owner, new Map());
    this.bound.get(owner).set(key, record);
    const ref = new WeakRef(record);
    this.properties.add(ref);
    this.collected.register(record, ref, ref);
    undo?.push(() => {
      const restored = previous
        ? Reflect.defineProperty(owner, key, previous)
        : Reflect.deleteProperty(owner, key);
      if (!restored) throw new AwError("source_cleanup_unconfirmed");
      this.bound.get(owner).delete(key);
      this.properties.delete(ref);
      this.collected.unregister(ref);
    });
  }

  verifyCurrent(target, original, current) {
    if (current === original) return;
    try {
      if (typeof current !== "function")
        throw new AwError("source_verify_target_missing");
      verifyFingerprint(target, current);
    } catch (error) {
      this.onFailure(error);
    }
  }

  async verify() {
    const loaded = parsedFiles();
    for (const file of this.modules.values()) {
      if (file.path) this.bindCJS(file.path, true);
      if (!file.path && !file.importURL)
        throw new AwError("source_verify_target_missing", {
          message: file.name,
        });
      const url = file.importURL;
      const commonJS =
        file.path &&
        url === pathToFileURL(file.path).href &&
        Module._cache[file.path]?.loaded &&
        !types.isModuleNamespaceObject(Module._cache[file.path].exports);
      if (!url || !loaded.has(url) || file.bound || commonJS) continue;
      // The raw namespace pre-dates our facade. Only mutable nested seams can
      // still be confirmed, since existing named imports hold the raw binding.
      const namespace = await import(
        `aw-profile:${this.id}:original:${encodeURIComponent(url)}`
      );
      if (!active.has(this.id)) throw new AwError("source_unavailable");
      this.bindESM([file.name], namespace);
    }
    for (const ref of this.properties) {
      if (ref.deref()?.installed() === false)
        throw new AwError("source_verify_unwritable_export");
    }
  }

  restore() {
    Atomics.store(this.enabled, 0, 0);
    this.hook?.deregister();
    active.delete(this.id);
    if (!active.size && dispatchLoad) {
      if (Module._load === dispatchLoad) Module._load = originalLoad;
      dispatchLoad = originalLoad = undefined;
    }
    for (const ref of this.properties) {
      const record = ref.deref();
      if (record) this.restores.push(record.restore);
    }
    this.properties.clear();
    this.bound = new WeakMap();
    const failure = restoreAll(this.restores.splice(0));
    if (failure) throw failure;
  }
}
