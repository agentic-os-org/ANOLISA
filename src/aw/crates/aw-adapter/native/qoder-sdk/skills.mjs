/** Resolve the pinned Worker's local Skill catalogs at the public PreTool boundary. */
import { readdir, realpath, stat, open } from "node:fs/promises";
import { constants } from "node:fs";
import { basename, dirname, isAbsolute, join, relative } from "node:path";

const failure = () => new Error("skill_resolution_error");
const extendedQueries = new WeakSet();
const readQueries = new WeakSet();

/** Track reads even before an idle policy update enables Skill checking. */
export function noteSkillRead(payload, query) {
  if (payload.hook_event_name === "PreToolUse" && payload.tool_name === "Read") readQueries.add(query);
}

/** Retain native directory grants while fencing unsupported Skill sources. */
export function bindSkillQuery(query, enabled) {
  if (enabled && typeof query.addDirectories === "function") {
    const add = query.addDirectories.bind(query);
    query.addDirectories = (...args) => {
      extendedQueries.add(query);
      return add(...args);
    };
  }
  return query;
}

async function manifest(directory) {
  const path = join(directory, "SKILL.md");
  let metadata;
  try { metadata = await stat(path); }
  catch (error) { if (error.code === "ENOENT") return false; throw error; }
  if (!metadata.isFile() || metadata.size > 1024 * 1024) throw failure();
  const file = await open(path, constants.O_RDONLY | constants.O_NONBLOCK);
  try {
    if (!(await file.stat()).isFile()) throw failure();
    // Count every asset as a competitor, including files the native parser may
    // skip. A skipped higher-priority file must never authorize a fallback.
    await file.readFile("utf8");
  } finally { await file.close(); }
  return true;
}

async function catalog(root, name) {
  let entries;
  try { entries = await readdir(root, { withFileTypes: true }); }
  catch (error) { if (error.code === "ENOENT") return []; throw error; }
  const matches = [];
  const visited = new Set();
  let count = 0;
  async function walk(directory, entries, depth) {
    for (const entry of entries) {
      if (["node_modules", ".git"].includes(entry.name)) continue;
      if (++count > 20000) throw failure();
      if (!entry.isDirectory() && !entry.isSymbolicLink()) continue;
      const path = join(directory, entry.name);
      const target = await realpath(path);
      if (visited.has(target)) continue;
      visited.add(target);
      if (!(await stat(target)).isDirectory()) continue;
      if (await manifest(path)) {
        // The Worker uses the leaf directory as identity; frontmatter name
        // supplies displayName. Nested duplicate leaf names are ambiguous.
        if (entry.name.toLowerCase() === name.toLowerCase()) matches.push({ name: entry.name, directory: target });
      } else if (depth < 4) {
        await walk(path, await readdir(path, { withFileTypes: true }), depth + 1);
      } else throw failure();
    }
  }
  await walk(root, entries, 1);
  return matches;
}

async function selectSkill(payload, options, query, signal) {
  const name = payload.tool_input?.skill;
  if (typeof name !== "string" || !name || name.length > 1024 || /[\\/:\0]/.test(name)) throw failure();
  if (options.additionalDirectories?.length || extendedQueries.has(query)) throw failure();
  if (Object.keys(options.extraArgs ?? {}).some(key =>
    ["setting-sources", "settingSources", "add-dir", "addDir",
      "enable-builtin-skills", "enableBuiltinSkills"].includes(key))) throw failure();
  const settings = await query.request({ type: "get_settings" }, { signal, timeoutMs: 1000 });
  if (!settings?.effective) throw failure();
  const declared = ["flag", "local", "workspace", "user"]
    .map(source => settings.sources?.[source]?.skills?.loadFromAgentsDirectory)
    .find(value => typeof value === "boolean");
  const agents = declared ?? false;
  const cwd = payload.cwd;
  const transcript = payload.transcript_path;
  if (!isAbsolute(cwd) || typeof transcript !== "string" || !isAbsolute(transcript)) throw failure();
  // The Worker supplies its actual global config path through the transcript;
  // this respects custom config/home directories and managed daemon identity.
  const user = dirname(dirname(dirname(transcript)));
  if (basename(dirname(dirname(transcript))) !== "projects") throw failure();
  const environment = { ...process.env, ...(options.env ?? {}) };
  const home = environment.QODER_CLI_HOME ?? environment.QODERCN_CLI_HOME ?? environment.HOME;
  const sources = options.settingSources ?? ["user", "project", "local"];
  // shortcut: Read can retain uninspectable dynamic Skills; allow project checks
  // after reads only when the native API can identify or clear that cache.
  if (sources.includes("project") && readQueries.has(query)) throw failure();
  const buckets = [
    { source: "user", base: user, agents: home && join(home, ".agents", "skills") },
    { source: "project", base: join(cwd, environment.QODER_CONFIG_DIR_NAME ?? environment.QODERCN_CONFIG_DIR_NAME ?? ".qoder"), agents: join(cwd, ".agents", "skills") },
  ].filter(bucket => sources.includes(bucket.source));
  // Dynamic ancestor resources can override session catalogs. A public Hook
  // cannot identify their active cache; reject a competing ancestor object.
  for (let parent = dirname(cwd); parent !== dirname(parent); parent = dirname(parent)) {
    if ((await catalog(join(parent, ".qoder", "skills"), name)).length ||
        (agents && (await catalog(join(parent, ".agents", "skills"), name)).length)) throw failure();
  }
  const candidates = [];
  for (const bucket of buckets) {
    const preferred = await catalog(join(bucket.base, "skills"), name);
    if (preferred.length > 1) throw failure();
    candidates.push(...preferred.map(skill => ({ ...skill, source: bucket.source })));
    if (agents && bucket.agents) {
      const compatible = await catalog(bucket.agents, name);
      if (compatible.length > 1) throw failure();
      candidates.push(...compatible.map(skill => ({ ...skill, source: bucket.source })));
    }
    // A matching legacy command has a different naming/signing root and can
    // participate in the Worker's case-sensitive resource merge.
    if (await commandMatches(join(bucket.base, "commands"), name)) throw failure();
    // Workflows become Skills before resource Skills are merged. Their identity
    // comes from JavaScript metadata, not the filename; without a selected-path
    // API, any local Workflow candidate makes this signing root unprovable.
    if (await hasWorkflows(join(bucket.base, "workflows"))) throw failure();
  }
  // Native parsing can discard a higher-priority asset. The public control
  // API exposes no selected path, so only a unique local asset is checkable.
  if (candidates.length !== 1) throw failure();
  return { ...candidates[0], name };
}

