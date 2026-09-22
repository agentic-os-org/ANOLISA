/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import {
  getErrorMessage,
  getCurrentGeminiMdFilename,
  loadServerHierarchicalMemory,
  QWEN_DIR,
  listInboxPatchFiles,
  listValidInboxPatchFiles,
  validateInboxMemoryPatchFile,
  type InboxMemoryPatchKind,
} from '@copilot-shell/core';
import path from 'node:path';
import os from 'node:os';
import fs from 'node:fs/promises';
import * as Diff from 'diff';
import { MessageType } from '../types.js';
import type { SlashCommand, SlashCommandActionReturn } from './types.js';
import { CommandKind } from './types.js';
import { t } from '../../i18n/index.js';

export const memoryCommand: SlashCommand = {
  name: 'memory',
  get description() {
    return t('Commands for interacting with memory.');
  },
  kind: CommandKind.BUILT_IN,
  subCommands: [
    {
      name: 'show',
      get description() {
        return t('Show the current memory contents.');
      },
      kind: CommandKind.BUILT_IN,
      action: async (context) => {
        const memoryContent = context.services.config?.getUserMemory() || '';
        const fileCount = context.services.config?.getGeminiMdFileCount() || 0;

        const messageContent =
          memoryContent.length > 0
            ? `${t('Current memory content from {{count}} file(s):', { count: String(fileCount) })}\n\n---\n${memoryContent}\n---`
            : t('Memory is currently empty.');

        context.ui.addItem(
          {
            type: MessageType.INFO,
            text: messageContent,
          },
          Date.now(),
        );
      },
      subCommands: [
        {
          name: '--project',
          get description() {
            return t('Show project-level memory contents.');
          },
          kind: CommandKind.BUILT_IN,
          action: async (context) => {
            try {
              const workingDir =
                context.services.config?.getWorkingDir?.() ?? process.cwd();
              const projectMemoryPath = path.join(
                workingDir,
                getCurrentGeminiMdFilename(),
              );
              const memoryContent = await fs.readFile(
                projectMemoryPath,
                'utf-8',
              );

              const messageContent =
                memoryContent.trim().length > 0
                  ? t(
                      'Project memory content from {{path}}:\n\n---\n{{content}}\n---',
                      {
                        path: projectMemoryPath,
                        content: memoryContent,
                      },
                    )
                  : t('Project memory is currently empty.');

              context.ui.addItem(
                {
                  type: MessageType.INFO,
                  text: messageContent,
                },
                Date.now(),
              );
            } catch (_error) {
              context.ui.addItem(
                {
                  type: MessageType.INFO,
                  text: t(
                    'Project memory file not found or is currently empty.',
                  ),
                },
                Date.now(),
              );
            }
          },
        },
        {
          name: '--global',
          get description() {
            return t('Show global memory contents.');
          },
          kind: CommandKind.BUILT_IN,
          action: async (context) => {
            try {
              const globalMemoryPath = path.join(
                os.homedir(),
                QWEN_DIR,
                getCurrentGeminiMdFilename(),
              );
              const globalMemoryContent = await fs.readFile(
                globalMemoryPath,
                'utf-8',
              );

              const messageContent =
                globalMemoryContent.trim().length > 0
                  ? t('Global memory content:\n\n---\n{{content}}\n---', {
                      content: globalMemoryContent,
                    })
                  : t('Global memory is currently empty.');

              context.ui.addItem(
                {
                  type: MessageType.INFO,
                  text: messageContent,
                },
                Date.now(),
              );
            } catch (_error) {
              context.ui.addItem(
                {
                  type: MessageType.INFO,
                  text: t(
                    'Global memory file not found or is currently empty.',
                  ),
                },
                Date.now(),
              );
            }
          },
        },
      ],
    },
    {
      name: 'add',
      get description() {
        return t(
          'Add content to the memory. Use --global for global memory or --project for project memory.',
        );
      },
      kind: CommandKind.BUILT_IN,
      action: (context, args): SlashCommandActionReturn | void => {
        if (!args || args.trim() === '') {
          return {
            type: 'message',
            messageType: 'error',
            content: t(
              'Usage: /memory add [--global|--project] <text to remember>',
            ),
          };
        }

        const trimmedArgs = args.trim();
        let scope: 'global' | 'project' | undefined;
        let fact: string;

        // Check for scope flags
        if (trimmedArgs.startsWith('--global ')) {
          scope = 'global';
          fact = trimmedArgs.substring('--global '.length).trim();
        } else if (trimmedArgs.startsWith('--project ')) {
          scope = 'project';
          fact = trimmedArgs.substring('--project '.length).trim();
        } else if (trimmedArgs === '--global' || trimmedArgs === '--project') {
          // Flag provided but no text after it
          return {
            type: 'message',
            messageType: 'error',
            content: t(
              'Usage: /memory add [--global|--project] <text to remember>',
            ),
          };
        } else {
          // No scope specified, will be handled by the tool
          fact = trimmedArgs;
        }

        if (!fact || fact.trim() === '') {
          return {
            type: 'message',
            messageType: 'error',
            content: t(
              'Usage: /memory add [--global|--project] <text to remember>',
            ),
          };
        }

        const scopeText = scope ? `(${scope})` : '';
        context.ui.addItem(
          {
            type: MessageType.INFO,
            text: t('Attempting to save to memory {{scope}}: "{{fact}}"', {
              scope: scopeText,
              fact,
            }),
          },
          Date.now(),
        );

        return {
          type: 'tool',
          toolName: 'save_memory',
          toolArgs: scope ? { fact, scope } : { fact },
        };
      },
      subCommands: [
        {
          name: '--project',
          get description() {
            return t('Add content to project-level memory.');
          },
          kind: CommandKind.BUILT_IN,
          action: (context, args): SlashCommandActionReturn | void => {
            if (!args || args.trim() === '') {
              return {
                type: 'message',
                messageType: 'error',
                content: t('Usage: /memory add --project <text to remember>'),
              };
            }

            context.ui.addItem(
              {
                type: MessageType.INFO,
                text: t('Attempting to save to project memory: "{{text}}"', {
                  text: args.trim(),
                }),
              },
              Date.now(),
            );

            return {
              type: 'tool',
              toolName: 'save_memory',
              toolArgs: { fact: args.trim(), scope: 'project' },
            };
          },
        },
        {
          name: '--global',
          get description() {
            return t('Add content to global memory.');
          },
          kind: CommandKind.BUILT_IN,
          action: (context, args): SlashCommandActionReturn | void => {
            if (!args || args.trim() === '') {
              return {
                type: 'message',
                messageType: 'error',
                content: t('Usage: /memory add --global <text to remember>'),
              };
            }

            context.ui.addItem(
              {
                type: MessageType.INFO,
                text: t('Attempting to save to global memory: "{{text}}"', {
                  text: args.trim(),
                }),
              },
              Date.now(),
            );

            return {
              type: 'tool',
              toolName: 'save_memory',
              toolArgs: { fact: args.trim(), scope: 'global' },
            };
          },
        },
      ],
    },
    {
      name: 'refresh',
      get description() {
        return t('Refresh the memory from the source.');
      },
      kind: CommandKind.BUILT_IN,
      action: async (context) => {
        context.ui.addItem(
          {
            type: MessageType.INFO,
            text: t('Refreshing memory from source files...'),
          },
          Date.now(),
        );

        try {
          const config = context.services.config;
          if (config) {
            const { memoryContent, fileCount } =
              await loadServerHierarchicalMemory(
                config.getWorkingDir(),
                config.shouldLoadMemoryFromIncludeDirectories()
                  ? config.getWorkspaceContext().getDirectories()
                  : [],
                config.getDebugMode(),
                config.getFileService(),
                config.getExtensionContextFilePaths(),
                config.getFolderTrust(),
                context.services.settings.merged.context?.importFormat ||
                  'tree', // Use setting or default to 'tree'
              );
            config.setUserMemory(memoryContent);
            config.setGeminiMdFileCount(fileCount);

            const successMessage =
              memoryContent.length > 0
                ? `Memory refreshed successfully. Loaded ${memoryContent.length} characters from ${fileCount} file(s).`
                : 'Memory refreshed successfully. No memory content found.';

            context.ui.addItem(
              {
                type: MessageType.INFO,
                text: successMessage,
              },
              Date.now(),
            );
          }
        } catch (error) {
          const errorMessage = getErrorMessage(error);
          context.ui.addItem(
            {
              type: MessageType.ERROR,
              text: `Error refreshing memory: ${errorMessage}`,
            },
            Date.now(),
          );
        }
      },
    },
    {
      name: 'inbox',
      get description() {
        return t(
          'Review pending memory candidates from Auto Memory extraction.',
        );
      },
      kind: CommandKind.BUILT_IN,
      action: async (context) => {
        const config = context.services.config;
        if (!config) {
          context.ui.addItem(
            { type: MessageType.ERROR, text: 'Config not available.' },
            Date.now(),
          );
          return;
        }

        const kinds: InboxMemoryPatchKind[] = ['private', 'global'];
        const results: string[] = [];

        for (const kind of kinds) {
          const validFiles = await listValidInboxPatchFiles(config, kind);
          if (validFiles.length === 0) continue;

          results.push(`## ${kind} (${validFiles.length} patch(es))`);
          for (const filePath of validFiles) {
            const relativeName = path.basename(filePath);
            try {
              const content = await fs.readFile(filePath, 'utf-8');
              const parsed = Diff.parsePatch(content);
              const targets = parsed
                .map((p) => p.newFileName || p.oldFileName || 'unknown')
                .join(', ');
              results.push(
                `  - ${relativeName}: targets ${targets} (${parsed.length} file(s))`,
              );
            } catch {
              results.push(`  - ${relativeName}: (unreadable)`);
            }
          }
        }

        if (results.length === 0) {
          context.ui.addItem(
            {
              type: MessageType.INFO,
              text: t(
                'Memory inbox is empty. No pending extraction candidates.',
              ),
            },
            Date.now(),
          );
        } else {
          context.ui.addItem(
            {
              type: MessageType.INFO,
              text: `Pending memory inbox:\n\n${results.join('\n')}\n\nUse /memory inbox approve or /memory inbox dismiss to manage.`,
            },
            Date.now(),
          );
        }
      },
      subCommands: [
        {
          name: 'approve',
          get description() {
            return t('Approve and apply all pending memory patches.');
          },
          kind: CommandKind.BUILT_IN,
          action: async (context, args) => {
            const config = context.services.config;
            if (!config) {
              context.ui.addItem(
                { type: MessageType.ERROR, text: 'Config not available.' },
                Date.now(),
              );
              return;
            }

            const kindArg = args?.trim();
            const kinds: InboxMemoryPatchKind[] =
              kindArg === 'private'
                ? ['private']
                : kindArg === 'global'
                  ? ['global']
                  : ['private', 'global'];

            let approvedFileCount = 0;
            let retainedFileCount = 0;
            const failures: string[] = [];

            for (const kind of kinds) {
              // Walk every inbox patch file, not just the pre-validated ones,
              // so users can see why malformed files are being retained.
              const patchFiles = await listInboxPatchFiles(config, kind);
              for (const filePath of patchFiles) {
                const validation = await validateInboxMemoryPatchFile(
                  config,
                  kind,
                  filePath,
                );
                if (!validation.valid) {
                  retainedFileCount++;
                  failures.push(
                    `${path.basename(filePath)}: ${validation.reason}`,
                  );
                  continue;
                }

                // Apply all hunks; keep the patch file unless every hunk of
                // this file applies successfully. This preserves an
                // all-or-nothing semantics per file for easy retry.
                let allApplied = true;
                const pendingWrites: Array<{
                  targetPath: string;
                  content: string;
                }> = [];

                for (let i = 0; i < validation.parsed.length; i++) {
                  const parsedDiff = validation.parsed[i];
                  const { targetPath, isNewFile } = validation.patches[i];

                  let original = '';
                  if (!isNewFile) {
                    try {
                      original = await fs.readFile(targetPath, 'utf-8');
                    } catch (error) {
                      allApplied = false;
                      failures.push(
                        `${path.basename(filePath)}: cannot read target ${targetPath}: ${getErrorMessage(error)}`,
                      );
                      break;
                    }
                  }

                  const applied = Diff.applyPatch(original, parsedDiff);
                  if (applied === false) {
                    allApplied = false;
                    failures.push(
                      `${path.basename(filePath)}: diff does not apply cleanly to ${targetPath}`,
                    );
                    break;
                  }

                  pendingWrites.push({ targetPath, content: applied });
                }

                if (!allApplied) {
                  // Keep the patch file so the user can fix and retry.
                  retainedFileCount++;
                  continue;
                }

                try {
                  for (const { targetPath, content } of pendingWrites) {
                    await fs.mkdir(path.dirname(targetPath), {
                      recursive: true,
                    });
                    await fs.writeFile(targetPath, content);
                  }
                  await fs.unlink(filePath);
                  approvedFileCount++;
                } catch (error) {
                  retainedFileCount++;
                  failures.push(
                    `${path.basename(filePath)}: write failed: ${getErrorMessage(error)}`,
                  );
                }
              }
            }

            const parts: string[] = [];
            if (approvedFileCount > 0) {
              parts.push(`Applied ${approvedFileCount} patch file(s).`);
            }
            if (retainedFileCount > 0) {
              parts.push(
                `Retained ${retainedFileCount} patch file(s) for retry.`,
              );
            }
            if (approvedFileCount === 0 && retainedFileCount === 0) {
              parts.push('No pending patches to approve.');
            }
            if (failures.length > 0) {
              parts.push(`Reasons:\n  - ${failures.join('\n  - ')}`);
            }
            const message = parts.join('\n');

            context.ui.addItem(
              { type: MessageType.INFO, text: message },
              Date.now(),
            );
          },
        },
        {
          name: 'dismiss',
          get description() {
            return t('Dismiss all pending memory patches without applying.');
          },
          kind: CommandKind.BUILT_IN,
          action: async (context, args) => {
            const config = context.services.config;
            if (!config) {
              context.ui.addItem(
                { type: MessageType.ERROR, text: 'Config not available.' },
                Date.now(),
              );
              return;
            }

            const kindArg = args?.trim();
            const kinds: InboxMemoryPatchKind[] =
              kindArg === 'private'
                ? ['private']
                : kindArg === 'global'
                  ? ['global']
                  : ['private', 'global'];

            let dismissedCount = 0;

            for (const kind of kinds) {
              const validFiles = await listValidInboxPatchFiles(config, kind);
              for (const filePath of validFiles) {
                try {
                  await fs.unlink(filePath);
                  dismissedCount++;
                } catch {
                  // Ignore
                }
              }
            }

            const message =
              dismissedCount > 0
                ? `Dismissed ${dismissedCount} patch(es).`
                : 'No pending patches to dismiss.';

            context.ui.addItem(
              { type: MessageType.INFO, text: message },
              Date.now(),
            );
          },
        },
      ],
    },
  ],
};
