/**
 * @license
 * Copyright 2025 Qwen Code
 * SPDX-License-Identifier: Apache-2.0
 */

import { describe, it, expect } from 'vitest';
import path from 'node:path';
import {
  extractSessionIdFromChatFilePath,
  getSessionAttemptCount,
  type ExtractionState,
  type SessionVersion,
} from './sessionAdapter.js';

describe('extractSessionIdFromChatFilePath', () => {
  const chatsDir = path.resolve('/tmp/qoder-chats');
  const uuid = '11111111-2222-3333-4444-555555555555';

  it('returns the sessionId when file sits directly under chatsDir', () => {
    const file = path.join(chatsDir, `${uuid}.jsonl`);
    expect(extractSessionIdFromChatFilePath(chatsDir, file)).toBe(uuid);
  });

  it('returns undefined for files in a subdirectory of chatsDir', () => {
    const file = path.join(chatsDir, 'sub', `${uuid}.jsonl`);
    expect(extractSessionIdFromChatFilePath(chatsDir, file)).toBeUndefined();
  });

  it('returns undefined when basename does not match the UUID.jsonl pattern', () => {
    const file = path.join(chatsDir, 'not-a-session.txt');
    expect(extractSessionIdFromChatFilePath(chatsDir, file)).toBeUndefined();
  });

  it('returns undefined when the file is outside chatsDir entirely', () => {
    const outsider = path.resolve('/tmp/other', `${uuid}.jsonl`);
    expect(
      extractSessionIdFromChatFilePath(chatsDir, outsider),
    ).toBeUndefined();
  });

  it('returns undefined when either argument is empty', () => {
    expect(extractSessionIdFromChatFilePath('', '/any')).toBeUndefined();
    expect(extractSessionIdFromChatFilePath(chatsDir, '')).toBeUndefined();
  });

  it('accepts non-canonical paths containing "." segments', () => {
    const file = path.join(chatsDir, '.', `${uuid}.jsonl`);
    expect(extractSessionIdFromChatFilePath(chatsDir, file)).toBe(uuid);
  });
});

describe('getSessionAttemptCount', () => {
  const makeSession = (id: string, lastUpdated: string): SessionVersion => ({
    sessionId: id,
    lastUpdated,
  });

  it('counts runs whose candidateSessions contain the same sessionId + lastUpdated', () => {
    const target = makeSession('s1', '2025-01-01T00:00:00Z');
    const state: ExtractionState = {
      runs: [
        {
          runAt: '2025-01-01T01:00:00Z',
          sessionIds: [],
          candidateSessions: [target],
          processedSessions: [],
          skillsCreated: [],
        },
        {
          runAt: '2025-01-01T02:00:00Z',
          sessionIds: [],
          candidateSessions: [target],
          processedSessions: [],
          skillsCreated: [],
        },
      ],
    };
    expect(getSessionAttemptCount(state, target)).toBe(2);
  });

  it('does not count runs whose lastUpdated differs (new version = new candidate)', () => {
    const target = makeSession('s1', '2025-01-01T00:00:00Z');
    const olderVersion = makeSession('s1', '2024-12-31T00:00:00Z');
    const state: ExtractionState = {
      runs: [
        {
          runAt: '2025-01-01T01:00:00Z',
          sessionIds: [],
          candidateSessions: [olderVersion],
          processedSessions: [],
          skillsCreated: [],
        },
      ],
    };
    expect(getSessionAttemptCount(state, target)).toBe(0);
  });

  it('returns 0 when the session has never been a candidate', () => {
    const target = makeSession('s-new', '2025-01-01T00:00:00Z');
    const state: ExtractionState = { runs: [] };
    expect(getSessionAttemptCount(state, target)).toBe(0);
  });
});
