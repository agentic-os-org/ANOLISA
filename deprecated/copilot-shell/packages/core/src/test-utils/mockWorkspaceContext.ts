/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import { vi } from 'vitest';
import path from 'node:path';
import type { WorkspaceContext } from '../utils/workspaceContext.js';

/**
 * Creates a mock WorkspaceContext for testing
 * @param rootDir The root directory to use for the mock
 * @param additionalDirs Optional additional directories to include in the workspace
 * @returns A mock WorkspaceContext instance
 */
export function createMockWorkspaceContext(
  rootDir: string,
  additionalDirs: string[] = [],
): WorkspaceContext {
  const allDirs = [rootDir, ...additionalDirs];

  const mockWorkspaceContext = {
    addDirectory: vi.fn(),
    getDirectories: vi.fn().mockReturnValue(allDirs),
    isPathWithinWorkspace: vi.fn().mockImplementation((pathToCheck: string) =>
      // Mirror the real WorkspaceContext containment semantics
      // (path.relative-based, as in its isPathWithinRoot) instead of a
      // raw startsWith, so sibling-prefix paths ('/ws-evil' vs workspace
      // '/ws') are rejected the same way the real class rejects them.
      allDirs.some((dir) => {
        const relative = path.relative(dir, pathToCheck);
        return (
          !relative.startsWith(`..${path.sep}`) &&
          relative !== '..' &&
          !path.isAbsolute(relative)
        );
      }),
    ),
  } as unknown as WorkspaceContext;

  return mockWorkspaceContext;
}
