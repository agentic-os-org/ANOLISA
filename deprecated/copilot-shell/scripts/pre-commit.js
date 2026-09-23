/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import { fileURLToPath } from 'node:url';
import path from 'node:path';
import lintStaged from 'lint-staged';

try {
  // lint-staged config and eslint.config.js live in deprecated/copilot-shell/,
  // so cwd must point there — not the git repository root.
  const scriptDir = path.dirname(fileURLToPath(import.meta.url));
  const packageRoot = path.resolve(scriptDir, '..');

  // Run lint-staged with API directly
  const passed = await lintStaged({ cwd: packageRoot });

  // Exit with appropriate code
  process.exit(passed ? 0 : 1);
} catch {
  // Exit with error code
  process.exit(1);
}
