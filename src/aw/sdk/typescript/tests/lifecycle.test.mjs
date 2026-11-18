import test from "node:test";
import assert from "node:assert/strict";
import {
  mkdtemp,
  mkdir,
  writeFile,
  readFile,
  rm,
  rename,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";
import { once } from "node:events";
import { Admission, Instance } from "../src/client.mjs";
import { rpc } from "../src/transport.mjs";
const binary =
  process.env.AW_TEST_BINARY ??
  fileURLToPath(new URL("../../../target/debug/aw", import.meta.url));
const config = {
  apiVersion: "aw/v1alpha1",
  kind: "AWConfiguration",
  metadata: { name: "sdk" },
  spec: {
    daemon: { startup: "on_demand", endpoint: "auto", state_dir: "auto" },
    execution: { guarantee: "native_hook", default_event_budget_ms: 1000 },
    audit: { enabled: true, payload: "metadata_only" },
    agents: { app: { adapter: "qoder-sdk", argv: ["/bin/true"] } },
    providers: {},
    events: Object.fromEntries(
      ["tool.before", "tool.after"].map((name) => [
        name,
        { enabled: true, required: false, steps: [] },
      ]),
    ),
  },
};
async function setup(configure = () => {}) {
  const directory = await mkdtemp(join(tmpdir(), "aw-sdk-test-"));
  const state = join(directory, "state");
  await mkdir(state, { mode: 0o700 });
  const file = join(directory, "config.json");
  const document = structuredClone(config);
  configure(document);
  await writeFile(file, JSON.stringify(document));
  const endpoint = join(state, "aw.sock");
  let service;
  return {
    endpoint,
    async start() {
      service = spawn(
        binary,
        ["service", "serve", "--config", file, "--state-dir", state],
        { stdio: "ignore" },
      );
      for (let i = 0; i < 100; i++) {
        try {
          await rpc(endpoint, null, { method: "status" }, 1000);
          return;
        } catch (error) {
          if (!error.unavailable) throw error;
        }
        await new Promise((resolve) => setTimeout(resolve, 10));
      }
      throw Error("service readiness deadline");
    },
    async stop() {
      if (service && service.exitCode === null) {
        const done = once(service, "exit");
        service.kill("SIGTERM");
        await done;
      }
    },
    async close() {
      await this.stop();
      await rm(directory, { recursive: true });
    },
  };
}
function instance(endpoint, options = {}) {
  return new Instance({
    endpoint,
    target: "app",
    availability: "native_fallback",
    timeoutMs: 300,
    capabilities: {
      adapter: "qoder-sdk",
      version: "public-hooks/v1",
      entrypoint: "sdk-worker",
      events: { "tool.before": ["observe"], "tool.after": ["observe"] },
    },
    install: async () => true,
    ...options,
  });
}
const contract = JSON.parse(
  await readFile(
    new URL("../../tests/fixtures/contract.json", import.meta.url),
  ),
);
const payload = () => ({
  ...structuredClone(contract.native_before),
  cwd: process.cwd(),
});

test(
  "absence, transport loss and recovery affect only new tasks",
  { timeout: 15000 },
  async () => {
    const server = await setup();
    const client = instance(server.endpoint);
    try {
      assert.equal(await client.connect(), false);
      const absent = await client.task();
      assert.equal(absent.fallback, true);
      await server.start();
      assert.equal(await client.connect(), true);
      const active = await client.task();
      const hook = active.routes.hooks[0];
      assert.equal((await active.invoke(hook, payload())).code, 0);
      await server.stop();
      assert.equal((await active.invoke(hook, payload())).code, 0);
      assert.equal(active.fallback, true);
      const start = performance.now();
      for (let i = 0; i < 10; i++) await active.invoke(hook, payload());
      assert.ok(
        performance.now() - start < 100,
        "fallback does not perform RPC",
      );
      await server.start();
      assert.equal(await client.connect(), true);
      assert.equal(active.fallback, true);
      assert.equal(absent.fallback, true);
      const fresh = await client.task();
      assert.equal(fresh.fallback, false);
      await fresh.close();
    } finally {
      await client.close();
      await server.close();
    }
  },
);
test(
  "explicit configuration errors and cancellation never become fallback",
  { timeout: 15000 },
  async () => {
    const server = await setup();
    const client = instance(server.endpoint, { target: "missing" });
    try {
      await server.start();
      await assert.rejects(client.connect(), (error) => !error.unavailable);
      await assert.rejects(client.task());
      const ok = instance(server.endpoint);
      await ok.connect();
      const abort = new AbortController();
      abort.abort();
      await assert.rejects(ok.task({ signal: abort.signal }));
      await ok.close();
    } finally {
      await client.close();
      await server.close();
    }
  },
);

async function until(check) {
  const expires = performance.now() + 5000;
  while (!check()) {
    assert.ok(performance.now() < expires, "reconnect deadline");
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
}

test(
  "fallback detach releases retained tasks",
  { timeout: 15000 },
  async () => {
    const server = await setup();
    const client = instance(server.endpoint);
    let moved = false;
    try {
      await server.start();
      await client.connect();
      const task = await client.task();
      await rename(server.endpoint, `${server.endpoint}.held`);
      moved = true;
      assert.equal(
        (await task.invoke(task.routes.hooks[0], payload())).code,
        0,
      );
      await rename(`${server.endpoint}.held`, server.endpoint);
      moved = false;
      assert(task.fallback);
      await client.detachControl();
      await task.close();
      assert(task.closed);
      assert(!client.tasks.has(task));
      await client.close();
      assert.equal(
        (await rpc(server.endpoint, null, { method: "status" }, 500)).result
          .bindings,
        0,
      );
    } finally {
      if (moved) await rename(`${server.endpoint}.held`, server.endpoint);
      try {
        await client.close();
      } finally {
        await server.close();
      }
    }
  },
);

test(
  "required background recovery retires the generation without fallback",
  { timeout: 15000 },
  async () => {
    const server = await setup();
    const client = instance(server.endpoint, {
      availability: "required",
      reconnectMs: 100,
    });
    try {
      client.start();
      await until(() => client.lastError?.unavailable);
      await server.start();
      await until(() => client.state === "ready");
      const active = await client.task();
      const sibling = await client.task();
      const original = client.connection.identity.generation;
      await server.stop();
      await assert.rejects(
        active.invoke(active.routes.hooks[0], payload()),
        (e) => e.unavailable,
      );
      assert.equal(client.state, "unavailable");
      assert.equal(active.fallback, false);
      assert.equal(sibling.fallback, false);
      await assert.rejects(client.task(), /instance_not_ready/);
      await new Promise((resolve) => setTimeout(resolve, 250));
      assert.ok(
        client.timer,
        "transport failures keep background recovery running",
      );
      await server.start();
      await until(() => client.state === "ready");
      assert.notEqual(client.connection.identity.generation, original);
      for (const task of [active, sibling]) {
        await assert.rejects(
          task.invoke(task.routes.hooks[0], payload()),
          (e) => e.unavailable,
        );
        assert.equal(task.fallback, false);
        await task.close();
      }
      const fresh = await client.task();
      assert.equal(
        (await fresh.invoke(fresh.routes.hooks[0], payload())).code,
        0,
      );
      await fresh.close();
    } finally {
      await client.close();
      await server.close();
    }
  },
);

test(
  "required recovery on the same Service drains the retired registration",
  { timeout: 15000 },
  async () => {
    const server = await setup();
    const client = instance(server.endpoint, { availability: "required" });
    let moved = false;
    try {
      await server.start();
      await client.connect();
      const task = await client.task();
      const original = client.connection;
      await rename(server.endpoint, `${server.endpoint}.held`);
      moved = true;
      await assert.rejects(task.invoke(task.routes.hooks[0], payload()), {
        code: "ENOENT",
      });
      await rename(`${server.endpoint}.held`, server.endpoint);
      moved = false;
      await client.connect();
      assert.equal(
        client.connection.identity.generation,
        original.identity.generation,
      );
      assert.notEqual(
        client.connection.handle.binding.instance_id,
        original.handle.binding.instance_id,
      );
      await task.close();
      const fresh = await client.task();
      assert.equal(
        (await fresh.invoke(fresh.routes.hooks[0], payload())).code,
        0,
      );
      await fresh.close();
    } finally {
      if (moved) await rename(`${server.endpoint}.held`, server.endpoint);
      await client.close();
      await server.close();
    }
  },
);

test(
  "shared Admission waits at input and shares one Task through cancellation",
  { timeout: 15000 },
  async () => {
    const server = await setup((document) => {
      document.spec.agents.app.adapter = "sdk";
      document.spec.events["input.submit"] = {
        enabled: true,
        required: false,
        steps: [],
      };
    });
    const client = instance(server.endpoint, {
      capabilities: {
        adapter: "sdk",
        version: "aw-hook-source/v1",
        entrypoint: "application",
        events: Object.fromEntries(
          ["input.submit", "tool.before", "tool.after"].map((event) => [
            event,
            ["observe"],
          ]),
        ),
      },
    });
    const admission = new Admission(client, {
      events: ["input.submit", "tool.before", "tool.after"],
    });
    let settled = false;
    const first = admission.prepare().finally(() => (settled = true));
    const second = admission.prepare();
    try {
      assert.equal(first instanceof Promise, true);
      await new Promise((resolve) => setTimeout(resolve, 30));
      assert.equal(settled, false);
      assert.equal(client.tasks.size, 0);
      await server.start();
      await client.connect();
      await Promise.all([first, second]);
      assert.equal(client.tasks.size, 1);
      assert.equal(admission.fallback, false);
      assert.deepEqual(
        await admission.emit("input.submit", {
          session_id: "session",
          turn_id: "turn",
          text: "hello",
        }),
        { decision: "continue", coverage: "not_configured" },
      );
      await Promise.all([admission.close(), admission.close()]);
      assert.equal(client.tasks.size, 0);
      assert.throws(
        () => admission.prepare(),
        (error) => error.name === "AbortError",
      );
    } finally {
      await admission.close();
      await Promise.allSettled([first, second]);
      await client.close();
      await server.close();
    }
  },
);

test(
  "shared Admission preserves deadlines, explicit errors and tool fallback",
  { timeout: 5000 },
  async () => {
    const client = instance("/tmp/aw-sdk-missing.sock");
    const tool = new Admission(client, { events: ["tool.before"] });
    const required = instance("/tmp/aw-sdk-missing.sock", {
      availability: "required",
    });
    const gate = new Admission(required, {
      events: ["tool.after"],
      timeoutMs: 100,
    });
    try {
      await tool.prepare();
      assert.equal(tool.fallback, true);
      await assert.rejects(
        gate.prepare(),
        (error) => error.name === "AbortError",
      );
      assert.equal(gate.task, undefined);
      client.lastError = new Error("policy_invalid");
      const failed = new Admission(client, { events: ["tool.before"] });
      await assert.rejects(failed.prepare(), /policy_invalid/);
      await failed.close();
      const cancelled = new Admission(required, { events: ["tool.after"] });
      const waiting = cancelled.prepare();
      const rejected = assert.rejects(
        waiting,
        (error) => error.name === "AbortError",
      );
      await cancelled.close();
      await rejected;
      assert.equal(required.tasks.size, 0);
    } finally {
      await Promise.all([tool.close(), gate.close()]);
      await Promise.all([client.close(), required.close()]);
    }
  },
);

test(
  "fallback close joins detach before root removal",
  { timeout: 15000 },
  async () => {
    const server = await setup();
    const client = instance(server.endpoint);
    try {
      await server.start();
      await client.connect();
      const task = await client.task();
      client.lost(task.connection);
      let released = false;
      let concurrentClose;
      Object.defineProperty(task.connection, "released", {
        get() {
          return released;
        },
        set(value) {
          // The root is removed remotely, before its local completion flag is set.
          concurrentClose = task.close();
          void concurrentClose.catch(() => {});
          released = value;
        },
      });
      await client.detachControl();
      await concurrentClose;
      assert(task.closed);
      assert(!client.tasks.has(task));
      assert.equal(
        (await rpc(server.endpoint, null, { method: "status" }, 500)).result
          .bindings,
        0,
      );
    } finally {
      await client.close();
      await server.close();
    }
  },
);
