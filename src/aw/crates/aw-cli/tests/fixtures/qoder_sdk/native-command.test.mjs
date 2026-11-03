/** Required hosted acceptance: installed SDK, official Worker and real wrapper processes. */
import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, mkdir, rm, access, writeFile, copyFile, symlink, readFile, readlink, readdir } from "node:fs/promises";
import { openSync, closeSync, writeFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { spawn, execFile } from "node:child_process";
import { promisify } from "node:util";
import { dirname, join, resolve } from "node:path";
import { tmpdir } from "node:os";
import { bounded } from "../../../../aw-adapter/native/qoder-sdk/resident.mjs";

const sdkPath = resolve(process.argv[2]), output = resolve(process.argv[3]), aw = resolve(process.argv[4]);
assert(process.env.QODER_PERSONAL_ACCESS_TOKEN, "hosted Worker acceptance requires a Qoder test credential");
async function ownedProcess(record, root) {
  assert.equal(record.cwd, root, "cleanup record escaped this case");
  assert(Number.isSafeInteger(record.pid) && record.pid > 1);
  assert.match(record.start_ticks, /^\d+$/);
  try {
    const fields = (await readFile(`/proc/${record.pid}/stat`, "utf8")).split(") ")[1].split(" ");
    if (fields[19] !== record.start_ticks) return false;
    if (["Z", "X"].includes(fields[0])) return true;
    assert.equal(await readlink(`/proc/${record.pid}/cwd`), root);
    const command = (await readFile(`/proc/${record.pid}/cmdline`, "utf8")).split("\0").filter(Boolean);
    if (record.kind === "native") assert.deepEqual(command, record.command);
    else {
      assert.equal(record.kind, "consumer");
      assert(command.some(value => value.includes(join(root, "wrapper.mjs")) || value.includes(join(root, "logical.mjs"))));
    }
    if (record.group) assert.equal(Number(fields[2]), record.pid, "consumer no longer owns its process group");
    return true;
  } catch (error) {
    if (error.code !== "ENOENT") throw error;
    try {
      const fields = (await readFile(`/proc/${record.pid}/stat`, "utf8")).split(") ")[1].split(" ");
      return fields[19] === record.start_ticks;
    } catch (missing) { if (missing.code === "ENOENT") return false; throw missing; }
  }
}
async function stopOwned(record, root) {
  for (const signal of ["SIGTERM", "SIGKILL"]) {
    if (!await ownedProcess(record, root)) return;
    try { process.kill(record.group ? -record.pid : record.pid, signal); }
    catch (error) { if (error.code !== "ESRCH") throw error; }
    const deadline = Date.now() + 3000;
    while (await ownedProcess(record, root)) {
      if (Date.now() >= deadline) break;
      await new Promise(resolve => setTimeout(resolve, 20));
    }
  }
  assert.equal(await ownedProcess(record, root), false, `owned PID ${record.pid} survived cleanup`);
}
for (const mode of ["daemon-allow", "daemon-deny", "daemon-rule-deny", "daemon-accept-edits-rule-deny",
  "daemon-accept-edits-allow", "daemon-timeout-eof", "direct-close"])
  test(`official Worker command wrapper ${mode}`, { timeout: 150000 }, async t => {
    const evidence = join(output, mode);
    await mkdir(evidence, { recursive: true });
    const root = await mkdtemp(join(tmpdir(), "aw-command-"));
    t.after(() => rm(root, { recursive: true, force: true }));
    const environment = { PATH: `${dirname(process.execPath)}:/usr/bin:/bin`, HOME: join(root, "home"),
      QODER_CONFIG_DIR: join(root, "profile"), XDG_CONFIG_HOME: join(root, "config"),
      XDG_RUNTIME_DIR: join(root, "runtime"), TMPDIR: join(root, "tmp"), LANG: "C.UTF-8",
      QODER_PERSONAL_ACCESS_TOKEN: process.env.QODER_PERSONAL_ACCESS_TOKEN,
      QODER_COMMAND_ROOT: root, QODER_COMMAND_MODE: mode };
    for (const key of ["HTTP_PROXY", "HTTPS_PROXY", "NO_PROXY", "http_proxy", "https_proxy", "no_proxy",
      "NODE_EXTRA_CA_CERTS", "SSL_CERT_FILE"])
      if (process.env[key]) environment[key] = process.env[key];
    for (const key of ["HOME", "QODER_CONFIG_DIR", "XDG_CONFIG_HOME", "XDG_RUNTIME_DIR", "TMPDIR"])
      await mkdir(environment[key], { recursive: true, mode: 0o700 });
    const app = join(root, "app.mjs"), wrapper = join(root, "wrapper.mjs"), file = join(root, "aw.json");
    await copyFile(new URL("./native-command-worker.mjs", import.meta.url), app);
    const consumer = `#!${process.execPath}\n` + await readFile(new URL("./native-command-consumer.mjs", import.meta.url), "utf8");
    await writeFile(wrapper, consumer, { mode: 0o700 });
    await symlink(dirname(dirname(dirname(dirname(sdkPath)))), join(root, "node_modules"), "dir");
    await writeFile(file, JSON.stringify({ apiVersion: "aw/v1alpha1", kind: "AWConfiguration",
      metadata: { name: "native-command" }, spec: {
        daemon: { startup: "external", endpoint: "auto", state_dir: join(root, "state") },
        execution: { guarantee: "native_hook", default_event_budget_ms: 5000 },
        audit: { enabled: true, payload: "metadata_only" },
        agents: { sdk: { adapter: "qoder-sdk", argv: [process.execPath, app], qoder_sdk: {
          command_wrappers: [{ path: wrapper, sha256: createHash("sha256").update(consumer).digest("hex") }],
        } } }, providers: {}, events: {},
      } }));
    const children = [];
    const start = (name, args) => {
      const log = openSync(join(evidence, name + ".log"), "w", 0o600);
      let child;
      try { child = spawn(aw, args, { cwd: root, env: environment, stdio: ["ignore", log, log] }); }
      finally { closeSync(log); }
      const exited = new Promise((resolve, reject) => { child.once("error", reject); child.once("exit", resolve); });
      exited.catch(() => {}); children.push({ name, child, exited, args });
      writeFileSync(join(evidence, "processes.json"), JSON.stringify(children.map(({ name, child, args }) => ({
        name, command: [aw, ...args], pid: child.pid, cwd: root, ports: [], log: join(evidence, name + ".log"),
        deadline_seconds: 140, stop: `kill -TERM ${child.pid}`,
      }))));
      return exited;
    };
    const failures = [];
    try {
      start("service", ["serve", "--config", file, "--state-dir", join(root, "state")]);
      for (let attempt = 0; attempt < 100; attempt++) {
        try { await access(join(root, "state/aw.sock")); break; }
        catch { await new Promise(resolve => setTimeout(resolve, 50)); }
      }
      await access(join(root, "state/aw.sock"));
      assert.equal(await bounded(start("worker", ["run", "--config", file, "--agent", "sdk"]), 130000),
        0, "official Worker wrapper failed; inspect worker.log");
      const proof = JSON.parse(await readFile(join(root, "proof.json"), "utf8"));
      assert.equal(proof.mode, mode); assert.equal(proof.cleanup, true);
      await writeFile(join(evidence, "proof.json"), JSON.stringify(proof));
      await copyFile(join(root, "native-processes.json"), join(evidence, "native-processes.json"));
      const status = await promisify(execFile)(aw, ["service", "status", "--config", file], { timeout: 5000, env: environment });
      const result = JSON.parse(status.stdout).result;
      assert.equal(result.bindings, 0); assert.equal(result.events, 0); assert.equal(result.audit_healthy, true);
    } catch (error) { failures.push(error); }
    finally {
      const attempt = async action => { try { await action(); } catch (error) { failures.push(error); } };
      for (const { child, exited } of children.reverse()) {
        await attempt(async () => {
          if (child.exitCode === null && child.signalCode === null) child.kill("SIGTERM");
          try { await bounded(exited, 5000); }
          catch { child.kill("SIGKILL"); await bounded(exited, 5000); }
        });
        await attempt(() => assert.rejects(access(`/proc/${child.pid}`), { code: "ENOENT" }));
      }
      // Worker failures must not strand detached consumers or skip later cleanup.
      const owned = [];
      await attempt(async () => {
        const file = join(root, "native-processes.json");
        try { owned.push(...JSON.parse(await readFile(file, "utf8"))); }
        catch (error) { if (error.code !== "ENOENT") throw error; }
      });
      await attempt(async () => {
        for (const name of await readdir(root)) if (/^[a-f0-9]{32}\.started$/.test(name))
          await attempt(async () => owned.unshift(...JSON.parse(await readFile(join(root, name), "utf8")).processes));
      });
      for (const record of owned) await attempt(() => stopOwned(record, root));
      for (const { name } of children) {
        const log = join(evidence, name + ".log");
        await attempt(async () => writeFile(log, (await readFile(log, "utf8"))
          .replaceAll(process.env.QODER_PERSONAL_ACCESS_TOKEN, "[REDACTED]")));
      }
    }
    if (failures.length) throw new AggregateError(failures, "hosted wrapper acceptance or cleanup failed");
  });
