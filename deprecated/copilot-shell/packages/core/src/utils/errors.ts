/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

interface GaxiosError {
  response?: {
    status?: number;
    data?: unknown;
  };
}

export function isNodeError(error: unknown): error is NodeJS.ErrnoException {
  return error instanceof Error && 'code' in error;
}

/**
 * Check if the error is an abort error (user cancellation).
 * This handles both DOMException-style AbortError and Node.js abort errors.
 */
export function isAbortError(error: unknown): boolean {
  if (!error || typeof error !== 'object') {
    return false;
  }

  // Check for AbortError by name (standard DOMException and custom AbortError)
  if (error instanceof Error && error.name === 'AbortError') {
    return true;
  }

  // Check for Node.js abort error code
  if (isNodeError(error) && error.code === 'ABORT_ERR') {
    return true;
  }

  return false;
}

export function getErrorMessage(error: unknown): string {
  if (error instanceof Error) {
    return error.message;
  }
  try {
    return String(error);
  } catch {
    return 'Failed to get error details';
  }
}

export class FatalError extends Error {
  constructor(
    message: string,
    readonly exitCode: number,
  ) {
    super(message);
  }
}

export class FatalAuthenticationError extends FatalError {
  constructor(message: string) {
    super(message, 41);
  }
}
export class FatalInputError extends FatalError {
  constructor(message: string) {
    super(message, 42);
  }
}
export class FatalConfigError extends FatalError {
  constructor(message: string) {
    super(message, 52);
  }
}
export class FatalTurnLimitedError extends FatalError {
  constructor(message: string) {
    super(message, 53);
  }
}
export class FatalToolExecutionError extends FatalError {
  constructor(message: string) {
    super(message, 54);
  }
}
export class FatalCancellationError extends FatalError {
  constructor(message: string) {
    super(message, 130); // Standard exit code for SIGINT
  }
}

export class ForbiddenError extends Error {}
export class UnauthorizedError extends Error {}
export class BadRequestError extends Error {}

interface ResponseData {
  error?: {
    code?: number;
    message?: string;
  };
}

/** Maximum length of a non-JSON response body inlined into a friendly error. */
const MAX_NON_JSON_BODY_LENGTH = 200;

export function toFriendlyError(error: unknown): unknown {
  if (error && typeof error === 'object' && 'response' in error) {
    const gaxiosError = error as GaxiosError;
    const data = parseResponseData(gaxiosError);
    if (data?.error?.message && data?.error?.code) {
      switch (data.error.code) {
        case 400:
          return new BadRequestError(data.error.message);
        case 401:
          return new UnauthorizedError(data.error.message);
        case 403:
          // It's import to pass the message here since it might
          // explain the cause like "the cloud project you're
          // using doesn't have code assist enabled".
          return new ForbiddenError(data.error.message);
        default:
      }
    }
    if (data === null) {
      // The body was a string that failed to parse (e.g. an HTML 502 page
      // from a proxy or gateway). Build a friendly error from the HTTP
      // status and a truncated body instead of throwing inside error
      // handling, so the caller's status handling still runs.
      return buildNonJsonBodyError(gaxiosError);
    }
  }
  return error;
}

/**
 * Parses the gaxios response data.
 * Returns:
 *  - the parsed object when the data is JSON (or already an object),
 *  - `null` when a string body is not valid JSON,
 *  - `undefined` when there is no body at all.
 */
function parseResponseData(
  error: GaxiosError,
): ResponseData | null | undefined {
  // Inexplicably, Gaxios sometimes doesn't JSONify the response data.
  if (typeof error.response?.data === 'string') {
    try {
      return JSON.parse(error.response.data) as ResponseData;
    } catch {
      return null;
    }
  }
  return error.response?.data as ResponseData | undefined;
}

function buildNonJsonBodyError(error: GaxiosError): Error {
  const status = error.response?.status;
  const raw =
    typeof error.response?.data === 'string' ? error.response.data : '';
  const snippet = raw.replace(/\s+/g, ' ').trim();
  const truncated =
    snippet.length > MAX_NON_JSON_BODY_LENGTH
      ? `${snippet.slice(0, MAX_NON_JSON_BODY_LENGTH)}...`
      : snippet;
  const message = `HTTP error ${status ?? 'unknown'}: ${truncated || 'no response body'}`;
  switch (status) {
    case 400:
      return new BadRequestError(message);
    case 401:
      return new UnauthorizedError(message);
    case 403:
      return new ForbiddenError(message);
    default:
      return new Error(message);
  }
}
