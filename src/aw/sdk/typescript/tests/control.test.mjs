import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, mkdir, writeFile, rm } from "node:fs/promises";
import { spawn, execFile } from "node:child_process";
import { promisify } from "node:util";
import { once } from "node:events";
import { fileURLToPath } from "node:url";
import { Instance } from "../src/client.mjs";

const binary =
  process.env.AW_TEST_BINARY ??
  fileURLToPath(new URL("../../../target/debug/aw", import.meta.url));
const execute = promisify(execFile);
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
async function until(check) {
  const deadline = Date.now() + 8000;
  while (!(await check())) {
    assert(Date.now() < deadline, "control deadline");
    await sleep(20);
  }
}

test(
  "CLI selection is cooperative, idle-only and independent for same-process instances",
  { timeout: 30000 },
  async () => {
    const root = await mkdtemp("/tmp/aw-sdk-control-");
    await mkdir(root + "/state", { mode: 0o700 });
    const endpoint = root + "/state/aw.sock";
    const config = {
      apiVersion: "aw/v1alpha1",
      kind: "AWConfiguration",
      metadata: { name: "sdk-control" },
      spec: {
        daemon: {
          startup: "external",
          endpoint: "auto",
          state_dir: root + "/state",
        },
        execution: { guarantee: "native_hook", default_event_budget_ms: 1000 },
        audit: { enabled: true, payload: "metadata_only" },
        agents: { app: { adapter: "qoder-sdk", argv: ["/bin/true"] } },
        providers: {},
        events: {
          "tool.before": { enabled: true, required: false, steps: [] },
        },
      },
    };
    await writeFile(root + "/config.json", JSON.stringify(config));
    let service;
    const cli = async (action, ...args) =>
      JSON.parse(
        (
          await execute(
            binary,
            [...(action === "list" ? ["instance", action] : [action]), "--socket", endpoint, ...args],
            { timeout: 10000 },
          )
        ).stdout,
      );
    const start = async () => {
      service = spawn(
        binary,
        [
          "service",
          "serve",
          "--config",
          root + "/config.json",
          "--state-dir",
          root + "/state",
        ],
        { stdio: "ignore" },
      );
      await until(async () => {
        try {
          await cli("list");
          return true;
        } catch {
          return false;
        }
      });
    };
    const stop = async () => {
      if (service?.exitCode === null) {
        const stopped = once(service, "exit");
        service.kill("SIGTERM");
        await stopped;
      }
    };
    const create = (extra = {}) =>
      new Instance({
        endpoint,
        target: "app",
        availability: "native_fallback",
        install: () => true,
        timeoutMs: 500,
        reconnectMs: 100,
        ...extra,
        capabilities: {
          adapter: "qoder-sdk",
          version: "public-hooks/v1",
          entrypoint: "sdk-worker",
          events: { "tool.before": ["observe"] },
        },
      });
    const first = create(),
      second = create();
    first.listen();
    second.listen();
    try {
      const native = await first.task();
      assert(native.fallback);
      await start();
      await until(() => first.applicationId && second.applicationId);
      assert.equal((await cli("list")).applications.length, 2);
      assert.throws(() => first.listen(), /already_listening/);
      assert.throws(() => first.start(), /already_listening/);
      await second.stop();
      assert.equal((await cli("list")).applications.length, 1);
      second.listen();
      await until(async () => (await cli("list")).applications.length === 2);
      const initialId = first.applicationId;
      await cli("connect", "--sdk-instance", first.applicationId);
      await until(() => first.state === "ready");
      assert.equal(second.state, "detached");
      assert(native.fallback);
      const active = await first.task();
      await assert.rejects(
        cli("disconnect", "--sdk-instance", first.applicationId),
        (error) => JSON.parse(error.stdout).result.error === "sdk_control_busy",
      );
      assert.equal(first.state, "ready");
      assert(!active.fallback);
      await cli("connect", "--sdk-instance", second.applicationId);
      await until(() => second.state === "ready");
      await active.close();
      await cli("disconnect", "--sdk-instance", first.applicationId);
      await until(() => first.state === "detached");
      assert.equal(second.state, "ready");
      const retained = await second.task();
      await stop();
      await until(() => retained.fallback);
      await start();
      await until(
        () => second.state === "ready" && first.applicationId !== initialId,
      );
      assert(retained.fallback);
      assert.equal(first.state, "detached");
      await assert.rejects(cli("connect", "--sdk-instance", initialId));
      await first.close();
      await second.close();
      assert.equal((await cli("list")).applications.length, 0);
      let entered, release;
      const installing = new Promise((resolve) => {
        entered = resolve;
      });
      const gate = new Promise((resolve) => {
        release = resolve;
      });
      const slow = create({
        install: async () => {
          entered();
          await gate;
          return true;
        },
      });
      slow.listen();
      try {
        await until(() => slow.applicationId);
        const request = assert.rejects(
          cli(
            "connect",
            "--sdk-instance",
            slow.applicationId,
            "--timeout-ms",
            "250",
          ),
        );
        await installing;
        await request;
        // CLI timeout can precede the Service intent deadline.
        const expires = Date.now() + 5000;
        while (true) {
          const applications = (await cli("list")).applications;
          const current = applications.find(
            (app) => app.application_id === slow.applicationId,
          );
          if (current.request.state === "expired") break;
          assert.ok(Date.now() < expires, "control intent expiration");
          await new Promise((resolve) => setTimeout(resolve, 10));
        }
        release();
        await until(
          () =>
            slow.state === "detached" &&
            !slow.controller.active &&
            !slow.pending,
        );
        assert.equal(slow.connection, undefined);
        assert((await slow.task()).fallback);
        await cli("connect", "--sdk-instance", slow.applicationId);
        await until(() => slow.state === "ready");
      } finally {
        release();
        await slow.close();
      }
    } finally {
      await first.close();
      await second.close();
      await stop();
      await rm(root, { recursive: true, force: true });
    }
  },
);
