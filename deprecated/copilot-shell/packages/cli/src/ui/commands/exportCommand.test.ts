/**
 * @license
 * Copyright 2025 Qwen
 * SPDX-License-Identifier: Apache-2.0
 */

import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import * as fs from 'node:fs/promises';
import { exportCommand } from './exportCommand.js';
import { createMockCommandContext } from '../../test-utils/mockCommandContext.js';
import type { ChatRecord } from '@copilot-shell/core';
import type { Part, Content } from '@google/genai';
import {
  collectSessionData,
  normalizeSessionData,
  toMarkdown,
  toHtml,
  toJson,
  toJsonl,
  generateExportFilename,
} from '../utils/export/index.js';

const mockSessionServiceMocks = vi.hoisted(() => ({
  loadLastSession: vi.fn(),
}));

vi.mock('@copilot-shell/core', () => {
  class SessionService {
    constructor(_cwd: string) {}
    async loadLastSession() {
      return mockSessionServiceMocks.loadLastSession();
    }
  }

  return {
    SessionService,
  };
});

vi.mock('../utils/export/index.js', () => ({
  collectSessionData: vi.fn(),
  normalizeSessionData: vi.fn(),
  toMarkdown: vi.fn(),
  toHtml: vi.fn(),
  toJson: vi.fn(),
  toJsonl: vi.fn(),
  generateExportFilename: vi.fn(),
}));

vi.mock('node:fs/promises', () => ({
  writeFile: vi.fn(),
}));

