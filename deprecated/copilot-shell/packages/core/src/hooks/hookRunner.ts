/**
 * @license
 * Copyright 2026 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import { spawn } from 'node:child_process';
import { HookEventName } from './types.js';
import type {
  HookConfig,
  HookInput,
  HookOutput,
  HookExecutionResult,
  PreToolUseInput,
  UserPromptSubmitInput,
} from './types.js';
import { createDebugLogger } from '../utils/debugLogger.js';
import {
  escapeShellArg,
  getShellConfiguration,
  type ShellType,
} from '../utils/shell-utils.js';

const debugLogger = createDebugLogger('TRUSTED_HOOKS');

/**
 * Default timeout for hook execution (60 seconds)
 */
const DEFAULT_HOOK_TIMEOUT = 60000;

/**
 * Grace period after SIGTERM before escalating to SIGKILL
 */
const SIGTERM_GRACE_PERIOD_MS = 5000;

/**
 * Bounded settlement window after SIGKILL. Descendants that inherit the
 * hook's stdio can keep the pipes open and suppress the 'close' event
 * forever, so the promise must resolve without it after this window.
 */
const FORCE_SETTLE_GRACE_PERIOD_MS = 1000;

/**
 * Maximum length for stdout/stderr output (1MB)
 * Prevents memory issues from unbounded output
 */
const MAX_OUTPUT_LENGTH = 1024 * 1024;

/**
 * Exit code constants for hook execution
 */
const EXIT_CODE_SUCCESS = 0;
const EXIT_CODE_NON_BLOCKING_ERROR = 1;

/**
 * Hook runner that executes command hooks
 */
export class HookRunner {
  /**
   * Execute a single hook
   *
   * An aborted `signal` terminates the hook's entire process tree and
   * settles the result within a bounded grace period.
   */
  async executeHook(
    hookConfig: HookConfig,
    eventName: HookEventName,
    input: HookInput,
    signal?: AbortSignal,
  ): Promise<HookExecutionResult> {
    const startTime = Date.now();

    try {
      return await this.executeCommandHook(
        hookConfig,
        eventName,
        input,
        startTime,
        signal,
      );
    } catch (error) {
      const duration = Date.now() - startTime;
      const hookId = hookConfig.name || hookConfig.command || 'unknown';
      const errorMessage = `Hook execution failed for event '${eventName}' (hook: ${hookId}): ${error}`;
      debugLogger.warn(`Hook execution error (non-fatal): ${errorMessage}`);

      return {
        hookConfig,
        eventName,
        success: false,
        error: error instanceof Error ? error : new Error(errorMessage),
        duration,
      };
    }
  }

  /**
   * Execute multiple hooks in parallel
   */
  async executeHooksParallel(
    hookConfigs: HookConfig[],
    eventName: HookEventName,
    input: HookInput,
    onHookStart?: (config: HookConfig, index: number) => void,
    onHookEnd?: (config: HookConfig, result: HookExecutionResult) => void,
    signal?: AbortSignal,
  ): Promise<HookExecutionResult[]> {
    const promises = hookConfigs.map(async (config, index) => {
      onHookStart?.(config, index);
      const result = await this.executeHook(config, eventName, input, signal);
      onHookEnd?.(config, result);
      return result;
    });

    return Promise.all(promises);
  }

  /**
   * Execute multiple hooks sequentially
   */
  async executeHooksSequential(
    hookConfigs: HookConfig[],
    eventName: HookEventName,
    input: HookInput,
    onHookStart?: (config: HookConfig, index: number) => void,
    onHookEnd?: (config: HookConfig, result: HookExecutionResult) => void,
    signal?: AbortSignal,
  ): Promise<HookExecutionResult[]> {
    const results: HookExecutionResult[] = [];
    let currentInput = input;

    for (let i = 0; i < hookConfigs.length; i++) {
      const config = hookConfigs[i];
      onHookStart?.(config, i);
      const result = await this.executeHook(
        config,
        eventName,
        currentInput,
        signal,
      );
      onHookEnd?.(config, result);
      results.push(result);

      // If the hook succeeded and has output, use it to modify the input for the next hook
      if (result.success && result.output) {
        currentInput = this.applyHookOutputToInput(
          currentInput,
          result.output,
          eventName,
        );
      }
    }

    return results;
  }

