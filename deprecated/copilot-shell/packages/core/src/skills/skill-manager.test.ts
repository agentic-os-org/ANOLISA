/**
 * @license
 * Copyright 2025 Qwen
 * SPDX-License-Identifier: Apache-2.0
 */

import { vi, describe, it, expect, beforeEach, afterEach } from 'vitest';
import * as fs from 'fs/promises';
import * as path from 'path';
import * as os from 'os';
import { SkillManager } from './skill-manager.js';
import { type SkillConfig, SkillError } from './types.js';
import type { Config } from '../config/config.js';
import { makeFakeConfig } from '../test-utils/config.js';

// Mock file system operations
vi.mock('fs/promises');
vi.mock('os');

// Mock yaml parser - use vi.hoisted for proper hoisting
const mockParseYaml = vi.hoisted(() => vi.fn());

vi.mock('../utils/yaml-parser.js', () => ({
  parse: mockParseYaml,
  stringify: vi.fn(),
}));

describe('SkillManager', () => {
  let manager: SkillManager;
  let mockConfig: Config;

  beforeEach(() => {
    // Create mock Config object using test utility
    mockConfig = makeFakeConfig({});

    // Mock the project root method
    vi.spyOn(mockConfig, 'getProjectRoot').mockReturnValue('/test/project');

    // Mock os.homedir
    vi.mocked(os.homedir).mockReturnValue('/home/user');

    // Reset and setup mocks
    vi.clearAllMocks();

    // Setup yaml parser mocks with sophisticated behavior
    mockParseYaml.mockImplementation((yamlString: string) => {
      // Handle different test cases based on YAML content
      if (yamlString.includes('allowedTools:')) {
        return {
          name: 'test-skill',
          description: 'A test skill',
          allowedTools: ['read_file', 'write_file'],
        };
      }
      if (yamlString.includes('name: skill1')) {
        return { name: 'skill1', description: 'First skill' };
      }
      if (yamlString.includes('name: skill2')) {
        return { name: 'skill2', description: 'Second skill' };
      }
      if (yamlString.includes('name: skill3')) {
        return { name: 'skill3', description: 'Third skill' };
      }
      if (!yamlString.includes('name:')) {
        return { description: 'A test skill' }; // Missing name case
      }
      if (!yamlString.includes('description:')) {
        return { name: 'test-skill' }; // Missing description case
      }
      // Default case
      return {
        name: 'test-skill',
        description: 'A test skill',
      };
    });

    manager = new SkillManager(mockConfig);
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  const validSkillConfig: SkillConfig = {
    name: 'test-skill',
    description: 'A test skill',
    level: 'project',
    filePath: '/test/project/.copilot-shell/skills/test-skill/SKILL.md',
    body: 'You are a helpful assistant with this skill.',
  };

  const validMarkdown = `---
name: test-skill
description: A test skill
---

You are a helpful assistant with this skill.
`;

  describe('parseSkillContent', () => {
    it('should parse valid markdown content', () => {
      const config = manager.parseSkillContent(
        validMarkdown,
        validSkillConfig.filePath,
        'project',
      );

      expect(config.name).toBe('test-skill');
      expect(config.description).toBe('A test skill');
      expect(config.body).toBe('You are a helpful assistant with this skill.');
      expect(config.level).toBe('project');
      expect(config.filePath).toBe(validSkillConfig.filePath);
    });

    it('should parse markdown with CRLF line endings', () => {
      const markdownCrlf = `---\r
name: test-skill\r
description: A test skill\r
---\r
\r
You are a helpful assistant with this skill.\r
`;

      const config = manager.parseSkillContent(
        markdownCrlf,
        validSkillConfig.filePath,
        'project',
      );

      expect(config.name).toBe('test-skill');
      expect(config.description).toBe('A test skill');
      expect(config.body).toBe('You are a helpful assistant with this skill.');
    });

    it('should parse markdown with UTF-8 BOM', () => {
      const markdownWithBom = `\uFEFF---
name: test-skill
description: A test skill
---

You are a helpful assistant with this skill.
`;

      const config = manager.parseSkillContent(
        markdownWithBom,
        validSkillConfig.filePath,
        'project',
      );

      expect(config.name).toBe('test-skill');
      expect(config.description).toBe('A test skill');
    });

    it('should parse markdown when body is empty and file ends after frontmatter', () => {
      const frontmatterOnly = `---
name: test-skill
description: A test skill
---`;

      const config = manager.parseSkillContent(
        frontmatterOnly,
        validSkillConfig.filePath,
        'project',
      );

      expect(config.name).toBe('test-skill');
      expect(config.description).toBe('A test skill');
      expect(config.body).toBe('');
    });

    it('should parse content with allowedTools', () => {
      const markdownWithTools = `---
name: test-skill
description: A test skill
allowedTools:
  - read_file
  - write_file
---

You are a helpful assistant with this skill.
`;

      const config = manager.parseSkillContent(
        markdownWithTools,
        validSkillConfig.filePath,
        'project',
      );

      expect(config.allowedTools).toEqual(['read_file', 'write_file']);
    });

    it('should determine level from file path', () => {
      const projectPath =
        '/test/project/.copilot-shell/skills/test-skill/SKILL.md';
      const userPath = '/home/user/.copilot-shell/skills/test-skill/SKILL.md';

      const projectConfig = manager.parseSkillContent(
        validMarkdown,
        projectPath,
        'project',
      );
      const userConfig = manager.parseSkillContent(
        validMarkdown,
        userPath,
        'user',
      );

      expect(projectConfig.level).toBe('project');
      expect(userConfig.level).toBe('user');
    });

    it('should throw error for invalid frontmatter format', () => {
      const invalidMarkdown = `No frontmatter here
Just content`;

      expect(() =>
        manager.parseSkillContent(
          invalidMarkdown,
          validSkillConfig.filePath,
          'project',
        ),
      ).toThrow(SkillError);
    });

    it('should throw error for missing name', () => {
      const markdownWithoutName = `---
description: A test skill
---

You are a helpful assistant.
`;

      expect(() =>
        manager.parseSkillContent(
          markdownWithoutName,
          validSkillConfig.filePath,
          'project',
        ),
      ).toThrow(SkillError);
    });

    it('should throw error for missing description', () => {
      const markdownWithoutDescription = `---
name: test-skill
---

You are a helpful assistant.
`;

      expect(() =>
        manager.parseSkillContent(
          markdownWithoutDescription,
          validSkillConfig.filePath,
          'project',
        ),
      ).toThrow(SkillError);
    });
  });

  describe('validateConfig', () => {
    it('should validate valid configuration', () => {
      const result = manager.validateConfig(validSkillConfig);

      expect(result.isValid).toBe(true);
      expect(result.errors).toHaveLength(0);
    });

    it('should report error for missing name', () => {
      const invalidConfig = { ...validSkillConfig, name: '' };
      const result = manager.validateConfig(invalidConfig);

      expect(result.isValid).toBe(false);
      expect(result.errors).toContain('"name" cannot be empty');
    });

    it('should report error for missing description', () => {
      const invalidConfig = { ...validSkillConfig, description: '' };
      const result = manager.validateConfig(invalidConfig);

      expect(result.isValid).toBe(false);
      expect(result.errors).toContain('"description" cannot be empty');
    });

    it('should report error for invalid allowedTools type', () => {
      const invalidConfig = {
        ...validSkillConfig,
        allowedTools: 'not-an-array' as unknown as string[],
      };
      const result = manager.validateConfig(invalidConfig);

      expect(result.isValid).toBe(false);
      expect(result.errors).toContain('"allowedTools" must be an array');
    });

    it('should warn for empty body', () => {
      const configWithEmptyBody = { ...validSkillConfig, body: '' };
      const result = manager.validateConfig(configWithEmptyBody);

      expect(result.isValid).toBe(true); // Still valid
      expect(result.warnings).toContain('Skill body is empty');
    });
  });

  describe('loadSkill', () => {
    it('should load skill from project level first', async () => {
      vi.mocked(fs.readdir).mockResolvedValue([
        { name: 'test-skill', isDirectory: () => true, isFile: () => false },
      ] as unknown as Awaited<ReturnType<typeof fs.readdir>>);
      vi.mocked(fs.access).mockResolvedValue(undefined);
      vi.mocked(fs.readFile).mockResolvedValue(validMarkdown);

      const config = await manager.loadSkill('test-skill');

      expect(config).toBeDefined();
      expect(config!.name).toBe('test-skill');
    });

    it('should fall back to user level if project level fails', async () => {
      vi.mocked(fs.readdir)
        .mockRejectedValueOnce(new Error('Project dir not found')) // project level fails
        .mockResolvedValueOnce([
          { name: 'test-skill', isDirectory: () => true, isFile: () => false },
        ] as unknown as Awaited<ReturnType<typeof fs.readdir>>); // user level succeeds
      vi.mocked(fs.access).mockResolvedValue(undefined);
      vi.mocked(fs.readFile).mockResolvedValue(validMarkdown);

      const config = await manager.loadSkill('test-skill');

      expect(config).toBeDefined();
      expect(config!.name).toBe('test-skill');
    });

    it('should return null if not found at either level', async () => {
      vi.mocked(fs.readdir).mockRejectedValue(new Error('Directory not found'));

      const config = await manager.loadSkill('nonexistent');

      expect(config).toBeNull();
    });
  });

  describe('loadSkillForRuntime', () => {
    it('should load skill for runtime', async () => {
      vi.mocked(fs.readdir).mockResolvedValueOnce([
        { name: 'test-skill', isDirectory: () => true, isFile: () => false },
      ] as unknown as Awaited<ReturnType<typeof fs.readdir>>);

      vi.mocked(fs.access).mockResolvedValue(undefined);
      vi.mocked(fs.readFile).mockResolvedValue(validMarkdown); // SKILL.md

      const config = await manager.loadSkillForRuntime('test-skill');

      expect(config).toBeDefined();
      expect(config!.name).toBe('test-skill');
    });

    it('should return null if skill not found', async () => {
      vi.mocked(fs.readdir).mockRejectedValue(new Error('Directory not found'));

      const config = await manager.loadSkillForRuntime('nonexistent');

      expect(config).toBeNull();
    });
  });

  describe('listSkills', () => {
    beforeEach(() => {
      // Mock directory listing for skills directories (with Dirent objects)
      vi.mocked(fs.readdir)
        .mockResolvedValueOnce([
          { name: 'skill1', isDirectory: () => true, isFile: () => false },
          { name: 'skill2', isDirectory: () => true, isFile: () => false },
          {
            name: 'not-a-dir.txt',
            isDirectory: () => false,
            isFile: () => true,
          },
        ] as unknown as Awaited<ReturnType<typeof fs.readdir>>)
        .mockResolvedValueOnce([
          { name: 'skill3', isDirectory: () => true, isFile: () => false },
          { name: 'skill1', isDirectory: () => true, isFile: () => false },
        ] as unknown as Awaited<ReturnType<typeof fs.readdir>>)
        .mockResolvedValueOnce(
          [] as unknown as Awaited<ReturnType<typeof fs.readdir>>,
        ); // system level - empty

      vi.mocked(fs.access).mockResolvedValue(undefined);

      // Mock file reading for valid skills
      vi.mocked(fs.readFile).mockImplementation((filePath) => {
        const pathStr = String(filePath);
        if (pathStr.includes('skill1')) {
          return Promise.resolve(`---
name: skill1
description: First skill
---
Skill 1 content`);
        } else if (pathStr.includes('skill2')) {
          return Promise.resolve(`---
name: skill2
description: Second skill
---
Skill 2 content`);
        } else if (pathStr.includes('skill3')) {
          return Promise.resolve(`---
name: skill3
description: Third skill
---
Skill 3 content`);
        }
        return Promise.reject(new Error('File not found'));
      });
    });

    it('should list skills from both levels', async () => {
      const skills = await manager.listSkills();

      expect(skills).toHaveLength(3); // skill1 (project takes precedence), skill2, skill3
      expect(skills.map((s) => s.name).sort()).toEqual([
        'skill1',
        'skill2',
        'skill3',
      ]);
    });

    it('should prioritize project level over user level', async () => {
      const skills = await manager.listSkills();
      const skill1 = skills.find((s) => s.name === 'skill1');

      expect(skill1!.level).toBe('project');
    });

    it('should filter by level', async () => {
      const projectSkills = await manager.listSkills({
        level: 'project',
      });

      expect(projectSkills).toHaveLength(2); // skill1, skill2
      expect(projectSkills.every((s) => s.level === 'project')).toBe(true);
    });

    it('should handle empty directories', async () => {
      vi.mocked(fs.readdir).mockReset();
      vi.mocked(fs.readdir).mockResolvedValue(
        [] as unknown as Awaited<ReturnType<typeof fs.readdir>>,
      );

      const skills = await manager.listSkills({ force: true });

      expect(skills).toHaveLength(0);
    });

    it('should handle directory read errors', async () => {
      vi.mocked(fs.readdir).mockReset();
      vi.mocked(fs.readdir).mockRejectedValue(new Error('Directory not found'));

      const skills = await manager.listSkills({ force: true });

      expect(skills).toHaveLength(0);
    });
  });

  describe('getSkillsBaseDir', () => {
    it('should return project-level base dir', () => {
      const baseDir = manager.getSkillsBaseDir('project');

      expect(baseDir).toBe(
        path.join('/test/project', '.copilot-shell', 'skills'),
      );
    });

    it('should return user-level base dir', () => {
      const baseDir = manager.getSkillsBaseDir('user');

      expect(baseDir).toBe(path.join('/home/user', '.copilot-shell', 'skills'));
    });

    it('should return system-level base dir', () => {
      const baseDir = manager.getSkillsBaseDir('system');

      expect(baseDir).toBe('/usr/share/anolisa/skills');
    });
  });

  describe('change listeners', () => {
    it('should notify listeners when cache is refreshed', async () => {
      const listener = vi.fn();
      manager.addChangeListener(listener);

      vi.mocked(fs.readdir).mockResolvedValue(
        [] as unknown as Awaited<ReturnType<typeof fs.readdir>>,
      );

      await manager.refreshCache();

      expect(listener).toHaveBeenCalled();
    });

    it('should remove listener when cleanup function is called', async () => {
      const listener = vi.fn();
      const removeListener = manager.addChangeListener(listener);

      removeListener();

      vi.mocked(fs.readdir).mockResolvedValue(
        [] as unknown as Awaited<ReturnType<typeof fs.readdir>>,
      );

      await manager.refreshCache();

      expect(listener).not.toHaveBeenCalled();
    });
  });

  describe('parse errors', () => {
    it('should track parse errors', async () => {
      vi.mocked(fs.readdir).mockResolvedValue([
        { name: 'bad-skill', isDirectory: () => true, isFile: () => false },
      ] as unknown as Awaited<ReturnType<typeof fs.readdir>>);
      vi.mocked(fs.access).mockResolvedValue(undefined);
      vi.mocked(fs.readFile).mockResolvedValue(
        'invalid content without frontmatter',
      );

      await manager.listSkills({ force: true });

      const errors = manager.getParseErrors();
      expect(errors.size).toBeGreaterThan(0);
    });
  });

  describe('custom skill paths', () => {
    it('refreshCache should include custom level in cache', async () => {
      vi.spyOn(mockConfig, 'getResolvedCustomSkillPaths').mockReturnValue([]);
      vi.mocked(fs.readdir).mockResolvedValue(
        [] as unknown as Awaited<ReturnType<typeof fs.readdir>>,
      );

      await manager.refreshCache();

      // Access internal cache via listSkills with level filter
      const customSkills = await manager.listSkills({ level: 'custom' });
      expect(customSkills).toEqual([]);
    });

    it('listSkills should prioritize project over custom', async () => {
      vi.spyOn(mockConfig, 'getResolvedCustomSkillPaths').mockReturnValue([
        '/custom/skills',
      ]);

      vi.mocked(fs.readdir)
        .mockResolvedValueOnce([
          { name: 'skill1', isDirectory: () => true, isFile: () => false },
        ] as unknown as Awaited<ReturnType<typeof fs.readdir>>) // project
        .mockResolvedValueOnce([
          { name: 'skill1', isDirectory: () => true, isFile: () => false },
        ] as unknown as Awaited<ReturnType<typeof fs.readdir>>) // custom
        .mockResolvedValueOnce(
          [] as unknown as Awaited<ReturnType<typeof fs.readdir>>,
        ) // user
        .mockResolvedValueOnce(
          [] as unknown as Awaited<ReturnType<typeof fs.readdir>>,
        ) // system
        .mockResolvedValue(
          [] as unknown as Awaited<ReturnType<typeof fs.readdir>>,
        );

      vi.mocked(fs.access).mockResolvedValue(undefined);
      vi.mocked(fs.readFile).mockResolvedValue(`---
name: skill1
description: First skill
---
Skill 1 content`);

      const skills = await manager.listSkills({ force: true });
      const skill1 = skills.find((s) => s.name === 'skill1');
      expect(skill1).toBeDefined();
      expect(skill1!.level).toBe('project');
    });

    it('listSkills should prioritize custom over user', async () => {
      vi.spyOn(mockConfig, 'getResolvedCustomSkillPaths').mockReturnValue([
        '/custom/skills',
      ]);

      vi.mocked(fs.readdir)
        .mockResolvedValueOnce(
          [] as unknown as Awaited<ReturnType<typeof fs.readdir>>,
        ) // project
        .mockResolvedValueOnce([
          { name: 'skill1', isDirectory: () => true, isFile: () => false },
        ] as unknown as Awaited<ReturnType<typeof fs.readdir>>) // custom
        .mockResolvedValueOnce([
          { name: 'skill1', isDirectory: () => true, isFile: () => false },
        ] as unknown as Awaited<ReturnType<typeof fs.readdir>>) // user
        .mockResolvedValueOnce(
          [] as unknown as Awaited<ReturnType<typeof fs.readdir>>,
        ) // system
        .mockResolvedValue(
          [] as unknown as Awaited<ReturnType<typeof fs.readdir>>,
        );

      vi.mocked(fs.access).mockResolvedValue(undefined);
      vi.mocked(fs.readFile).mockResolvedValue(`---
name: skill1
description: First skill
---
Skill 1 content`);

      const skills = await manager.listSkills({ force: true });
      const skill1 = skills.find((s) => s.name === 'skill1');
      expect(skill1).toBeDefined();
      expect(skill1!.level).toBe('custom');
    });

    it('loadSkill should find custom skill before user skill', async () => {
      vi.spyOn(mockConfig, 'getResolvedCustomSkillPaths').mockReturnValue([
        '/custom/skills',
      ]);

      vi.mocked(fs.readdir)
        .mockRejectedValueOnce(new Error('No project dir')) // project
        .mockResolvedValueOnce([
          { name: 'test-skill', isDirectory: () => true, isFile: () => false },
        ] as unknown as Awaited<ReturnType<typeof fs.readdir>>) // custom
        .mockResolvedValue(
          [] as unknown as Awaited<ReturnType<typeof fs.readdir>>,
        );

      vi.mocked(fs.access).mockResolvedValue(undefined);
      vi.mocked(fs.readFile).mockResolvedValue(`---
name: test-skill
description: A test skill
---
Content`);

      const skill = await manager.loadSkill('test-skill');
      expect(skill).toBeDefined();
      expect(skill!.level).toBe('custom');
    });

    it('should handle empty custom paths gracefully', async () => {
      vi.spyOn(mockConfig, 'getResolvedCustomSkillPaths').mockReturnValue([]);
      vi.mocked(fs.readdir).mockResolvedValue(
        [] as unknown as Awaited<ReturnType<typeof fs.readdir>>,
      );

      const skills = await manager.listSkills({
        level: 'custom',
        force: true,
      });
      expect(skills).toEqual([]);
    });

    it('should handle non-existent custom directories', async () => {
      vi.spyOn(mockConfig, 'getResolvedCustomSkillPaths').mockReturnValue([
        '/nonexistent/dir',
      ]);
      vi.mocked(fs.readdir).mockRejectedValue(new Error('Directory not found'));

      const skills = await manager.listSkills({
        level: 'custom',
        force: true,
      });
      expect(skills).toEqual([]);
    });

    it('should load skills from multiple custom directories', async () => {
      vi.spyOn(mockConfig, 'getResolvedCustomSkillPaths').mockReturnValue([
        '/custom/dir1',
        '/custom/dir2',
      ]);

      let callCount = 0;
      vi.mocked(fs.readdir).mockImplementation(async () => {
        callCount++;
        if (callCount === 1) {
          // project - empty
          return [] as unknown as Awaited<ReturnType<typeof fs.readdir>>;
        }
        if (callCount === 2) {
          // custom dir1
          return [
            {
              name: 'skill-a',
              isDirectory: () => true,
              isFile: () => false,
            },
          ] as unknown as Awaited<ReturnType<typeof fs.readdir>>;
        }
        if (callCount === 3) {
          // custom dir2
          return [
            {
              name: 'skill-b',
              isDirectory: () => true,
              isFile: () => false,
            },
          ] as unknown as Awaited<ReturnType<typeof fs.readdir>>;
        }
        // user, system
        return [] as unknown as Awaited<ReturnType<typeof fs.readdir>>;
      });

      vi.mocked(fs.access).mockResolvedValue(undefined);

      mockParseYaml.mockImplementation((yamlString: string) => {
        if (yamlString.includes('name: skill-a')) {
          return { name: 'skill-a', description: 'Skill A' };
        }
        if (yamlString.includes('name: skill-b')) {
          return { name: 'skill-b', description: 'Skill B' };
        }
        return { name: 'unknown', description: 'Unknown' };
      });

      vi.mocked(fs.readFile).mockImplementation((filePath) => {
        const pathStr = String(filePath);
        if (pathStr.includes('skill-a')) {
          return Promise.resolve(`---
name: skill-a
description: Skill A
---
Skill A content`);
        }
        if (pathStr.includes('skill-b')) {
          return Promise.resolve(`---
name: skill-b
description: Skill B
---
Skill B content`);
        }
        return Promise.reject(new Error('File not found'));
      });

      const skills = await manager.listSkills({ force: true });
      const customSkills = skills.filter((s) => s.level === 'custom');
      expect(customSkills).toHaveLength(2);
      expect(customSkills.map((s) => s.name).sort()).toEqual([
        'skill-a',
        'skill-b',
      ]);
    });

    it('getSkillsBaseDir should return first custom dir for custom level', () => {
      vi.spyOn(mockConfig, 'getResolvedCustomSkillPaths').mockReturnValue([
        '/custom/dir1',
        '/custom/dir2',
      ]);
      const baseDir = manager.getSkillsBaseDir('custom');
      expect(baseDir).toBe('/custom/dir1');
    });

    it('getSkillsBaseDir should return empty string when no custom paths', () => {
      vi.spyOn(mockConfig, 'getResolvedCustomSkillPaths').mockReturnValue([]);
      const baseDir = manager.getSkillsBaseDir('custom');
      expect(baseDir).toBe('');
    });
  });
});