describe('exportCommand', () => {
  const mockSessionData = {
    conversation: {
      sessionId: 'test-session-id',
      startTime: '2025-01-01T00:00:00Z',
      messages: [
        {
          type: 'user',
          message: {
            parts: [{ text: 'Hello' }] as Part[],
          } as Content,
        },
      ] as ChatRecord[],
    },
  };

  let mockContext: ReturnType<typeof createMockCommandContext>;

  beforeEach(() => {
    vi.clearAllMocks();

    mockSessionServiceMocks.loadLastSession.mockResolvedValue(mockSessionData);

    mockContext = createMockCommandContext({
      services: {
        config: {
          getWorkingDir: vi.fn().mockReturnValue('/test/dir'),
          getProjectRoot: vi.fn().mockReturnValue('/test/project'),
        },
      },
    });

    vi.mocked(collectSessionData).mockResolvedValue({
      sessionId: 'test-session-id',
      startTime: '2025-01-01T00:00:00Z',
      messages: [],
    });
    vi.mocked(normalizeSessionData).mockImplementation((data) => data);
    vi.mocked(toMarkdown).mockReturnValue('# Test Markdown');
    vi.mocked(toHtml).mockReturnValue(
      '<html><script id="chat-data" type="application/json">{"data": "test"}</script></html>',
    );
    vi.mocked(toJson).mockReturnValue('{ "sessionId": "test-session-id" }');
    vi.mocked(toJsonl).mockReturnValue(
      '{"type":"session_metadata","sessionId":"test-session-id"}',
    );
    vi.mocked(generateExportFilename).mockImplementation(
      (ext: string) => `export-2025-01-01T00-00-00-000Z.${ext}`,
    );
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  describe('command structure', () => {
    it('should have correct name and description', () => {
      expect(exportCommand.name).toBe('export');
      expect(exportCommand.description).toBe(
        'Export current session message history to a file',
      );
    });

    it('should have md, html, json, and jsonl subcommands', () => {
      expect(exportCommand.subCommands).toHaveLength(4);
      expect(exportCommand.subCommands?.[0]?.name).toBe('md');
      expect(exportCommand.subCommands?.[1]?.name).toBe('html');
      expect(exportCommand.subCommands?.[2]?.name).toBe('json');
      expect(exportCommand.subCommands?.[3]?.name).toBe('jsonl');
    });

    it('should have default action for export command', () => {
      expect(exportCommand.action).toBeDefined();
    });
  });

  describe('default action', () => {
    it('should export session to markdown by default', async () => {
      const result = await exportCommand.action?.(mockContext, '');

      expect(result).toEqual({
        type: 'message',
        messageType: 'info',
        content:
          'Session exported to markdown: export-2025-01-01T00-00-00-000Z.md',
      });

      expect(toMarkdown).toHaveBeenCalled();
      expect(fs.writeFile).toHaveBeenCalledWith(
        '/test/dir/export-2025-01-01T00-00-00-000Z.md',
        '# Test Markdown',
        'utf-8',
      );
    });
  });

  describe('md subcommand', () => {
    it('should export session to markdown file', async () => {
      const mdCommand = exportCommand.subCommands?.[0];
      expect(mdCommand).toBeDefined();
      expect(mdCommand?.action).toBeDefined();

      const result = await mdCommand?.action?.(mockContext, '');

      expect(result).toEqual({
        type: 'message',
        messageType: 'info',
        content:
          'Session exported to markdown: export-2025-01-01T00-00-00-000Z.md',
      });

      expect(fs.writeFile).toHaveBeenCalledWith(
        '/test/dir/export-2025-01-01T00-00-00-000Z.md',
        '# Test Markdown',
        'utf-8',
      );
    });

    it('should return error when config is not available', async () => {
      mockContext = createMockCommandContext({
        services: {
          config: null,
        },
      });

      const mdCommand = exportCommand.subCommands?.[0];
      const result = await mdCommand?.action?.(mockContext, '');

      expect(result).toEqual({
        type: 'message',
        messageType: 'error',
        content: 'Configuration not available.',
      });
    });

    it('should return error when no session found', async () => {
      mockSessionServiceMocks.loadLastSession.mockResolvedValue(undefined);

      const mdCommand = exportCommand.subCommands?.[0];
      const result = await mdCommand?.action?.(mockContext, '');

      expect(result).toEqual({
        type: 'message',
        messageType: 'error',
        content: 'No active session found to export.',
      });
    });

    it('should return error when write fails', async () => {
      vi.mocked(fs.writeFile).mockRejectedValue(
        new Error('Write permission denied'),
      );

      const mdCommand = exportCommand.subCommands?.[0];
      const result = await mdCommand?.action?.(mockContext, '');

      expect(result).toEqual({
        type: 'message',
        messageType: 'error',
        content: 'Failed to export session: Write permission denied',
      });
    });
  });

  describe('html subcommand', () => {
    it('should export session to HTML file', async () => {
      const htmlCommand = exportCommand.subCommands?.[1];
      expect(htmlCommand).toBeDefined();

      const result = await htmlCommand?.action?.(mockContext, '');

      expect(result).toEqual({
        type: 'message',
        messageType: 'info',
        content:
          'Session exported to HTML: export-2025-01-01T00-00-00-000Z.html',
      });

      expect(fs.writeFile).toHaveBeenCalledWith(
        '/test/dir/export-2025-01-01T00-00-00-000Z.html',
        '<html><script id="chat-data" type="application/json">{"data": "test"}</script></html>',
        'utf-8',
      );
    });
  });

  describe('json subcommand', () => {
    it('should export session to JSON file', async () => {
      const jsonCommand = exportCommand.subCommands?.[2];
      expect(jsonCommand).toBeDefined();

      const result = await jsonCommand?.action?.(mockContext, '');

      expect(result).toEqual({
        type: 'message',
        messageType: 'info',
        content:
          'Session exported to JSON: export-2025-01-01T00-00-00-000Z.json',
      });

      expect(fs.writeFile).toHaveBeenCalledWith(
        '/test/dir/export-2025-01-01T00-00-00-000Z.json',
        '{ "sessionId": "test-session-id" }',
        'utf-8',
      );
    });
  });

  describe('jsonl subcommand', () => {
    it('should export session to JSONL file', async () => {
      const jsonlCommand = exportCommand.subCommands?.[3];
      expect(jsonlCommand).toBeDefined();

      const result = await jsonlCommand?.action?.(mockContext, '');

      expect(result).toEqual({
        type: 'message',
        messageType: 'info',
        content:
          'Session exported to JSONL: export-2025-01-01T00-00-00-000Z.jsonl',
      });

      expect(fs.writeFile).toHaveBeenCalledWith(
        '/test/dir/export-2025-01-01T00-00-00-000Z.jsonl',
        '{"type":"session_metadata","sessionId":"test-session-id"}',
        'utf-8',
      );
    });
  });
});