  /**
   * Apply hook output to modify input for the next hook in sequential execution
   */
  private applyHookOutputToInput(
    originalInput: HookInput,
    hookOutput: HookOutput,
    eventName: HookEventName,
  ): HookInput {
    // Create a copy of the original input
    const modifiedInput = { ...originalInput };

    // Apply modifications based on hook output and event type
    if (hookOutput.hookSpecificOutput) {
      switch (eventName) {
        case HookEventName.UserPromptSubmit:
          if ('additionalContext' in hookOutput.hookSpecificOutput) {
            // For UserPromptSubmit, we could modify the prompt with additional context
            const additionalContext =
              hookOutput.hookSpecificOutput['additionalContext'];
            if (
              typeof additionalContext === 'string' &&
              'prompt' in modifiedInput
            ) {
              (modifiedInput as UserPromptSubmitInput).prompt +=
                '\n\n' + additionalContext;
            }
          }
          break;

        case HookEventName.PreToolUse:
          if ('tool_input' in hookOutput.hookSpecificOutput) {
            const newToolInput = hookOutput.hookSpecificOutput[
              'tool_input'
            ] as Record<string, unknown>;
            if (newToolInput && 'tool_input' in modifiedInput) {
              (modifiedInput as PreToolUseInput).tool_input = {
                ...(modifiedInput as PreToolUseInput).tool_input,
                ...newToolInput,
              };
            }
          }
          break;

        default:
          // For other events, no special input modification is needed
          break;
      }
    }

    return modifiedInput;
  }

