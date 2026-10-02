import { mkdtempSync, mkdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { defineConfig } from "vitest/config";

const testHome = mkdtempSync(join(tmpdir(), "selective-claw-vitest-home-"));
mkdirSync(join(testHome, ".openclaw"), { recursive: true });

export default defineConfig({
  test: {
    dir: "test",
    include: ["**/*.test.ts"],
    env: {
      HOME: testHome,
    },
    pool: "forks",
    poolOptions: {
      forks: {
        execArgv: ["--experimental-sqlite"],
      },
    },
  },
});
