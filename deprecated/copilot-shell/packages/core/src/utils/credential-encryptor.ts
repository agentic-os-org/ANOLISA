/**
 * @license
 * Copyright 2026 Alibaba Cloud
 * SPDX-License-Identifier: Apache-2.0
 */

/**
 * Credential encryption using AES-256-GCM with a persisted random salt.
 *
 * Encrypted values are stored with the prefix "enc:" followed by hex-encoded
 * IV, auth tag, and ciphertext separated by colons:
 *
 *   enc:<iv_hex>:<authTag_hex>:<ciphertext_hex>
 *
 * This provides obfuscation-level protection: the password is hardcoded in the
 * source, but it prevents casual exposure via `cat settings.json`, accidental
 * Git commits, or log/error-report leaks.
 */

import * as crypto from 'node:crypto';
import * as fs from 'node:fs';
import * as path from 'node:path';
import { Storage } from '../config/storage.js';

const ENCRYPTED_PREFIX = 'enc:';
const CREDENTIAL_PASSWORD = 'copilot-credential-encrypt';
const SALT_LENGTH = 32;

let cachedKey: Buffer | null = null;

/**
 * Read or create the persisted random salt at ~/.copilot-shell/.encryption-salt.
 * The salt is 32 raw bytes stored with mode 0o600.
 */
function getOrCreateSalt(): Buffer {
  const configDir = Storage.getGlobalQwenDir();
  const saltPath = path.join(configDir, '.encryption-salt');

  try {
    const existing = fs.readFileSync(saltPath);
    if (existing.length === SALT_LENGTH) {
      return existing;
    }
  } catch (error: unknown) {
    const err = error as NodeJS.ErrnoException;
    if (err.code !== 'ENOENT') {
      // Unexpected error — proceed to create a new salt
    }
  }

  const salt = crypto.randomBytes(SALT_LENGTH);
  fs.mkdirSync(configDir, { recursive: true, mode: 0o700 });
  fs.writeFileSync(saltPath, salt, { mode: 0o600 });
  return salt;
}

function ensureKey(): Buffer {
  if (cachedKey) {
    return cachedKey;
  }
  const salt = getOrCreateSalt();
  cachedKey = crypto.scryptSync(CREDENTIAL_PASSWORD, salt, 32);
  return cachedKey;
}

/**
 * Encrypt a plaintext credential string.
 * Returns the ciphertext in the format "enc:<iv>:<authTag>:<ciphertext>".
 * Empty strings are returned as-is (not encrypted).
 */
export function encryptCredential(plaintext: string): string {
  if (!plaintext) {
    return plaintext;
  }
  const key = ensureKey();
  const iv = crypto.randomBytes(16);
  const cipher = crypto.createCipheriv('aes-256-gcm', key, iv);
  let encrypted = cipher.update(plaintext, 'utf8', 'hex');
  encrypted += cipher.final('hex');
  const authTag = cipher.getAuthTag();
  return `${ENCRYPTED_PREFIX}${iv.toString('hex')}:${authTag.toString('hex')}:${encrypted}`;
}

/**
 * Decrypt a credential value. Handles three cases:
 * - No "enc:" prefix → returns the value as-is (backward compat with plaintext)
 * - Valid encrypted value → decrypts and returns plaintext
 * - Decryption failure → returns `undefined` (graceful degradation)
 */
export function decryptCredential(value: string): string | undefined {
  if (!isEncryptedCredential(value)) {
    return value;
  }

  try {
    const withoutPrefix = value.slice(ENCRYPTED_PREFIX.length);
    const parts = withoutPrefix.split(':');
    if (parts.length !== 3) {
      return undefined;
    }

    const key = ensureKey();
    const iv = Buffer.from(parts[0]!, 'hex');
    const authTag = Buffer.from(parts[1]!, 'hex');
    const encrypted = parts[2]!;

    const decipher = crypto.createDecipheriv('aes-256-gcm', key, iv);
    decipher.setAuthTag(authTag);
    let decrypted = decipher.update(encrypted, 'hex', 'utf8');
    decrypted += decipher.final('utf8');
    return decrypted;
  } catch {
    // Decryption failed — salt changed, data tampered, etc.
    return undefined;
  }
}

/**
 * Check whether a value is an encrypted credential (has the "enc:" prefix).
 */
export function isEncryptedCredential(value: string): boolean {
  return value.startsWith(ENCRYPTED_PREFIX);
}