  /**
   * Execute a command hook
   */
  private async executeCommandHook(
    hookConfig: HookConfig,
    eventName: HookEventName,
    input: HookInput,
    startTime: number,
    signal?: AbortSignal,
  ): Promise<HookExecutionResult> {
    const timeout = hookConfig.timeout ?? DEFAULT_HOOK_TIMEOUT;
    const hookId = hookConfig.name || hookConfig.command || 'unknown';

    return new Promise((resolve) => {
      if (!hookConfig.command) {
        const errorMessage = 'Command hook missing command';
        debugLogger.warn(
          `Hook configuration error (non-fatal): ${errorMessage}`,
        );
        resolve({
          hookConfig,
          eventName,
          success: false,
          error: new Error(errorMessage),
          duration: Date.now() - startTime,
        });
        return;
      }

      let stdout = '';
      let stderr = '';
      let timedOut = false;
      let cancelled = false;
      let settled = false;

      const shellConfig = getShellConfiguration();
      const command = this.expandCommand(
        hookConfig.command,
        input,
        shellConfig.shell,
      );

      const env = {
        ...process.env,
        COPILOT_SHELL_PROJECT_DIR: input.cwd,
        GEMINI_PROJECT_DIR: input.cwd, // For Gemini CLI compatibility
        CLAUDE_PROJECT_DIR: input.cwd, // For Claude Code compatibility
        QWEN_PROJECT_DIR: input.cwd, // For Qwen Code compatibility
        ...hookConfig.env,
      };

      // If the caller already cancelled, do not spawn at all
      if (signal?.aborted) {
        resolve({
          hookConfig,
          eventName,
          success: false,
          error: new Error('Hook cancelled before start'),
          duration: Date.now() - startTime,
        });
        return;
      }

      // POSIX: run the hook in its own process group so that timeout and
      // cancellation can terminate the entire process tree. Otherwise
      // descendants survive, get reparented to PID 1, and keep the
      // inherited stdio pipes open (see issue #1582).
      const useProcessGroup = process.platform !== 'win32';

      const child = spawn(
        shellConfig.executable,
        [...shellConfig.argsPrefix, command],
        {
          env,
          cwd: input.cwd,
          stdio: ['pipe', 'pipe', 'pipe'],
          shell: false,
          detached: useProcessGroup,
        },
      );

      // child.killed only records that a signal was sent; exitCode and
      // signalCode reflect whether the process actually terminated.
      const hasExited = (): boolean =>
        child.exitCode !== null || child.signalCode !== null;

      // Signal the whole process group when available, falling back to the
      // direct child if the group is already gone.
      const killProcessTree = (killSignal: NodeJS.Signals): void => {
        if (child.pid === undefined) {
          return;
        }
        if (useProcessGroup) {
          try {
            process.kill(-child.pid, killSignal);
            return;
          } catch {
            // Group no longer exists; fall through to the direct child.
          }
        }
        try {
          child.kill(killSignal);
        } catch {
          // Process already exited.
        }
      };

      let sigkillHandle: NodeJS.Timeout | undefined;
      let settleHandle: NodeJS.Timeout | undefined;

      const onAbort = (): void => {
        cancelled = true;
        debugLogger.warn(
          `Hook '${hookId}' cancelled; terminating process group (pid=${child.pid})`,
        );
        terminate();
      };

      const settle = (result: HookExecutionResult): void => {
        if (settled) {
          return;
        }
        settled = true;
        clearTimeout(timeoutHandle);
        if (sigkillHandle) {
          clearTimeout(sigkillHandle);
        }
        if (settleHandle) {
          clearTimeout(settleHandle);
        }
        signal?.removeEventListener('abort', onAbort);
        resolve(result);
      };

      const terminationError = (): Error => {
        // Include the child's actual exit state so logs can distinguish a
        // hook that honored SIGTERM from one that had to be SIGKILLed or
        // never exited at all.
        const exitState = `exitCode=${child.exitCode}, signalCode=${child.signalCode}`;
        return cancelled
          ? new Error(`Hook cancelled (${exitState})`)
          : new Error(`Hook timed out after ${timeout}ms (${exitState})`);
      };

      // Escalation path shared by timeout and cancellation: SIGTERM the
      // tree, SIGKILL survivors after a grace period, then force-settle so
      // descendants holding the inherited stdio cannot block the promise.
      const terminate = (): void => {
        if (settled || sigkillHandle) {
          return;
        }
        killProcessTree('SIGTERM');

        sigkillHandle = setTimeout(() => {
          if (!hasExited()) {
            debugLogger.warn(
              `Hook '${hookId}' did not exit after SIGTERM; sending SIGKILL to process group (pid=${child.pid})`,
            );
          }
          // Always re-signal the group: even if the direct child exited,
          // descendants may still be running in the same group.
          killProcessTree('SIGKILL');

          settleHandle = setTimeout(() => {
            debugLogger.warn(
              `Hook '${hookId}' stdio still open after SIGKILL (pid=${child.pid}); force-settling without close event`,
            );
            // Release our ends of the pipes so no descendant can keep the
            // event loop or this promise alive.
            child.stdin?.destroy();
            child.stdout?.destroy();
            child.stderr?.destroy();
            settle({
              hookConfig,
              eventName,
              success: false,
              error: terminationError(),
              stdout,
              stderr,
              duration: Date.now() - startTime,
            });
          }, FORCE_SETTLE_GRACE_PERIOD_MS);
        }, SIGTERM_GRACE_PERIOD_MS);
      };

      // Set up timeout
      const timeoutHandle = setTimeout(() => {
        timedOut = true;
        debugLogger.warn(
          `Hook '${hookId}' timed out after ${timeout}ms; terminating process group (pid=${child.pid})`,
        );
        terminate();
      }, timeout);

      signal?.addEventListener('abort', onAbort, { once: true });

      // Send input to stdin
      if (child.stdin) {
        child.stdin.on('error', (err: NodeJS.ErrnoException) => {
          // Ignore EPIPE errors which happen when the child process closes stdin early
          if (err.code !== 'EPIPE') {
            debugLogger.debug(`Hook stdin error: ${err}`);
          }
        });

        // Wrap write operations in try-catch to handle synchronous EPIPE errors
        // that occur when the child process exits before we finish writing
        try {
          child.stdin.write(JSON.stringify(input));
          child.stdin.end();
        } catch (err) {
          // Ignore EPIPE errors which happen when the child process closes stdin early
          if (err instanceof Error && 'code' in err && err.code !== 'EPIPE') {
            debugLogger.debug(`Hook stdin write error: ${err}`);
          }
        }
      }

      // Collect stdout
      child.stdout?.on('data', (data: Buffer) => {
        if (stdout.length < MAX_OUTPUT_LENGTH) {
          const remaining = MAX_OUTPUT_LENGTH - stdout.length;
          stdout += data.slice(0, remaining).toString();
          if (data.length > remaining) {
            debugLogger.warn(
              `Hook stdout exceeded max length (${MAX_OUTPUT_LENGTH} bytes), truncating`,
            );
          }
        }
      });

      // Collect stderr
      child.stderr?.on('data', (data: Buffer) => {
        if (stderr.length < MAX_OUTPUT_LENGTH) {
          const remaining = MAX_OUTPUT_LENGTH - stderr.length;
          stderr += data.slice(0, remaining).toString();
          if (data.length > remaining) {
            debugLogger.warn(
              `Hook stderr exceeded max length (${MAX_OUTPUT_LENGTH} bytes), truncating`,
            );
          }
        }
      });

      // Handle process exit
      child.on('close', (exitCode) => {
        const duration = Date.now() - startTime;

        // Forward hook stderr to terminal for visibility
        if (stderr.trim()) {
          debugLogger.info(
            `[Hook Debug] hookRunner: hook '${hookId}' stderr:\n${stderr.trim()}`,
          );
        }

        if (timedOut || cancelled) {
          settle({
            hookConfig,
            eventName,
            success: false,
            error: terminationError(),
            stdout,
            stderr,
            duration,
          });
          return;
        }

        // Parse output
        // Exit code 2 is a blocking error - ignore stdout, use stderr only
        let output: HookOutput | undefined;
        const isBlockingError = exitCode === 2;

        // For exit code 2, only use stderr (ignore stdout)
        const textToParse = isBlockingError
          ? stderr.trim()
          : stdout.trim() || stderr.trim();

        if (textToParse) {
          // Only parse JSON on exit 0
          if (!isBlockingError) {
            try {
              let parsed = JSON.parse(textToParse);
              if (typeof parsed === 'string') {
                parsed = JSON.parse(parsed);
              }
              if (parsed && typeof parsed === 'object') {
                output = parsed as HookOutput;
              }
            } catch {
              // Not JSON, convert plain text to structured output
              output = this.convertPlainTextToHookOutput(
                textToParse,
                exitCode || EXIT_CODE_SUCCESS,
              );
            }
          } else {
            // Exit code 2: blocking error, use stderr as reason
            output = this.convertPlainTextToHookOutput(textToParse, exitCode);
          }
        }

        settle({
          hookConfig,
          eventName,
          success: exitCode === EXIT_CODE_SUCCESS,
          output,
          stdout,
          stderr,
          exitCode: exitCode || EXIT_CODE_SUCCESS,
          duration,
        });
      });

      // Handle process errors
      child.on('error', (error) => {
        const duration = Date.now() - startTime;

        settle({
          hookConfig,
          eventName,
          success: false,
          error,
          stdout,
          stderr,
          duration,
        });
      });
    });
  }

