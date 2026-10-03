/**
 * @license
 * Copyright 2025 Qwen Code
 * SPDX-License-Identifier: Apache-2.0
 */

import type React from 'react';
import { renderHook, act, waitFor } from '@testing-library/react';
import { describe, it, expect, vi, beforeEach } from 'vitest';
import type { Mock } from 'vitest';
import { useStdin } from 'ink';
import { EventEmitter } from 'node:events';
import type {
  ListSessionsResult,
  SessionListItem,
  SessionService,
} from '@copilot-shell/core';
import { KeypressProvider } from '../contexts/KeypressContext.js';
import { useSessionPicker } from './useSessionPicker.js';
import { SESSION_PAGE_SIZE } from '../utils/sessionPickerUtils.js';

// Mock the 'ink' module to control stdin.
vi.mock('ink', async (importOriginal) => {
  const original = await importOriginal<typeof import('ink')>();
  return {
    ...original,
    useStdin: vi.fn(),
  };
});

class MockStdin extends EventEmitter {
  isTTY = true;
  isRaw = true;
  setRawMode = vi.fn();
  write = vi.fn();
  resume = vi.fn();
  pause = vi.fn();

  pressKey(key: {
    name: string;
    sequence?: string;
    ctrl?: boolean;
    meta?: boolean;
    shift?: boolean;
    paste?: boolean;
  }) {
    this.emit('keypress', null, {
      ctrl: false,
      meta: false,
      shift: false,
      paste: false,
      ...key,
    });
  }
}

const TOTAL_SESSIONS = SESSION_PAGE_SIZE + 5;

function buildSessions(): SessionListItem[] {
  return Array.from({ length: TOTAL_SESSIONS }, (_, i) => ({
    sessionId: `s-${i + 1}`,
    cwd: '/project',
    startTime: '2026-01-01T00:00:00Z',
    mtime: 1_000_000 - i * 100,
    prompt: `Session ${i + 1}`,
    name: `Session ${i + 1}`,
    gitBranch: 'main',
    filePath: `/chats/s-${i + 1}.json`,
    messageCount: i + 1,
  }));
}

describe('useSessionPicker rename refresh', () => {
  let stdin: MockStdin;
  const mockSetRawMode = vi.fn();

  beforeEach(() => {
    vi.clearAllMocks();
    stdin = new MockStdin();
    (useStdin as Mock).mockReturnValue({
      stdin,
      setRawMode: mockSetRawMode,
    });
  });

  it('keeps sessions loaded beyond the first page after a rename', async () => {
    const renamed = new Map<string, string>();
    const allSessions = buildSessions();

    const listSessions = vi.fn(
      async (options: { size?: number; cursor?: number } = {}) => {
        const size = options.size ?? SESSION_PAGE_SIZE;
        let pool = allSessions;
        if (options.cursor !== undefined) {
          pool = pool.filter((s) => s.mtime < options.cursor!);
        }
        const items = pool.slice(0, size).map((s) => ({
          ...s,
          name: renamed.get(s.sessionId) ?? s.name,
        }));
        const hasMore = pool.length > items.length;
        const last = items[items.length - 1];
        const result: ListSessionsResult = {
          items,
          hasMore,
          nextCursor: hasMore && last ? last.mtime : undefined,
        };
        return result;
      },
    );

    const sessionService = {
      listSessions,
      renameSession: vi.fn(async (id: string, name: string) => {
        renamed.set(id, name);
      }),
    } as unknown as SessionService;

    const wrapper = ({ children }: { children: React.ReactNode }) => (
      <KeypressProvider kittyProtocolEnabled={false}>
        {children}
      </KeypressProvider>
    );

    const { result } = renderHook(
      () =>
        useSessionPicker({
          sessionService,
          onSelect: vi.fn(),
          onCancel: vi.fn(),
          maxVisibleItems: 10,
        }),
      { wrapper },
    );

    // Initial page load.
    await waitFor(() => {
      expect(result.current.sessionState.sessions).toHaveLength(
        SESSION_PAGE_SIZE,
      );
    });

    // Load the second page.
    await act(async () => {
      await result.current.loadMoreSessions();
    });
    await waitFor(() => {
      expect(result.current.sessionState.sessions).toHaveLength(
        TOTAL_SESSIONS,
      );
    });
    expect(
      result.current.filteredSessions.some((s) => s.name === 'Session 25'),
    ).toBe(true);

    // Rename the first session via the keyboard flow: Ctrl+R, 'z', Enter.
    act(() => {
      stdin.pressKey({ name: 'r', ctrl: true, sequence: '\x12' });
    });
    await waitFor(() => {
      expect(result.current.renameIndex).toBe(0);
    });
    act(() => {
      stdin.pressKey({ name: 'z', sequence: 'z' });
    });
    await act(async () => {
      stdin.pressKey({ name: 'return', sequence: '\r' });
      // Let the async confirm + reload settle.
      await new Promise((resolve) => setTimeout(resolve, 50));
    });

    await waitFor(() => {
      expect(sessionService.renameSession).toHaveBeenCalledWith(
        's-1',
        'Session 1z',
      );
    });
    expect(listSessions).toHaveBeenCalledTimes(3);

    // The rename must be reflected AND every already-loaded session from
    // later pages must still be present.
    await waitFor(() => {
      expect(result.current.sessionState.sessions).toHaveLength(
        TOTAL_SESSIONS,
      );
    });
    expect(
      result.current.filteredSessions.some((s) => s.name === 'Session 1z'),
    ).toBe(true);
    expect(
      result.current.filteredSessions.some((s) => s.name === 'Session 25'),
    ).toBe(true);
  });
});
