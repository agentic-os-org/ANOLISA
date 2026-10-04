/**
 * @license
 * Copyright 2025 Qwen
 * SPDX-License-Identifier: Apache-2.0
 */

import { describe, it, expect, vi, beforeEach } from 'vitest';
import * as fs from 'node:fs/promises';
import * as os from 'node:os';
import * as path from 'node:path';
import { Session } from './Session.js';
import type { Config, GeminiChat } from '@copilot-shell/core';
import { ApprovalMode } from '@copilot-shell/core';
import type * as acp from '../acp.js';
import type { LoadedSettings } from '../../config/settings.js';
import * as nonInteractiveCliCommands from '../../nonInteractiveCliCommands.js';

vi.mock('../../nonInteractiveCliCommands.js', () => ({
  getAvailableCommands: vi.fn(),
  handleSlashCommand: vi.fn(),
}));

describe('Session', () => {
  let mockChat: GeminiChat;
  let mockConfig: Config;
  let mockClient: acp.Client;
  let mockSettings: LoadedSettings;
  let session: Session;
  let currentModel: string;
  let setModelSpy: ReturnType<typeof vi.fn>;
  let getAvailableCommandsSpy: ReturnType<typeof vi.fn>;

  beforeEach(() => {
    currentModel = 'qwen3-code-plus';
    setModelSpy = vi.fn().mockImplementation(async (modelId: string) => {
      currentModel = modelId;
    });

    mockChat = {
      sendMessageStream: vi.fn(),
      addHistory: vi.fn(),
    } as unknown as GeminiChat;

    const toolRegistry = { getTool: vi.fn() };
    const fileService = { shouldGitIgnoreFile: vi.fn().mockReturnValue(false) };

    mockConfig = {
      setApprovalMode: vi.fn(),
      setModel: setModelSpy,
      getModel: vi.fn().mockImplementation(() => currentModel),
      getSessionId: vi.fn().mockReturnValue('test-session-id'),
      getTelemetryLogPromptsEnabled: vi.fn().mockReturnValue(false),
      getUsageStatisticsEnabled: vi.fn().mockReturnValue(false),
      getContentGeneratorConfig: vi.fn().mockReturnValue(undefined),
      getChatRecordingService: vi.fn().mockReturnValue({
        recordUserMessage: vi.fn(),
        recordUiTelemetryEvent: vi.fn(),
      }),
      getToolRegistry: vi.fn().mockReturnValue(toolRegistry),
      getFileService: vi.fn().mockReturnValue(fileService),
      getFileFilteringRespectGitIgnore: vi.fn().mockReturnValue(true),
      getEnableRecursiveFileSearch: vi.fn().mockReturnValue(false),
      getTargetDir: vi.fn().mockReturnValue(process.cwd()),
      getDebugMode: vi.fn().mockReturnValue(false),
    } as unknown as Config;

    mockClient = {
      sessionUpdate: vi.fn().mockResolvedValue(undefined),
      requestPermission: vi.fn().mockResolvedValue({
        outcome: { outcome: 'selected', optionId: 'proceed_once' },
      }),
      sendCustomNotification: vi.fn().mockResolvedValue(undefined),
    } as unknown as acp.Client;

    mockSettings = {
      merged: {},
    } as LoadedSettings;

    getAvailableCommandsSpy = vi.mocked(nonInteractiveCliCommands)
      .getAvailableCommands as unknown as ReturnType<typeof vi.fn>;
    getAvailableCommandsSpy.mockResolvedValue([]);

    session = new Session(
      'test-session-id',
      mockChat,
      mockConfig,
      mockClient,
      mockSettings,
    );
  });

  describe('setMode', () => {
    it.each([
      ['plan', ApprovalMode.PLAN],
      ['default', ApprovalMode.DEFAULT],
      ['auto-edit', ApprovalMode.AUTO_EDIT],
      ['yolo', ApprovalMode.YOLO],
    ] as const)('maps %s mode', async (modeId, expected) => {
      const result = await session.setMode({
        sessionId: 'test-session-id',
        modeId,
      });

      expect(mockConfig.setApprovalMode).toHaveBeenCalledWith(expected);
      expect(result).toEqual({ modeId });
    });
  });

  describe('setModel', () => {
    it('sets model via config and returns current model', async () => {
      const result = await session.setModel({
        sessionId: 'test-session-id',
        modelId: '  qwen3-coder-plus  ',
      });

      expect(mockConfig.setModel).toHaveBeenCalledWith('qwen3-coder-plus', {
        reason: 'user_request_acp',
        context: 'session/set_model',
      });
      expect(mockConfig.getModel).toHaveBeenCalled();
      expect(result).toEqual({ modelId: 'qwen3-coder-plus' });
    });

    it('rejects empty/whitespace model IDs', async () => {
      await expect(
        session.setModel({
          sessionId: 'test-session-id',
          modelId: '   ',
        }),
      ).rejects.toThrow('Invalid params');

      expect(mockConfig.setModel).not.toHaveBeenCalled();
    });

    it('propagates errors from config.setModel', async () => {
      const configError = new Error('Invalid model');
      setModelSpy.mockRejectedValueOnce(configError);

      await expect(
        session.setModel({
          sessionId: 'test-session-id',
          modelId: 'invalid-model',
        }),
      ).rejects.toThrow('Invalid model');
    });
  });

  describe('sendAvailableCommandsUpdate', () => {
    it('sends available_commands_update from getAvailableCommands()', async () => {
      getAvailableCommandsSpy.mockResolvedValueOnce([
        {
          name: 'init',
          description: 'Initialize project context',
        },
      ]);

      await session.sendAvailableCommandsUpdate();

      expect(getAvailableCommandsSpy).toHaveBeenCalledWith(
        mockConfig,
        expect.any(AbortSignal),
      );
      expect(mockClient.sessionUpdate).toHaveBeenCalledWith({
        sessionId: 'test-session-id',
        update: {
          sessionUpdate: 'available_commands_update',
          availableCommands: [
            {
              name: 'init',
              description: 'Initialize project context',
              input: null,
            },
          ],
        },
      });
    });

    it('swallows errors and does not throw', async () => {
      const consoleErrorSpy = vi
        .spyOn(console, 'error')
        .mockImplementation(() => undefined);
      getAvailableCommandsSpy.mockRejectedValueOnce(
        new Error('Command discovery failed'),
      );

      await expect(
        session.sendAvailableCommandsUpdate(),
      ).resolves.toBeUndefined();
      expect(mockClient.sessionUpdate).not.toHaveBeenCalled();
      expect(consoleErrorSpy).toHaveBeenCalled();
      consoleErrorSpy.mockRestore();
    });
  });

  describe('prompt', () => {
    it('passes resolved paths to read_many_files tool', async () => {
      const tempDir = await fs.mkdtemp(
        path.join(os.tmpdir(), 'qwen-acp-session-'),
      );
      const fileName = 'README.md';
      const filePath = path.join(tempDir, fileName);

      try {
        await fs.writeFile(filePath, '# Test\n', 'utf8');

        const readManyFilesTool = {
          buildAndExecute: vi.fn().mockResolvedValue({
            llmContent: 'file content',
            returnDisplay: 'ok',
          }),
        };
        const toolRegistry = {
          getTool: vi.fn((name: string) =>
            name === 'read_many_files' ? readManyFilesTool : undefined,
          ),
        };
        const fileService = {
          shouldGitIgnoreFile: vi.fn().mockReturnValue(false),
        };

        mockConfig.getTargetDir = vi.fn().mockReturnValue(tempDir);
        mockConfig.getToolRegistry = vi.fn().mockReturnValue(toolRegistry);
        mockConfig.getFileService = vi.fn().mockReturnValue(fileService);
        mockChat.sendMessageStream = vi
          .fn()
          .mockResolvedValue((async function* () {})());

        const promptRequest: acp.PromptRequest = {
          sessionId: 'test-session-id',
          prompt: [
            { type: 'text', text: 'Check this file' },
            {
              type: 'resource_link',
              name: fileName,
              uri: `file://${fileName}`,
            },
          ],
        };

        await session.prompt(promptRequest);

        expect(readManyFilesTool.buildAndExecute).toHaveBeenCalledWith(
          { paths: [fileName] },
          expect.any(AbortSignal),
        );
      } finally {
        await fs.rm(tempDir, { recursive: true, force: true });
      }
    });

    it('percent-decodes file URIs so paths with spaces resolve', async () => {
      const tempDir = await fs.mkdtemp(
        path.join(os.tmpdir(), 'qwen-acp-session-'),
      );
      const fileName = 'my file.md';
      const filePath = path.join(tempDir, fileName);

      try {
        await fs.writeFile(filePath, '# Has a space\n', 'utf8');

        const readManyFilesTool = {
          buildAndExecute: vi.fn().mockResolvedValue({
            llmContent: 'file content',
            returnDisplay: 'ok',
          }),
        };
        const toolRegistry = {
          getTool: vi.fn((name: string) =>
            name === 'read_many_files' ? readManyFilesTool : undefined,
          ),
        };
        const fileService = {
          shouldGitIgnoreFile: vi.fn().mockReturnValue(false),
        };

        mockConfig.getTargetDir = vi.fn().mockReturnValue(tempDir);
        mockConfig.getToolRegistry = vi.fn().mockReturnValue(toolRegistry);
        mockConfig.getFileService = vi.fn().mockReturnValue(fileService);
        mockChat.sendMessageStream = vi
          .fn()
          .mockResolvedValue((async function* () {})());

        const promptRequest: acp.PromptRequest = {
          sessionId: 'test-session-id',
          prompt: [
            { type: 'text', text: 'Check this file' },
            {
              type: 'resource_link',
              name: fileName,
              uri: `file://${encodeURIComponent(fileName)}`,
            },
          ],
        };

        await session.prompt(promptRequest);

        expect(readManyFilesTool.buildAndExecute).toHaveBeenCalledWith(
          { paths: [fileName] },
          expect.any(AbortSignal),
        );
      } finally {
        await fs.rm(tempDir, { recursive: true, force: true });
      }
    });

    it('resolves Windows drive-letter file URIs with percent escapes', async () => {
      const tempDir = await fs.mkdtemp(
        path.join(os.tmpdir(), 'qwen-acp-session-'),
      );
      // `C:` is a legal directory name on POSIX hosts, so the resolved
      // absolute path stays within the temp root.
      const windowsStylePath = 'C:/repo/my file.md';

      try {
        await fs.mkdir(path.join(tempDir, 'C:', 'repo'), { recursive: true });
        await fs.writeFile(
          path.join(tempDir, 'C:', 'repo', 'my file.md'),
          '# On C\n',
          'utf8',
        );

        const readManyFilesTool = {
          buildAndExecute: vi.fn().mockResolvedValue({
            llmContent: 'file content',
            returnDisplay: 'ok',
          }),
        };
        const toolRegistry = {
          getTool: vi.fn((name: string) =>
            name === 'read_many_files' ? readManyFilesTool : undefined,
          ),
        };
        const fileService = {
          shouldGitIgnoreFile: vi.fn().mockReturnValue(false),
        };

        mockConfig.getTargetDir = vi.fn().mockReturnValue(tempDir);
        mockConfig.getToolRegistry = vi.fn().mockReturnValue(toolRegistry);
        mockConfig.getFileService = vi.fn().mockReturnValue(fileService);
        mockChat.sendMessageStream = vi
          .fn()
          .mockResolvedValue((async function* () {})());

        const promptRequest: acp.PromptRequest = {
          sessionId: 'test-session-id',
          prompt: [
            { type: 'text', text: 'Check this file' },
            {
              type: 'resource_link',
              name: 'my file.md',
              // Standard Zed-on-Windows form: drive letter in the path
              // component, percent-encoded space.
              uri: 'file:///C:/repo/my%20file.md',
            },
          ],
        };

        await session.prompt(promptRequest);

        expect(readManyFilesTool.buildAndExecute).toHaveBeenCalledWith(
          { paths: [windowsStylePath] },
          expect.any(AbortSignal),
        );
      } finally {
        await fs.rm(tempDir, { recursive: true, force: true });
      }
    });

    it('resolves Windows UNC file URIs with percent escapes', async () => {
      const tempDir = await fs.mkdtemp(
        path.join(os.tmpdir(), 'qwen-acp-session-'),
      );
      const uncPath = '\\\\server\\share\\my file.md';

      try {
        // Backslashes are legal filename characters on POSIX hosts, so the
        // whole UNC remainder resolves to one file inside the temp root.
        await fs.writeFile(path.join(tempDir, uncPath), '# On share\n', 'utf8');

        const readManyFilesTool = {
          buildAndExecute: vi.fn().mockResolvedValue({
            llmContent: 'file content',
            returnDisplay: 'ok',
          }),
        };
        const toolRegistry = {
          getTool: vi.fn((name: string) =>
            name === 'read_many_files' ? readManyFilesTool : undefined,
          ),
        };
        const fileService = {
          shouldGitIgnoreFile: vi.fn().mockReturnValue(false),
        };

        mockConfig.getTargetDir = vi.fn().mockReturnValue(tempDir);
        mockConfig.getToolRegistry = vi.fn().mockReturnValue(toolRegistry);
        mockConfig.getFileService = vi.fn().mockReturnValue(fileService);
        mockChat.sendMessageStream = vi
          .fn()
          .mockResolvedValue((async function* () {})());

        const promptRequest: acp.PromptRequest = {
          sessionId: 'test-session-id',
          prompt: [
            { type: 'text', text: 'Check this file' },
            {
              type: 'resource_link',
              name: 'my file.md',
              uri: 'file://server/share/my%20file.md',
            },
          ],
        };

        await session.prompt(promptRequest);

        expect(readManyFilesTool.buildAndExecute).toHaveBeenCalledWith(
          { paths: [uncPath] },
          expect.any(AbortSignal),
        );
      } finally {
        await fs.rm(tempDir, { recursive: true, force: true });
      }
    });

    it('keeps POSIX absolute file URIs as absolute decoded paths', async () => {
      const tempDir = await fs.mkdtemp(
        path.join(os.tmpdir(), 'qwen-acp-session-'),
      );
      const fileName = 'posix file.md';
      const filePath = path.join(tempDir, fileName);

      try {
        await fs.writeFile(filePath, '# POSIX\n', 'utf8');

        const readManyFilesTool = {
          buildAndExecute: vi.fn().mockResolvedValue({
            llmContent: 'file content',
            returnDisplay: 'ok',
          }),
        };
        const toolRegistry = {
          getTool: vi.fn((name: string) =>
            name === 'read_many_files' ? readManyFilesTool : undefined,
          ),
        };
        const fileService = {
          shouldGitIgnoreFile: vi.fn().mockReturnValue(false),
        };

        mockConfig.getTargetDir = vi.fn().mockReturnValue(tempDir);
        mockConfig.getToolRegistry = vi.fn().mockReturnValue(toolRegistry);
        mockConfig.getFileService = vi.fn().mockReturnValue(fileService);
        mockChat.sendMessageStream = vi
          .fn()
          .mockResolvedValue((async function* () {})());

        const encodedPath = filePath
          .split(path.sep)
          .map((segment) => encodeURIComponent(segment))
          .join('/');

        const promptRequest: acp.PromptRequest = {
          sessionId: 'test-session-id',
          prompt: [
            { type: 'text', text: 'Check this file' },
            {
              type: 'resource_link',
              name: fileName,
              uri: `file://${encodedPath}`,
            },
          ],
        };

        await session.prompt(promptRequest);

        expect(readManyFilesTool.buildAndExecute).toHaveBeenCalledWith(
          { paths: [filePath] },
          expect.any(AbortSignal),
        );
      } finally {
        await fs.rm(tempDir, { recursive: true, force: true });
      }
    });
  });
});
