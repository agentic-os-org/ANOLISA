/**
 * Real-subprocess coverage of the CLI output capacity.
 *
 * Unlike commands.test.ts, this file does NOT mock child_process: the
 * default 1 MiB execFile maxBuffer is a property of the real spawn path,
 * and only a real child emitting oversized stdout demonstrates it.
 */
import { describe, it, expect } from "vitest";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

import { CommandExecutor } from "../commands.js";

// Just above execFile's 1 MiB default, far below the raised cap. The
// daemon's single response frame may be 16 MiB (MAX_FRAME_SIZE) and
// `ws-ckpt list` aggregates every page into one stdout stream.
const OVERSIZED_BYTES = 1024 * 1024 + 512 * 1024;

describe("CommandExecutor output capacity", () => {
  it("captures CLI output larger than execFile's 1 MiB default", async () => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), "ws-ckpt-cli-cap-"));
    const bin = path.join(dir, "ws-ckpt");
    fs.writeFileSync(
      bin,
      "#!/bin/sh\nhead -c 1572864 /dev/zero | tr '\\0' 'x'\n",
      "utf-8",
    );
    fs.chmodSync(bin, 0o755);
    const previousPath = process.env.PATH;
    process.env.PATH = `${dir}${path.delimiter}${previousPath}`;
    try {
      const executor = new CommandExecutor(10_000);
      const result = await executor.list("/ws", "json");
      expect(result.exitCode).toBe(0);
      expect(result.stdout.length).toBe(OVERSIZED_BYTES);
    } finally {
      process.env.PATH = previousPath;
      fs.rmSync(dir, { recursive: true, force: true });
    }
  }, 30_000);
});
