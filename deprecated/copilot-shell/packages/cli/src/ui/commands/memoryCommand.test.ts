/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import type { Mock } from 'vitest';
import { vi, describe, it, expect, beforeEach } from 'vitest';
import { memoryCommand } from './memoryCommand.js';
import type { SlashCommand, CommandContext } from './types.js';
import { createMockCommandContext } from '../../test-utils/mockCommandContext.js';
import { MessageType } from '../types.js';
import type { LoadedSettings } from '../../config/settings.js';
import { readFile, writeFile, unlink, mkdir } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import * as Diff from 'diff';
import {
  getErrorMessage,
  loadServerHierarchicalMemory,
  QWEN_DIR,
  setGeminiMdFilename,
  listInboxPatchFiles,
  validateInboxMemoryPatchFile,
  type FileDiscoveryService,
  type LoadServerHierarchicalMemoryResponse,
} from '@copilot-shell/core';

vi.mock('@copilot-shell/core', async (importOriginal) => {
  const original = await importOriginal<typeof import('@copilot-shell/core')>();
  return {
    ...original,
    getErrorMessage: vi.fn((error: unknown) => {
      if (error instanceof Error) return error.message;
      return String(error);
    }),
    loadServerHierarchicalMemory: vi.fn(),
    listInboxPatchFiles: vi.fn(),
    validateInboxMemoryPatchFile: vi.fn(),
  };
});

vi.mock('node:fs/promises', () => {
  const readFile = vi.fn();
  const writeFile = vi.fn();
  const unlink = vi.fn();
  const mkdir = vi.fn();
  return {
    readFile,
    writeFile,
    unlink,
    mkdir,
    default: {
      readFile,
      writeFile,
      unlink,
      mkdir,
    },
  };
});

vi.mock('diff', () => ({
  applyPatch: vi.fn(),
  parsePatch: vi.fn(),
}));

const mockLoadServerHierarchicalMemory = loadServerHierarchicalMemory as Mock;
const mockReadFile = readFile as unknown as Mock;
const mockWriteFile = writeFile as unknown as Mock;
const mockUnlink = unlink as unknown as Mock;
const mockMkdir = mkdir as unknown as Mock;
const mockListInboxPatchFiles = listInboxPatchFiles as unknown as Mock;
const mockValidateInboxMemoryPatchFile =
  validateInboxMemoryPatchFile as unknown as Mock;
const mockApplyPatch = Diff.applyPatch as unknown as Mock;

