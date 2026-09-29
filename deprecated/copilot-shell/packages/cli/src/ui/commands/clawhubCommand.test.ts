/**
 * @license
 * Copyright 2026 Alibaba Cloud。
 * SPDX-License-Identifier: Apache-2.0
 */

import { describe, it, expect, vi, beforeEach, afterAll } from 'vitest';
import { EventEmitter } from 'node:events';
import { CommandKind, type CommandContext } from './types.js';
import { createMockCommandContext } from '../../test-utils/mockCommandContext.js';
import { MessageType } from '../types.js';
import { clawhubCommand, _resetClawhubCache, _deps } from './clawhubCommand.js';

// ── Helpers ─────────────────────────────────────────────────────────

/** Create a fake ChildProcess that emits close with the given outputs. */
function createFakeProcess(
  code: number,
  stdout: string,
  stderr = '',
): EventEmitter {
  const proc = new EventEmitter();
  const stdoutEmitter = new EventEmitter();
  const stderrEmitter = new EventEmitter();
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  (proc as any).stdout = stdoutEmitter;
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  (proc as any).stderr = stderrEmitter;

  process.nextTick(() => {
    if (stdout) stdoutEmitter.emit('data', Buffer.from(stdout));
    if (stderr) stderrEmitter.emit('data', Buffer.from(stderr));
    proc.emit('close', code);
  });

  return proc;
}

