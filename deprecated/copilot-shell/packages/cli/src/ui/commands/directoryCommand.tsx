/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import type {
  SlashCommand,
  CommandContext,
  CommandCompletionItem,
} from './types.js';
import { CommandKind } from './types.js';
import { MessageType } from '../types.js';
import * as os from 'node:os';
import * as path from 'node:path';
import * as fs from 'node:fs';
import { loadServerHierarchicalMemory } from '@copilot-shell/core';
import { t } from '../../i18n/index.js';
import process from 'node:process';

export function expandHomeDir(p: string): string {
  if (!p) {
    return '';
  }
  let expandedPath = p;
  if (p.toLowerCase().startsWith('%userprofile%')) {
    expandedPath = os.homedir() + p.substring('%userprofile%'.length);
  } else if (p === '~' || p.startsWith('~/')) {
    expandedPath = os.homedir() + p.substring(1);
  }
  return path.normalize(expandedPath);
}

/**
 * Lists subdirectory suggestions for a given partial path input.
 * Supports ~ expansion and returns results in ~/... format when under home dir.
 */
async function listDirectorySuggestions(
  partialPath: string,
): Promise<CommandCompletionItem[]> {
  const input = partialPath || '~/';
  const endsWithSep = input.endsWith('/') || input.endsWith(path.sep);
  const expanded = expandHomeDir(input);

  let searchDir: string;
  let filterPrefix: string;

  if (endsWithSep) {
    searchDir = expanded;
    filterPrefix = '';
  } else {
    searchDir = path.dirname(expanded);
    filterPrefix = path.basename(expanded);
  }

  try {
    const entries = fs.readdirSync(searchDir, { withFileTypes: true });
    const results: CommandCompletionItem[] = [];
    const homedir = os.homedir();

    for (const entry of entries) {
      if (!entry.isDirectory()) continue;
      if (entry.name.startsWith('.')) continue;
      if (filterPrefix && !entry.name.startsWith(filterPrefix)) continue;

      const fullPath = path.join(searchDir, entry.name);
      let suggestion: string;
      if (fullPath === homedir || fullPath.startsWith(homedir + path.sep)) {
        suggestion = '~' + fullPath.substring(homedir.length) + '/';
      } else {
        suggestion = fullPath + '/';
      }
      results.push({ value: suggestion, label: suggestion });
    }
    return results;
  } catch {
    return [];
  }
}