describe('memoryCommand', () => {
  let mockContext: CommandContext;

  const getSubCommand = (name: 'show' | 'add' | 'refresh'): SlashCommand => {
    const subCommand = memoryCommand.subCommands?.find(
      (cmd) => cmd.name === name,
    );
    if (!subCommand) {
      throw new Error(`/memory ${name} command not found.`);
    }
    return subCommand;
  };

  const getInboxSubCommand = (name: 'approve' | 'dismiss'): SlashCommand => {
    const inbox = memoryCommand.subCommands?.find((c) => c.name === 'inbox');
    const sub = inbox?.subCommands?.find((c) => c.name === name);
    if (!sub) throw new Error(`/memory inbox ${name} command not found.`);
    return sub;
  };

  describe('/memory show', () => {
    let showCommand: SlashCommand;
    let mockGetUserMemory: Mock;
    let mockGetGeminiMdFileCount: Mock;

    beforeEach(() => {
      setGeminiMdFilename('COPILOT.md');
      mockReadFile.mockReset();
      vi.restoreAllMocks();

      showCommand = getSubCommand('show');

      mockGetUserMemory = vi.fn();
      mockGetGeminiMdFileCount = vi.fn();

      mockContext = createMockCommandContext({
        services: {
          config: {
            getUserMemory: mockGetUserMemory,
            getGeminiMdFileCount: mockGetGeminiMdFileCount,
          },
        },
      });
    });

    it('should display a message if memory is empty', async () => {
      if (!showCommand.action) throw new Error('Command has no action');

      mockGetUserMemory.mockReturnValue('');
      mockGetGeminiMdFileCount.mockReturnValue(0);

      await showCommand.action(mockContext, '');

      expect(mockContext.ui.addItem).toHaveBeenCalledWith(
        {
          type: MessageType.INFO,
          text: 'Memory is currently empty.',
        },
        expect.any(Number),
      );
    });

    it('should display the memory content and file count if it exists', async () => {
      if (!showCommand.action) throw new Error('Command has no action');

      const memoryContent = 'This is a test memory.';

      mockGetUserMemory.mockReturnValue(memoryContent);
      mockGetGeminiMdFileCount.mockReturnValue(1);

      await showCommand.action(mockContext, '');

      expect(mockContext.ui.addItem).toHaveBeenCalledWith(
        {
          type: MessageType.INFO,
          text: `Current memory content from 1 file(s):\n\n---\n${memoryContent}\n---`,
        },
        expect.any(Number),
      );
    });

    it('should show project memory from the configured context file', async () => {
      const projectCommand = showCommand.subCommands?.find(
        (cmd) => cmd.name === '--project',
      );
      if (!projectCommand?.action) throw new Error('Command has no action');

      setGeminiMdFilename('AGENTS.md');
      vi.spyOn(process, 'cwd').mockReturnValue('/test/project');
      mockReadFile.mockResolvedValue('project memory');

      await projectCommand.action(mockContext, '');

      const expectedProjectPath = path.join('/test/project', 'AGENTS.md');
      expect(mockReadFile).toHaveBeenCalledWith(expectedProjectPath, 'utf-8');
      expect(mockContext.ui.addItem).toHaveBeenCalledWith(
        {
          type: MessageType.INFO,
          text: expect.stringContaining(expectedProjectPath),
        },
        expect.any(Number),
      );
    });

    it('should show global memory from the configured context file', async () => {
      const globalCommand = showCommand.subCommands?.find(
        (cmd) => cmd.name === '--global',
      );
      if (!globalCommand?.action) throw new Error('Command has no action');

      setGeminiMdFilename('AGENTS.md');
      vi.spyOn(os, 'homedir').mockReturnValue('/home/user');
      mockReadFile.mockResolvedValue('global memory');

      await globalCommand.action(mockContext, '');

      const expectedGlobalPath = path.join('/home/user', QWEN_DIR, 'AGENTS.md');
      expect(mockReadFile).toHaveBeenCalledWith(expectedGlobalPath, 'utf-8');
      expect(mockContext.ui.addItem).toHaveBeenCalledWith(
        {
          type: MessageType.INFO,
          text: expect.stringContaining('Global memory content'),
        },
        expect.any(Number),
      );
    });
  });

  describe('/memory add', () => {
    let addCommand: SlashCommand;

    beforeEach(() => {
      addCommand = getSubCommand('add');
      mockContext = createMockCommandContext();
    });

    it('should return an error message if no arguments are provided', () => {
      if (!addCommand.action) throw new Error('Command has no action');

      const result = addCommand.action(mockContext, '  ');
      expect(result).toEqual({
        type: 'message',
        messageType: 'error',
        content: 'Usage: /memory add [--global|--project] <text to remember>',
      });

      expect(mockContext.ui.addItem).not.toHaveBeenCalled();
    });

    it('should return a tool action and add an info message when arguments are provided', () => {
      if (!addCommand.action) throw new Error('Command has no action');

      const fact = 'remember this';
      const result = addCommand.action(mockContext, `  ${fact}  `);

      expect(mockContext.ui.addItem).toHaveBeenCalledWith(
        {
          type: MessageType.INFO,
          text: `Attempting to save to memory : "${fact}"`,
        },
        expect.any(Number),
      );

      expect(result).toEqual({
        type: 'tool',
        toolName: 'save_memory',
        toolArgs: { fact },
      });
    });

    it('should handle --global flag and add scope to tool args', () => {
      if (!addCommand.action) throw new Error('Command has no action');

      const fact = 'remember this globally';
      const result = addCommand.action(mockContext, `--global ${fact}`);

      expect(mockContext.ui.addItem).toHaveBeenCalledWith(
        {
          type: MessageType.INFO,
          text: `Attempting to save to memory (global): "${fact}"`,
        },
        expect.any(Number),
      );

      expect(result).toEqual({
        type: 'tool',
        toolName: 'save_memory',
        toolArgs: { fact, scope: 'global' },
      });
    });

    it('should handle --project flag and add scope to tool args', () => {
      if (!addCommand.action) throw new Error('Command has no action');

      const fact = 'remember this for project';
      const result = addCommand.action(mockContext, `--project ${fact}`);

      expect(mockContext.ui.addItem).toHaveBeenCalledWith(
        {
          type: MessageType.INFO,
          text: `Attempting to save to memory (project): "${fact}"`,
        },
        expect.any(Number),
      );

      expect(result).toEqual({
        type: 'tool',
        toolName: 'save_memory',
        toolArgs: { fact, scope: 'project' },
      });
    });

    it('should return error if flag is provided but no fact follows', () => {
      if (!addCommand.action) throw new Error('Command has no action');

      const result = addCommand.action(mockContext, '--global   ');
      expect(result).toEqual({
        type: 'message',
        messageType: 'error',
        content: 'Usage: /memory add [--global|--project] <text to remember>',
      });

      expect(mockContext.ui.addItem).not.toHaveBeenCalled();
    });
  });

  describe('/memory refresh', () => {
    let refreshCommand: SlashCommand;
    let mockSetUserMemory: Mock;
    let mockSetGeminiMdFileCount: Mock;

    beforeEach(() => {
      refreshCommand = getSubCommand('refresh');
      mockSetUserMemory = vi.fn();
      mockSetGeminiMdFileCount = vi.fn();
      const mockConfig = {
        setUserMemory: mockSetUserMemory,
        setGeminiMdFileCount: mockSetGeminiMdFileCount,
        getWorkingDir: () => '/test/dir',
        getDebugMode: () => false,
        getFileService: () => ({}) as FileDiscoveryService,
        getExtensionContextFilePaths: () => [],
        shouldLoadMemoryFromIncludeDirectories: () => false,
        getWorkspaceContext: () => ({
          getDirectories: () => [],
        }),
        getFileFilteringOptions: () => ({
          ignore: [],
          include: [],
        }),
        getFolderTrust: () => false,
      };

      mockContext = createMockCommandContext({
        services: {
          config: mockConfig,
          settings: {
            merged: {},
          } as LoadedSettings,
        },
      });
      mockLoadServerHierarchicalMemory.mockClear();
    });

    it('should display success message when memory is refreshed with content', async () => {
      if (!refreshCommand.action) throw new Error('Command has no action');

      const refreshResult: LoadServerHierarchicalMemoryResponse = {
        memoryContent: 'new memory content',
        fileCount: 2,
      };
      mockLoadServerHierarchicalMemory.mockResolvedValue(refreshResult);

      await refreshCommand.action(mockContext, '');

      expect(mockContext.ui.addItem).toHaveBeenCalledWith(
        {
          type: MessageType.INFO,
          text: 'Refreshing memory from source files...',
        },
        expect.any(Number),
      );

      expect(loadServerHierarchicalMemory).toHaveBeenCalledOnce();
      expect(mockSetUserMemory).toHaveBeenCalledWith(
        refreshResult.memoryContent,
      );
      expect(mockSetGeminiMdFileCount).toHaveBeenCalledWith(
        refreshResult.fileCount,
      );

      expect(mockContext.ui.addItem).toHaveBeenCalledWith(
        {
          type: MessageType.INFO,
          text: 'Memory refreshed successfully. Loaded 18 characters from 2 file(s).',
        },
        expect.any(Number),
      );
    });

    it('should display success message when memory is refreshed with no content', async () => {
      if (!refreshCommand.action) throw new Error('Command has no action');

      const refreshResult = { memoryContent: '', fileCount: 0 };
      mockLoadServerHierarchicalMemory.mockResolvedValue(refreshResult);

      await refreshCommand.action(mockContext, '');

      expect(loadServerHierarchicalMemory).toHaveBeenCalledOnce();
      expect(mockSetUserMemory).toHaveBeenCalledWith('');
      expect(mockSetGeminiMdFileCount).toHaveBeenCalledWith(0);

      expect(mockContext.ui.addItem).toHaveBeenCalledWith(
        {
          type: MessageType.INFO,
          text: 'Memory refreshed successfully. No memory content found.',
        },
        expect.any(Number),
      );
    });

    it('should display an error message if refreshing fails', async () => {
      if (!refreshCommand.action) throw new Error('Command has no action');

      const error = new Error('Failed to read memory files.');
      mockLoadServerHierarchicalMemory.mockRejectedValue(error);

      await refreshCommand.action(mockContext, '');

      expect(loadServerHierarchicalMemory).toHaveBeenCalledOnce();
      expect(mockSetUserMemory).not.toHaveBeenCalled();
      expect(mockSetGeminiMdFileCount).not.toHaveBeenCalled();

      expect(mockContext.ui.addItem).toHaveBeenCalledWith(
        {
          type: MessageType.ERROR,
          text: `Error refreshing memory: ${error.message}`,
        },
        expect.any(Number),
      );

      expect(getErrorMessage).toHaveBeenCalledWith(error);
    });

    it('should not throw if config service is unavailable', async () => {
      if (!refreshCommand.action) throw new Error('Command has no action');

      const nullConfigContext = createMockCommandContext({
        services: { config: null },
      });

      await expect(
        refreshCommand.action(nullConfigContext, ''),
      ).resolves.toBeUndefined();

      expect(nullConfigContext.ui.addItem).toHaveBeenCalledWith(
        {
          type: MessageType.INFO,
          text: 'Refreshing memory from source files...',
        },
        expect.any(Number),
      );

      expect(loadServerHierarchicalMemory).not.toHaveBeenCalled();
    });
  });

  describe('/memory inbox approve', () => {
    let approveCommand: SlashCommand;

    beforeEach(() => {
      approveCommand = getInboxSubCommand('approve');
      mockListInboxPatchFiles.mockReset();
      mockValidateInboxMemoryPatchFile.mockReset();
      mockReadFile.mockReset();
      mockWriteFile.mockReset();
      mockUnlink.mockReset();
      mockMkdir.mockReset();
      mockApplyPatch.mockReset();
      mockMkdir.mockResolvedValue(undefined);
      mockWriteFile.mockResolvedValue(undefined);
      mockUnlink.mockResolvedValue(undefined);
      mockContext = createMockCommandContext({
        services: {
          config: {} as never,
        },
      });
    });

    it('applies and deletes a patch file when all hunks apply cleanly', async () => {
      if (!approveCommand.action) throw new Error('no action');
      const patchFile = '/mem/.inbox/private/ok.patch';
      const target = '/mem/hello.md';
      const parsedDiff = { hunks: [{}] };

      mockListInboxPatchFiles.mockImplementation(async (_c, kind) =>
        kind === 'private' ? [patchFile] : [],
      );
      mockValidateInboxMemoryPatchFile.mockResolvedValue({
        valid: true,
        patches: [{ targetPath: target, isNewFile: true }],
        parsed: [parsedDiff],
      });
      mockApplyPatch.mockReturnValue('hello\n');

      await approveCommand.action(mockContext, '');

      expect(mockWriteFile).toHaveBeenCalledWith(target, 'hello\n');
      expect(mockUnlink).toHaveBeenCalledWith(patchFile);
      expect(mockContext.ui.addItem).toHaveBeenCalledWith(
        {
          type: MessageType.INFO,
          text: expect.stringContaining('Applied 1 patch file(s).'),
        },
        expect.any(Number),
      );
    });

    it('retains patch file and does not unlink when validation fails', async () => {
      if (!approveCommand.action) throw new Error('no action');
      const patchFile = '/mem/.inbox/private/bad.patch';

      mockListInboxPatchFiles.mockImplementation(async (_c, kind) =>
        kind === 'private' ? [patchFile] : [],
      );
      mockValidateInboxMemoryPatchFile.mockResolvedValue({
        valid: false,
        reason: 'no hunks found in patch',
      });

      await approveCommand.action(mockContext, '');

      expect(mockWriteFile).not.toHaveBeenCalled();
      expect(mockUnlink).not.toHaveBeenCalled();
      expect(mockContext.ui.addItem).toHaveBeenCalledWith(
        {
          type: MessageType.INFO,
          text: expect.stringContaining('Retained 1 patch file(s) for retry.'),
        },
        expect.any(Number),
      );
    });

    it('retains patch file when applyPatch returns false (all-or-nothing)', async () => {
      if (!approveCommand.action) throw new Error('no action');
      const patchFile = '/mem/.inbox/private/fail.patch';
      const target = '/mem/hello.md';

      mockListInboxPatchFiles.mockImplementation(async (_c, kind) =>
        kind === 'private' ? [patchFile] : [],
      );
      mockValidateInboxMemoryPatchFile.mockResolvedValue({
        valid: true,
        patches: [{ targetPath: target, isNewFile: false }],
        parsed: [{ hunks: [{}] }],
      });
      mockReadFile.mockResolvedValue('existing\n');
      mockApplyPatch.mockReturnValue(false);

      await approveCommand.action(mockContext, '');

      expect(mockWriteFile).not.toHaveBeenCalled();
      expect(mockUnlink).not.toHaveBeenCalled();
      expect(mockContext.ui.addItem).toHaveBeenCalledWith(
        {
          type: MessageType.INFO,
          text: expect.stringContaining(
            'diff does not apply cleanly to /mem/hello.md',
          ),
        },
        expect.any(Number),
      );
    });

    it('retains patch file when write fails after applyPatch success', async () => {
      if (!approveCommand.action) throw new Error('no action');
      const patchFile = '/mem/.inbox/private/write-fail.patch';
      const target = '/mem/hello.md';

      mockListInboxPatchFiles.mockImplementation(async (_c, kind) =>
        kind === 'private' ? [patchFile] : [],
      );
      mockValidateInboxMemoryPatchFile.mockResolvedValue({
        valid: true,
        patches: [{ targetPath: target, isNewFile: true }],
        parsed: [{ hunks: [{}] }],
      });
      mockApplyPatch.mockReturnValue('new\n');
      mockWriteFile.mockRejectedValue(new Error('disk full'));

      await approveCommand.action(mockContext, '');

      expect(mockUnlink).not.toHaveBeenCalled();
      expect(mockContext.ui.addItem).toHaveBeenCalledWith(
        {
          type: MessageType.INFO,
          text: expect.stringContaining('write failed: disk full'),
        },
        expect.any(Number),
      );
    });

    it('filters by kind argument', async () => {
      if (!approveCommand.action) throw new Error('no action');
      mockListInboxPatchFiles.mockResolvedValue([]);

      await approveCommand.action(mockContext, 'global');

      expect(mockListInboxPatchFiles).toHaveBeenCalledTimes(1);
      expect(mockListInboxPatchFiles).toHaveBeenCalledWith(
        expect.anything(),
        'global',
      );
    });

    it('shows empty message when no patches are pending', async () => {
      if (!approveCommand.action) throw new Error('no action');
      mockListInboxPatchFiles.mockResolvedValue([]);

      await approveCommand.action(mockContext, '');

      expect(mockContext.ui.addItem).toHaveBeenCalledWith(
        {
          type: MessageType.INFO,
          text: expect.stringContaining('No pending patches to approve.'),
        },
        expect.any(Number),
      );
    });
  });
});
