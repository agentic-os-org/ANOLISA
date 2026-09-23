import { defineConfig } from 'vitest/config';

export default defineConfig({
  test: {
    projects: ['packages/cli', 'packages/core', 'integration-tests', 'scripts'],
  },
});
