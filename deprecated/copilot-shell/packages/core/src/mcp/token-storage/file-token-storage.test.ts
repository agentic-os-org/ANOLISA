/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import { promises as fs } from 'node:fs';
import * as path from 'node:path';
import * as crypto from 'node:crypto';
import { FileTokenStorage } from './file-token-storage.js';
import type { OAuthCredentials } from './types.js';

vi.mock('node:fs', () => ({
  promises: {
    readFile: vi.fn(),
    writeFile: vi.fn(),
    unlink: vi.fn(),
    mkdir: vi.fn(),
  },
}));

vi.mock('node:os', () => ({
  default: {
    homedir: vi.fn(() => '/home/test'),
    hostname: vi.fn(() => 'test-host'),
    userInfo: vi.fn(() => ({ username: 'test-user' })),
  },
  homedir: vi.fn(() => '/home/test'),
  hostname: vi.fn(() => 'test-host'),
  userInfo: vi.fn(() => ({ username: 'test-user' })),
}));

/**
 * Helper: encrypt data with a known key (matching what getOrCreateSalt produces)
 * for feeding into mock readFile results.
 */
function encryptWithKey(text: string, key: Buffer): string {
  const iv = crypto.randomBytes(16);
  const cipher = crypto.createCipheriv('aes-256-gcm', key, iv);
  let encrypted = cipher.update(text, 'utf8', 'hex');
  encrypted += cipher.final('hex');
  const authTag = cipher.getAuthTag();
  return iv.toString('hex') + ':' + authTag.toString('hex') + ':' + encrypted;
}

function decryptWithKey(encryptedData: string, key: Buffer): string {
  const parts = encryptedData.split(':');
  const iv = Buffer.from(parts[0], 'hex');
  const authTag = Buffer.from(parts[1], 'hex');
  const encrypted = parts[2];
  const decipher = crypto.createDecipheriv('aes-256-gcm', key, iv);
  decipher.setAuthTag(authTag);
  let decrypted = decipher.update(encrypted, 'hex', 'utf8');
  decrypted += decipher.final('utf8');
  return decrypted;
}

