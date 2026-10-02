/**
 * @license
 * Copyright 2026 Alibaba Cloud
 * SPDX-License-Identifier: Apache-2.0
 */

import { describe, it, expect, beforeEach, vi } from 'vitest';
import * as crypto from 'node:crypto';

// We use vi.resetModules() + dynamic import to get fresh module state per test
// because credential-encryptor caches the derived key at module level.

const TEST_SALT = crypto.randomBytes(32);

vi.mock('node:os', () => ({
  default: { homedir: vi.fn(() => '/home/test') },
  homedir: vi.fn(() => '/home/test'),
}));

vi.mock('node:fs', () => ({
  default: {
    readFileSync: vi.fn(),
    writeFileSync: vi.fn(),
    mkdirSync: vi.fn(),
  },
  readFileSync: vi.fn(),
  writeFileSync: vi.fn(),
  mkdirSync: vi.fn(),
}));

describe('credential-encryptor', () => {
  let encryptCredential: (plaintext: string) => string;
  let decryptCredential: (value: string) => string | undefined;
  let isEncryptedCredential: (value: string) => boolean;
  let mockReadFileSync: ReturnType<typeof vi.fn>;
  let mockWriteFileSync: ReturnType<typeof vi.fn>;
  let mockMkdirSync: ReturnType<typeof vi.fn>;

  beforeEach(async () => {
    vi.resetModules();
    vi.clearAllMocks();

    // Re-acquire the mocked fs functions after module reset
    const fsModule = await import('node:fs');
    mockReadFileSync = fsModule.readFileSync as unknown as ReturnType<
      typeof vi.fn
    >;
    mockWriteFileSync = fsModule.writeFileSync as unknown as ReturnType<
      typeof vi.fn
    >;
    mockMkdirSync = fsModule.mkdirSync as unknown as ReturnType<typeof vi.fn>;

    // Default: salt file exists
    mockReadFileSync.mockReturnValue(TEST_SALT);
    mockWriteFileSync.mockReturnValue(undefined);
    mockMkdirSync.mockReturnValue(undefined);

    // Import the module fresh (clears cached key)
    const mod = await import('./credential-encryptor.js');
    encryptCredential = mod.encryptCredential;
    decryptCredential = mod.decryptCredential;
    isEncryptedCredential = mod.isEncryptedCredential;
  });

  describe('getOrCreateSalt', () => {
    it('should read existing salt file', () => {
      encryptCredential('test');
      expect(mockReadFileSync).toHaveBeenCalledWith(
        '/home/test/.copilot-shell/.encryption-salt',
      );
    });

    it('should create salt file when it does not exist', async () => {
      vi.resetModules();
      vi.clearAllMocks();

      const fsModule = await import('node:fs');
      mockReadFileSync = fsModule.readFileSync as unknown as ReturnType<
        typeof vi.fn
      >;
      mockWriteFileSync = fsModule.writeFileSync as unknown as ReturnType<
        typeof vi.fn
      >;
      mockMkdirSync = fsModule.mkdirSync as unknown as ReturnType<typeof vi.fn>;

      const enoent = Object.assign(new Error('ENOENT'), { code: 'ENOENT' });
      mockReadFileSync.mockImplementation(() => {
        throw enoent;
      });
      mockWriteFileSync.mockReturnValue(undefined);
      mockMkdirSync.mockReturnValue(undefined);

      const mod = await import('./credential-encryptor.js');
      mod.encryptCredential('test');

      expect(mockMkdirSync).toHaveBeenCalledWith('/home/test/.copilot-shell', {
        recursive: true,
        mode: 0o700,
      });
      expect(mockWriteFileSync).toHaveBeenCalledWith(
        '/home/test/.copilot-shell/.encryption-salt',
        expect.any(Buffer),
        { mode: 0o600 },
      );
      // Verify salt is 32 bytes
      const writtenSalt = mockWriteFileSync.mock.calls[0][1] as Buffer;
      expect(writtenSalt.length).toBe(32);
    });

    it('should recreate salt when file has wrong length', async () => {
      vi.resetModules();
      vi.clearAllMocks();

      const fsModule = await import('node:fs');
      mockReadFileSync = fsModule.readFileSync as unknown as ReturnType<
        typeof vi.fn
      >;
      mockWriteFileSync = fsModule.writeFileSync as unknown as ReturnType<
        typeof vi.fn
      >;
      mockMkdirSync = fsModule.mkdirSync as unknown as ReturnType<typeof vi.fn>;

      // Return a buffer with wrong length
      mockReadFileSync.mockReturnValue(Buffer.alloc(16));
      mockWriteFileSync.mockReturnValue(undefined);
      mockMkdirSync.mockReturnValue(undefined);

      const mod = await import('./credential-encryptor.js');
      mod.encryptCredential('test');

      expect(mockWriteFileSync).toHaveBeenCalled();
      const writtenSalt = mockWriteFileSync.mock.calls[0][1] as Buffer;
      expect(writtenSalt.length).toBe(32);
    });
  });

  describe('encryptCredential', () => {
    it('should return enc: prefixed string', () => {
      const result = encryptCredential('my-secret-key');
      expect(result.startsWith('enc:')).toBe(true);
    });

    it('should produce format enc:iv:tag:cipher with hex parts', () => {
      const result = encryptCredential('test');
      const parts = result.split(':');
      expect(parts.length).toBe(4);
      expect(parts[0]).toBe('enc');
      // IV is 16 bytes = 32 hex chars
      expect(parts[1]!.length).toBe(32);
      // Auth tag is 16 bytes = 32 hex chars
      expect(parts[2]!.length).toBe(32);
      // Ciphertext is non-empty hex
      expect(parts[3]!.length).toBeGreaterThan(0);
    });

    it('should return empty string for empty input', () => {
      expect(encryptCredential('')).toBe('');
    });

    it('should produce different ciphertext each time (random IV)', () => {
      const a = encryptCredential('same-input');
      const b = encryptCredential('same-input');
      expect(a).not.toBe(b);
    });
  });

  describe('decryptCredential', () => {
    it('should round-trip correctly', () => {
      const original = 'sk-my-secret-api-key-12345';
      const encrypted = encryptCredential(original);
      const decrypted = decryptCredential(encrypted);
      expect(decrypted).toBe(original);
    });

    it('should pass through plaintext values (no enc: prefix)', () => {
      expect(decryptCredential('plain-api-key')).toBe('plain-api-key');
    });

    it('should pass through empty string', () => {
      expect(decryptCredential('')).toBe('');
    });

    it('should return undefined for malformed encrypted value', () => {
      expect(decryptCredential('enc:bad')).toBeUndefined();
    });

    it('should return undefined for tampered ciphertext', () => {
      const encrypted = encryptCredential('secret');
      // Tamper with the last character of ciphertext
      const tampered = encrypted.slice(0, -1) + 'x';
      expect(decryptCredential(tampered)).toBeUndefined();
    });

    it('should return undefined for wrong auth tag', () => {
      const encrypted = encryptCredential('secret');
      const parts = encrypted.split(':');
      // Replace auth tag with zeros
      parts[2] = '0'.repeat(32);
      const modified = parts.join(':');
      expect(decryptCredential(modified)).toBeUndefined();
    });

    it('should handle unicode strings', () => {
      const original = '密钥-中文测试-🔑';
      const encrypted = encryptCredential(original);
      expect(decryptCredential(encrypted)).toBe(original);
    });
  });

  describe('isEncryptedCredential', () => {
    it('should return true for enc: prefixed string', () => {
      expect(isEncryptedCredential('enc:a:b:c')).toBe(true);
    });

    it('should return false for plaintext', () => {
      expect(isEncryptedCredential('plain-value')).toBe(false);
    });

    it('should return false for empty string', () => {
      expect(isEncryptedCredential('')).toBe(false);
    });
  });

  describe('key caching', () => {
    it('should only read salt file once per module load', () => {
      encryptCredential('first');
      encryptCredential('second');
      encryptCredential('third');
      // readFileSync is called once for salt, then cached
      expect(mockReadFileSync).toHaveBeenCalledTimes(1);
    });
  });
});