const identity = metadata => [metadata.dev, metadata.ino, metadata.size,
  metadata.mtimeNs, metadata.ctimeNs].join(":");

/** Keep discovery failures as metadata so the selected Hook's on_error applies. */
export async function resolveSkill(payload, options, query, signal) {
  if (payload.tool_name !== "Skill" || payload.hook_event_name !== "PreToolUse") return;
  try {
    const selected = await selectSkill(payload, options, query, signal);
    return await pin(payload, options, query, signal, selected);
  } catch {
    payload.aw_skill = { error_code: "skill_resolution_error" };
  }
}

async function pin(payload, options, query, signal, selected) {
  const path = join(selected.directory, "SKILL.md");
  const before = identity(await stat(path, { bigint: true }));
  // Refresh before native execution, but do not use the UI listing as admission:
  // model-only Skills intentionally do not appear there. Uniqueness establishes
  // the check target; file identity brackets reload and Provider execution.
  const snapshot = await query.request({ type: "reload_skills" }, { signal, timeoutMs: 1000 });
  if (!Array.isArray(snapshot?.skills)) throw failure();
  const verify = async () => {
    const after = await selectSkill(payload, options, query, signal);
    if (JSON.stringify(after) !== JSON.stringify(selected)) throw failure();
    if (extendedQueries.has(query) || await realpath(selected.directory) !== selected.directory ||
        identity(await stat(path, { bigint: true })) !== before) throw failure();
  };
  await verify();
  payload.aw_skill = selected;
  return verify;
}

async function hasWorkflows(root) {
  let entries;
  try { entries = await readdir(root, { withFileTypes: true }); }
  catch (error) { if (error.code === "ENOENT") return false; throw error; }
  if (entries.length > 20000) throw failure();
  for (const entry of entries) {
    if (entry.name.startsWith(".")) continue;
    if (entry.isFile() && entry.name.endsWith(".js")) return true;
    if (!entry.isDirectory()) continue;
    // Match the pinned Worker's two directory entrypoints, including a symlink
    // at either entrypoint. Metadata parsing is deliberately left to the Worker.
    for (const name of ["workflow.js", "index.js"]) {
      try { await stat(join(root, entry.name, name)); return true; }
      catch (error) { if (error.code !== "ENOENT") throw error; }
    }
  }
  return false;
}

async function commandMatches(root, name) {
  let count = 0;
  const visited = new Set();
  async function walk(directory) {
    let entries;
    try { entries = await readdir(directory, { withFileTypes: true }); }
    catch (error) { if (error.code === "ENOENT") return false; throw error; }
    for (const entry of entries) {
      if (["node_modules", ".git"].includes(entry.name)) continue;
      if (++count > 20000) throw failure();
      const path = join(directory, entry.name);
      const target = await realpath(path);
      if (visited.has(target)) continue;
      visited.add(target);
      const metadata = await stat(target);
      if (metadata.isDirectory()) {
        if (await walk(path)) return true;
      } else if (metadata.isFile() && entry.name.toLowerCase().endsWith(".md")) {
        const skillFile = entry.name.toLowerCase() === "skill.md";
        const id = (skillFile ? relative(root, dirname(path)) :
          relative(root, join(dirname(path), basename(path, ".md")))).split("/").join(":");
        if (id.toLowerCase() === name.toLowerCase()) return true;
      }
    }
    return false;
  }
  return await walk(root);
}
