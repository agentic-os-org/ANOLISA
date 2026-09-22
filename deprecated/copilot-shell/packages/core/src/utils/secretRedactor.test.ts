/**
 * @license
 * Copyright 2026 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import { describe, it, expect } from 'vitest';
import { redactSecrets, containsSecrets } from './secretRedactor.js';

describe('secretRedactor', () => {
  describe('redactSecrets', () => {
    it('should return non-string input unchanged', () => {
      expect(redactSecrets('')).toBe('');
    });

    it('should not modify text without secrets', () => {
      const text = 'Hello, this is a normal message without any secrets.';
      expect(redactSecrets(text)).toBe(text);
    });

    it('should not modify normal code', () => {
      const code = 'const x = 123;\nconsole.log("hello world");';
      expect(redactSecrets(code)).toBe(code);
    });

    // --- OpenAI API Key ---
    it('should redact OpenAI-style API keys (sk-...)', () => {
      const text = 'My key is sk-abcdefghijklmnopqrstuvwx';
      const result = redactSecrets(text);
      expect(result).toContain('sk-');
      expect(result).not.toContain('sk-abcdefghijklmnopqrstuvwx');
      expect(result).toContain('*');
    });

    it('should not redact short sk- prefixed strings', () => {
      const text = 'sk-short';
      expect(redactSecrets(text)).toBe(text);
    });

    // --- Bearer Token ---
    it('should redact Bearer tokens', () => {
      const text =
        'Authorization: Bearer eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.abc123';
      const result = redactSecrets(text);
      expect(result).toContain('Bearer ');
      expect(result).not.toContain('eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9');
      expect(result).toContain('*');
    });

    // --- Aliyun AccessKeySecret ---
    it('should redact Aliyun accessKeySecret in JSON', () => {
      const text = '{"accessKeySecret": "abcdefghijklmnopqrstuvwxyz123456"}';
      const result = redactSecrets(text);
      expect(result).not.toContain('abcdefghijklmnopqrstuvwxyz123456');
      expect(result).toContain('*');
    });

    it('should redact access_key_secret in JSON', () => {
      const text = '{"access_key_secret": "abcdefghijklmnopqrstuvwxyz12"}';
      const result = redactSecrets(text);
      expect(result).not.toContain('abcdefghijklmnopqrstuvwxyz12');
      expect(result).toContain('*');
    });

    // --- Generic secret fields ---
    it('should redact apiKey field in JSON', () => {
      const text = '{"apiKey": "sk-this-is-a-long-api-key-value"}';
      const result = redactSecrets(text);
      expect(result).not.toContain('sk-this-is-a-long-api-key-value');
      expect(result).toContain('*');
    });

    it('should redact api_key field in JSON', () => {
      const text = '{"api_key": "my-super-secret-key-12345678"}';
      const result = redactSecrets(text);
      expect(result).not.toContain('my-super-secret-key-12345678');
      expect(result).toContain('*');
    });

    it('should redact password field in JSON', () => {
      const text = '{"password": "supersecretpassword123"}';
      const result = redactSecrets(text);
      expect(result).not.toContain('supersecretpassword123');
      expect(result).toContain('*');
    });

    // --- Environment variables ---
    it('should redact API_KEY environment variable', () => {
      const text = 'API_KEY=my-super-secret-api-key-value';
      const result = redactSecrets(text);
      expect(result).toContain('API_KEY=');
      expect(result).not.toContain('my-super-secret-api-key-value');
      expect(result).toContain('*');
    });

    it('should redact SECRET_KEY in export statement', () => {
      const text = 'export SECRET_KEY="abcdef1234567890abcdef"';
      const result = redactSecrets(text);
      expect(result).toContain('SECRET_KEY=');
      expect(result).not.toContain('abcdef1234567890abcdef');
    });

    it('should redact PASSWORD in env', () => {
      const text = 'PASSWORD=mysecretpassword12345678';
      const result = redactSecrets(text);
      expect(result).toContain('PASSWORD=');
      expect(result).not.toContain('mysecretpassword12345678');
    });

    // --- Anthropic-style keys ---
    it('should redact Anthropic-style API keys (ant-...)', () => {
      const text = 'key: ant-abcdefghijklmnopqrstuvwx';
      const result = redactSecrets(text);
      expect(result).toContain('ant-');
      expect(result).not.toContain('ant-abcdefghijklmnopqrstuvwx');
    });

    // --- Multiple secrets ---
    it('should redact multiple secrets in one text', () => {
      const text =
        'API_KEY=sk-abcdefghijklmnopqrstuvwx and password=supersecretpassword123';
      const result = redactSecrets(text);
      expect(result).not.toContain('sk-abcdefghijklmnopqrstuvwx');
      expect(result).not.toContain('supersecretpassword123');
    });

    // --- Edge cases ---
    it('should not redact short values', () => {
      const text = '{"password": "short"}';
      expect(redactSecrets(text)).toBe(text);
    });

    it('should handle mixed content with code and secrets', () => {
      const code = `function getConfig() {
  return {
    host: "localhost",
    apiKey: "sk-this-is-a-long-api-key-value-for-testing",
    port: 3000
  };
}`;
      const result = redactSecrets(code);
      expect(result).toContain('localhost');
      expect(result).toContain('3000');
      expect(result).not.toContain(
        'sk-this-is-a-long-api-key-value-for-testing',
      );
    });
  });

  describe('containsSecrets', () => {
    it('should return true for text containing secrets', () => {
      expect(containsSecrets('sk-abcdefghijklmnopqrstuvwx')).toBe(true);
    });

    it('should return false for clean text', () => {
      expect(containsSecrets('Hello world')).toBe(false);
    });

    it('should return false for empty string', () => {
      expect(containsSecrets('')).toBe(false);
    });
  });
});
