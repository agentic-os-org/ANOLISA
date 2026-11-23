/** Real pinned SDK/Worker daemon credential callbacks, with no login credentials. */
import test from "node:test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdir, mkdtemp, readFile, rm, writeFile, access } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { setTimeout as delay } from "node:timers/promises";
import { managedDaemon } from "../../../../aw-adapter/native/qoder-sdk/daemon.mjs";
import { bounded } from "../../../../aw-adapter/native/qoder-sdk/resident.mjs";

const sdkPath = resolve(process.argv[2]);
const output = resolve(process.argv[3]);
const worker = join(dirname(sdkPath), "_worker/qoder-worker-runtime.obf.mjs");
const deferred = () => {
  let resolve;
  const promise = new Promise(done => { resolve = done; });
  return { promise, resolve };
};
const credentialName = kind => kind === "jobToken" ? "fetchJobToken" : "fetchServiceAccountToken";
const authFailure = error => error?.name === "DaemonRpcError" && typeof error.code === "string";

async function absent(pid) {
  await assert.rejects(access(`/proc/${pid}`), { code: "ENOENT" });
}

async function collect(failures, operation) {
  try { await operation(); } catch (error) { failures.push(error); }
}

async function reap(child, exited) {
  if (child.exitCode === null && child.signalCode === null) child.kill("SIGTERM");
  try { await bounded(exited, 2000); }
  catch { child.kill("SIGKILL"); await bounded(exited, 2000); }
  await absent(child.pid);
}

async function startTicks(pid) {
  const stat = await readFile(`/proc/${pid}/stat`, "utf8");
  return stat.slice(stat.lastIndexOf(")") + 2).split(" ")[19];
}

async function stopRecorded(record) {
  let current;
  try { current = await startTicks(record.pid); }
  catch (error) { if (error.code === "ENOENT") return; throw error; }
  assert.equal(current, record.startTicks, "recorded daemon PID was reused; refusing to signal it");
  for (const signal of ["SIGTERM", "SIGKILL"]) {
    process.kill(record.pid, signal);
    for (let attempt = 0; attempt < 40; attempt++) {
      try { await access(`/proc/${record.pid}`); }
      catch (error) { if (error.code === "ENOENT") return; throw error; }
      await delay(50);
    }
  }
  throw new Error(`Owned daemon ${record.pid} remained after bounded cleanup`);
}

async function runCase(kind, mode) {
  const sdk = await import(pathToFileURL(sdkPath));
  const entered = deferred(), release = deferred();
  let finalized = false, calls = 0, daemon, child, landing;
  const failures = [];
  const callback = async (...args) => {
    calls++;
    if (kind === "jobToken") {
      assert.equal(args[0].reason, "initial");
      assert(args[1].signal instanceof AbortSignal);
    } else assert.equal(args.length, 0);
    entered.resolve();
    try {
      if (mode !== "reuse") await release.promise;
      throw new Error("fixture credential callback rejected without a token");
    } finally { finalized = true; }
  };
  const payload = { type: kind, [credentialName(kind)]: callback };
  const environment = { PATH: `${dirname(process.execPath)}:/usr/bin:/bin`,
    HOME: join(output, "home"), TMPDIR: join(output, "tmp"),
    QODER_CONFIG_DIR: join(output, "profile"), XDG_CONFIG_HOME: join(output, "config"),
    XDG_RUNTIME_DIR: join(output, "runtime"), LANG: "C.UTF-8" };
  for (const key of ["HOME", "TMPDIR", "QODER_CONFIG_DIR", "XDG_CONFIG_HOME", "XDG_RUNTIME_DIR"])
    await mkdir(environment[key], { recursive: true, mode: 0o700 });
  const record = { command: [process.execPath, worker, "--sdk"], cwd: process.cwd(),
    ports: "none", log: join(output, "daemon.log"), expectedLifetimeSeconds: 20,
    stop: "daemon.stop(); await child exit" };
  await writeFile(join(output, "process.json"), JSON.stringify(record));
  const terminate = () => { release.resolve(); child?.kill("SIGTERM"); };
  process.once("SIGTERM", terminate);
  process.once("SIGINT", terminate);
  try {
    daemon = await managedDaemon(sdk, { pathToQoderCLIExecutable: worker,
      initializeTimeoutMs: 10000, debugFile: record.log,
      ...(mode === "startup" ? { auth: payload } : {}) },
    { binding: "native-daemon-fixture" }, bounded, environment);
    child = daemon.processController.process;
    assert(Number.isInteger(child.pid));
    record.pid = child.pid;
    record.command = [...child.spawnargs];
    record.startTicks = await startTicks(child.pid);
    record.stop = `kill -TERM ${child.pid}`;
    await writeFile(join(output, "process.json"), JSON.stringify(record));
    console.log(JSON.stringify({ daemonPid: child.pid, command: record.command, mode, kind }));
    assert.equal(daemon.state, "ready");
    assert.equal(calls, 0, "initialization must leave credential landing lazy");
    if (mode !== "startup") {
      for (const invoke of [() => daemon.auth(), () => daemon.auth(undefined)])
        await assert.rejects(invoke(), error => authFailure(error) && error.code === "AUTH_PAYLOAD_REQUIRED");
    }
    if (mode === "reuse") {
      const authenticating = daemon.auth(payload);
      payload[credentialName(kind)] = async () => { throw new Error("caller mutated credential payload"); };
      let failure;
      await assert.rejects(authenticating, error => { failure = error; return authFailure(error); });
      await assert.rejects(daemon.auth(), { name: failure.name, code: failure.code });
      await assert.rejects(daemon.auth(undefined), { name: failure.name, code: failure.code });
      assert.equal(calls, 3, "both omitted and undefined auth must reuse the installed callback snapshot");
      assert.equal(finalized, true);
    } else {
      landing = mode === "startup" ? daemon.auth() : daemon.auth(payload);
      const rejected = assert.rejects(landing, authFailure);
      rejected.catch(() => {});
      await bounded(entered.promise, 10000);
      let stopped = false;
      const stopping = daemon.stop().then(() => { stopped = true; });
      try {
        await Promise.race([stopping, delay(5000)]);
        assert.equal(stopped, false, "daemon.stop must join the original credential callback");
        assert.equal(finalized, false);
      } finally {
        release.resolve();
        await Promise.all([stopping, rejected]);
      }
      assert.equal(calls, 1);
      assert.equal(finalized, true);
    }
  } catch (error) { failures.push(error); }
  finally {
    release.resolve();
    if (daemon) await collect(failures, () => bounded(daemon.stop(), 10000));
    if (child) {
      await collect(failures, async () => {
        await reap(child, daemon.processController.exited);
        console.log(JSON.stringify({ daemonPid: child.pid, absent: true }));
      });
    }
    if (landing) await collect(failures, () => bounded(landing.catch(() => {}), 2000));
    process.removeListener("SIGTERM", terminate);
    process.removeListener("SIGINT", terminate);
  }
  if (failures.length === 1) throw failures[0];
  if (failures.length) throw new AggregateError(failures, `Native daemon cleanup failed: ${failures[0].message}`);
}

