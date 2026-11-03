/** Exercise the installed Bridge and real daemon with no caller-supplied settings. */
import assert from "node:assert/strict";
import { access, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { pathToFileURL } from "node:url";

const bridge = await import(pathToFileURL(process.env.AW_QODER_SDK_BRIDGE));
const config = JSON.parse(await readFile(process.env.AW_QODER_SDK_CONFIG, "utf8"));
const sdk = await import(pathToFileURL(config.sdk));
const root = process.env.QODER_PROFILE_ROOT;
const plugin = join(root, "role");
await mkdir(join(plugin, "skills/profile-proof"), { recursive: true });
await mkdir(join(plugin, ".qoder-plugin"));
await writeFile(join(plugin, ".qoder-plugin/plugin.json"), JSON.stringify({ name: "aw-profile", version: "1.0.0" }));
await writeFile(join(plugin, "skills/profile-proof/SKILL.md"), "---\nname: profile-proof\ndescription: profile proof\n---\nUse an ordinary tool.\n");
const daemon = await bridge.startDaemon({ auth: sdk.accessTokenFromEnv(),
  pathToQoderCLIExecutable: join(dirname(config.sdk), "_worker/qoder-worker-runtime.obf.mjs"),
  initializeTimeoutMs: 10000 });
const pid = daemon.processController.process.pid;
await writeFile(join(root, "daemon.json"), JSON.stringify({ pid, stop: `kill -TERM ${pid}` }));
const snapshots = [];
try {
  for (let index = 0; index < 2; index++) {
    await rm(config.ready_path, { force: true });
    let release, started = false;
    const held = new Promise(resolve => { release = resolve; });
    async function* prompt() {
      assert(existsSync(config.ready_path), "prompt advanced before native profile acknowledgement");
      started = true;
      await held;
    }
    const creating = bridge.createQuery({ prompt: prompt(), options: {
      settingSources: [], daemon, cwd: root, plugins: [{ type: "local", path: plugin }],
    } });
    assert.equal(started, false);
    let query, snapshot;
    try {
      query = await creating;
      for (let attempt = 0; !started && attempt < 100; attempt++)
        await new Promise(resolve => setImmediate(resolve));
      assert.equal(started, true);
      const plugins = await query.listPlugins();
      snapshot = plugins.find(item => item.name === "aw-profile")?.path;
      assert(snapshot?.startsWith(dirname(config.binding) + "/plugins-"));
      assert.notEqual(snapshot, plugin);
      assert(!snapshots.includes(snapshot), "Queries shared a mutable plugin snapshot");
      assert.match(await readFile(join(snapshot, "skills/profile-proof/SKILL.md"), "utf8"), /ordinary tool/);
      snapshots.push(snapshot);
    } finally { release(); await query?.close(); }
    await assert.rejects(access(snapshot), { code: "ENOENT" });
    process.kill(pid, 0);
    assert.equal(daemon.processController.process.pid, pid);
  }
} finally {
  await daemon.stop();
  await assert.rejects(access(`/proc/${pid}`), { code: "ENOENT" });
}
await writeFile(join(root, "proof.json"), JSON.stringify({ queries: 2,
  profile_ack_before_prompt: true, native_snapshot_observed: true,
  snapshots_removed: true, daemon_reused: true, daemon_pid_absent: true }));
