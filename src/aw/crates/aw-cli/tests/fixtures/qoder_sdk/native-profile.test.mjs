/** Confirm fixed profile adoption in the pinned Worker before releasing any prompt. */
import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, mkdir, rm, access, writeFile, copyFile, symlink, readFile } from "node:fs/promises";
import { openSync, closeSync, writeFileSync } from "node:fs";
import { spawn, execFile } from "node:child_process";
import { promisify } from "node:util";
import { dirname, join, resolve } from "node:path";
import { tmpdir } from "node:os";
import { bounded } from "../../../../aw-adapter/native/qoder-sdk/resident.mjs";

const sdkPath = resolve(process.argv[2]);
const output = resolve(process.argv[3]);
const bridge = await import("../../../../aw-adapter/native/qoder-sdk/bridge.mjs");
const config = { native_policy: { declarative_plugins: true } };
const aw = resolve(process.argv[4]);

async function fixture(t) {
  await mkdir(output, { recursive: true });
  const root = await mkdtemp(join(tmpdir(), "aw-profile-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const environment = { PATH: `${dirname(process.execPath)}:/usr/bin:/bin`, HOME: join(root, "home"),
    QODER_CONFIG_DIR: join(root, "profile"), XDG_CONFIG_HOME: join(root, "config"),
    XDG_RUNTIME_DIR: join(root, "runtime"), TMPDIR: join(root, "tmp"), LANG: "C.UTF-8" };
  for (const key of ["HTTP_PROXY", "HTTPS_PROXY", "NO_PROXY", "http_proxy", "https_proxy", "no_proxy",
    "NODE_EXTRA_CA_CERTS", "SSL_CERT_FILE"])
    if (process.env[key]) environment[key] = process.env[key];
  for (const key of ["HOME", "QODER_CONFIG_DIR", "XDG_CONFIG_HOME", "XDG_RUNTIME_DIR", "TMPDIR"])
    await mkdir(environment[key], { recursive: true, mode: 0o700 });
  return { root, environment };
}

test("declarative profile rejects the unconfirmed direct transport before launch", { timeout: 5000 }, async t => {
  const f = await fixture(t);
  const file = join(f.root, "bridge.json");
  await writeFile(file, JSON.stringify({ ...config, sdk: sdkPath, resident: true,
    executable: process.execPath, binding: join(f.root, "binding"), cwd: f.root,
    ready_path: join(f.root, "ready"), ready_token: "native-profile", hooks: [] }));
  const previous = process.env.AW_QODER_SDK_CONFIG;
  process.env.AW_QODER_SDK_CONFIG = file;
  try {
    await assert.rejects(bridge.createQuery({ prompt: "never dispatched", options: {
      settingSources: [], cwd: f.root } }), /requires a managed daemon/);
    await assert.rejects(access(join(f.root, "ready")), {code:"ENOENT"});
  } finally {
    if (previous === undefined) delete process.env.AW_QODER_SDK_CONFIG;
    else process.env.AW_QODER_SDK_CONFIG = previous;
  }
});

test("installed Bridge acknowledges profile and retires snapshots across two daemon Queries", { timeout: 60000 }, async t => {
  const f = await fixture(t);
  const file = join(f.root, "aw.json");
  const app = join(f.root, "app.mjs");
  await copyFile(new URL("./native-profile-worker.mjs", import.meta.url), app);
  await symlink(dirname(dirname(dirname(dirname(sdkPath)))), join(f.root, "node_modules"), "dir");
  await writeFile(file, JSON.stringify({ apiVersion: "aw/v1alpha1", kind: "AWConfiguration",
    metadata: { name: "native-profile" }, spec: {
      daemon: { startup: "external", endpoint: "auto", state_dir: join(f.root, "state") },
      execution: { guarantee: "native_hook", default_event_budget_ms: 5000 },
      audit: { enabled: true, payload: "metadata_only" },
      agents: { sdk: { adapter: "qoder-sdk", argv: [process.execPath, app], qoder_sdk: { declarative_plugins: true } } },
      providers: {}, events: {},
    } }));
  const children = [];
  const start = (name, args) => {
    const log = openSync(join(output, name + ".log"), "w", 0o600);
    let child;
    try { child = spawn(aw, args, { cwd: f.root, env: { ...process.env, ...f.environment,
      QODER_PROFILE_ROOT: f.root }, stdio: ["ignore", log, log] }); }
    finally { closeSync(log); }
    const exited = new Promise((resolve, reject) => { child.once("error", reject); child.once("exit", resolve); });
    exited.catch(() => {}); children.push({ name, child, exited, args });
    writeFileSync(join(output, "processes.json"), JSON.stringify(children.map(({ name, child, args }) => ({
      name, command: [aw, ...args], pid: child.pid, cwd: f.root, ports: [],
      log: join(output, name + ".log"), deadline_seconds: 55, stop: `kill -TERM ${child.pid}`,
    }))));
    return exited;
  };
  try {
    start("service", ["serve", "--config", file, "--state-dir", join(f.root, "state")]);
    for (let attempt = 0; attempt < 100; attempt++) {
      try { await access(join(f.root, "state/aw.sock")); break; }
      catch { await new Promise(resolve => setTimeout(resolve, 50)); }
    }
    await access(join(f.root, "state/aw.sock"));
    const worker = start("worker", ["run", "--config", file, "--agent", "sdk"]);
    assert.equal(await bounded(worker, 45000), 0, "Bridge profile failed; inspect worker.log");
    const proof = JSON.parse(await readFile(join(f.root, "proof.json"), "utf8"));
    assert.equal(proof.queries, 2); assert.equal(proof.daemon_pid_absent, true);
    await writeFile(join(output, "proof.json"), JSON.stringify(proof));
    const status = await promisify(execFile)(aw, ["service", "status", "--config", file], { timeout: 5000 });
    const result = JSON.parse(status.stdout).result;
    assert.equal(result.bindings, 0); assert.equal(result.events, 0); assert.equal(result.audit_healthy, true);
  } finally {
    for (const { child, exited } of children.reverse()) {
      if (child.exitCode === null && child.signalCode === null) child.kill("SIGTERM");
      try { await bounded(exited, 5000); }
      catch { child.kill("SIGKILL"); await bounded(exited, 5000); }
      await assert.rejects(access(`/proc/${child.pid}`), { code: "ENOENT" });
    }
    for (const { name } of children) {
      const log = join(output, name + ".log");
      if (process.env.QODER_PERSONAL_ACCESS_TOKEN)
        await writeFile(log, (await readFile(log, "utf8")).replaceAll(process.env.QODER_PERSONAL_ACCESS_TOKEN, "[REDACTED]"));
    }
  }
});
