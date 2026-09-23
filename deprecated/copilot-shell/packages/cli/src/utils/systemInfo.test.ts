/**
 * @license
 * Copyright 2025 Qwen
 * SPDX-License-Identifier: Apache-2.0
 */

import { describe, it, expect, beforeEach, vi, afterEach } from 'vitest';
import {
  getSystemInfo,
  getExtendedSystemInfo,
  getNpmVersion,
  getIdeClientName,
} from './systemInfo.js';
import type { CommandContext } from '../ui/commands/types.js';
import { createMockCommandContext } from '../test-utils/mockCommandContext.js';
import * as child_process from 'node:child_process';
import os from 'node:os';
import { IdeClient } from '@copilot-shell/core';
import * as versionUtils from './version.js';
import type { ExecSyncOptions } from 'node:child_process';

vi.mock('node:child_process');

vi.mock('node:os', () => ({
  default: {
    release: vi.fn(),
  },
}));

vi.mock('./version.js', () => ({
  getCliVersion: vi.fn(),
}));

vi.mock('@copilot-shell/core', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@copilot-shell/core')>();
  return {
    ...actual,
    IdeClient: {
      getInstance: vi.fn(),
    },
  };
});

describe('systemInfo', () => {
  let mockContext: CommandContext;
  const originalPlatform = process.platform;
  const originalArch = process.arch;
  const originalVersion = process.version;
  const originalEnv = { ...process.env };

  beforeEach(() => {
    mockContext = createMockCommandContext({
      services: {
        config: {
          getModel: vi.fn().mockReturnValue('test-model'),
          getIdeMode: vi.fn().mockReturnValue(true),
          getSessionId: vi.fn().mockReturnValue('test-session-id'),
          getAuthType: vi.fn().mockReturnValue('test-auth'),
          getProxy: vi.fn().mockReturnValue(undefined),
          getContentGeneratorConfig: vi.fn().mockReturnValue({
            baseUrl: 'https://api.openai.com',
          }),
        },
        settings: {
          merged: {
            security: {
              auth: {
                selectedType: 'test-auth',
              },
            },
          },
        },
      },
    } as unknown as CommandContext);

    vi.mocked(versionUtils.getCliVersion).mockResolvedValue('test-version');
    vi.mocked(child_process.execSync).mockImplementation(
      (command: string, options?: ExecSyncOptions) => {
        if (
          options &&
          typeof options === 'object' &&
          'encoding' in options &&
          options.encoding === 'utf-8'
        ) {
          return '10.0.0';
        }
        return Buffer.from('10.0.0', 'utf-8');
      },
    );
    vi.mocked(os.release).mockReturnValue('22.0.0');
    process.env['GOOGLE_CLOUD_PROJECT'] = 'test-gcp-project';
    Object.defineProperty(process, 'platform', {
      value: 'test-os',
    });
    Object.defineProperty(process, 'arch', {
      value: 'x64',
    });
    Object.defineProperty(process, 'version', {
      value: 'v20.0.0',
    });
  });

  afterEach(() => {
    vi.unstubAllEnvs();
    Object.defineProperty(process, 'platform', {
      value: originalPlatform,
    });
    Object.defineProperty(process, 'arch', {
      value: originalArch,
    });
    Object.defineProperty(process, 'version', {
      value: originalVersion,
    });
    process.env = originalEnv;
    vi.clearAllMocks();
    vi.resetAllMocks();
  });

  describe('getNpmVersion', () => {
    it('should return npm version when available', async () => {
      vi.mocked(child_process.execSync).mockImplementation(
        (command: string, options?: ExecSyncOptions) => {
          if (
            options &&
            typeof options === 'object' &&
            'encoding' in options &&
            options.encoding === 'utf-8'
          ) {
            return '10.0.0';
          }
          return Buffer.from('10.0.0', 'utf-8');
        },
      );
      const version = await getNpmVersion();
      expect(version).toBe('10.0.0');
    });

    it('should return unknown when npm command fails', async () => {
      vi.mocked(child_process.execSync).mockImplementation(() => {
        throw new Error('npm not found');
      });
      const version = await getNpmVersion();
      expect(version).toBe('unknown');
    });
  });

  describe('getIdeClientName', () => {
    it('should return IDE client name when IDE mode is enabled', async () => {
      vi.mocked(IdeClient.getInstance).mockResolvedValue({
        getDetectedIdeDisplayName: vi.fn().mockReturnValue('test-ide'),
      } as unknown as IdeClient);

      const ideClient = await getIdeClientName(mockContext);
      expect(ideClient).toBe('test-ide');
    });

    it('should return empty string when IDE mode is disabled', async () => {
      vi.mocked(mockContext.services.config!.getIdeMode).mockReturnValue(false);

      const ideClient = await getIdeClientName(mockContext);
      expect(ideClient).toBe('');
    });

    it('should return empty string when IDE client detection fails', async () => {
      vi.mocked(IdeClient.getInstance).mockRejectedValue(
        new Error('IDE client error'),
      );

      const ideClient = await getIdeClientName(mockContext);
      expect(ideClient).toBe('');
    });
  });

  describe('getSystemInfo', () => {
    it('should collect all system information', async () => {
      vi.mocked(IdeClient.getInstance).mockResolvedValue({
        getDetectedIdeDisplayName: vi.fn().mockReturnValue('test-ide'),
      } as unknown as IdeClient);
      vi.mocked(child_process.execSync).mockImplementation(
        (command: string, options?: ExecSyncOptions) => {
          if (
            options &&
            typeof options === 'object' &&
            'encoding' in options &&
            options.encoding === 'utf-8'
          ) {
            return '10.0.0';
          }
          return Buffer.from('10.0.0', 'utf-8');
        },
      );

      const systemInfo = await getSystemInfo(mockContext);

      expect(systemInfo).toEqual({
        cliVersion: 'test-version',
        osPlatform: 'test-os',
        osArch: 'x64',
        osRelease: '22.0.0',
        nodeVersion: 'v20.0.0',
        npmVersion: '10.0.0',
        modelVersion: 'test-model',
        selectedAuthType: 'test-auth',
        ideClient: 'test-ide',
        sessionId: 'test-session-id',
        proxy: undefined,
      });
    });

    it('should handle missing config gracefully', async () => {
      mockContext.services.config = null;
      vi.mocked(IdeClient.getInstance).mockResolvedValue({
        getDetectedIdeDisplayName: vi.fn().mockReturnValue(''),
      } as unknown as IdeClient);

      const systemInfo = await getSystemInfo(mockContext);

      expect(systemInfo.modelVersion).toBe('Unknown');
      expect(systemInfo.sessionId).toBe('unknown');
    });
  });

  describe('getExtendedSystemInfo', () => {
    it('should include memory usage and base URL', async () => {
      vi.mocked(IdeClient.getInstance).mockResolvedValue({
        getDetectedIdeDisplayName: vi.fn().mockReturnValue('test-ide'),
      } as unknown as IdeClient);
      vi.mocked(child_process.execSync).mockImplementation(
        (command: string, options?: ExecSyncOptions) => {
          if (
            options &&
            typeof options === 'object' &&
            'encoding' in options &&
            options.encoding === 'utf-8'
          ) {
            return '10.0.0';
          }
          return Buffer.from('10.0.0', 'utf-8');
        },
      );

      const { AuthType } = await import('@copilot-shell/core');
      // Update the mock context to use OpenAI auth
      mockContext.services.settings.merged.security!.auth!.selectedType =
        AuthType.USE_OPENAI;
      vi.mocked(mockContext.services.config!.getAuthType).mockReturnValue(
        AuthType.USE_OPENAI,
      );

      const extendedInfo = await getExtendedSystemInfo(mockContext);

      expect(extendedInfo.memoryUsage).toBeDefined();
      expect(extendedInfo.memoryUsage).toMatch(/\d+\.\d+ (KB|MB|GB)/);
      expect(extendedInfo.baseUrl).toBe('https://api.openai.com');
    });

    it('should not include base URL for non-OpenAI auth', async () => {
      vi.mocked(IdeClient.getInstance).mockResolvedValue({
        getDetectedIdeDisplayName: vi.fn().mockReturnValue(''),
      } as unknown as IdeClient);
      vi.mocked(child_process.execSync).mockImplementation(
        (command: string, options?: ExecSyncOptions) => {
          if (
            options &&
            typeof options === 'object' &&
            'encoding' in options &&
            options.encoding === 'utf-8'
          ) {
            return '10.0.0';
          }
          return Buffer.from('10.0.0', 'utf-8');
        },
      );

      const extendedInfo = await getExtendedSystemInfo(mockContext);

      expect(extendedInfo.baseUrl).toBeUndefined();
    });
  });
});
