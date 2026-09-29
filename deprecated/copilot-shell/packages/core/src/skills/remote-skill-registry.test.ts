/**
 * @license
 * Copyright 2025 Qwen
 * SPDX-License-Identifier: Apache-2.0
 */

import { vi, describe, it, expect, beforeEach, afterEach } from 'vitest';
import { RemoteSkillRegistry } from './remote-skill-registry.js';

// Mock global fetch
const mockFetch = vi.fn();
vi.stubGlobal('fetch', mockFetch);

// Helper to build a mock Response
function makeMockResponse(options: {
  ok?: boolean;
  status?: number;
  contentType?: string;
  body?: unknown;
  text?: string;
}): Response {
  const {
    ok = true,
    status = 200,
    contentType = 'application/json',
    body,
    text,
  } = options;

  return {
    ok,
    status,
    headers: {
      get: (name: string) => {
        if (name.toLowerCase() === 'content-type') {
          return contentType ?? null;
        }
        return null;
      },
    },
    json: vi.fn().mockResolvedValue(body),
    text: vi.fn().mockResolvedValue(text ?? JSON.stringify(body)),
  } as unknown as Response;
}

const MOCK_SKILLS = [
  {
    path: 'system/network/firewall',
    name: 'firewall',
    version: '1.0.0',
    description: 'Manage firewall rules',
    layer: 'system',
    lifecycle: 'stable',
    tags: ['network', 'security'],
    status: 'active',
    dependencies: [],
  },
];

