/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import { describe, it, expect } from 'vitest';
import {
  BadRequestError,
  ForbiddenError,
  UnauthorizedError,
  isAbortError,
  isNodeError,
  toFriendlyError,
} from './errors.js';

describe('isAbortError', () => {
  it('should return true for DOMException-style AbortError', () => {
    const abortError = new Error('The operation was aborted');
    abortError.name = 'AbortError';

    expect(isAbortError(abortError)).toBe(true);
  });

  it('should return true for custom AbortError class', () => {
    class AbortError extends Error {
      constructor(message: string) {
        super(message);
        this.name = 'AbortError';
      }
    }

    const error = new AbortError('Custom abort error');
    expect(isAbortError(error)).toBe(true);
  });

  it('should return true for Node.js abort error (ABORT_ERR code)', () => {
    const nodeAbortError = new Error(
      'Request aborted',
    ) as NodeJS.ErrnoException;
    nodeAbortError.code = 'ABORT_ERR';

    expect(isAbortError(nodeAbortError)).toBe(true);
  });

  it('should return false for regular errors', () => {
    expect(isAbortError(new Error('Regular error'))).toBe(false);
  });

  it('should return false for null', () => {
    expect(isAbortError(null)).toBe(false);
  });

  it('should return false for undefined', () => {
    expect(isAbortError(undefined)).toBe(false);
  });

  it('should return false for non-object values', () => {
    expect(isAbortError('string error')).toBe(false);
    expect(isAbortError(123)).toBe(false);
    expect(isAbortError(true)).toBe(false);
  });

  it('should return false for errors with different names', () => {
    const timeoutError = new Error('Request timed out');
    timeoutError.name = 'TimeoutError';

    expect(isAbortError(timeoutError)).toBe(false);
  });

  it('should return false for errors with other error codes', () => {
    const networkError = new Error('Network error') as NodeJS.ErrnoException;
    networkError.code = 'ECONNREFUSED';

    expect(isAbortError(networkError)).toBe(false);
  });
});

describe('isNodeError', () => {
  it('should return true for Error with code property', () => {
    const nodeError = new Error('File not found') as NodeJS.ErrnoException;
    nodeError.code = 'ENOENT';

    expect(isNodeError(nodeError)).toBe(true);
  });

  it('should return false for Error without code property', () => {
    const regularError = new Error('Regular error');

    expect(isNodeError(regularError)).toBe(false);
  });

  it('should return false for non-Error objects', () => {
    expect(isNodeError({ code: 'ENOENT' })).toBe(false);
    expect(isNodeError('string')).toBe(false);
    expect(isNodeError(null)).toBe(false);
  });
});

describe('toFriendlyError', () => {
  function gaxiosError(status?: number, data?: unknown): unknown {
    return { response: { status, data } };
  }

  it('should not throw on a non-JSON HTML body', () => {
    const err = gaxiosError(502, '<html>502 Bad Gateway</html>');

    let result: unknown;
    expect(() => {
      result = toFriendlyError(err);
    }).not.toThrow();

    expect(result).toBeInstanceOf(Error);
    expect((result as Error).message).toContain('502');
    expect((result as Error).message).toContain('Bad Gateway');
  });

  it('should map a non-JSON body with 401 status to UnauthorizedError', () => {
    const result = toFriendlyError(gaxiosError(401, '<html>login required'));

    expect(result).toBeInstanceOf(UnauthorizedError);
    expect((result as Error).message).toContain('401');
  });

  it('should map a non-JSON body with 400/403 status accordingly', () => {
    expect(toFriendlyError(gaxiosError(400, 'plain text'))).toBeInstanceOf(
      BadRequestError,
    );
    expect(toFriendlyError(gaxiosError(403, 'plain text'))).toBeInstanceOf(
      ForbiddenError,
    );
  });

  it('should truncate long non-JSON bodies', () => {
    const longBody = 'x'.repeat(500);
    const result = toFriendlyError(gaxiosError(500, longBody)) as Error;

    expect(result).toBeInstanceOf(Error);
    expect(result.message.length).toBeLessThan(300);
    expect(result.message).toContain('...');
  });

  it('should still map JSON error bodies by embedded code', () => {
    const body = JSON.stringify({ error: { code: 401, message: 'expired' } });
    const result = toFriendlyError(gaxiosError(undefined, body));

    expect(result).toBeInstanceOf(UnauthorizedError);
    expect((result as Error).message).toBe('expired');
  });

  it('should return the original error when there is no response body', () => {
    const err = gaxiosError(502);

    expect(toFriendlyError(err)).toBe(err);
  });

  it('should return non-response errors unchanged', () => {
    const err = new Error('network down');

    expect(toFriendlyError(err)).toBe(err);
  });
});
