/**
 * @license
 * Copyright 2026 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { HookRunner } from './hookRunner.js';
import { HookEventName, HookType, HooksConfigSource } from './types.js';
import type { HookConfig, HookInput } from './types.js';

// Hoisted mock
const mockSpawn = vi.hoisted(() => vi.fn());

vi.mock('node:child_process', async () => {
  const actual = await vi.importActual('node:child_process');
  return {
    ...actual,
    spawn: mockSpawn,
  };
});

describe('HookRunner', () => {
  let hookRunner: HookRunner;

  beforeEach(() => {
    hookRunner = new HookRunner();
    vi.clearAllMocks();
  });

  const createMockInput = (overrides: Partial<HookInput> = {}): HookInput => ({
    session_id: 'test-session',
    transcript_path: '/test/transcript',
    cwd: '/test',
    hook_event_name: 'test-event',
    timestamp: '2024-01-01T00:00:00Z',
    ...overrides,
  });

  const createMockProcess = (
    exitCode: number = 0,
    stdout: string = '',
    stderr: string = '',
  ) => {
    const mockProcess = {
      stdin: {
        on: vi.fn(),
        write: vi.fn(),
        end: vi.fn(),
      },
      stdout: {
        on: vi.fn((event: string, callback: (data: Buffer) => void) => {
          if (event === 'data' && stdout) {
            setTimeout(() => callback(Buffer.from(stdout)), 0);
          }
        }),
      },
      stderr: {
        on: vi.fn((event: string, callback: (data: Buffer) => void) => {
          if (event === 'data' && stderr) {
            setTimeout(() => callback(Buffer.from(stderr)), 0);
          }
        }),
      },
      on: vi.fn((event: string, callback: (code: number) => void) => {
        if (event === 'close') {
          setTimeout(() => callback(exitCode), 0);
        }
      }),
      kill: vi.fn(),
    };
    return mockProcess;
  };

  describe('executeHook', () => {
    it('should return error when hook command is missing', async () => {
      const hookConfig: HookConfig = {
        type: HookType.Command,
        command: '',
        source: HooksConfigSource.Project,
      };
      const input = createMockInput();

      const result = await hookRunner.executeHook(
        hookConfig,
        HookEventName.PreToolUse,
        input,
      );

      expect(result.success).toBe(false);
      expect(result.error?.message).toBe('Command hook missing command');
    });

    it('should execute hook and return success for exit code 0', async () => {
      const mockProcess = createMockProcess(0, 'hello');
      mockSpawn.mockImplementation(() => mockProcess);

      const hookConfig: HookConfig = {
        type: HookType.Command,
        command: 'echo hello',
        source: HooksConfigSource.Project,
      };
      const input = createMockInput();

      const result = await hookRunner.executeHook(
        hookConfig,
        HookEventName.PreToolUse,
        input,
      );

      expect(result.success).toBe(true);
      expect(result.stdout).toBe('hello');
      expect(mockSpawn).toHaveBeenCalled();
    });

    it('should return failure for non-zero exit code', async () => {
      const mockProcess = createMockProcess(1, '', 'error');
      mockSpawn.mockImplementation(() => mockProcess);

      const hookConfig: HookConfig = {
        type: HookType.Command,
        command: 'exit 1',
        source: HooksConfigSource.Project,
      };
      const input = createMockInput();

      const result = await hookRunner.executeHook(
        hookConfig,
        HookEventName.PreToolUse,
        input,
      );

      expect(result.success).toBe(false);
      expect(result.exitCode).toBe(1);
    });

    it('should parse JSON output from stdout', async () => {
      const output = JSON.stringify({
        decision: 'allow',
        systemMessage: 'test',
      });
      const mockProcess = createMockProcess(0, output);
      mockSpawn.mockImplementation(() => mockProcess);

      const hookConfig: HookConfig = {
        type: HookType.Command,
        command: 'echo json',
        source: HooksConfigSource.Project,
      };
      const input = createMockInput();

      const result = await hookRunner.executeHook(
        hookConfig,
        HookEventName.PreToolUse,
        input,
      );

      expect(result.success).toBe(true);
      expect(result.output?.decision).toBe('allow');
      expect(result.output?.systemMessage).toBe('test');
    });

    it('should convert plain text to allow output on success', async () => {
      const mockProcess = createMockProcess(0, 'some text output');
      mockSpawn.mockImplementation(() => mockProcess);

      const hookConfig: HookConfig = {
        type: HookType.Command,
        command: 'echo text',
        source: HooksConfigSource.Project,
      };
      const input = createMockInput();

      const result = await hookRunner.executeHook(
        hookConfig,
        HookEventName.PreToolUse,
        input,
      );

      expect(result.success).toBe(true);
      expect(result.output?.decision).toBe('allow');
      expect(result.output?.systemMessage).toBe('some text output');
    });

    it('should convert plain text to deny output on exit code 2', async () => {
      const mockProcess = createMockProcess(2, '', 'error message');
      mockSpawn.mockImplementation(() => mockProcess);

      const hookConfig: HookConfig = {
        type: HookType.Command,
        command: 'echo error && exit 2',
        source: HooksConfigSource.Project,
      };
      const input = createMockInput();

      const result = await hookRunner.executeHook(
        hookConfig,
        HookEventName.PreToolUse,
        input,
      );

      expect(result.success).toBe(false);
      expect(result.output?.decision).toBe('deny');
      expect(result.output?.reason).toBe('error message');
    });

    it('should ignore stdout on exit code 2 and use stderr only', async () => {
      // Exit code 2 should ignore stdout and use stderr as the error message
      const mockProcess = createMockProcess(
        2,
        'stdout should be ignored',
        'stderr error message',
      );
      mockSpawn.mockImplementation(() => mockProcess);

      const hookConfig: HookConfig = {
        type: HookType.Command,
        command: 'echo stdout && echo stderr >&2 && exit 2',
        source: HooksConfigSource.Project,
      };
      const input = createMockInput();

      const result = await hookRunner.executeHook(
        hookConfig,
        HookEventName.PreToolUse,
        input,
      );

      expect(result.success).toBe(false);
      expect(result.output?.decision).toBe('deny');
      expect(result.output?.reason).toBe('stderr error message');
    });

    it('should not parse JSON on exit code 2', async () => {
      // Exit code 2 should ignore JSON in stdout
      const mockProcess = createMockProcess(
        2,
        '{"decision":"allow"}',
        'blocking error',
      );
      mockSpawn.mockImplementation(() => mockProcess);

      const hookConfig: HookConfig = {
        type: HookType.Command,
        command: 'echo json && exit 2',
        source: HooksConfigSource.Project,
      };
      const input = createMockInput();

      const result = await hookRunner.executeHook(
        hookConfig,
        HookEventName.PreToolUse,
        input,
      );

      // Should NOT parse JSON, should use stderr as reason
      expect(result.success).toBe(false);
      expect(result.output?.decision).toBe('deny');
      expect(result.output?.reason).toBe('blocking error');
    });

    it('should handle exit code 1 as non-blocking warning', async () => {
      const mockProcess = createMockProcess(1, '', 'warning');
      mockSpawn.mockImplementation(() => mockProcess);

      const hookConfig: HookConfig = {
        type: HookType.Command,
        command: 'exit 1',
        source: HooksConfigSource.Project,
      };
      const input = createMockInput();

      const result = await hookRunner.executeHook(
        hookConfig,
        HookEventName.PreToolUse,
        input,
      );

      expect(result.success).toBe(false);
      expect(result.output?.decision).toBe('allow');
      expect(result.output?.systemMessage).toBe('Warning: warning');
    });

    it('should include duration in result', async () => {
      const mockProcess = createMockProcess(0, 'test');
      mockSpawn.mockImplementation(() => mockProcess);

      const hookConfig: HookConfig = {
        type: HookType.Command,
        command: 'echo test',
        source: HooksConfigSource.Project,
      };
      const input = createMockInput();

      const result = await hookRunner.executeHook(
        hookConfig,
        HookEventName.PreToolUse,
        input,
      );

      expect(result.duration).toBeGreaterThanOrEqual(0);
    });

    it('should handle process error', async () => {
      const mockProcess = {
        stdin: { on: vi.fn(), write: vi.fn(), end: vi.fn() },
        stdout: { on: vi.fn() },
        stderr: { on: vi.fn() },
        on: vi.fn((event: string, callback: (error: Error) => void) => {
          if (event === 'error') {
            callback(new Error('spawn error'));
          }
        }),
        kill: vi.fn(),
      };
      mockSpawn.mockImplementation(() => mockProcess);

      const hookConfig: HookConfig = {
        type: HookType.Command,
        command: 'echo test',
        source: HooksConfigSource.Project,
      };
      const input = createMockInput();

      const result = await hookRunner.executeHook(
        hookConfig,
        HookEventName.PreToolUse,
        input,
      );

      expect(result.success).toBe(false);
      expect(result.error).toBeDefined();
    });
  });

  describe('executeHooksParallel', () => {
    it('should execute multiple hooks in parallel', async () => {
      const mockProcess = createMockProcess(0, 'result');
      mockSpawn.mockImplementation(() => mockProcess);

      const hookConfigs: HookConfig[] = [
        {
          type: HookType.Command,
          command: 'echo hook1',
          source: HooksConfigSource.Project,
        },
        {
          type: HookType.Command,
          command: 'echo hook2',
          source: HooksConfigSource.Project,
        },
      ];
      const input = createMockInput();

      const results = await hookRunner.executeHooksParallel(
        hookConfigs,
        HookEventName.PreToolUse,
        input,
      );

      expect(results).toHaveLength(2);
      expect(results[0].success).toBe(true);
      expect(results[1].success).toBe(true);
    });

    it('should call onHookStart and onHookEnd callbacks', async () => {
      const mockProcess = createMockProcess(0, 'result');
      mockSpawn.mockImplementation(() => mockProcess);

      const hookConfigs: HookConfig[] = [
        {
          type: HookType.Command,
          command: 'echo test',
          source: HooksConfigSource.Project,
        },
      ];
      const input = createMockInput();
      const onHookStart = vi.fn();
      const onHookEnd = vi.fn();

      await hookRunner.executeHooksParallel(
        hookConfigs,
        HookEventName.PreToolUse,
        input,
        onHookStart,
        onHookEnd,
      );

      expect(onHookStart).toHaveBeenCalledTimes(1);
      expect(onHookEnd).toHaveBeenCalledTimes(1);
    });
  });

  describe('executeHooksSequential', () => {
    it('should execute hooks sequentially', async () => {
      const mockProcess = createMockProcess(0, 'result');
      mockSpawn.mockImplementation(() => mockProcess);

      const hookConfigs: HookConfig[] = [
        {
          type: HookType.Command,
          command: 'echo first',
          source: HooksConfigSource.Project,
        },
        {
          type: HookType.Command,
          command: 'echo second',
          source: HooksConfigSource.Project,
        },
      ];
      const input = createMockInput();

      const results = await hookRunner.executeHooksSequential(
        hookConfigs,
        HookEventName.PreToolUse,
        input,
      );

      expect(results).toHaveLength(2);
      expect(results[0].success).toBe(true);
      expect(results[1].success).toBe(true);
    });

    it('should call onHookStart and onHookEnd callbacks', async () => {
      const mockProcess = createMockProcess(0, 'result');
      mockSpawn.mockImplementation(() => mockProcess);

      const hookConfigs: HookConfig[] = [
        {
          type: HookType.Command,
          command: 'echo test',
          source: HooksConfigSource.Project,
        },
      ];
      const input = createMockInput();
      const onHookStart = vi.fn();
      const onHookEnd = vi.fn();

      await hookRunner.executeHooksSequential(
        hookConfigs,
        HookEventName.PreToolUse,
        input,
        onHookStart,
        onHookEnd,
      );

      expect(onHookStart).toHaveBeenCalledTimes(1);
      expect(onHookEnd).toHaveBeenCalledTimes(1);
    });
  });

  describe('output truncation', () => {
    it('should truncate stdout when exceeding MAX_OUTPUT_LENGTH', async () => {
      // Create a process that outputs more than 1MB of data
      const largeOutput = 'x'.repeat(2 * 1024 * 1024); // 2MB
      const mockProcess = createMockProcess(0, largeOutput);
      mockSpawn.mockImplementation(() => mockProcess);

      const hookConfig: HookConfig = {
        type: HookType.Command,
        command: 'echo large',
        source: HooksConfigSource.Project,
      };
      const input = createMockInput();

      const result = await hookRunner.executeHook(
        hookConfig,
        HookEventName.PreToolUse,
        input,
      );

      // stdout should be truncated to MAX_OUTPUT_LENGTH (1MB)
      expect(result.stdout?.length).toBeLessThanOrEqual(1024 * 1024);
    });

    it('should truncate stderr when exceeding MAX_OUTPUT_LENGTH', async () => {
      const largeOutput = 'x'.repeat(2 * 1024 * 1024); // 2MB
      const mockProcess = createMockProcess(0, '', largeOutput);
      mockSpawn.mockImplementation(() => mockProcess);

      const hookConfig: HookConfig = {
        type: HookType.Command,
        command: 'echo large',
        source: HooksConfigSource.Project,
      };
      const input = createMockInput();

      const result = await hookRunner.executeHook(
        hookConfig,
        HookEventName.PreToolUse,
        input,
      );

      // stderr should be truncated to MAX_OUTPUT_LENGTH (1MB)
      expect(result.stderr?.length).toBeLessThanOrEqual(1024 * 1024);
    });

    it('should handle partial truncation gracefully', async () => {
      // Output exactly at the limit
      const exactOutput = 'x'.repeat(1024 * 1024); // 1MB exactly
      const mockProcess = createMockProcess(0, exactOutput);
      mockSpawn.mockImplementation(() => mockProcess);

      const hookConfig: HookConfig = {
        type: HookType.Command,
        command: 'echo exact',
        source: HooksConfigSource.Project,
      };
      const input = createMockInput();

      const result = await hookRunner.executeHook(
        hookConfig,
        HookEventName.PreToolUse,
        input,
      );

      expect(result.stdout?.length).toBe(1024 * 1024);
    });
  });

  describe('expandCommand', () => {
    it('should expand COPILOT_SHELL_PROJECT_DIR placeholder', async () => {
      const mockProcess = createMockProcess(0, 'result');
      mockSpawn.mockImplementation(() => mockProcess);

      const hookConfig: HookConfig = {
        type: HookType.Command,
        command: 'echo $COPILOT_SHELL_PROJECT_DIR',
        source: HooksConfigSource.Project,
      };
      const input = createMockInput({ cwd: '/test/project' });

      await hookRunner.executeHook(hookConfig, HookEventName.PreToolUse, input);

      // Verify spawn was called with expanded command
      const spawnCall = mockSpawn.mock.calls[0];
      const command = spawnCall[1][spawnCall[1].length - 1]; // Last arg is the command
      expect(command).toContain('/test/project');
    });

    it('should expand GEMINI_PROJECT_DIR placeholder for compatibility', async () => {
      const mockProcess = createMockProcess(0, 'result');
      mockSpawn.mockImplementation(() => mockProcess);

      const hookConfig: HookConfig = {
        type: HookType.Command,
        command: 'echo $GEMINI_PROJECT_DIR',
        source: HooksConfigSource.Project,
      };
      const input = createMockInput({ cwd: '/test/project' });

      await hookRunner.executeHook(hookConfig, HookEventName.PreToolUse, input);

      // Verify spawn was called with expanded command
      const spawnCall = mockSpawn.mock.calls[0];
      const command = spawnCall[1][spawnCall[1].length - 1]; // Last arg is the command
      expect(command).toContain('/test/project');
    });

    it('should expand CLAUDE_PROJECT_DIR placeholder for Claude Code compatibility', async () => {
      const mockProcess = createMockProcess(0, 'result');
      mockSpawn.mockImplementation(() => mockProcess);

      const hookConfig: HookConfig = {
        type: HookType.Command,
        command: 'echo $CLAUDE_PROJECT_DIR',
        source: HooksConfigSource.Project,
      };
      const input = createMockInput({ cwd: '/test/project' });

      await hookRunner.executeHook(hookConfig, HookEventName.PreToolUse, input);

      const spawnCall = mockSpawn.mock.calls[0];
      const command = spawnCall[1][spawnCall[1].length - 1]; // Last arg is the command
      expect(command).toContain('/test/project');
    });

    it('should not modify command without placeholders', async () => {
      const mockProcess = createMockProcess(0, 'result');
      mockSpawn.mockImplementation(() => mockProcess);

      const hookConfig: HookConfig = {
        type: HookType.Command,
        command: 'echo hello',
        source: HooksConfigSource.Project,
      };
      const input = createMockInput({ cwd: '/test/project' });

      await hookRunner.executeHook(hookConfig, HookEventName.PreToolUse, input);

      const spawnCall = mockSpawn.mock.calls[0];
      const command = spawnCall[1][spawnCall[1].length - 1]; // Last arg is the command
      expect(command).toBe('echo hello');
    });
  });

  describe('convertPlainTextToHookOutput', () => {
    it('should convert plain text to allow output on success', async () => {
      const mockProcess = createMockProcess(0, 'plain text response');
      mockSpawn.mockImplementation(() => mockProcess);

      const hookConfig: HookConfig = {
        type: HookType.Command,
        command: 'echo text',
        source: HooksConfigSource.Project,
      };
      const input = createMockInput();

      const result = await hookRunner.executeHook(
        hookConfig,
        HookEventName.PreToolUse,
        input,
      );

      expect(result.success).toBe(true);
      expect(result.output?.decision).toBe('allow');
      expect(result.output?.systemMessage).toBe('plain text response');
    });

    it('should convert non-zero exit code to deny output', async () => {
      const mockProcess = createMockProcess(3, '', 'error message');
      mockSpawn.mockImplementation(() => mockProcess);

      const hookConfig: HookConfig = {
        type: HookType.Command,
        command: 'exit 3',
        source: HooksConfigSource.Project,
      };
      const input = createMockInput();

      const result = await hookRunner.executeHook(
        hookConfig,
        HookEventName.PreToolUse,
        input,
      );

      expect(result.success).toBe(false);
      expect(result.output?.decision).toBe('deny');
      expect(result.output?.reason).toBe('error message');
    });

    it('should use stderr when stdout is empty on success', async () => {
      const mockProcess = createMockProcess(0, '', 'stderr output');
      mockSpawn.mockImplementation(() => mockProcess);

      const hookConfig: HookConfig = {
        type: HookType.Command,
        command: 'echo test',
        source: HooksConfigSource.Project,
      };
      const input = createMockInput();

      const result = await hookRunner.executeHook(
        hookConfig,
        HookEventName.PreToolUse,
        input,
      );

      expect(result.output?.systemMessage).toBe('stderr output');
    });

    it('should handle empty output gracefully', async () => {
      const mockProcess = createMockProcess(0, '', '');
      mockSpawn.mockImplementation(() => mockProcess);

      const hookConfig: HookConfig = {
        type: HookType.Command,
        command: 'echo test',
        source: HooksConfigSource.Project,
      };
      const input = createMockInput();

      const result = await hookRunner.executeHook(
        hookConfig,
        HookEventName.PreToolUse,
        input,
      );

      expect(result.output).toBeUndefined();
    });

    it('should parse nested JSON strings', async () => {
      const nestedJson = JSON.stringify(JSON.stringify({ decision: 'allow' }));
      const mockProcess = createMockProcess(0, nestedJson);
      mockSpawn.mockImplementation(() => mockProcess);

      const hookConfig: HookConfig = {
        type: HookType.Command,
        command: 'echo json',
        source: HooksConfigSource.Project,
      };
      const input = createMockInput();

      const result = await hookRunner.executeHook(
        hookConfig,
        HookEventName.PreToolUse,
        input,
      );

      expect(result.output?.decision).toBe('allow');
    });
  });

  // Process-group semantics are POSIX-only; on Windows the runner falls
  // back to signalling the direct child.
  describe.skipIf(process.platform === 'win32')(
    'timeout and cancellation',
    () => {
      // Simulates a child (or descendant) that ignores SIGTERM and keeps the
      // inherited stdio open: no 'close' event ever fires (issue #1582).
      const createHungProcess = () => ({
        pid: 12345,
        exitCode: null as number | null,
        signalCode: null as NodeJS.Signals | null,
        killed: false,
        stdin: { on: vi.fn(), write: vi.fn(), end: vi.fn(), destroy: vi.fn() },
        stdout: { on: vi.fn(), destroy: vi.fn() },
        stderr: { on: vi.fn(), destroy: vi.fn() },
        on: vi.fn(),
        kill: vi.fn().mockReturnValue(true),
      });

      const hungHookConfig: HookConfig = {
        type: HookType.Command,
        name: 'timeout-repro',
        command: "trap '' TERM; sleep 600",
        timeout: 1000,
        source: HooksConfigSource.Project,
      };

      let killSpy: ReturnType<typeof vi.spyOn>;

      beforeEach(() => {
        vi.useFakeTimers();
        killSpy = vi
          .spyOn(process, 'kill')
          .mockImplementation(() => true) as ReturnType<typeof vi.spyOn>;
      });

      afterEach(() => {
        vi.useRealTimers();
        vi.restoreAllMocks();
      });

      it('should spawn the hook in its own process group', async () => {
        const proc = createHungProcess();
        mockSpawn.mockImplementation(() => proc);

        void hookRunner.executeHook(
          hungHookConfig,
          HookEventName.PreToolUse,
          createMockInput(),
        );

        expect(mockSpawn.mock.calls[0][2]).toMatchObject({ detached: true });
      });

      it('should SIGKILL the process group even when child.killed is true', async () => {
        const proc = createHungProcess();
        mockSpawn.mockImplementation(() => proc);

        const promise = hookRunner.executeHook(
          hungHookConfig,
          HookEventName.PreToolUse,
          createMockInput(),
        );

        // Timeout elapses: SIGTERM goes to the whole group, not the child
        await vi.advanceTimersByTimeAsync(1000);
        expect(killSpy).toHaveBeenCalledWith(-12345, 'SIGTERM');

        // child.killed only means a signal was sent; the process is still
        // alive (exitCode/signalCode are null), so SIGKILL must follow.
        proc.killed = true;
        await vi.advanceTimersByTimeAsync(5000);
        expect(killSpy).toHaveBeenCalledWith(-12345, 'SIGKILL');

        // Bounded settlement: no 'close' event, yet the promise resolves
        await vi.advanceTimersByTimeAsync(1000);
        const result = await promise;
        expect(result.success).toBe(false);
        expect(result.error?.message).toContain('Hook timed out after 1000ms');
        // Exit state is surfaced for observability; the process never exited
        expect(result.error?.message).toContain(
          'exitCode=null, signalCode=null',
        );
        expect(proc.stdout.destroy).toHaveBeenCalled();
        expect(proc.stderr.destroy).toHaveBeenCalled();
      });

      it('should settle via close and stop escalation when the child exits after SIGTERM', async () => {
        const proc = createHungProcess();
        let closeCallback: ((code: number | null) => void) | undefined;
        proc.on = vi.fn(
          (event: string, callback: (code: number | null) => void) => {
            if (event === 'close') {
              closeCallback = callback;
            }
          },
        );
        mockSpawn.mockImplementation(() => proc);

        const promise = hookRunner.executeHook(
          hungHookConfig,
          HookEventName.PreToolUse,
          createMockInput(),
        );

        await vi.advanceTimersByTimeAsync(1000);
        expect(killSpy).toHaveBeenCalledWith(-12345, 'SIGTERM');

        // Child exits within the grace period
        proc.signalCode = 'SIGTERM';
        closeCallback?.(null);

        const result = await promise;
        expect(result.success).toBe(false);
        expect(result.error?.message).toContain('Hook timed out after 1000ms');
        // The child honored SIGTERM; the error message records that
        expect(result.error?.message).toContain('signalCode=SIGTERM');

        // Escalation timers were cleared: no SIGKILL is ever sent
        await vi.advanceTimersByTimeAsync(60000);
        expect(killSpy).not.toHaveBeenCalledWith(-12345, 'SIGKILL');
      });

      it('should terminate the process tree when the abort signal fires', async () => {
        const proc = createHungProcess();
        mockSpawn.mockImplementation(() => proc);

        const controller = new AbortController();
        const promise = hookRunner.executeHook(
          { ...hungHookConfig, timeout: 60000 },
          HookEventName.PreToolUse,
          createMockInput(),
          controller.signal,
        );

        controller.abort();
        expect(killSpy).toHaveBeenCalledWith(-12345, 'SIGTERM');

        await vi.advanceTimersByTimeAsync(5000);
        expect(killSpy).toHaveBeenCalledWith(-12345, 'SIGKILL');

        await vi.advanceTimersByTimeAsync(1000);
        const result = await promise;
        expect(result.success).toBe(false);
        expect(result.error?.message).toContain('Hook cancelled');
      });

      it('should not spawn when the signal is already aborted', async () => {
        const controller = new AbortController();
        controller.abort();

        const result = await hookRunner.executeHook(
          hungHookConfig,
          HookEventName.PreToolUse,
          createMockInput(),
          controller.signal,
        );

        expect(mockSpawn).not.toHaveBeenCalled();
        expect(result.success).toBe(false);
        expect(result.error?.message).toBe('Hook cancelled before start');
      });

      it('should fall back to killing the direct child when the group is gone', async () => {
        const proc = createHungProcess();
        mockSpawn.mockImplementation(() => proc);
        killSpy.mockImplementation(() => {
          throw new Error('ESRCH');
        });

        const promise = hookRunner.executeHook(
          hungHookConfig,
          HookEventName.PreToolUse,
          createMockInput(),
        );

        await vi.advanceTimersByTimeAsync(1000);
        expect(proc.kill).toHaveBeenCalledWith('SIGTERM');

        await vi.advanceTimersByTimeAsync(6000);
        const result = await promise;
        expect(result.success).toBe(false);
      });
    },
  );
});
