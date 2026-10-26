import assert from "node:assert/strict";
import { existsSync, readFileSync, rmSync } from "node:fs";
import { mkdtemp } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, it } from "node:test";

import { createPilotHarness } from "../e2e/pilot/harness.mjs";
import { parseArgs } from "../e2e/pilot/args.mjs";

const tempDirs: string[] = [];
const realSetTimeout = globalThis.setTimeout;
const realClearTimeout = globalThis.clearTimeout;

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

  for (const startupDelayMs of [0, 400]) {
    it(
      `kills a timed-out command process group after ${startupDelayMs}ms startup`,
      { skip: process.platform === "win32" },
      async (t) => {
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
setTimeout(() => {
const child = spawn(process.execPath, ["-e", "setInterval(() => {}, 30000)"], {
  stdio: ["ignore", "inherit", "inherit"],
});
writeFileSync(process.argv[1], String(child.pid));
setInterval(() => {}, 30000);
}, ${startupDelayMs});
`;
        t.mock.timers.enable({ apis: ["setTimeout"] });
        const timedOut = assert.rejects(
          runRequiredStep("timeout-process-tree", process.execPath, [
            "-e",
            parentScript,
            childPidFile,
          ]),
          (error: any) => error?.name === "StepError" && error?.step?.timedOut === true,
        );

        let cleanupDeadline: ReturnType<typeof setTimeout> | undefined;
        try {
          // Child startup uses real time; advance the harness clock only once its
          // descendant exists so scheduler delays cannot bypass the cleanup check.
          await waitForPidFile(childPidFile);
          t.mock.timers.tick(250);
          await Promise.race([
            timedOut,
            new Promise<never>((_, reject) => {
              cleanupDeadline = realSetTimeout(() => {
                reject(new Error("timeout waited for the grandchild to exit naturally"));
              }, 1_500);
            }),
          ]);
        } finally {
          realClearTimeout(cleanupDeadline);
          // Exercise the harness escalation even if readiness or cleanup failed.
          t.mock.timers.tick(5_250);
          await timedOut;
          t.mock.timers.reset();
        }
        await waitForProcessStop(Number(readFileSync(childPidFile, "utf8")));
      },
    );
  }
});

async function waitForPidFile(path: string): Promise<void> {
  const deadline = performance.now() + 10_000;
  while (!existsSync(path)) {
    assert.ok(performance.now() < deadline, "command did not publish its child PID");
    await new Promise((resolve) => realSetTimeout(resolve, 10));
  }
}

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
