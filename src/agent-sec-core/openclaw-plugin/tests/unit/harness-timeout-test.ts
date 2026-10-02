import assert from "node:assert/strict";
import { readFileSync, rmSync } from "node:fs";
import { mkdtemp } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, it } from "node:test";

import { createPilotHarness } from "../e2e/pilot/harness.mjs";
import { parseArgs } from "../e2e/pilot/args.mjs";

const tempDirs: string[] = [];

afterEach(() => {
  for (const dir of tempDirs.splice(0)) {
    rmSync(dir, { recursive: true, force: true });
  }
});

describe("OpenClaw E2E harness", () => {
  it("parses a prebuilt plugin package", () => {
    assert.deepEqual(parseArgs(["--plugin-package", "plugin.tgz"]), {
      pluginPackage: "plugin.tgz",
    });
  });

  it("kills a timed-out command process group", { skip: process.platform === "win32" }, async () => {
    const logsDir = await mkdtemp(join(tmpdir(), "openclaw-harness-timeout-"));
    tempDirs.push(logsDir);
    const childPidFile = join(logsDir, "child.pid");
    const result = { logsDir, steps: [] };
    const { runRequiredStep } = createPilotHarness({
      defaultCommandTimeoutMs: 250,
      pluginRoot: logsDir,
      result,
      startedProcesses: [],
      startedServers: [],
    });
    const parentScript = `
const { spawn } = require("node:child_process");
const { writeFileSync } = require("node:fs");
const child = spawn(process.execPath, ["-e", "setTimeout(() => {}, 2000)"], {
  stdio: ["ignore", "inherit", "inherit"],
});
writeFileSync(process.argv[1], String(child.pid));
setInterval(() => {}, 2000);
`;
    const startedAt = Date.now();

    await assert.rejects(
      runRequiredStep("timeout-process-tree", process.execPath, ["-e", parentScript, childPidFile]),
      (error: any) => error?.name === "StepError" && error?.step?.timedOut === true,
    );

    assert.ok(Date.now() - startedAt < 1_500, "timeout waited for the grandchild to exit naturally");
    await waitForProcessStop(Number(readFileSync(childPidFile, "utf8")));
  });
});

async function waitForProcessStop(pid: number): Promise<void> {
  for (let attempt = 0; attempt < 20; attempt += 1) {
    let running = true;
    try {
      process.kill(pid, 0);
    } catch (error: any) {
      if (error?.code === "ESRCH") {
        return;
      }
      throw error;
    }

    if (process.platform === "linux") {
      try {
        const stat = readFileSync(`/proc/${pid}/stat`, "utf8");
        const stateOffset = stat.lastIndexOf(")") + 2;
        const state = stat[stateOffset];
        running = state !== "Z" && state !== "X";
      } catch (error: any) {
        if (error?.code === "ENOENT") {
          return;
        }
        throw error;
      }
    }

    if (!running) {
      return;
    }
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  assert.fail(`grandchild process ${pid} remained running after the timeout`);
}
