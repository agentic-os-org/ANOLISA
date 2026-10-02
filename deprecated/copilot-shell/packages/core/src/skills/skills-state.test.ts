/**
 * @license
 * Copyright 2026 Alibaba Cloud
 * SPDX-License-Identifier: Apache-2.0
 */

import { vi, describe, it, expect, beforeEach } from 'vitest';
import * as fs from 'fs/promises';
import { SkillsStateManager, type SkillsState } from './skills-state.js';
import { Storage } from '../config/storage.js';

vi.mock('fs/promises');

describe('SkillsStateManager', () => {
  let manager: SkillsStateManager;

  beforeEach(() => {
    vi.clearAllMocks();
    vi.spyOn(Storage, 'getSkillsStatePath').mockReturnValue(
      '/home/user/.copilot-shell/skills-state.json',
    );
    manager = new SkillsStateManager();
  });

  describe('readState', () => {
    it('should return default state when file does not exist', async () => {
      vi.mocked(fs.readFile).mockRejectedValue(
        new Error('ENOENT: no such file'),
      );

      const state = await manager.readState();
      expect(state.disabledSkills.project).toEqual([]);
      expect(state.disabledSkills.user).toEqual([]);
      expect(state.disabledSkills.system).toEqual([]);
    });

    it('should parse valid state file', async () => {
      const stored: SkillsState = {
        disabledSkills: {
          project: ['skill-a'],
          custom: [],
          user: ['skill-b'],
          extension: [],
          system: [],
        },
      };
      vi.mocked(fs.readFile).mockResolvedValue(JSON.stringify(stored));

      const state = await manager.readState();
      expect(state.disabledSkills.project).toEqual(['skill-a']);
      expect(state.disabledSkills.user).toEqual(['skill-b']);
    });

    it('should return default state for invalid JSON', async () => {
      vi.mocked(fs.readFile).mockResolvedValue('not json');

      const state = await manager.readState();
      expect(state.disabledSkills.project).toEqual([]);
    });

    it('should return default state when disabledSkills is missing', async () => {
      vi.mocked(fs.readFile).mockResolvedValue(JSON.stringify({ other: true }));

      const state = await manager.readState();
      expect(state.disabledSkills.project).toEqual([]);
    });

    it('should filter out non-string entries in disabled lists', async () => {
      const stored = {
        disabledSkills: {
          project: ['valid', 123, null],
          custom: [],
          user: [],
          extension: [],
          system: [],
        },
      };
      vi.mocked(fs.readFile).mockResolvedValue(JSON.stringify(stored));

      const state = await manager.readState();
      expect(state.disabledSkills.project).toEqual(['valid']);
    });
  });

  describe('writeState', () => {
    it('should write state atomically', async () => {
      vi.mocked(fs.mkdir).mockResolvedValue(undefined);
      vi.mocked(fs.writeFile).mockResolvedValue();
      vi.mocked(fs.rename).mockResolvedValue();

      const state: SkillsState = {
        disabledSkills: {
          project: ['skill-a'],
          custom: [],
          user: [],
          extension: [],
          system: [],
        },
      };

      await manager.writeState(state);

      expect(fs.mkdir).toHaveBeenCalledWith('/home/user/.copilot-shell', {
        recursive: true,
      });
      expect(fs.writeFile).toHaveBeenCalledWith(
        expect.stringContaining('.tmp.'),
        expect.stringContaining('"skill-a"'),
        'utf8',
      );
      expect(fs.rename).toHaveBeenCalled();
    });

    it('should not throw on write failure', async () => {
      vi.mocked(fs.mkdir).mockRejectedValue(new Error('permission denied'));

      const state: SkillsState = {
        disabledSkills: {
          project: [],
          custom: [],
          user: [],
          extension: [],
          system: [],
        },
      };

      // Should not throw
      await expect(manager.writeState(state)).resolves.toBeUndefined();
    });
  });

  describe('isDisabled', () => {
    it('should return true for disabled skills', async () => {
      const stored: SkillsState = {
        disabledSkills: {
          project: ['disabled-skill'],
          custom: [],
          user: [],
          extension: [],
          system: [],
        },
      };
      vi.mocked(fs.readFile).mockResolvedValue(JSON.stringify(stored));

      expect(await manager.isDisabled('disabled-skill', 'project')).toBe(true);
      expect(await manager.isDisabled('other-skill', 'project')).toBe(false);
    });
  });

  describe('toggleDisabled', () => {
    it('should disable an enabled skill', async () => {
      const stored: SkillsState = {
        disabledSkills: {
          project: [],
          custom: [],
          user: [],
          extension: [],
          system: [],
        },
      };
      vi.mocked(fs.readFile).mockResolvedValue(JSON.stringify(stored));
      vi.mocked(fs.mkdir).mockResolvedValue(undefined);
      vi.mocked(fs.writeFile).mockResolvedValue();
      vi.mocked(fs.rename).mockResolvedValue();

      const result = await manager.toggleDisabled('my-skill', 'user');
      expect(result).toBe(true); // now disabled

      // Verify it was written with the skill disabled
      const writtenContent = vi.mocked(fs.writeFile).mock.calls[0][1] as string;
      const writtenState = JSON.parse(writtenContent) as SkillsState;
      expect(writtenState.disabledSkills.user).toContain('my-skill');
    });

    it('should enable a disabled skill', async () => {
      const stored: SkillsState = {
        disabledSkills: {
          project: [],
          custom: [],
          user: ['my-skill'],
          extension: [],
          system: [],
        },
      };
      vi.mocked(fs.readFile).mockResolvedValue(JSON.stringify(stored));
      vi.mocked(fs.mkdir).mockResolvedValue(undefined);
      vi.mocked(fs.writeFile).mockResolvedValue();
      vi.mocked(fs.rename).mockResolvedValue();

      const result = await manager.toggleDisabled('my-skill', 'user');
      expect(result).toBe(false); // now enabled

      const writtenContent = vi.mocked(fs.writeFile).mock.calls[0][1] as string;
      const writtenState = JSON.parse(writtenContent) as SkillsState;
      expect(writtenState.disabledSkills.user).not.toContain('my-skill');
    });
  });

  describe('cleanupStaleEntries', () => {
    it('should remove entries for skills that no longer exist', async () => {
      const stored: SkillsState = {
        disabledSkills: {
          project: ['exists', 'stale'],
          custom: [],
          user: ['also-stale'],
          extension: [],
          system: [],
        },
      };
      vi.mocked(fs.readFile).mockResolvedValue(JSON.stringify(stored));
      vi.mocked(fs.mkdir).mockResolvedValue(undefined);
      vi.mocked(fs.writeFile).mockResolvedValue();
      vi.mocked(fs.rename).mockResolvedValue();

      const existing = new Map([
        ['project', ['exists']],
        ['user', []],
      ] as Array<[import('./types.js').SkillLevel, string[]]>);

      await manager.cleanupStaleEntries(existing);

      const writtenContent = vi.mocked(fs.writeFile).mock.calls[0][1] as string;
      const writtenState = JSON.parse(writtenContent) as SkillsState;
      expect(writtenState.disabledSkills.project).toEqual(['exists']);
      expect(writtenState.disabledSkills.user).toEqual([]);
    });

    it('should not write if nothing changed', async () => {
      const stored: SkillsState = {
        disabledSkills: {
          project: ['exists'],
          custom: [],
          user: [],
          extension: [],
          system: [],
        },
      };
      vi.mocked(fs.readFile).mockResolvedValue(JSON.stringify(stored));

      const existing = new Map([['project', ['exists']]] as Array<
        [import('./types.js').SkillLevel, string[]]
      >);

      await manager.cleanupStaleEntries(existing);

      expect(fs.writeFile).not.toHaveBeenCalled();
    });
  });
});
