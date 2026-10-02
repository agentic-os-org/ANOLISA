import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    include: ["src/**/*.test.ts"],
    pool: "threads",
    coverage: {
      provider: "v8",
      thresholds: {
        lines: 90,
        functions: 90,
        statements: 90,
      },
    },
  },
});
