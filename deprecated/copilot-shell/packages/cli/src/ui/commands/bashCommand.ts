/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import fs from 'node:fs';
import path from 'node:path';
import {
  CommandKind,
  type SlashCommand,
  type SpawnShellActionReturn,
  type MessageActionReturn,
} from './types.js';
import { t } from '../../i18n/index.js';

/** Known interactive shells used as a fallback whitelist. */
const KNOWN_SHELLS = new Set([
  'bash',
  'zsh',
  'fish',
  'sh',
  'ksh',
  'ksh93',
  'mksh',
  'csh',
  'tcsh',
  'dash',
  'ash',
  'pwsh',
  'powershell',
  'cmd.exe',
]);

/**
 * Returns true if the given value looks like a valid interactive shell.
 * Checks against /etc/shells (Unix standard) and falls back to a known-shells whitelist.
 */
function isValidShell(shell: string): boolean {
  const shellBasename = path.basename(shell);

  // Fast path: check against known shell whitelist by basename
  if (KNOWN_SHELLS.has(shellBasename.toLowerCase())) {
    return true;
  }

  // On Unix, consult /etc/shells for system-registered login shells
  if (process.platform !== 'win32') {
    try {
      const contents = fs.readFileSync('/etc/shells', 'utf8');
      const registeredShells = contents
        .split('\n')
        .map((line) => line.trim())
        .filter((line) => line && !line.startsWith('#'));
      // Match either exact path or basename
      return registeredShells.some(
        (s) => s === shell || path.basename(s) === shellBasename,
      );
    } catch {
      // /etc/shells not available; fall through
    }
  }

  return false;
}

export const bashCommand: SlashCommand = {
  name: 'bash',
  get description() {
    return t('Launch an interactive bash shell; exit to return to the TUI');
  },
  kind: CommandKind.BUILT_IN,
  action: (context, args): SpawnShellActionReturn | MessageActionReturn => {
    if (
      context.executionMode === 'non_interactive' ||
      context.executionMode === 'acp'
    ) {
      return {
        type: 'message',
        messageType: 'error',
        content: t('/bash is only available in interactive mode.'),
      };
    }
    const trimmedArgs = args.trim();
    // Validate that args is a shell executable name, not a full command
    if (trimmedArgs && trimmedArgs.includes(' ')) {
      return {
        type: 'message',
        messageType: 'error',
        content: t(
          '/bash only accepts a shell executable (e.g. /bash, /bash zsh, /bash fish). To run a command, just type it directly.',
        ),
      };
    }
    const shell =
      trimmedArgs || (process.platform === 'win32' ? 'cmd.exe' : 'bash');
    // Validate that the resolved shell name is actually an interactive shell
    if (trimmedArgs && !isValidShell(trimmedArgs)) {
      return {
        type: 'message',
        messageType: 'error',
        content: t(
          '"{{shell}}" does not appear to be a valid interactive shell. Use a shell name like bash, zsh, or fish.',
          { shell: trimmedArgs },
        ),
      };
    }
    return { type: 'spawn_shell', shell };
  },
};
