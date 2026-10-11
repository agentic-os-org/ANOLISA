/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import {
  afterAll,
  afterEach,
  beforeEach,
  describe,
  expect,
  it,
  vi,
} from 'vitest';
import { handleDisableHook } from './disable.js';
import { handleEnableHook } from './enable.js';

/**
 * USER_SETTINGS_PATH and USER_SETTINGS_DIR are derived from os.homedir() when
 * settings.ts evaluates, so the redirect has to be installed before that
 * module graph is imported. Everything lives in a throwaway directory: the
 * real user configuration is neither read nor written.
 */
const mockOs = vi.hoisted(() => ({ homeDir: '' }));

vi.mock('node:os', async (importOriginal) => {
  const actual = await importOriginal<typeof import('node:os')>();
  const fsMod = await import('node:fs');
  const pathMod = await import('node:path');
  return {
    ...actual,
    homedir: () => {
      if (!mockOs.homeDir) {
        mockOs.homeDir = fsMod.mkdtempSync(
          pathMod.join(actual.tmpdir(), 'cosh-home-'),
        );
      }
      return mockOs.homeDir;
    },
  };
});

const USER_HOOK_COMMAND = '/home/alice/bin/hook.sh';

const tempWorkspaces: string[] = [];
let previousHome: string | undefined;
let previousSystemPath: string | undefined;

function writeSettings(filePath: string, value: unknown): void {
  fs.mkdirSync(path.dirname(filePath), { recursive: true });
  fs.writeFileSync(filePath, JSON.stringify(value, null, 2), 'utf-8');
}

function workspaceSettingsPath(workspace: string): string {
  return path.join(workspace, '.copilot-shell', 'settings.json');
}

/**
 * Seed the real load path: a workspace settings file whose own hook is written
 * `$HOME/p.sh`, and a user settings file whose hook is already absolute. The
 * user hook is what a merged-view write-back would drag into the workspace
 * file; the workspace hook is what an env-resolved write-back would freeze.
 */
function setUpWorkspace(disabled: string[]): string {
  const workspace = fs.mkdtempSync(path.join(os.tmpdir(), 'cosh-ws-'));
  tempWorkspaces.push(workspace);
  writeSettings(workspaceSettingsPath(workspace), {
    $version: 2,
    hooks: {
      PostToolUse: [
        { type: 'command', name: 'project-hook', command: '$HOME/p.sh' },
      ],
      disabled,
    },
  });
  writeSettings(path.join(os.homedir(), '.copilot-shell', 'settings.json'), {
    $version: 2,
    hooks: {
      PreToolUse: [
        { type: 'command', name: 'user-hook', command: USER_HOOK_COMMAND },
      ],
    },
  });
  return workspace;
}

describe('hooks commands persist through the real load path', () => {
  beforeEach(() => {
    previousHome = process.env['HOME'];
    // resolveEnvVarsInObject expands $HOME from the environment; pin it to the
    // mocked home so the expansion is deterministic on every platform.
    process.env['HOME'] = os.homedir();
    // Keep the system scope away from the machine's real configuration.
    previousSystemPath = process.env['QWEN_CODE_SYSTEM_SETTINGS_PATH'];
    process.env['QWEN_CODE_SYSTEM_SETTINGS_PATH'] = path.join(
      os.tmpdir(),
      'cosh-hooks-nonexistent',
      'settings.json',
    );
  });

  afterEach(() => {
    vi.restoreAllMocks();
    if (previousHome === undefined) {
      delete process.env['HOME'];
    } else {
      process.env['HOME'] = previousHome;
    }
    if (previousSystemPath === undefined) {
      delete process.env['QWEN_CODE_SYSTEM_SETTINGS_PATH'];
    } else {
      process.env['QWEN_CODE_SYSTEM_SETTINGS_PATH'] = previousSystemPath;
    }
  });

  afterAll(() => {
    for (const dir of tempWorkspaces.splice(0)) {
      fs.rmSync(dir, { recursive: true, force: true });
    }
    if (mockOs.homeDir) {
      fs.rmSync(mockOs.homeDir, { recursive: true, force: true });
    }
  });

  it('disables a hook without freezing the workspace $HOME command', async () => {
    const workspace = setUpWorkspace([]);
    vi.spyOn(process, 'cwd').mockReturnValue(workspace);

    await handleDisableHook('project-hook');

    const settingsPath = workspaceSettingsPath(workspace);
    const raw = fs.readFileSync(settingsPath, 'utf-8');
    const saved = JSON.parse(raw) as {
      hooks: { disabled: string[]; PostToolUse: unknown };
    };
    expect(saved.hooks.disabled).toEqual(['project-hook']);
    // The workspace's own placeholder survives verbatim - the defect persisted
    // the env-resolved absolute path instead.
    expect(raw).toContain('$HOME/p.sh');
    // No other scope's hooks were copied into this file.
    expect(raw).not.toContain(USER_HOOK_COMMAND);
    expect(saved.hooks.PostToolUse).toBeDefined();
  });

  it('enables a hook without freezing the workspace $HOME command', async () => {
    const workspace = setUpWorkspace(['project-hook']);
    vi.spyOn(process, 'cwd').mockReturnValue(workspace);

    await handleEnableHook('project-hook');

    const settingsPath = workspaceSettingsPath(workspace);
    const raw = fs.readFileSync(settingsPath, 'utf-8');
    const saved = JSON.parse(raw) as { hooks: { disabled: string[] } };
    expect(saved.hooks.disabled).toEqual([]);
    expect(raw).toContain('$HOME/p.sh');
    expect(raw).not.toContain(USER_HOOK_COMMAND);
  });
});
