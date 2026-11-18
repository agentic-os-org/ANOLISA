import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, mkdir, readFile, writeFile, rm } from "node:fs/promises";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { Instance } from "../src/client.mjs";
import { runPiWorker } from "../src/pi.mjs";
import {
  piProfile,
  strictOutputRelease,
  workerBinding,
} from "../../../crates/aw-adapter/native/pi/sdk.mjs";

const execute = promisify(execFile);
const binary = process.env.AW_TEST_BINARY;
const fixture = JSON.parse(
  await readFile(new URL("../../tests/fixtures/deployment.json", import.meta.url)),
);

test("strict output follows the release range shared with managed launch", () => {
  for (const [version, coreVersion, admitted] of [
    ["0.84.2", "0.84.2", true],
    ["1.3.0", "1.3.0", true],
    ["1.5.2", "1.5.2", true],
    ["0.84.1", "0.84.1", false],
    ["1.5.3", "1.5.3", false],
    ["1.5.2", "1.5.1", false],
    ["1.5.2-rc.1", "1.5.2-rc.1", false],
    ["01.5.2", "01.5.2", false],
  ])
    assert.equal(
      strictOutputRelease({ version, coreVersion }),
      admitted,
      `${version}:${coreVersion}`,
    );
});

test("worker capabilities follow the seams the loaded worker provides", () => {
  const runWorker = { runWorker() {} };
  const strict = workerBinding({ worker: runWorker, inputEnabled: true, strictOutput: true });
  assert.deepEqual(
    { ...strict.capabilities, events: undefined, failure_actions: undefined },
    { ...piProfile, events: undefined, failure_actions: undefined },
  );
  assert.deepEqual(Object.keys(strict.capabilities.events).sort(), [
    "input.submit",
    "tool.after",
    "tool.before",
  ]);
  assert.deepEqual(strict.capabilities.failure_actions, {
    "tool.after": ["withhold_result"],
  });
  assert.equal(strict.startup.strict_output_supported, true);

  // An unverified release keeps tool Hooks but cannot claim withheld results.
  const plain = workerBinding({ worker: runWorker, inputEnabled: false, strictOutput: false });
  assert.equal(plain.capabilities.failure_actions, undefined);
  assert.equal(plain.capabilities.events["input.submit"], undefined);
  assert.equal(
    plain.install({ hooks: [{ event: "input.submit", step: "s", on_error: "block", budget_ms: 1000 }] }),
    false,
  );

  assert.throws(
    () => workerBinding({ worker: { run() {}, runWorker() {} }, inputEnabled: true, strictOutput: true }),
    /runWorker factory/,
  );
});

test(
  "a pi-worker-hooks descriptor selects wiring before any Pi package is loaded",
  { skip: !binary },
  async (t) => {
    const root = await mkdtemp("/tmp/aw-js-pi-descriptor-");
    t.after(() => rm(root, { recursive: true, force: true }));
    const configFile = `${root}/config.json`;
    const descriptorFile = `${root}/descriptor.json`;
    const application = `${root}/worker.mjs`;
    await writeFile(application, "export function createSession() {}\n");
    const document = structuredClone(fixture.configuration);
    document.spec.agents.app.adapter = "pi";
    document.spec.daemon.state_dir = `${root}/state`;
    await writeFile(configFile, JSON.stringify(document), { mode: 0o600 });
    const compile = async (profile) =>
      writeFile(
        descriptorFile,
        (
          await execute(binary, [
            "config", "descriptor", "--config", configFile,
            "--agent", "app", "--profile", profile,
          ], { timeout: 10000 })
        ).stdout,
        { mode: 0o600 },
      );
    const options = { application, configFile, descriptorFile, executable: "/missing/aw" };

    await compile("pi-worker-hooks");
    const descriptor = JSON.parse(await readFile(descriptorFile, "utf8"));
    assert.deepEqual(descriptor.profile, piProfile);
    assert.equal(descriptor.availability, "required");
    // Wiring is accepted without the parser; only the missing Pi packages remain.
    await assert.rejects(runPiWorker(options), /Cannot resolve installed @earendil-works\/pi-coding-agent/);

    await writeFile(
      descriptorFile,
      JSON.stringify({ ...descriptor, profile: { ...piProfile, version: "1.5.2" } }),
      { mode: 0o600 },
    );
    await assert.rejects(
      runPiWorker(options),
      (error) => error.code === "sdk_descriptor_profile_mismatch",
    );

    // Stub installed Pi packages to verify primary worker crash preservation on close failure.
    for (const [name, body] of [
      ["pi-coding-agent", "export class AgentSession {}\n"],
      ["pi-agent-core", "export class Agent { processEvents() {} }\n"],
    ]) {
      const dir = `${root}/node_modules/@earendil-works/${name}`;
      await mkdir(`${dir}/dist`, { recursive: true });
      await writeFile(
        `${dir}/package.json`,
        JSON.stringify({
          name: `@earendil-works/${name}`,
          version: "0.84.2",
          type: "module",
          main: "./dist/index.js",
        }),
      );
      await writeFile(`${dir}/dist/index.js`, body);
    }
    await writeFile(
      application,
      'export function createSession() {}\nexport async function runWorker() { throw new Error("primary_worker_crash"); }\n',
    );
    await compile("pi-worker-hooks");
    await assert.rejects(runPiWorker(options), /^Error: primary_worker_crash$/);

    const originalClose = Instance.prototype.close;
    Instance.prototype.close = async function () {
      await originalClose.call(this);
      throw new Error("secondary_close_failure");
    };
    try {
      await assert.rejects(runPiWorker(options), (error) => {
        assert.ok(error instanceof AggregateError);
        assert.equal(error.cause?.message, "primary_worker_crash");
        assert.equal(error.errors[1]?.message, "secondary_close_failure");
        return true;
      });
    } finally {
      Instance.prototype.close = originalClose;
    }
  },
);