async function runChild(t, kind, mode) {
  const directory = await mkdtemp(join(output, `${kind}-${mode}-`));
  const environment = { PATH: `${dirname(process.execPath)}:/usr/bin:/bin`,
    HOME: join(directory, "home"), TMPDIR: join(directory, "tmp"),
    QODER_CONFIG_DIR: join(directory, "profile"), XDG_CONFIG_HOME: join(directory, "config"),
    XDG_RUNTIME_DIR: join(directory, "runtime"), LANG: "C.UTF-8" };
  for (const key of ["HOME", "TMPDIR", "QODER_CONFIG_DIR", "XDG_CONFIG_HOME", "XDG_RUNTIME_DIR"])
    await mkdir(environment[key], { recursive: true, mode: 0o700 });
  const args = [fileURLToPath(import.meta.url), sdkPath, directory, "--case", kind, mode];
  const child = spawn(process.execPath, args, { env: environment, cwd: directory,
    stdio: ["ignore", "pipe", "pipe"] });
  let stdout = "", stderr = "";
  const failures = [];
  child.stdout.on("data", data => { stdout += data; });
  child.stderr.on("data", data => { stderr += data; });
  const exited = new Promise((done, reject) => {
    child.once("error", reject);
    child.once("exit", (code, signal) => done({ code, signal }));
  });
  t.diagnostic(JSON.stringify({ pid: child.pid, command: [process.execPath, ...args], cwd: directory,
    ports: "none", expectedLifetimeSeconds: 20, stop: `kill -TERM ${child.pid}` }));
  try {
    const result = await bounded(exited, 20000);
    assert.deepEqual(result, { code: 0, signal: null }, `${stdout}\n${stderr}`);
  } catch (error) { failures.push(error); }
  finally {
    await collect(failures, () => reap(child, exited));
    if (stdout.trim()) t.diagnostic(stdout.trim());
    await collect(failures, async () => {
      const record = JSON.parse(await readFile(join(directory, "process.json"), "utf8"));
      if (record.pid) await stopRecorded(record);
    });
    await collect(failures, async () => {
      await rm(directory, { recursive: true });
      await assert.rejects(access(directory), { code: "ENOENT" });
    });
  }
  if (failures.length === 1) throw failures[0];
  if (failures.length) throw new AggregateError(failures, `Native daemon subprocess cleanup failed: ${failures[0].message}`);
}

if (process.argv[4] === "--case") {
  try { await runCase(process.argv[5], process.argv[6]); }
  catch (error) { console.error(error); process.exitCode = 1; }
} else {
  const metadata = JSON.parse(await readFile(join(dirname(sdkPath), "../package.json"), "utf8"));
  const runtime = JSON.parse(await readFile(join(dirname(worker), "runtime-info.json"), "utf8"));
  assert.equal(metadata.version, "1.0.56");
  assert.equal(runtime.version, "1.1.78");
  await mkdir(output, { recursive: true, mode: 0o700 });
  for (const kind of ["jobToken", "serviceAccount"])
    for (const mode of ["startup", "replacement", "reuse"])
      test(`real daemon ${kind} ${mode} retains credential callback ownership`,
        { timeout: 30000 }, async t => runChild(t, kind, mode));
}
