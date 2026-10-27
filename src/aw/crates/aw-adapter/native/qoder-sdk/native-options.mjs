/** Admit pinned execution consumers and snapshot declarative SDK plugin assets. */
import { createHash } from "node:crypto";
import { lstat, readFile, readdir, mkdir, writeFile, mkdtemp, rm, open } from "node:fs/promises";
import { openSync, fstatSync, readSync, closeSync, realpathSync, constants } from "node:fs";
import { dirname, isAbsolute, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const admitted = new WeakSet();
const unsupported = () => new Error("AW rejected unadmitted runtime overrides or plugin assets");
const noFollow = constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK;
const verifiedAssets = new Map();

function assetIdentity(stat) {
  // ctime prevents restored mtime from concealing an in-place edit.
  return [stat.dev, stat.ino, stat.size, stat.mtimeNs, stat.ctimeNs].join(":");
}

export function sdkEntry(config, site = config.active_site ?? config.default_site ?? "global") {
  const entry = config.sdks ? config.sdks[site] : site === "global" ? config.sdk : undefined;
  if (typeof entry !== "string" || !isAbsolute(entry))
    throw new Error("AW Qoder SDK site has no admitted absolute SDK entry");
  return entry;
}

function verified(grant) {
  const descriptor = openSync(grant.path, noFollow);
  try {
    const limit = 16 * 1024 * 1024;
    const stat = fstatSync(descriptor, { bigint: true });
    if (!stat.isFile() || stat.size > limit) throw unsupported();
    const identity = assetIdentity(stat), cached = verifiedAssets.get(grant.path);
    // Linux inode times may share one clock tick. Recent or whole-second
    // timestamps cannot safely establish an unchanged generation.
    const cacheable = stat.ctimeNs % 1000000000n !== 0n &&
      stat.ctimeNs < BigInt(Date.now() - 1000) * 1000000n;
    if (cacheable && cached?.sha256 === grant.sha256 && cached.identity === identity) return grant.path;
    verifiedAssets.delete(grant.path);
    const hash = createHash("sha256"), chunk = Buffer.alloc(65536);
    let bytes = 0;
    for (;;) {
      const count = readSync(descriptor, chunk, 0, Math.min(chunk.length, limit - bytes + 1), null);
      if (!count) break;
      bytes += count;
      if (bytes > limit) throw unsupported();
      hash.update(chunk.subarray(0, count));
    }
    if (hash.digest("hex") !== grant.sha256 ||
        assetIdentity(fstatSync(descriptor, { bigint: true })) !== identity) throw unsupported();
    // Eviction costs a later rehash; cache capacity never limits task admission.
    if (cacheable) {
      if (verifiedAssets.size >= 64) verifiedAssets.delete(verifiedAssets.keys().next().value);
      verifiedAssets.set(grant.path, { identity, sha256: grant.sha256 });
    }
    return grant.path;
  } finally { closeSync(descriptor); }
}

/** Runtime replacement remains limited to the selected official Worker and pinned imports. */
export function runtimeOptions(config, options, operation = "query") {
  if (options.executable !== undefined || options.spawnQoderCLIProcess !== undefined)
    throw unsupported();
  if (options.pathToQoderCLIExecutable != null) {
    const declared = join(dirname(sdkEntry(config, options.awSite)), "_worker/qoder-worker-runtime.obf.mjs");
    try {
      if (realpathSync(options.pathToQoderCLIExecutable) !== realpathSync(declared)) throw unsupported();
    } catch { throw unsupported(); }
  }
  if (options.executableArgs !== undefined) {
    if (operation !== "daemon") throw unsupported();
    const args = options.executableArgs;
    if (!Array.isArray(args) || args.length < 2 || args.length % 2 ||
        options.pathToQoderCLIExecutable == null) throw unsupported();
    const used = new Set();
    for (let index = 0; index < args.length; index += 2) {
      if (args[index] !== "--import" || typeof args[index + 1] !== "string") throw unsupported();
      const path = args[index + 1].startsWith("file:") ? fileURLToPath(args[index + 1]) : args[index + 1];
      const grant = config.native_policy?.runtime_preloads?.find(item => item.path === path);
      if (!grant || used.has(path)) throw unsupported();
      verified(grant);
      used.add(path);
    }
  }
  for (const grant of config.native_policy?.command_wrappers ?? []) verified(grant);
}

export function pluginsAdmitted(plugins) {
  return !plugins?.length || admitted.has(plugins);
}

// The Worker executes skill/command interpolation and frontmatter Hooks outside
// its model-authored Bash boundary. This profile accepts a closed metadata
// vocabulary; permission overlays such as allowed-tools and permissionMode
// cannot silently grant native authorization through these assets.
function declarativeMarkdown(bytes) {
  const text = bytes.toString("utf8").replace(/\r\n?|[\u0085\u2028\u2029]/g, "\n");
  if (/!`|```[ \t]*!/.test(text)) throw unsupported();
  const front = /^\s*---\n([\s\S]*?)\n---(?:\n|$)/.exec(text);
  if (!front) {
    if (/^\s*---/.test(text)) throw unsupported();
    return;
  }
  const fields = new Set(["name", "description", "tools", "model", "color",
    "argument-hint", "disable-model-invocation", "user-invocable",
    "license", "compatibility", "metadata", "required_permissions"]);
  const seen = new Set();
  let field;
  for (const line of front[1].split("\n")) {
    if (!line.trim() || /^\s*#/.test(line)) continue;
    if (/^[ \t]/.test(line)) {
      if (!field) throw unsupported();
      continue;
    }
    const key = /^([A-Za-z][A-Za-z_-]*):(?:[ \t]|$)/.exec(line)?.[1];
    if (!fields.has(key) || seen.has(key)) throw unsupported();
    seen.add(key); field = key;
  }
  if (!seen.size) throw unsupported();
}

function declarativeManifest(metadata) {
  if (!metadata || Array.isArray(metadata) || typeof metadata !== "object" ||
      typeof metadata.name !== "string" || !metadata.name ||
      Object.keys(metadata).some(key => !["name", "version", "description", "author", "license",
        "keywords", "homepage", "repository"].includes(key))) throw unsupported();
}

async function openRoot(source) {
  let handle = await open("/", noFollow | constants.O_DIRECTORY);
  try {
    // Anchor every ancestor to an owned descriptor; a renamed directory cannot
    // redirect later child opens through a replacement symlink.
    const parts = source.split("/").filter(Boolean);
    for (const [index, part] of parts.entries()) {
      const next = await open(`/proc/self/fd/${handle.fd}/${part}`,
        noFollow | (index < parts.length - 1 ? constants.O_DIRECTORY : 0));
      await handle.close();
      handle = next;
    }
    return handle;
  } catch (error) { await handle.close(); throw error; }
}

/** Freeze role/skill assets; SDK plugin Hooks, MCP and settings use their managed factories. */
export async function prepareNativeOptions(config, options) {
  if (!options.plugins?.length) return { options, close: async () => {} };
  if (!config.native_policy?.declarative_plugins || !config.resident || !Array.isArray(options.plugins))
    throw unsupported();
  const root = await mkdtemp(join(dirname(config.binding), "plugins-"));
  let entries = 0, bytes = 0;
  const directories = new Set(["skills", "agents", "commands", ".qoder-plugin"]);
  const allowedFiles = /^(?:plugin\.json|system-prompt\.md|README(?:_zh)?\.md|LICENSE(?:\.md)?|NOTICE)$/;
  async function snapshot(handle, source, target, depth = 0) {
    if (++entries > 10000 || depth > 64) throw unsupported();
    const stat = await handle.stat();
    if (stat.isDirectory()) {
      const names = await readdir(`/proc/self/fd/${handle.fd}`);
      if (depth === 0 && names.some(name => !directories.has(name) && !allowedFiles.test(name)))
        throw unsupported();
      await mkdir(target, { mode: 0o700 });
      for (const name of names) {
        const child = await open(`/proc/self/fd/${handle.fd}/${name}`, noFollow);
        try { await snapshot(child, join(source, name), join(target, name), depth + 1); }
        finally { await child.close(); }
      }
    } else if (stat.isFile()) {
      const limit = 128 * 1024 * 1024;
      if (bytes + stat.size > limit) throw unsupported();
      const chunks = [];
      for (;;) {
        const chunk = Buffer.alloc(Math.min(65536, limit - bytes + 1));
        const { bytesRead } = await handle.read(chunk);
        if (!bytesRead) break;
        bytes += bytesRead;
        if (bytes > limit) throw unsupported();
        chunks.push(chunk.subarray(0, bytesRead));
      }
      const content = Buffer.concat(chunks);
      if (/\.md$/i.test(source)) declarativeMarkdown(content);
      await writeFile(target, content, { mode: 0o600, flag: "wx" });
    } else throw unsupported();
  }
  try {
    const plugins = [];
    for (const [index, plugin] of options.plugins.entries()) {
      if (!plugin || Object.keys(plugin).some(key => !["type", "path"].includes(key)) ||
          plugin.type !== "local" || typeof plugin.path !== "string" || !isAbsolute(plugin.path))
        throw unsupported();
      const target = join(root, String(index));
      const source = await openRoot(plugin.path);
      try { await snapshot(source, plugin.path, target); }
      finally { await source.close(); }
      const names = await readdir(target);
      for (const name of names) {
        const stat = await lstat(join(target, name));
        if (stat.isDirectory() ? !directories.has(name) : !allowedFiles.test(name)) throw unsupported();
      }
      if (!names.includes("plugin.json") && !names.includes(".qoder-plugin")) throw unsupported();
      if (names.includes("plugin.json"))
        declarativeManifest(JSON.parse(await readFile(join(target, "plugin.json"), "utf8")));
      if (names.includes(".qoder-plugin")) {
        if ((await readdir(join(target, ".qoder-plugin"))).some(name => name !== "plugin.json"))
          throw unsupported();
        declarativeManifest(JSON.parse(await readFile(join(target, ".qoder-plugin/plugin.json"), "utf8")));
      }
      plugins.push({ type: "local", path: target });
    }
    admitted.add(plugins);
    return { options: { ...options, plugins }, close: async () => { await rm(root, { recursive: true, force: true }); } };
  } catch (error) {
    await rm(root, { recursive: true, force: true });
    throw error;
  }
}


/** Native readiness must confirm the fixed profile before any prompt is released. */
export async function verifyNativeSettings(config, query, wait) {
  if (!config.native_policy?.declarative_plugins) return;
  if (typeof query.request !== "function") throw unsupported();
  const settings = await wait(query.request({ type: "get_settings" }));
  if (settings?.sources?.flag?.skills?.disableShellExecution !== true ||
      settings?.effective?.skills?.disableShellExecution !== true)
    throw new Error("AW native declarative profile was not adopted");
}