export const directoryCommand: SlashCommand = {
  name: 'directory',
  altNames: ['dir'],
  get description() {
    return t('Manage workspace directories');
  },
  kind: CommandKind.BUILT_IN,
  subCommands: [
    {
      name: 'add',
      get description() {
        return t(
          'Add directories to the workspace. Use comma to separate multiple paths',
        );
      },
      kind: CommandKind.BUILT_IN,
      action: async (context: CommandContext, args: string) => {
        const {
          ui: { addItem },
          services: { config },
        } = context;
        const [...rest] = args.split(' ');

        if (!config) {
          addItem(
            {
              type: MessageType.ERROR,
              text: t('Configuration is not available.'),
            },
            Date.now(),
          );
          return;
        }

        const workspaceContext = config.getWorkspaceContext();

        const pathsToAdd = rest
          .join(' ')
          .split(',')
          .filter((p) => p);
        if (pathsToAdd.length === 0) {
          addItem(
            {
              type: MessageType.ERROR,
              text: t('Please provide at least one path to add.'),
            },
            Date.now(),
          );
          return;
        }

        const added: string[] = [];
        const errors: string[] = [];

        for (const pathToAdd of pathsToAdd) {
          try {
            workspaceContext.addDirectory(expandHomeDir(pathToAdd.trim()));
            added.push(pathToAdd.trim());
          } catch (e) {
            const error = e as Error;
            errors.push(
              t("Error adding '{{path}}': {{error}}", {
                path: pathToAdd.trim(),
                error: error.message,
              }),
            );
          }
        }

        try {
          if (config.shouldLoadMemoryFromIncludeDirectories()) {
            const { memoryContent, fileCount } =
              await loadServerHierarchicalMemory(
                config.getWorkingDir(),
                [
                  ...config.getWorkspaceContext().getDirectories(),
                  ...pathsToAdd,
                ],
                config.getDebugMode(),
                config.getFileService(),
                config.getExtensionContextFilePaths(),
                config.getFolderTrust(),
                context.services.settings.merged.context?.importFormat ||
                  'tree', // Use setting or default to 'tree'
              );
            config.setUserMemory(memoryContent);
            config.setGeminiMdFileCount(fileCount);
            context.ui.setGeminiMdFileCount(fileCount);
          }
          addItem(
            {
              type: MessageType.INFO,
              text: t(
                'Successfully added COPILOT.md files from the following directories if there are:\n- {{directories}}',
                {
                  directories: added.join('\n- '),
                },
              ),
            },
            Date.now(),
          );
        } catch (error) {
          errors.push(
            t('Error refreshing memory: {{error}}', {
              error: (error as Error).message,
            }),
          );
        }

        if (added.length > 0) {
          const gemini = config.getGeminiClient();
          if (gemini) {
            await gemini.addDirectoryContext();
          }
          context.ui.dismissStartupWarning?.('home directory');
          context.ui.dismissStartupWarning?.('home-directory');
          addItem(
            {
              type: MessageType.INFO,
              text: t('Successfully added directories:\n- {{directories}}', {
                directories: added.join('\n- '),
              }),
            },
            Date.now(),
          );
        }

        if (errors.length > 0) {
          addItem(
            { type: MessageType.ERROR, text: errors.join('\n') },
            Date.now(),
          );
        }
        return;
      },
    },
    {
      name: 'cd',
      get description() {
        return t('Switch the working directory for the current session');
      },
      kind: CommandKind.BUILT_IN,
      completion: async (_context: CommandContext, partialArg: string) =>
        listDirectorySuggestions(partialArg),
      action: async (context: CommandContext, args: string) => {
        const {
          ui: { addItem, dismissStartupWarning },
          services: { config },
        } = context;

        if (!config) {
          addItem(
            {
              type: MessageType.ERROR,
              text: t('Configuration is not available.'),
            },
            Date.now(),
          );
          return;
        }

        const rawPath = args.trim();
        if (!rawPath) {
          addItem(
            {
              type: MessageType.ERROR,
              text: t(
                'Please provide a path to switch to. Usage: /dir cd <path>',
              ),
            },
            Date.now(),
          );
          return;
        }

        const targetPath = expandHomeDir(rawPath).replace(/[/\\]+$/, '');

        // Validate the target directory exists
        try {
          const stat = fs.statSync(targetPath);
          if (!stat.isDirectory()) {
            addItem(
              {
                type: MessageType.ERROR,
                text: t('"{{path}}" is not a directory.', { path: rawPath }),
              },
              Date.now(),
            );
            return;
          }
        } catch {
          addItem(
            {
              type: MessageType.ERROR,
              text: t('Directory "{{path}}" does not exist.', {
                path: rawPath,
              }),
            },
            Date.now(),
          );
          return;
        }

        // 1. Switch process working directory
        try {
          process.chdir(targetPath);
        } catch (e) {
          addItem(
            {
              type: MessageType.ERROR,
              text: t('Failed to change directory to "{{path}}": {{error}}', {
                path: rawPath,
                error: (e as Error).message,
              }),
            },
            Date.now(),
          );
          return;
        }

        // 2. Replace workspace context directories with the new path
        const workspaceContext = config.getWorkspaceContext();
        workspaceContext.setDirectories([targetPath]);

        // 3. Update config targetDir so Header and file completion reflect new path
        config.setTargetDir(targetPath);

        // 4. Refresh AI directory context
        const gemini = config.getGeminiClient();
        if (gemini) {
          await gemini.addDirectoryContext();
        }

        // 5. Dismiss home directory warning if present
        dismissStartupWarning?.('home directory');
        dismissStartupWarning?.('home-directory');

        addItem(
          {
            type: MessageType.INFO,
            text: t('Switched working directory to: {{path}}', {
              path: targetPath,
            }),
          },
          Date.now(),
        );
      },
    },
    {
      name: 'show',
      get description() {
        return t('Show all directories in the workspace');
      },
      kind: CommandKind.BUILT_IN,
      action: async (context: CommandContext) => {
        const {
          ui: { addItem },
          services: { config },
        } = context;
        if (!config) {
          addItem(
            {
              type: MessageType.ERROR,
              text: t('Configuration is not available.'),
            },
            Date.now(),
          );
          return;
        }
        const workspaceContext = config.getWorkspaceContext();
        const directories = workspaceContext.getDirectories();
        const directoryList = directories.map((dir) => `- ${dir}`).join('\n');
        addItem(
          {
            type: MessageType.INFO,
            text: t('Current workspace directories:\n{{directories}}', {
              directories: directoryList,
            }),
          },
          Date.now(),
        );
      },
    },
  ],
};