  /**
   * Expand command with environment variables and input context
   */
  private expandCommand(
    command: string,
    input: HookInput,
    shellType: ShellType,
  ): string {
    debugLogger.debug(`Expanding hook command: ${command} (cwd: ${input.cwd})`);
    const escapedCwd = escapeShellArg(input.cwd, shellType);
    return command
      .replace(/\$COPILOT_SHELL_PROJECT_DIR/g, () => escapedCwd)
      .replace(/\$GEMINI_PROJECT_DIR/g, () => escapedCwd) // For Gemini CLI compatibility
      .replace(/\$CLAUDE_PROJECT_DIR/g, () => escapedCwd) // For Claude Code compatibility
      .replace(/\$QWEN_PROJECT_DIR/g, () => escapedCwd); // For Qwen Code compatibility
  }

  /**
   * Convert plain text output to structured HookOutput
   */
  private convertPlainTextToHookOutput(
    text: string,
    exitCode: number,
  ): HookOutput {
    if (exitCode === EXIT_CODE_SUCCESS) {
      // Success - treat as system message or additional context
      return {
        decision: 'allow',
        systemMessage: text,
      };
    } else if (exitCode === EXIT_CODE_NON_BLOCKING_ERROR) {
      // Non-blocking error (EXIT_CODE_NON_BLOCKING_ERROR = 1)
      return {
        decision: 'allow',
        systemMessage: `Warning: ${text}`,
      };
    } else {
      // All other non-zero exit codes (including 2) are blocking
      return {
        decision: 'deny',
        reason: text,
      };
    }
  }
}