describe('FileTokenStorage', () => {
  let storage: FileTokenStorage;
  const mockFs = fs as unknown as {
    readFile: ReturnType<typeof vi.fn>;
    writeFile: ReturnType<typeof vi.fn>;
    unlink: ReturnType<typeof vi.fn>;
    mkdir: ReturnType<typeof vi.fn>;
  };

  // A fixed 32-byte salt for deterministic test encryption
  const testSalt = crypto.randomBytes(32);
  const testKey = crypto.scryptSync('qwen-code-oauth', testSalt, 32);

  const saltPath = path.join(
    '/home/test',
    '.copilot-shell',
    '.encryption-salt',
  );
  const tokenPath = path.join(
    '/home/test',
    '.copilot-shell',
    'mcp-oauth-tokens-v2.json',
  );

  const existingCredentials: OAuthCredentials = {
    serverName: 'existing-server',
    token: {
      accessToken: 'existing-token',
      tokenType: 'Bearer',
    },
    updatedAt: Date.now() - 10000,
  };

  /**
   * Configure mock readFile to return the test salt for the salt path
   * and a given value for the token file path.
   */
  function setupReadFileMock(
    tokenFileResult?: string | Error | { code: string },
  ) {
    mockFs.readFile.mockImplementation((filePath: string) => {
      if (filePath === saltPath) {
        return Promise.resolve(testSalt);
      }
      if (filePath === tokenPath) {
        if (tokenFileResult instanceof Error) {
          return Promise.reject(tokenFileResult);
        }
        if (
          tokenFileResult &&
          typeof tokenFileResult === 'object' &&
          'code' in tokenFileResult
        ) {
          return Promise.reject(tokenFileResult);
        }
        if (tokenFileResult !== undefined) {
          return Promise.resolve(tokenFileResult);
        }
      }
      return Promise.reject({ code: 'ENOENT' });
    });
  }

  beforeEach(() => {
    vi.clearAllMocks();
    mockFs.mkdir.mockResolvedValue(undefined);
    mockFs.writeFile.mockResolvedValue(undefined);
    storage = new FileTokenStorage('test-storage');
  });

  afterEach(() => {
    vi.clearAllMocks();
  });

  describe('getCredentials', () => {
    it('should throw error when token file does not exist', async () => {
      setupReadFileMock({ code: 'ENOENT' });

      await expect(storage.getCredentials('test-server')).rejects.toThrow(
        'Token file does not exist',
      );
    });

    it('should return null for expired tokens', async () => {
      const credentials: OAuthCredentials = {
        serverName: 'test-server',
        token: {
          accessToken: 'access-token',
          tokenType: 'Bearer',
          expiresAt: Date.now() - 3600000,
        },
        updatedAt: Date.now(),
      };

      const encryptedData = encryptWithKey(
        JSON.stringify({ 'test-server': credentials }),
        testKey,
      );
      setupReadFileMock(encryptedData);

      const result = await storage.getCredentials('test-server');
      expect(result).toBeNull();
    });

    it('should return credentials for valid tokens', async () => {
      const credentials: OAuthCredentials = {
        serverName: 'test-server',
        token: {
          accessToken: 'access-token',
          tokenType: 'Bearer',
          expiresAt: Date.now() + 3600000,
        },
        updatedAt: Date.now(),
      };

      const encryptedData = encryptWithKey(
        JSON.stringify({ 'test-server': credentials }),
        testKey,
      );
      setupReadFileMock(encryptedData);

      const result = await storage.getCredentials('test-server');
      expect(result).toEqual(credentials);
    });

    it('should throw error for corrupted files', async () => {
      setupReadFileMock('corrupted-data');

      await expect(storage.getCredentials('test-server')).rejects.toThrow(
        'Token file corrupted',
      );
    });
  });

  describe('setCredentials', () => {
    it('should save credentials with encryption', async () => {
      // Token file doesn't exist yet — setCredentials should start fresh
      setupReadFileMock({ code: 'ENOENT' });

      const credentials: OAuthCredentials = {
        serverName: 'test-server',
        token: {
          accessToken: 'access-token',
          tokenType: 'Bearer',
        },
        updatedAt: Date.now(),
      };

      await storage.setCredentials(credentials);

      // Find the writeFile call for the token file (not the salt file)
      const tokenWriteCall = mockFs.writeFile.mock.calls.find(
        (call: unknown[]) => call[0] === tokenPath,
      );
      expect(tokenWriteCall).toBeDefined();
      expect(tokenWriteCall![1]).toMatch(/^[0-9a-f]+:[0-9a-f]+:[0-9a-f]+$/);
      expect(tokenWriteCall![2]).toEqual({ mode: 0o600 });
    });

    it('should update existing credentials', async () => {
      const encryptedData = encryptWithKey(
        JSON.stringify({ 'existing-server': existingCredentials }),
        testKey,
      );
      setupReadFileMock(encryptedData);

      const newCredentials: OAuthCredentials = {
        serverName: 'test-server',
        token: {
          accessToken: 'new-token',
          tokenType: 'Bearer',
        },
        updatedAt: Date.now(),
      };

      await storage.setCredentials(newCredentials);

      const tokenWriteCall = mockFs.writeFile.mock.calls.find(
        (call: unknown[]) => call[0] === tokenPath,
      );
      expect(tokenWriteCall).toBeDefined();
      const decrypted = decryptWithKey(tokenWriteCall![1] as string, testKey);
      const saved = JSON.parse(decrypted);

      expect(saved['existing-server']).toEqual(existingCredentials);
      expect(saved['test-server'].token.accessToken).toBe('new-token');
    });
  });

  describe('deleteCredentials', () => {
    it('should throw when token file does not exist', async () => {
      setupReadFileMock({ code: 'ENOENT' });

      await expect(storage.deleteCredentials('test-server')).rejects.toThrow(
        'Token file does not exist',
      );
    });

    it('should delete file when last credential is removed', async () => {
      const credentials: OAuthCredentials = {
        serverName: 'test-server',
        token: {
          accessToken: 'access-token',
          tokenType: 'Bearer',
        },
        updatedAt: Date.now(),
      };

      const encryptedData = encryptWithKey(
        JSON.stringify({ 'test-server': credentials }),
        testKey,
      );
      setupReadFileMock(encryptedData);
      mockFs.unlink.mockResolvedValue(undefined);

      await storage.deleteCredentials('test-server');

      expect(mockFs.unlink).toHaveBeenCalledWith(tokenPath);
    });

    it('should update file when other credentials remain', async () => {
      const credentials1: OAuthCredentials = {
        serverName: 'server1',
        token: {
          accessToken: 'token1',
          tokenType: 'Bearer',
        },
        updatedAt: Date.now(),
      };

      const credentials2: OAuthCredentials = {
        serverName: 'server2',
        token: {
          accessToken: 'token2',
          tokenType: 'Bearer',
        },
        updatedAt: Date.now(),
      };

      const encryptedData = encryptWithKey(
        JSON.stringify({ server1: credentials1, server2: credentials2 }),
        testKey,
      );
      setupReadFileMock(encryptedData);

      await storage.deleteCredentials('server1');

      const tokenWriteCall = mockFs.writeFile.mock.calls.find(
        (call: unknown[]) => call[0] === tokenPath,
      );
      expect(tokenWriteCall).toBeDefined();
      expect(mockFs.unlink).not.toHaveBeenCalled();

      const decrypted = decryptWithKey(tokenWriteCall![1] as string, testKey);
      const saved = JSON.parse(decrypted);

      expect(saved['server1']).toBeUndefined();
      expect(saved['server2']).toEqual(credentials2);
    });
  });

  describe('listServers', () => {
    it('should throw error when token file does not exist', async () => {
      setupReadFileMock({ code: 'ENOENT' });

      await expect(storage.listServers()).rejects.toThrow(
        'Token file does not exist',
      );
    });

    it('should return list of server names', async () => {
      const credentials: Record<string, OAuthCredentials> = {
        server1: {
          serverName: 'server1',
          token: { accessToken: 'token1', tokenType: 'Bearer' },
          updatedAt: Date.now(),
        },
        server2: {
          serverName: 'server2',
          token: { accessToken: 'token2', tokenType: 'Bearer' },
          updatedAt: Date.now(),
        },
      };

      const encryptedData = encryptWithKey(
        JSON.stringify(credentials),
        testKey,
      );
      setupReadFileMock(encryptedData);

      const result = await storage.listServers();
      expect(result).toEqual(['server1', 'server2']);
    });
  });

  describe('clearAll', () => {
    it('should delete the token file', async () => {
      mockFs.unlink.mockResolvedValue(undefined);

      await storage.clearAll();

      expect(mockFs.unlink).toHaveBeenCalledWith(tokenPath);
    });

    it('should not throw when file does not exist', async () => {
      mockFs.unlink.mockRejectedValue({ code: 'ENOENT' });

      await expect(storage.clearAll()).resolves.not.toThrow();
    });
  });

  describe('legacy migration', () => {
    it('should decrypt with legacy key when new key fails', async () => {
      // Encrypt with the legacy hostname-based key
      const legacySalt = 'test-host-test-user-qwen-code';
      const legacyKey = crypto.scryptSync('qwen-code-oauth', legacySalt, 32);

      const credentials: OAuthCredentials = {
        serverName: 'legacy-server',
        token: {
          accessToken: 'legacy-token',
          tokenType: 'Bearer',
          expiresAt: Date.now() + 3600000,
        },
        updatedAt: Date.now(),
      };

      const legacyEncrypted = encryptWithKey(
        JSON.stringify({ 'legacy-server': credentials }),
        legacyKey,
      );

      // Salt file returns our test salt (which won't match the legacy encryption)
      // Token file returns legacy-encrypted data
      mockFs.readFile.mockImplementation((filePath: string) => {
        if (filePath === saltPath) {
          return Promise.resolve(testSalt);
        }
        if (filePath === tokenPath) {
          return Promise.resolve(legacyEncrypted);
        }
        return Promise.reject({ code: 'ENOENT' });
      });

      const result = await storage.getCredentials('legacy-server');
      expect(result).toEqual(credentials);

      // Should have re-encrypted and saved with the new key
      const tokenWriteCall = mockFs.writeFile.mock.calls.find(
        (call: unknown[]) => call[0] === tokenPath,
      );
      expect(tokenWriteCall).toBeDefined();
    });
  });

  describe('getOrCreateSalt', () => {
    it('should create a new salt when salt file does not exist', async () => {
      mockFs.readFile.mockImplementation((filePath: string) => {
        if (filePath === saltPath) {
          return Promise.reject({ code: 'ENOENT' });
        }
        return Promise.reject({ code: 'ENOENT' });
      });

      // Trigger salt creation via setCredentials (which handles missing token file)
      const credentials: OAuthCredentials = {
        serverName: 'test-server',
        token: { accessToken: 'token', tokenType: 'Bearer' },
        updatedAt: Date.now(),
      };
      await storage.setCredentials(credentials);

      // Salt file should have been written
      const saltWriteCall = mockFs.writeFile.mock.calls.find(
        (call: unknown[]) => call[0] === saltPath,
      );
      expect(saltWriteCall).toBeDefined();
      expect(saltWriteCall![1]).toBeInstanceOf(Buffer);
      expect((saltWriteCall![1] as Buffer).length).toBe(32);
      expect(saltWriteCall![2]).toEqual({ mode: 0o600 });
    });
  });
});
