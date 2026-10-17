/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { ShellTool } from './shell.js';
import { WorkspaceContext } from '../utils/workspaceContext.js';
import type { Config } from '../config/config.js';

// Real-filesystem twin of the string-based containment tests in
// shell.test.ts: that file mocks the fs module wholesale, so the
// symlink cases — where containment must be decided on the
// dereferenced (real) path — live here with the real fs.
describe('ShellTool directory containment (real filesystem)', () => {
  let workspaceRoot: string;
  let outsideRoot: string;

  beforeEach(() => {
    // os.tmpdir() can itself contain symlinks; pin the real paths.
    workspaceRoot = fs.realpathSync(
      fs.mkdtempSync(path.join(os.tmpdir(), 'shell-ws-')),
    );
    outsideRoot = fs.realpathSync(
      fs.mkdtempSync(path.join(os.tmpdir(), 'shell-outside-')),
    );
  });

  afterEach(() => {
    fs.rmSync(workspaceRoot, { recursive: true, force: true });
    fs.rmSync(outsideRoot, { recursive: true, force: true });
  });

  function makeTool(): ShellTool {
    const config = {
      getCoreTools: () => [],
      getExcludeTools: () => [],
      getDebugMode: () => false,
      getTargetDir: () => workspaceRoot,
      getChildProcessEnv: () => ({ COSH_SESSION_ID: 'test-session-id' }),
      getWorkspaceContext: () => new WorkspaceContext(workspaceRoot),
      storage: {
        getUserSkillsDir: () =>
          path.join(workspaceRoot, '.copilot-shell/skills'),
      },
      getResolvedCustomSkillPaths: () => [],
    } as unknown as Config;
    return new ShellTool(config);
  }

  it('rejects a workspace symlink whose target is outside the workspace', () => {
    // path.resolve only folds '..' — it keeps the symlinked spelling, so
    // isSubpath('/ws', '/ws/link-out') passes while the execution layer
    // spawns with the raw '/ws/link-out' as cwd and lands outside. The
    // containment decision must run on the dereferenced path.
    const linkOut = path.join(workspaceRoot, 'link-out');
    fs.symlinkSync(outsideRoot, linkOut, 'dir');

    expect(() =>
      makeTool().build({
        command: 'ls',
        directory: linkOut,
        is_background: false,
      }),
    ).toThrow(
      `Directory '${linkOut}' is not within any of the registered workspace directories.`,
    );
  });

  it('accepts a symlink whose target stays inside the workspace', () => {
    const insideDir = path.join(workspaceRoot, 'inside');
    fs.mkdirSync(insideDir, { recursive: true });
    const linkIn = path.join(workspaceRoot, 'link-in');
    fs.symlinkSync(insideDir, linkIn, 'dir');

    expect(
      makeTool().build({
        command: 'ls',
        directory: linkIn,
        is_background: false,
      }),
    ).toBeDefined();
  });

  it('still accepts a plain nested directory and the workspace root', () => {
    const nested = path.join(workspaceRoot, 'a', 'b');
    fs.mkdirSync(nested, { recursive: true });

    expect(
      makeTool().build({
        command: 'ls',
        directory: nested,
        is_background: false,
      }),
    ).toBeDefined();
    expect(
      makeTool().build({
        command: 'ls',
        directory: workspaceRoot,
        is_background: false,
      }),
    ).toBeDefined();
  });
});
