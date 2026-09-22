/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

/**
 * This file contains helper functions for FileCommandLoader to create SlashCommand
 * objects from parsed command definitions (TOML or Markdown).
 */

import { spawn as _nodeSpawn } from 'node:child_process';
import path from 'node:path';
import { hydrateString } from '@copilot-shell/core';

/**
 * Thin wrapper around spawn, exported so unit tests can substitute a fake
 * without relying on ESM built-in module mocking (which is unreliable in
 * jsdom environment).
 * @internal Do not use outside of command-factory and its tests.
 */
export const _spawnImpl = { fn: _nodeSpawn };
import type {
  CommandContext,
  SlashCommand,
  SlashCommandActionReturn,
} from '../ui/commands/types.js';
import { CommandKind } from '../ui/commands/types.js';
import { DefaultArgumentProcessor } from './prompt-processors/argumentProcessor.js';
import type {
  IPromptProcessor,
  PromptPipelineContent,
} from './prompt-processors/types.js';
import {
  SHORTHAND_ARGS_PLACEHOLDER,
  SHELL_INJECTION_TRIGGER,
  AT_FILE_INJECTION_TRIGGER,
} from './prompt-processors/types.js';
import {
  ConfirmationRequiredError,
  ShellProcessor,
} from './prompt-processors/shellProcessor.js';
import { AtFileProcessor } from './prompt-processors/atFileProcessor.js';

export interface CommandDefinition {
  /** Prompt to send to the model. Required if `run` is absent. */
  prompt?: string;
  /** Shell command to execute directly without involving the model. */
  run?: string;
  description?: string;
  /**
   * When true, prepends `$ <command>` to the output.
   * Defaults to false — output is shown without the raw command header.
   */
  show_command?: boolean;
}

/**
 * Creates a SlashCommand from a parsed command definition.
 * This function is used by both TOML and Markdown command loaders.
 *
 * @param filePath The absolute path to the command file
 * @param baseDir The root command directory for name calculation
 * @param definition The parsed command definition (prompt and optional description)
 * @param extensionName Optional extension name to prefix commands with
 * @param fileExtension The file extension (e.g., '.toml' or '.md')
 * @returns A SlashCommand object
 */
