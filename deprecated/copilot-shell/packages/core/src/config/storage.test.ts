/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import * as os from 'node:os';
import * as path from 'node:path';
import { Storage } from './storage.js';

describe('Storage – getGlobalSettingsPath', () => {
  it('returns path to ~/.copilot-shell/settings.json', () => {
    const expected = path.join(os.homedir(), '.copilot-shell', 'settings.json');
    expect(Storage.getGlobalSettingsPath()).toBe(expected);
  });
});

describe('Storage – additional helpers', () => {
  const projectRoot = '/tmp/project';
  const storage = new Storage(projectRoot);

  it('getWorkspaceSettingsPath returns project/.copilot-shell/settings.json', () => {
    const expected = path.join(projectRoot, '.copilot-shell', 'settings.json');
    expect(storage.getWorkspaceSettingsPath()).toBe(expected);
  });

  it('getUserCommandsDir returns ~/.copilot-shell/commands', () => {
    const expected = path.join(os.homedir(), '.copilot-shell', 'commands');
    expect(Storage.getUserCommandsDir()).toBe(expected);
  });

  it('getProjectCommandsDir returns project/.copilot-shell/commands', () => {
    const expected = path.join(projectRoot, '.copilot-shell', 'commands');
    expect(storage.getProjectCommandsDir()).toBe(expected);
  });

  it('getMcpOAuthTokensPath returns ~/.copilot-shell/mcp-oauth-tokens.json', () => {
    const expected = path.join(
      os.homedir(),
      '.copilot-shell',
      'mcp-oauth-tokens.json',
    );
    expect(Storage.getMcpOAuthTokensPath()).toBe(expected);
  });
});

describe('Storage – resolveCustomSkillPaths', () => {
  const storage = new Storage('/tmp/project');
  const homeDir = os.homedir();

  let originalEnv: NodeJS.ProcessEnv;

  beforeEach(() => {
    originalEnv = { ...process.env };
  });

  afterEach(() => {
    process.env = originalEnv;
  });

  it('expands tilde to home directory', () => {
    const result = storage.resolveCustomSkillPaths(['~/my-skills']);
    expect(result).toEqual([path.join(homeDir, 'my-skills')]);
  });

  it('expands standalone tilde', () => {
    const result = storage.resolveCustomSkillPaths(['~']);
    expect(result).toEqual([homeDir]);
  });

  it('expands $HOME variable', () => {
    process.env['HOME'] = homeDir;
    const result = storage.resolveCustomSkillPaths(['$HOME/skills']);
    expect(result).toEqual([path.join(homeDir, 'skills')]);
  });

  it('expands ${USER} variable', () => {
    const username = process.env['USER'] || 'testuser';
    process.env['USER'] = username;
    const result = storage.resolveCustomSkillPaths([`/opt/\${USER}/skills`]);
    expect(result).toEqual([`/opt/${username}/skills`]);
  });

  it('preserves absolute paths unchanged', () => {
    const result = storage.resolveCustomSkillPaths(['/usr/local/skills']);
    expect(result).toEqual(['/usr/local/skills']);
  });

  it('resolves relative paths to absolute', () => {
    const result = storage.resolveCustomSkillPaths(['./local-skills']);
    expect(result).toEqual([path.resolve('./local-skills')]);
  });

  it('filters out empty and whitespace-only strings', () => {
    const result = storage.resolveCustomSkillPaths(['', '  ', '/valid']);
    expect(result).toEqual(['/valid']);
  });

  it('returns empty array for empty input', () => {
    const result = storage.resolveCustomSkillPaths([]);
    expect(result).toEqual([]);
  });

  it('handles mixed paths correctly', () => {
    process.env['HOME'] = homeDir;
    const result = storage.resolveCustomSkillPaths(['~/a', '$HOME/b', '/c']);
    expect(result).toHaveLength(3);
    expect(result[0]).toBe(path.join(homeDir, 'a'));
    expect(result[1]).toBe(path.join(homeDir, 'b'));
    expect(result[2]).toBe('/c');
  });

  it('preserves undefined env vars as-is', () => {
    delete process.env['UNDEFINED_VAR_XYZ'];
    const result = storage.resolveCustomSkillPaths([
      '$UNDEFINED_VAR_XYZ/skills',
    ]);
    // resolveEnvVarsInString preserves unresolved vars, then path.resolve makes absolute
    expect(result).toHaveLength(1);
    expect(result[0]).toContain('$UNDEFINED_VAR_XYZ');
  });
});
