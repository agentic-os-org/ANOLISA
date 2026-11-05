/** Hold real Pi retirement at prompt completion and inject bounded transport failure. */
import assert from "node:assert/strict";
import { readFileSync, mkdirSync, writeFileSync, existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { pathToFileURL } from "node:url";

const deferred = () => {
  let resolve;
  const promise = new Promise((done) => { resolve = done; });
  return { promise, resolve };
};
async function bounded(promise) {
  let timer;
  try {
    return await Promise.race([promise, new Promise((_, reject) => {
      timer = setTimeout(() => reject(new Error("Pi drain fixture deadline exceeded")), 8000);
    })]);
  } finally { clearTimeout(timer); }
}

async function check({ createSession, root, evidence }, mode) {
  const module = (name) => import(pathToFileURL(join(dirname(process.argv[1]), name)));
  const { Scope } = await module("scope.mjs");
  const { run } = await module("transport.mjs");
  const finish = Scope.prototype.finish;
  const command = Scope.prototype.command;
  const cwd = join(root, `drain-${mode}`);
  mkdirSync(cwd);
  const entered = deferred(), release = deferred();
  let owner;
  // Keep one acknowledged invocation for prompt drain, as when native completion
  // omits PostTool. Admission, both Hook phases and final retirement remain real.
  Scope.prototype.finish = async function (id) {
    owner = this;
    if (this.closing) return await finish.call(this, id);
  };
  Scope.prototype.command = async function (action, ...args) {
    if (action === "finish" && this === owner) {
      entered.resolve();
      await release.promise;
      if (mode === "reject") return { code: 1 };
      if (mode === "timeout") {
        const script = "require('node:fs').writeFileSync(process.argv[1],String(process.pid));setTimeout(()=>{},60000)";
        const pidPath = join(cwd, "transport.pid");
        writeFileSync(join(cwd, "transport.json"), JSON.stringify({
          command: [process.execPath, "-e", script, pidPath], cwd: process.cwd(),
          timeout_ms: 200, pid_file: pidPath, stop: "kill -TERM -- -<transport.pid>", ports: [],
        }));
        return await run(process.execPath, ["-e", script, pidPath], Buffer.alloc(0), 200);
      }
    }
    return await command.call(this, action, ...args);
  };
  let session, close, prompting;
  try {
    ({ session, close } = await createSession({ cwd, index: 0,
      environment: { AW_TEST_MODE: "allow", AW_TEST_TENANT: mode } }));
    prompting = session.prompt("hold final retirement").then(
      () => ({ success: true }), (error) => ({ success: false, error: String(error) }));
    await bounded(entered.promise);
    assert.equal(owner.invocations.size, 1);
    const stats = evidence.get(session);
    assert.equal(stats.requests.length, 2);
    await assert.rejects(session.prompt("while draining"), /active prompt/);
    release.resolve();
    const result = await bounded(prompting);
    if (mode === "success") {
      assert(result.success);
      assert.equal(owner.invocations.size, 0);
      assert.throws(() => session.agent.streamFunction(), /execution is unavailable/);
      await assert.rejects(owner.register("late", "test", {}), /execution is unavailable/);
      Scope.prototype.finish = finish;
      Scope.prototype.command = command;
      await session.prompt("reuse the same healthy session");
      assert.equal(stats.requests.length, 4);
    } else {
      assert(!result.success);
      assert.match(result.error, mode === "timeout" ? /callback_timeout/ : /drain failed/);
      assert(owner.failed);
      assert.equal(owner.invocations.size, 1);
      await assert.rejects(session.prompt("failed drain"), /closed/);
      await assert.rejects(owner.register("late", "test", {}), /closed/);
      assert.throws(() => session.agent.streamFunction(), /closed/);
      assert.equal(stats.requests.length, 2);
      if (mode === "timeout") {
        assert(existsSync(join(cwd, "transport.pid")));
        const pid = Number(readFileSync(join(cwd, "transport.pid"), "utf8"));
        assert.throws(() => process.kill(pid, 0), { code: "ESRCH" });
        writeFileSync(join(cwd, "transport-cleanup.json"), JSON.stringify({ pid, absent: true }));
      }
    }
    writeFileSync(join(cwd, "requests.json"), JSON.stringify(stats.requests));
    return { case: `drain-${mode}`, passed: true };
  } finally {
    release.resolve();
    Scope.prototype.finish = finish;
    Scope.prototype.command = command;
    if (prompting) await bounded(prompting);
    if (close) await bounded(close());
  }
}

export async function run(options) {
  const results = [];
  for (const mode of ["success", "reject", "timeout"]) results.push(await check(options, mode));
  await options.recover();
  return results;
}