describe('clawhubCommand', () => {
  let mockContext: CommandContext;
  const originalExecSync = _deps.execSync;
  const originalSpawn = _deps.spawn;

  beforeEach(() => {
    _resetClawhubCache();

    // Replace _deps with mocks
    _deps.execSync = vi.fn().mockReturnValue(Buffer.from('1.0.0'));
    _deps.spawn = vi.fn();

    mockContext = createMockCommandContext({
      ui: { addItem: vi.fn() },
    } as unknown as CommandContext);
  });

  afterAll(() => {
    _deps.execSync = originalExecSync;
    _deps.spawn = originalSpawn;
  });

  const mockExecSync = () => _deps.execSync as ReturnType<typeof vi.fn>;
  const mockSpawn = () => _deps.spawn as ReturnType<typeof vi.fn>;

  // ── Metadata ────────────────────────────────────────────────────

  describe('metadata', () => {
    it('should have correct name and kind', () => {
      expect(clawhubCommand.name).toBe('clawhub');
      expect(clawhubCommand.kind).toBe(CommandKind.BUILT_IN);
      expect(clawhubCommand.description).toBeTruthy();
    });

    it('should have the expected subcommands', () => {
      const names = clawhubCommand.subCommands!.map((c) => c.name);
      expect(names).toEqual([
        'search',
        'install',
        'uninstall',
        'update',
        'list',
        'inspect',
        'login',
        'whoami',
      ]);
    });

    it('all subcommands should be BUILT_IN with descriptions', () => {
      for (const sub of clawhubCommand.subCommands!) {
        expect(sub.kind).toBe(CommandKind.BUILT_IN);
        expect(sub.description).toBeTruthy();
      }
    });
  });

  // ── Installation check ──────────────────────────────────────────

  describe('installation check', () => {
    it('should return confirm_action when clawhub is not installed', async () => {
      mockExecSync().mockImplementation(() => {
        throw new Error('not found');
      });

      const result = await clawhubCommand.action!(mockContext, 'search test');

      expect(result).toMatchObject({ type: 'confirm_action' });
      expect(mockSpawn()).not.toHaveBeenCalled();
    });

    it('should proceed with install when overwriteConfirmed and then run command', async () => {
      mockExecSync().mockImplementation(() => {
        throw new Error('not found');
      });

      // Create fake processes lazily so events fire after listeners attach
      mockSpawn()
        .mockImplementationOnce(() => createFakeProcess(0, 'installed ok'))
        .mockImplementationOnce(() => createFakeProcess(0, 'testuser'));

      const confirmedCtx = createMockCommandContext({
        overwriteConfirmed: true,
        ui: { addItem: vi.fn() },
      } as unknown as CommandContext);

      await clawhubCommand.action!(confirmedCtx, 'whoami');

      expect(mockSpawn()).toHaveBeenCalledTimes(2);
      expect(mockSpawn()).toHaveBeenNthCalledWith(
        1,
        'npm',
        expect.arrayContaining(['install', '--prefix', 'clawhub']),
        expect.any(Object),
      );
    });
  });

  // ── Help output ─────────────────────────────────────────────────

  describe('help output', () => {
    it('should show usage help when called with no args', async () => {
      await clawhubCommand.action!(mockContext, '');

      expect(mockContext.ui.addItem).toHaveBeenCalledWith(
        expect.objectContaining({
          type: MessageType.CLAWHUB_OUTPUT,
          title: 'Clawhub',
          text: expect.stringContaining('/clawhub'),
        }),
        expect.any(Number),
      );
    });
  });

  // ── search subcommand ───────────────────────────────────────────

  describe('search', () => {
    it('should parse structured search results', async () => {
      const fakeOutput = [
        'foo/bar  A skill description  (0.95)',
        'baz/qux  Another skill  (0.80)',
      ].join('\n');
      mockSpawn().mockReturnValue(createFakeProcess(0, fakeOutput));

      const sub = clawhubCommand.subCommands!.find((c) => c.name === 'search')!;
      await sub.action!(mockContext, 'test');

      expect(mockContext.ui.addItem).toHaveBeenCalledWith(
        expect.objectContaining({
          type: MessageType.CLAWHUB_OUTPUT,
          items: expect.arrayContaining([
            expect.objectContaining({
              slug: 'foo/bar',
              description: 'A skill description',
              score: '0.95',
            }),
          ]),
        }),
        expect.any(Number),
      );
    });

    it('should show "No results" when search output is empty', async () => {
      mockSpawn().mockReturnValue(createFakeProcess(0, ''));

      const sub = clawhubCommand.subCommands!.find((c) => c.name === 'search')!;
      await sub.action!(mockContext, 'nonexistent');

      expect(mockContext.ui.addItem).toHaveBeenCalledWith(
        expect.objectContaining({
          type: MessageType.CLAWHUB_OUTPUT,
          text: expect.any(String),
        }),
        expect.any(Number),
      );
    });

    it('should pass --dir and --registry args', async () => {
      mockSpawn().mockReturnValue(createFakeProcess(0, ''));

      const sub = clawhubCommand.subCommands!.find((c) => c.name === 'search')!;
      await sub.action!(mockContext, 'test');

      const args = mockSpawn().mock.calls[0]![1] as string[];
      expect(args).toContain('--dir');
      expect(args).toContain('--registry');
      expect(args).toContain('--no-input');
    });
  });

  // ── list subcommand ─────────────────────────────────────────────

  describe('list', () => {
    it('should parse list output with slug and version', async () => {
      mockSpawn().mockReturnValue(
        createFakeProcess(0, 'my-skill  1.2.0\nother-skill  0.3.1'),
      );

      const sub = clawhubCommand.subCommands!.find((c) => c.name === 'list')!;
      await sub.action!(mockContext, '');

      expect(mockContext.ui.addItem).toHaveBeenCalledWith(
        expect.objectContaining({
          type: MessageType.CLAWHUB_OUTPUT,
          items: expect.arrayContaining([
            expect.objectContaining({ slug: 'my-skill', description: '1.2.0' }),
          ]),
        }),
        expect.any(Number),
      );
    });
  });

  // ── login subcommand ────────────────────────────────────────────

  describe('login', () => {
    it('should show error when called without token', async () => {
      const sub = clawhubCommand.subCommands!.find((c) => c.name === 'login')!;
      await sub.action!(mockContext, '');

      expect(mockContext.ui.addItem).toHaveBeenCalledWith(
        expect.objectContaining({
          type: MessageType.CLAWHUB_OUTPUT,
          isError: true,
          text: expect.stringContaining('/clawhub login'),
        }),
        expect.any(Number),
      );
      expect(mockSpawn()).not.toHaveBeenCalled();
    });

    it('should pass --token flag when token is provided', async () => {
      mockSpawn().mockReturnValue(createFakeProcess(0, '✔ Logged in'));

      const sub = clawhubCommand.subCommands!.find((c) => c.name === 'login')!;
      await sub.action!(mockContext, 'my-secret-token');

      const args = mockSpawn().mock.calls[0]![1] as string[];
      expect(args).toContain('--token');
      expect(args).toContain('my-secret-token');
    });
  });

  // ── whoami subcommand ───────────────────────────────────────────

  describe('whoami', () => {
    it('should display cleaned output', async () => {
      mockSpawn().mockReturnValue(
        createFakeProcess(0, '✔ Logged in as testuser'),
      );

      const sub = clawhubCommand.subCommands!.find((c) => c.name === 'whoami')!;
      await sub.action!(mockContext, '');

      expect(mockContext.ui.addItem).toHaveBeenCalledWith(
        expect.objectContaining({
          type: MessageType.CLAWHUB_OUTPUT,
          text: 'Logged in as testuser',
        }),
        expect.any(Number),
      );
    });

    it('should NOT pass --dir or --registry', async () => {
      mockSpawn().mockReturnValue(createFakeProcess(0, 'testuser'));

      const sub = clawhubCommand.subCommands!.find((c) => c.name === 'whoami')!;
      await sub.action!(mockContext, '');

      const args = mockSpawn().mock.calls[0]![1] as string[];
      expect(args).not.toContain('--dir');
      expect(args).not.toContain('--registry');
    });
  });

  // ── install subcommand ──────────────────────────────────────────

  describe('install', () => {
    it('should pass --dir and --registry', async () => {
      mockSpawn().mockReturnValue(createFakeProcess(0, '✔ Installed'));

      const sub = clawhubCommand.subCommands!.find(
        (c) => c.name === 'install',
      )!;
      await sub.action!(mockContext, 'some-skill');

      const args = mockSpawn().mock.calls[0]![1] as string[];
      expect(args).toContain('install');
      expect(args).toContain('some-skill');
      expect(args).toContain('--dir');
      expect(args).toContain('--registry');
    });
  });

  // ── uninstall subcommand ────────────────────────────────────────

  describe('uninstall', () => {
    it('should pass --dir and --yes', async () => {
      mockSpawn().mockReturnValue(createFakeProcess(0, '✔ Uninstalled'));

      const sub = clawhubCommand.subCommands!.find(
        (c) => c.name === 'uninstall',
      )!;
      await sub.action!(mockContext, 'some-skill');

      const args = mockSpawn().mock.calls[0]![1] as string[];
      expect(args).toContain('uninstall');
      expect(args).toContain('some-skill');
      expect(args).toContain('--dir');
      expect(args).toContain('--yes');
    });
  });

  // ── Error handling ──────────────────────────────────────────────

  describe('error handling', () => {
    it('should display error when clawhub exits with non-zero code', async () => {
      mockSpawn().mockReturnValue(
        createFakeProcess(1, '', 'something went wrong'),
      );

      const sub = clawhubCommand.subCommands!.find((c) => c.name === 'search')!;
      await sub.action!(mockContext, 'test');

      expect(mockContext.ui.addItem).toHaveBeenCalledWith(
        expect.objectContaining({
          type: MessageType.CLAWHUB_OUTPUT,
          isError: true,
          text: expect.stringContaining('something went wrong'),
        }),
        expect.any(Number),
      );
    });

    it('should add rate-limit hint when output contains Rate limit exceeded', async () => {
      mockSpawn().mockReturnValue(
        createFakeProcess(1, '', 'Rate limit exceeded'),
      );

      const sub = clawhubCommand.subCommands!.find((c) => c.name === 'search')!;
      await sub.action!(mockContext, 'test');

      expect(mockContext.ui.addItem).toHaveBeenCalledWith(
        expect.objectContaining({
          type: MessageType.CLAWHUB_OUTPUT,
          isError: true,
          text: expect.stringContaining('clawhub login'),
        }),
        expect.any(Number),
      );
    });
  });

  // ── Output cleaning ─────────────────────────────────────────────

  describe('output cleaning', () => {
    it('should strip ANSI escape codes', async () => {
      mockSpawn().mockReturnValue(
        createFakeProcess(0, '\x1b[32m✔ Logged in as user\x1b[0m'),
      );

      const sub = clawhubCommand.subCommands!.find((c) => c.name === 'whoami')!;
      await sub.action!(mockContext, '');

      expect(mockContext.ui.addItem).toHaveBeenCalledWith(
        expect.objectContaining({ text: 'Logged in as user' }),
        expect.any(Number),
      );
    });

    it('should strip spinner lines but keep result content', async () => {
      mockSpawn().mockReturnValue(
        createFakeProcess(
          0,
          '⠋ Loading...\n⠙ Still loading...\n✔ Done loading, here is your result',
        ),
      );

      const sub = clawhubCommand.subCommands!.find((c) => c.name === 'whoami')!;
      await sub.action!(mockContext, '');

      expect(mockContext.ui.addItem).toHaveBeenCalledWith(
        expect.objectContaining({
          text: 'Done loading, here is your result',
        }),
        expect.any(Number),
      );
    });

    it('should show fallback when output is all spinners', async () => {
      mockSpawn().mockReturnValue(
        createFakeProcess(0, '⠋ Loading...\n⠙ Still loading...'),
      );

      const sub = clawhubCommand.subCommands!.find((c) => c.name === 'whoami')!;
      await sub.action!(mockContext, '');

      expect(mockContext.ui.addItem).toHaveBeenCalledWith(
        expect.objectContaining({
          type: MessageType.CLAWHUB_OUTPUT,
          text: expect.any(String),
        }),
        expect.any(Number),
      );
    });
  });

  // ── Registry config ─────────────────────────────────────────────

  describe('registry config', () => {
    it('should use default CN registry when no config is set', async () => {
      mockSpawn().mockReturnValue(createFakeProcess(0, ''));

      const sub = clawhubCommand.subCommands!.find((c) => c.name === 'search')!;
      await sub.action!(mockContext, 'test');

      const args = mockSpawn().mock.calls[0]![1] as string[];
      const idx = args.indexOf('--registry');
      expect(idx).toBeGreaterThan(-1);
      expect(args[idx + 1]).toBe('https://cn.clawhub-mirror.com');
    });

    it('should use custom registry from settings', async () => {
      mockSpawn().mockReturnValue(createFakeProcess(0, ''));

      const customCtx = createMockCommandContext({
        services: {
          settings: {
            merged: { clawhub: { registry: 'https://custom.registry.com' } },
          },
        },
        ui: { addItem: vi.fn() },
      } as unknown as CommandContext);

      const sub = clawhubCommand.subCommands!.find((c) => c.name === 'search')!;
      await sub.action!(customCtx, 'test');

      const args = mockSpawn().mock.calls[0]![1] as string[];
      const idx = args.indexOf('--registry');
      expect(args[idx + 1]).toBe('https://custom.registry.com');
    });
  });
});