describe('RemoteSkillRegistry.fetchIndex', () => {
  let registry: RemoteSkillRegistry;

  beforeEach(() => {
    vi.clearAllMocks();
    // Use a very short cache TTL so tests don't get stale cache hits
    registry = new RemoteSkillRegistry({
      baseUrl: 'https://example.com',
      cacheTTL: 0,
      cacheDir: '/tmp/test-remote-skills',
    });
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  describe('success path', () => {
    it('should return skills when server responds with valid JSON', async () => {
      mockFetch.mockResolvedValueOnce(
        makeMockResponse({
          ok: true,
          status: 200,
          contentType: 'application/json',
          body: { total: 1, skills: MOCK_SKILLS },
        }),
      );

      // Mock file cache to avoid fs calls
      vi.spyOn(registry as never, 'loadIndexFromFile').mockResolvedValue(null);
      vi.spyOn(registry as never, 'saveIndexToFile').mockResolvedValue(
        undefined,
      );

      const result = await registry.fetchIndex();
      expect(result).toHaveLength(1);
      expect(result[0].name).toBe('firewall');
    });

    it('should accept content-type with charset suffix', async () => {
      mockFetch.mockResolvedValueOnce(
        makeMockResponse({
          ok: true,
          status: 200,
          contentType: 'application/json; charset=utf-8',
          body: { total: 1, skills: MOCK_SKILLS },
        }),
      );

      vi.spyOn(registry as never, 'loadIndexFromFile').mockResolvedValue(null);
      vi.spyOn(registry as never, 'saveIndexToFile').mockResolvedValue(
        undefined,
      );

      const result = await registry.fetchIndex();
      expect(result).toHaveLength(1);
    });
  });

  describe('non-JSON response (HTML page)', () => {
    it('should throw a friendly error when server returns HTML (200 ok)', async () => {
      mockFetch.mockResolvedValueOnce(
        makeMockResponse({
          ok: true,
          status: 200,
          contentType: 'text/html; charset=utf-8',
          text: '<!DOCTYPE html><html><body>Login</body></html>',
        }),
      );

      vi.spyOn(registry as never, 'loadIndexFromFile').mockResolvedValue(null);

      await expect(registry.fetchIndex()).rejects.toThrow(
        'Remote skill server returned non-JSON response',
      );
    });

    it('should include the actual Content-Type in the error message', async () => {
      mockFetch.mockResolvedValueOnce(
        makeMockResponse({
          ok: true,
          status: 200,
          contentType: 'text/html',
          text: '<!DOCTYPE html>',
        }),
      );

      vi.spyOn(registry as never, 'loadIndexFromFile').mockResolvedValue(null);

      await expect(registry.fetchIndex()).rejects.toThrow('text/html');
    });

    it('should throw when content-type header is missing', async () => {
      mockFetch.mockResolvedValueOnce(
        makeMockResponse({
          ok: true,
          status: 200,
          contentType: '',
          text: '<!DOCTYPE html>',
        }),
      );

      vi.spyOn(registry as never, 'loadIndexFromFile').mockResolvedValue(null);

      await expect(registry.fetchIndex()).rejects.toThrow(
        'Remote skill server returned non-JSON response',
      );
    });
  });

  describe('HTTP error responses', () => {
    it('should throw when server returns 404', async () => {
      mockFetch.mockResolvedValueOnce(
        makeMockResponse({
          ok: false,
          status: 404,
          contentType: 'text/html',
        }),
      );

      vi.spyOn(registry as never, 'loadIndexFromFile').mockResolvedValue(null);

      await expect(registry.fetchIndex()).rejects.toThrow(
        'Failed to fetch skill index: 404',
      );
    });

    it('should throw when server returns 500', async () => {
      mockFetch.mockResolvedValueOnce(
        makeMockResponse({
          ok: false,
          status: 500,
          contentType: 'text/html',
        }),
      );

      vi.spyOn(registry as never, 'loadIndexFromFile').mockResolvedValue(null);

      await expect(registry.fetchIndex()).rejects.toThrow(
        'Failed to fetch skill index: 500',
      );
    });
  });

  describe('network errors', () => {
    it('should throw a friendly error on request timeout (AbortError)', async () => {
      const abortError = new Error('The operation was aborted');
      abortError.name = 'AbortError';
      mockFetch.mockRejectedValueOnce(abortError);

      vi.spyOn(registry as never, 'loadIndexFromFile').mockResolvedValue(null);

      await expect(registry.fetchIndex()).rejects.toThrow(
        'Remote skill server timed out',
      );
    });

    it('should include the configured timeout value in the timeout error', async () => {
      const registryWithTimeout = new RemoteSkillRegistry({
        baseUrl: 'https://example.com',
        cacheTTL: 0,
        cacheDir: '/tmp/test-remote-skills',
        timeout: 5000,
      });

      const abortError = new Error('The operation was aborted');
      abortError.name = 'AbortError';
      mockFetch.mockRejectedValueOnce(abortError);

      vi.spyOn(
        registryWithTimeout as never,
        'loadIndexFromFile',
      ).mockResolvedValue(null);

      await expect(registryWithTimeout.fetchIndex()).rejects.toThrow('5000ms');
    });

    it('should throw a friendly error on DNS / connection failure', async () => {
      mockFetch.mockRejectedValueOnce(
        new Error('fetch failed: getaddrinfo ENOTFOUND example.com'),
      );

      vi.spyOn(registry as never, 'loadIndexFromFile').mockResolvedValue(null);

      await expect(registry.fetchIndex()).rejects.toThrow(
        'Failed to connect to remote skill server',
      );
    });

    it('should include the underlying error message in connection error', async () => {
      mockFetch.mockRejectedValueOnce(new Error('ECONNREFUSED 127.0.0.1:443'));

      vi.spyOn(registry as never, 'loadIndexFromFile').mockResolvedValue(null);

      await expect(registry.fetchIndex()).rejects.toThrow('ECONNREFUSED');
    });
  });

  describe('memory cache', () => {
    it('should return cached result without fetching if within TTL', async () => {
      const registryWithTTL = new RemoteSkillRegistry({
        baseUrl: 'https://example.com',
        cacheTTL: 60000,
        cacheDir: '/tmp/test-remote-skills',
      });

      // Seed the in-memory cache directly
      (registryWithTTL as never as { indexCache: unknown }).indexCache = {
        fetchedAt: Date.now(),
        skills: MOCK_SKILLS,
      };

      const result = await registryWithTTL.fetchIndex();
      expect(mockFetch).not.toHaveBeenCalled();
      expect(result).toHaveLength(1);
    });

    it('should re-fetch when force=true even if cache is valid', async () => {
      const registryWithTTL = new RemoteSkillRegistry({
        baseUrl: 'https://example.com',
        cacheTTL: 60000,
        cacheDir: '/tmp/test-remote-skills',
      });

      (registryWithTTL as never as { indexCache: unknown }).indexCache = {
        fetchedAt: Date.now(),
        skills: MOCK_SKILLS,
      };

      mockFetch.mockResolvedValueOnce(
        makeMockResponse({
          ok: true,
          contentType: 'application/json',
          body: { total: 1, skills: MOCK_SKILLS },
        }),
      );
      vi.spyOn(registryWithTTL as never, 'saveIndexToFile').mockResolvedValue(
        undefined,
      );

      await registryWithTTL.fetchIndex(true);
      expect(mockFetch).toHaveBeenCalledOnce();
    });
  });
});
