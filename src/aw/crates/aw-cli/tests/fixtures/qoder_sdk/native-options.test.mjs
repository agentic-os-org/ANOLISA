/** Native assets retain declarative roles without granting executable plugin Hooks. */
import test from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdtemp, mkdir, writeFile, readFile, rm, symlink, stat } from "node:fs/promises";
import { dirname, join } from "node:path";
import { pathToFileURL } from "node:url";
import fs from "node:fs";
import { syncBuiltinESMExports } from "node:module";
import { prepareNativeOptions, pluginsAdmitted, runtimeOptions, sdkEntry, verifyNativeSettings } from "../../../../aw-adapter/native/qoder-sdk/native-options.mjs";

async function fixture(t) {
  const root = await mkdtemp(join(process.cwd(), "native-assets-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const plugin = join(root, "role");
  await mkdir(join(plugin, "skills/role"), { recursive: true });
  await writeFile(join(plugin, "plugin.json"), JSON.stringify({ name: "role", version: "1.0.0" }));
  await writeFile(join(plugin, "skills/role/SKILL.md"), "---\nname: role\ndescription: role\n---\nRun a tool.\n");
  const preload = join(root, "preload.mjs");
  await writeFile(preload, "// fixed import\n");
  const worker = join(root, "_worker/qoder-worker-runtime.obf.mjs");
  await mkdir(join(root, "_worker"));
  await writeFile(worker, "// pinned worker\n");
  const grant = { path: preload, sha256: createHash("sha256").update(await readFile(preload)).digest("hex") };
  const config = { resident: true, binding: join(root, "binding"), sdk: join(root, "global.mjs"),
    sdks: { global: join(root, "global.mjs"), cn: join(root, "cn.mjs") },
    native_policy: { declarative_plugins: true, runtime_preloads: [grant] } };
  return { root, plugin, config, preload, worker, grant, options: { plugins: [{ type: "local", path: plugin }] } };
}

test("declarative plugin snapshot preserves role content and retires after use", async t => {
  const f = await fixture(t);
  const admitted = await prepareNativeOptions(f.config, f.options);
  assert(pluginsAdmitted(admitted.options.plugins));
  const snapshot = admitted.options.plugins[0].path;
  assert.notEqual(snapshot, f.plugin);
  await writeFile(join(f.plugin, "skills/role/SKILL.md"), "changed");
  assert.match(await readFile(join(snapshot, "skills/role/SKILL.md"), "utf8"), /Run a tool/);
  await admitted.close();
  await assert.rejects(stat(snapshot), { code: "ENOENT" });
});

test("plugins require an explicit declarative grant", async t => {
  const f = await fixture(t);
  await assert.rejects(prepareNativeOptions({ ...f.config, native_policy: {} }, f.options));
  assert.equal(pluginsAdmitted(f.options.plugins), false);
});

for (const member of ["hooks/hooks.json", ".mcp.json", "settings.json"]) {
  test(`declarative plugins reject executable component ${member}`, async t => {
    const f = await fixture(t);
    await mkdir(join(f.plugin, "hooks"));
    await writeFile(join(f.plugin, member), "{}");
    await assert.rejects(prepareNativeOptions(f.config, f.options));
  });
}

test("manifest cannot redirect Hook discovery", async t => {
  const f = await fixture(t);
  await writeFile(join(f.plugin, "plugin.json"), JSON.stringify({ name: "role", hooks: "skills/hooks.json" }));
  await assert.rejects(prepareNativeOptions(f.config, f.options));
});

test("plugin snapshots reject symlinks outside their declared tree", async t => {
  const f = await fixture(t);
  await symlink(f.preload, join(f.plugin, "skills/role/escape"));
  await assert.rejects(prepareNativeOptions(f.config, f.options));
});

for (const race of ["file-symlink", "directory-symlink", "file-growth"]) {
  test(`descriptor snapshot contains ${race} after metadata validation`, async t => {
    const f = await fixture(t);
    const directory = race === "directory-symlink";
    const target = join(f.plugin, directory ? "skills/role" : "skills/role/SKILL.md");
    const identity = await fs.promises.stat(target);
    const outside = join(f.root, "outside");
    if (directory) await mkdir(outside);
    await writeFile(directory ? join(outside, "SKILL.md") : outside, "outside sentinel\n");
    const originals = { lstat: fs.promises.lstat, open: fs.promises.open };
    let changed = false, result;
    const checked = async metadata => {
      if (!changed && metadata.ino === identity.ino && metadata.dev === identity.dev) {
        changed = true;
        if (race === "file-growth") await fs.promises.truncate(target, 128 * 1024 * 1024 + 1);
        else {
          await fs.promises.rename(target, target + ".original");
          await symlink(outside, target, directory ? "dir" : "file");
        }
      }
      return metadata;
    };
    fs.promises.lstat = async (...args) => checked(await originals.lstat(...args));
    fs.promises.open = async (...args) => {
      const handle = await originals.open(...args), original = handle.stat.bind(handle);
      handle.stat = async (...options) => checked(await original(...options));
      return handle;
    };
    syncBuiltinESMExports();
    try {
      if (race === "file-growth") await assert.rejects(prepareNativeOptions(f.config, f.options));
      else {
        result = await prepareNativeOptions(f.config, f.options);
        assert.match(await readFile(join(result.options.plugins[0].path, "skills/role/SKILL.md"), "utf8"), /Run a tool/);
      }
      assert.equal(changed, true, "deterministic metadata race was not exercised");
    } finally {
      Object.assign(fs.promises, originals); syncBuiltinESMExports();
      await result?.close();
    }
  });
}

test("runtime imports require the admitted digest and exact import syntax", async t => {
  const f = await fixture(t);
  runtimeOptions(f.config, { pathToQoderCLIExecutable: f.worker, executableArgs: ["--import", pathToFileURL(f.preload).href] }, "daemon");
  assert.throws(() => runtimeOptions(f.config, { pathToQoderCLIExecutable: f.worker, executableArgs: ["--eval", "1"] }));
  assert.throws(() => runtimeOptions(f.config, { pathToQoderCLIExecutable: f.worker, executableArgs: ["--import", f.preload, "--import", f.preload] }));
  await writeFile(f.preload, "changed");
  assert.throws(() => runtimeOptions(f.config, { pathToQoderCLIExecutable: f.worker, executableArgs: ["--import", f.preload] }));
});

test("SDK site selection requires a declared pinned package", async t => {
  const f = await fixture(t);
  assert.equal(sdkEntry(f.config, "cn"), f.config.sdks.cn);
  assert.throws(() => sdkEntry(f.config, "unknown"));
  assert.throws(() => sdkEntry({ sdk: f.config.sdk }, "cn"));
});

for (const content of [
  "---\nname: role\ndescription: role\nhooks: {PreToolUse: []}\n---\nbody",
  '---\nname: role\n"h\\u006foks": {}\n---\nbody',
  "---\nname: role\n<<: *hooks\n---\nbody",
  "---\n name: role\n hooks: {}\n---\nbody",
  "---\nname: role\n---\n!`echo unsafe`",
  "---\nname: role\n---\n```!\necho unsafe\n```",
]) test("declarative Markdown refuses active or ambiguous metadata " + content.slice(-20), async t => {
  const f = await fixture(t);
  await writeFile(join(f.plugin, "skills/role/SKILL.md"), content);
  await assert.rejects(prepareNativeOptions(f.config, f.options));
});

test("canonical plugin manifest cannot shadow a declarative root manifest", async t => {
  const f = await fixture(t);
  await mkdir(join(f.plugin, ".qoder-plugin"));
  await writeFile(join(f.plugin, ".qoder-plugin/plugin.json"), JSON.stringify({name:"actual",hooks:"skills/hook.json"}));
  await assert.rejects(prepareNativeOptions(f.config, f.options));
});

test("runtime imports without the pinned official Worker are rejected", async t => {
  const f = await fixture(t);
  assert.throws(() => runtimeOptions(f.config, {executableArgs:["--import",f.preload]}));
});

test("runtime entry rejects an existing nonofficial Worker before Query or daemon launch", async t => {
  const f = await fixture(t);
  const replacement = join(f.root, "qoder-worker-runtime.obf.mjs");
  await writeFile(replacement, "// unadmitted worker\n");
  for (const operation of ["query", "daemon"]) {
    runtimeOptions(f.config, { pathToQoderCLIExecutable: f.worker }, operation);
    assert.throws(() => runtimeOptions(f.config,
      { pathToQoderCLIExecutable: replacement }, operation), /unadmitted/);
  }
});


test("declarative skill permission metadata remains inert and is preserved", async t => {
  const f = await fixture(t);
  const content = "---\nname: role\nrequired_permissions:\n  - workspace.read\nmetadata:\n  category: helper\n---\nUse a tool.\n";
  await writeFile(join(f.plugin, "skills/role/SKILL.md"), content);
  const result = await prepareNativeOptions(f.config, f.options);
  assert.equal(await readFile(join(result.options.plugins[0].path, "skills/role/SKILL.md"), "utf8"), content);
  await result.close();
});

for (const separator of ["\u0085", "\u2028", "\u2029"]) {
  test("alternate YAML line separators cannot conceal active metadata " + separator.charCodeAt(0), async t => {
    const f = await fixture(t);
    await writeFile(join(f.plugin, "skills/role/SKILL.md"), "---\nname: role" + separator + "hooks: {}\n---\nbody");
    await assert.rejects(prepareNativeOptions(f.config, f.options));
  });
}


test("direct Query imports reject before the SDK can silently discard them", async t => {
  const f = await fixture(t);
  const options = { pathToQoderCLIExecutable: f.worker, executableArgs: ["--import", f.preload] };
  assert.throws(() => runtimeOptions(f.config, options));
  runtimeOptions(f.config, options, "daemon");
});


test("native settings acknowledgement requires both flag and effective profile", async () => {
  const config = { native_policy: { declarative_plugins: true } };
  const fixed = { skills: { disableShellExecution: true } };
  for (const settings of [{}, {sources:{flag:fixed}}, {effective:fixed},
    {sources:{flag:fixed},effective:{skills:{disableShellExecution:false}}}])
    await assert.rejects(verifyNativeSettings(config, { request: async () => settings }, value => value));
  await verifyNativeSettings(config, { request: async () => ({sources:{flag:fixed},effective:fixed}) }, value => value);
});

for (const mode of ["bypassPermissions", "dontAsk", "acceptEdits", "default"]) {
  test(`declarative agents cannot override native permissions with ${mode}`, async t => {
    const f = await fixture(t);
    await mkdir(join(f.plugin, "agents"));
    await writeFile(join(f.plugin, "agents/role.md"),
      `---\nname: role\ndescription: role\npermissionMode: ${mode}\n---\nUse a tool.\n`);
    await assert.rejects(prepareNativeOptions(f.config, f.options));
  });
}

for (const path of ["skills/role/SKILL.md", "commands/role.md", "agents/role.md"]) {
  for (const permissions of ["[Bash, Read, mcp__remote__*]", "\n  - Bash\n  - Read"]) {
    test(`declarative ${path} cannot grant allowed-tools ${permissions.trim()}`, async t => {
      const f = await fixture(t);
      await mkdir(dirname(join(f.plugin, path)), { recursive: true });
      await writeFile(join(f.plugin, path),
        `---\nname: role\ndescription: role\nallowed-tools: ${permissions}\n---\nUse a tool.\n`);
      await assert.rejects(prepareNativeOptions(f.config, f.options), /unadmitted/);
      assert.equal(pluginsAdmitted(f.options.plugins), false);
      assert.deepEqual((await fs.promises.readdir(f.root)).filter(name => name.startsWith("plugins-")), []);
    });
  }
}

test("declarative agents retain tool selection without enabling forked skill authorization", async t => {
  const f = await fixture(t);
  await mkdir(join(f.plugin, "agents"));
  const role = "---\nname: role\ndescription: role\ntools: [Read]\n---\nRead a file.\n";
  await writeFile(join(f.plugin, "agents/role.md"), role);
  const result = await prepareNativeOptions(f.config, f.options);
  try { assert.equal(await readFile(join(result.options.plugins[0].path, "agents/role.md"), "utf8"), role); }
  finally { await result.close(); }
  for (const path of ["skills/role/SKILL.md", "commands/role.md"]) {
    for (const metadata of ["context: fork", "agent: role"]) {
      const fork = await fixture(t);
      await mkdir(dirname(join(fork.plugin, path)), { recursive: true });
      await writeFile(join(fork.plugin, path), `---\nname: role\n${metadata}\n---\nUse a tool.\n`);
      await assert.rejects(prepareNativeOptions(fork.config, fork.options), /unadmitted/);
    }
  }
});

for (const removed of [false, true]) {
  test(`snapshot cleanup is idempotent with external removal ${removed}`, async t => {
    const f = await fixture(t);
    const result = await prepareNativeOptions(f.config, f.options);
    if (removed) await rm(dirname(result.options.plugins[0].path), { recursive: true });
    await result.close();
    await result.close();
  });
}

for (const mutation of ["oversize", "growth"]) {
  test(`runtime asset verification bounds reads for ${mutation}`, async t => {
    const f = await fixture(t);
    const limit = 16 * 1024 * 1024;
    const identity = await stat(f.preload, { bigint: true });
    if (mutation === "oversize") await fs.promises.truncate(f.preload, limit + 1);
    const original = { fstatSync: fs.fstatSync, readSync: fs.readSync, readFileSync: fs.readFileSync };
    let inspected = false, bytes = 0, wholeReads = 0;
    fs.fstatSync = (...args) => {
      const metadata = original.fstatSync(...args);
      if (!inspected && metadata.ino === identity.ino && metadata.dev === identity.dev) {
        inspected = true;
        if (mutation === "growth") fs.truncateSync(f.preload, limit + 1);
      }
      return metadata;
    };
    fs.readSync = (...args) => { const count = original.readSync(...args); bytes += count; return count; };
    fs.readFileSync = (...args) => { wholeReads++; return original.readFileSync(...args); };
    syncBuiltinESMExports();
    try {
      assert.throws(() => runtimeOptions({ ...f.config,
        native_policy: { command_wrappers: [f.grant] } }, {}));
      assert.equal(inspected, true);
      assert.equal(wholeReads, 0);
      assert(bytes <= limit + 1);
      if (mutation === "oversize") assert.equal(bytes, 0);
      else assert.equal(bytes, limit + 1);
    } finally { Object.assign(fs, original); syncBuiltinESMExports(); }
  });
}

test("snapshot rejects excessive directory depth without reading any tool content", async t => {
  const f = await fixture(t);
  await mkdir(join(f.plugin, "skills", ...Array(65).fill("d")), { recursive: true });
  await assert.rejects(prepareNativeOptions(f.config, f.options));
});

function assetReads(t) {
  const original = fs.readSync;
  let bytes = 0;
  fs.readSync = (...args) => { const count = original(...args); bytes += count; return count; };
  syncBuiltinESMExports();
  t.after(() => { fs.readSync = original; syncBuiltinESMExports(); });
  return () => bytes;
}

async function stableAssets() {
  // Advance beyond the conservative timestamp window using the real filesystem.
  await new Promise(resolve => setTimeout(resolve, 1100));
}

test("unchanged grants reuse verification across Query configs and daemon imports", async t => {
  const f = await fixture(t), bytes = assetReads(t);
  const config = { ...f.config, native_policy: { ...f.config.native_policy, command_wrappers: [f.grant] } };
  await stableAssets();
  runtimeOptions(config, {});
  const initial = bytes();
  assert(initial > 0);
  for (let index = 0; index < 32; index++) runtimeOptions(JSON.parse(JSON.stringify(config)), {});
  runtimeOptions(config, { pathToQoderCLIExecutable: f.worker, executableArgs: ["--import", f.preload] }, "daemon");
  assert.equal(bytes(), initial, "unchanged grants must not be read again");
  assert.throws(() => runtimeOptions({ native_policy: {
    command_wrappers: [{ ...f.grant, sha256: "0".repeat(64) }] } }, {}), /unadmitted/);
  assert(bytes() > initial, "a changed expected digest must trigger verification");
  runtimeOptions(config, {});
  await fs.promises.rename(f.preload, f.preload + ".original");
  await symlink(f.preload + ".original", f.preload);
  assert.throws(() => runtimeOptions(config, {}), { code: "ELOOP" });
});

test("cached grants reject restored mtime after same-inode edits or inode replacement", async t => {
  const fixtures = [await fixture(t), await fixture(t)], bytes = assetReads(t);
  const timestamp = new Date("2020-01-01T00:00:00Z");
  for (const f of fixtures) await fs.promises.utimes(f.preload, timestamp, timestamp);
  await stableAssets();
  for (const [index, f] of fixtures.entries()) {
    const replacement = index === 1;
    const original = await stat(f.preload, { bigint: true });
    const config = { native_policy: { command_wrappers: [f.grant] } };
    runtimeOptions(config, {});
    const initial = bytes(), changed = Buffer.from(await readFile(f.preload));
    changed[0] ^= 1;
    const target = replacement ? f.preload + ".replacement" : f.preload;
    await writeFile(target, changed);
    await fs.promises.utimes(target, timestamp, timestamp);
    if (replacement) await fs.promises.rename(target, f.preload);
    const current = await stat(f.preload, { bigint: true });
    assert.equal(current.size, original.size);
    assert.equal(current.mtimeNs, original.mtimeNs);
    if (replacement) assert.notEqual(current.ino, original.ino);
    else {
      assert.equal(current.ino, original.ino);
      assert.notEqual(current.ctimeNs, original.ctimeNs);
    }
    assert.throws(() => runtimeOptions(config, {}), /unadmitted/);
    assert(bytes() > initial, "changed identity must trigger content verification");
  }
});

test("verification rejects same-size mutation after its bytes were hashed", async t => {
  const f = await fixture(t), original = fs.readSync;
  const changed = Buffer.from(await readFile(f.preload));
  changed[0] ^= 1;
  let mutated = false;
  fs.readSync = (...args) => {
    const count = original(...args);
    if (count && !mutated) {
      mutated = true;
      fs.writeFileSync(f.preload, changed);
      fs.utimesSync(f.preload, new Date("2020-01-01T00:00:00Z"), new Date("2020-01-01T00:00:00Z"));
    }
    return count;
  };
  syncBuiltinESMExports();
  try {
    assert.throws(() => runtimeOptions({ native_policy: { command_wrappers: [f.grant] } }, {}), /unadmitted/);
    assert.equal(mutated, true);
  } finally { fs.readSync = original; syncBuiltinESMExports(); }
  assert.throws(() => runtimeOptions({ native_policy: { command_wrappers: [f.grant] } }, {}), /unadmitted/);
});

test("verification cache evicts old entries without imposing an admission limit", async t => {
  const f = await fixture(t), bytes = assetReads(t);
  const content = await readFile(f.preload);
  const grants = [];
  for (let index = 0; index < 64; index++) {
    const path = join(f.root, `consumer-${index}`);
    await writeFile(path, content);
    grants.push({ ...f.grant, path });
  }
  await stableAssets();
  runtimeOptions({ native_policy: { command_wrappers: [f.grant] } }, {});
  for (const grant of grants) runtimeOptions({ native_policy: { command_wrappers: [grant] } }, {});
  const initial = bytes();
  runtimeOptions({ native_policy: { command_wrappers: [f.grant] } }, {});
  assert(bytes() > initial, "the oldest entry must have been evicted");
});

test("recent and coarse timestamps never reuse cached content verification", async t => {
  const f = await fixture(t), bytes = assetReads(t);
  const config = { native_policy: { command_wrappers: [f.grant] } };
  runtimeOptions(config, {});
  const initial = bytes();
  runtimeOptions(config, {});
  assert(bytes() > initial, "recent file must be read again even without a metadata change");
  const original = fs.fstatSync;
  fs.fstatSync = (...args) => {
    const metadata = original(...args);
    metadata.ctimeNs = metadata.ctimeNs / 1000000000n * 1000000000n - 2000000000n;
    return metadata;
  };
  syncBuiltinESMExports();
  try {
    runtimeOptions(config, {});
    const before = bytes();
    runtimeOptions(config, {});
    assert(bytes() > before, "whole-second timestamps cannot establish a cache hit");
  } finally { fs.fstatSync = original; syncBuiltinESMExports(); }
});

test("snapshot entry budget counts empty directories", { timeout: 30000 }, async t => {
  const f = await fixture(t);
  for (let start = 0; start < 10000; start += 100)
    await Promise.all(Array.from({ length: 100 }, (_, offset) =>
      mkdir(join(f.plugin, "skills", `empty-${start + offset}`))));
  await assert.rejects(prepareNativeOptions(f.config, f.options));
});

test("snapshot rejects unadmitted root entries before traversing them", async t => {
  const f = await fixture(t);
  await mkdir(join(f.plugin, "unadmitted"));
  const original = fs.promises.open;
  let traversed = false;
  fs.promises.open = async (...args) => {
    if (String(args[0]).endsWith("/unadmitted")) traversed = true;
    return await original(...args);
  };
  syncBuiltinESMExports();
  try {
    await assert.rejects(prepareNativeOptions(f.config, f.options));
    assert.equal(traversed, false);
  } finally { fs.promises.open = original; syncBuiltinESMExports(); }
});