export function createSlashCommandFromDefinition(
  filePath: string,
  baseDir: string,
  definition: CommandDefinition,
  extensionName: string | undefined,
  fileExtension: string,
  extensionPath?: string,
): SlashCommand {
  const relativePathWithExt = path.relative(baseDir, filePath);
  const relativePath = relativePathWithExt.substring(
    0,
    relativePathWithExt.length - fileExtension.length,
  );
  const baseCommandName = relativePath
    .split(path.sep)
    // Sanitize each path segment to prevent ambiguity. Since ':' is our
    // namespace separator, we replace any literal colons in filenames
    // with underscores to avoid naming conflicts.
    .map((segment) => segment.replaceAll(':', '_'))
    .join(':');

  // Add extension name tag for extension commands
  const defaultDescription = `Custom command from ${path.basename(filePath)}`;
  let description = definition.description || defaultDescription;
  if (extensionName) {
    description = `[${extensionName}] ${description}`;
  }

  const processors: IPromptProcessor[] = [];

  // ── Shell-only mode: `run` field executes directly, no model ──────────────
  if (definition.run != null) {
    // Apply ${extensionPath} and other variables if the command comes from an extension.
    const shellCmd =
      extensionPath != null
        ? hydrateString(definition.run, {
            extensionPath,
            CLAUDE_PLUGIN_ROOT: extensionPath,
            workspacePath: process.cwd(),
            '/': path.sep,
            pathSeparator: path.sep,
          })
        : definition.run;
    const showCommand = definition.show_command ?? false;
    return {
      name: baseCommandName,
      description,
      kind: CommandKind.FILE,
      extensionName,
      action: async (
        _context: CommandContext,
        args: string,
      ): Promise<SlashCommandActionReturn> => {
        // Substitute {{args}} with the actual user input at execution time.
        const resolvedCmd = shellCmd.replace(/\{\{args\}\}/g, args.trim());
        /** Maximum bytes collected from stdout+stderr before truncation. */
        const OUTPUT_CAP_BYTES = 512 * 1024; // 512 KiB
        /** Kill process after this many milliseconds. */
        const TIMEOUT_MS = 30_000;

        const result = await new Promise<{
          output: string;
          code: number | null;
          timedOut: boolean;
          spawnError?: boolean;
        }>((resolve) => {
          let totalBytes = 0;
          let truncated = false;
          const proc = _spawnImpl.fn('sh', ['-c', resolvedCmd], {
            stdio: ['ignore', 'pipe', 'pipe'],
          });

          const timer = setTimeout(() => {
            proc.kill();
            resolve({ output: chunks.join(''), code: null, timedOut: true });
          }, TIMEOUT_MS);

          const chunks: string[] = [];

          const collect = (d: Buffer) => {
            if (truncated) return;
            const remaining = OUTPUT_CAP_BYTES - totalBytes;
            if (d.length >= remaining) {
              chunks.push(d.subarray(0, remaining).toString());
              chunks.push(
                `\n[output truncated at ${OUTPUT_CAP_BYTES / 1024} KiB]`,
              );
              truncated = true;
              proc.stdout.destroy();
              proc.stderr.destroy();
            } else {
              totalBytes += d.length;
              chunks.push(d.toString());
            }
          };

          proc.stdout.on('data', collect);
          proc.stderr.on('data', collect);

          proc.on('error', (err) => {
            clearTimeout(timer);
            resolve({
              output: `[failed to start process: ${err.message}]`,
              code: null,
              timedOut: false,
              spawnError: true,
            });
          });

          proc.on('close', (code) => {
            clearTimeout(timer);
            resolve({ output: chunks.join(''), code, timedOut: false });
          });
        });

        const suffix = result.timedOut
          ? `\n[process killed after ${TIMEOUT_MS / 1000}s timeout]`
          : result.code !== 0 && result.code !== null
            ? `\n[exited with code ${result.code}]`
            : '';
        const isError =
          result.timedOut ||
          result.spawnError === true ||
          (result.code !== 0 && result.code !== null);
        return {
          type: 'message',
          messageType: isError ? 'error' : 'info',
          content: `${showCommand ? `$ ${resolvedCmd.trim()}\n` : ''}${result.output}${suffix}`,
        };
      },
    };
  }

  // ── Prompt mode: send to model ────────────────────────────────────────────
  const promptText = definition.prompt!;
  const usesArgs = promptText.includes(SHORTHAND_ARGS_PLACEHOLDER);
  const usesShellInjection = promptText.includes(SHELL_INJECTION_TRIGGER);
  const usesAtFileInjection = promptText.includes(AT_FILE_INJECTION_TRIGGER);

  // 1. @-File Injection (Security First).
  // This runs first to ensure we're not executing shell commands that
  // could dynamically generate malicious @-paths.
  if (usesAtFileInjection) {
    processors.push(new AtFileProcessor(baseCommandName));
  }

  // 2. Argument and Shell Injection.
  // This runs after file content has been safely injected.
  if (usesShellInjection || usesArgs) {
    processors.push(new ShellProcessor(baseCommandName));
  }

  // 3. Default Argument Handling.
  // Appends the raw invocation if no explicit {{args}} are used.
  if (!usesArgs) {
    processors.push(new DefaultArgumentProcessor());
  }

  return {
    name: baseCommandName,
    description,
    kind: CommandKind.FILE,
    extensionName,
    action: async (
      context: CommandContext,
      _args: string,
    ): Promise<SlashCommandActionReturn> => {
      if (!context.invocation) {
        console.error(
          `[FileCommandLoader] Critical error: Command '${baseCommandName}' was executed without invocation context.`,
        );
        return {
          type: 'submit_prompt',
          content: [{ text: promptText }], // Fallback to unprocessed prompt
        };
      }

      try {
        let processedContent: PromptPipelineContent = [{ text: promptText }];
        for (const processor of processors) {
          processedContent = await processor.process(processedContent, context);
        }

        return {
          type: 'submit_prompt',
          content: processedContent,
        };
      } catch (e) {
        // Check if it's our specific error type
        if (e instanceof ConfirmationRequiredError) {
          // Halt and request confirmation from the UI layer.
          return {
            type: 'confirm_shell_commands',
            commandsToConfirm: e.commandsToConfirm,
            originalInvocation: {
              raw: context.invocation.raw,
            },
          };
        }
        // Re-throw other errors to be handled by the global error handler.
        throw e;
      }
    },
  };
}
