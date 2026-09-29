/**
 * @license
 * Copyright 2026 Alibaba Cloud
 * SPDX-License-Identifier: Apache-2.0
 */

import { execSync, spawn } from 'node:child_process';

/** @internal Thin wrapper for testing – override in tests. */
export const _deps = { execSync, spawn };
import {
  CommandKind,
  type CommandContext,
  type SlashCommand,
  type SlashCommandActionReturn,
} from './types.js';
import {
  MessageType,
  type ClawhubResultItem,
  type HistoryItemClawhubOutput,
} from '../types.js';
import { Storage } from '@copilot-shell/core';
import { t } from '../../i18n/index.js';
import React from 'react';
import { Text } from 'ink';

const SKILLS_DIR = new Storage('').getUserSkillsDir();

/**
 * User-local prefix for clawhub: ~/.copilot-shell/bin
 * `npm install --prefix <dir>` creates a symlink at <dir>/node_modules/.bin/clawhub,
 * so no root / sudo is required for any user.
 */
const CLAWHUB_PREFIX_DIR = Storage.getGlobalBinDir();
const CLAWHUB_LOCAL_BIN = `${CLAWHUB_PREFIX_DIR}/node_modules/.bin/clawhub`;

/**
 * Returns the clawhub executable path to use.
 * Prefers the user-local install; falls back to whatever is on PATH.
 */
function getClawhubExecutable(): string {
  try {
    _deps.execSync(`"${CLAWHUB_LOCAL_BIN}" -V`, { stdio: 'pipe' });
    return CLAWHUB_LOCAL_BIN;
  } catch {
    return 'clawhub';
  }
}

type ClawhubOutputItem = Omit<HistoryItemClawhubOutput, 'id'>;

function clawhubOutputItem(
  props: Omit<ClawhubOutputItem, 'type'>,
): ClawhubOutputItem {
  return { type: MessageType.CLAWHUB_OUTPUT, ...props };
}

/** Subcommands that require the --dir parameter. */
const COMMANDS_NEED_DIR = new Set([
  'search',
  'inspect',
  'install',
  'uninstall',
  'update',
  'list',
]);

/** Title keys for each subcommand (passed to t() at runtime). */
const SUBCOMMAND_TITLE_KEYS: Record<string, string> = {
  search: 'Clawhub Search',
  install: 'Clawhub Install',
  uninstall: 'Clawhub Uninstall',
  update: 'Clawhub Update',
  list: 'Clawhub Installed Skills',
  inspect: 'Clawhub Inspect',
  login: 'Clawhub Login',
  whoami: 'Clawhub Identity',
};

function installClawhubProcess(): Promise<{
  success: boolean;
  output: string;
}> {
  return new Promise((resolve) => {
    // Install into the user-local prefix – no root/sudo required.
    const proc = _deps.spawn(
      'npm',
      ['install', '--prefix', CLAWHUB_PREFIX_DIR, 'clawhub'],
      { stdio: 'pipe', shell: true },
    );
    let stdout = '';
    let stderr = '';
    proc.stdout?.on('data', (data: Buffer) => {
      stdout += data.toString();
    });
    proc.stderr?.on('data', (data: Buffer) => {
      stderr += data.toString();
    });
    proc.on('close', (code) =>
      resolve({ success: code === 0, output: stderr || stdout }),
    );
    proc.on('error', (err) => resolve({ success: false, output: err.message }));
  });
}

const DEFAULT_REGISTRY = 'https://cn.clawhub-mirror.com';

function getClawhubRegistry(context: CommandContext): string {
  const settings = context.services.settings.merged as Record<string, unknown>;
  const clawhubSettings = settings['clawhub'] as
    | Record<string, unknown>
    | undefined;
  return (
    (clawhubSettings?.['registry'] as string | undefined) ?? DEFAULT_REGISTRY
  );
}

function runClawhubProcess(
  args: string[],
): Promise<{ code: number; stdout: string; stderr: string }> {
  return new Promise((resolve) => {
    const proc = _deps.spawn(getClawhubExecutable(), args, {
      stdio: 'pipe',
      shell: true,
    });
    let stdout = '';
    let stderr = '';
    proc.stdout?.on('data', (data: Buffer) => {
      stdout += data.toString();
    });
    proc.stderr?.on('data', (data: Buffer) => {
      stderr += data.toString();
    });
    proc.on('close', (code) => resolve({ code: code ?? 1, stdout, stderr }));
    proc.on('error', (err) =>
      resolve({ code: 1, stdout: '', stderr: err.message }),
    );
  });
}

/**
 * Parse clawhub search/list output lines into structured items.
 *
 * Matches lines like:
 *   slug  description  (score)
 *   slug  version
 */
function parseResultItems(output: string): ClawhubResultItem[] {
  const items: ClawhubResultItem[] = [];
  for (const line of output.split('\n')) {
    const trimmed = line.trim();
    // Skip spinner lines, empty lines, error lines
    if (!trimmed || trimmed.startsWith('-') || trimmed.startsWith('✖')) {
      continue;
    }

    // Match: slug  description  (score)
    const searchMatch = trimmed.match(/^(\S+)\s{2,}(.+?)\s{2,}\(([^)]+)\)\s*$/);
    if (searchMatch) {
      items.push({
        slug: searchMatch[1],
        description: searchMatch[2].trim(),
        score: searchMatch[3],
      });
      continue;
    }

    // Match: slug  version (list output)
    const listMatch = trimmed.match(/^(\S+)\s{2,}(\S+)\s*$/);
    if (listMatch) {
      items.push({
        slug: listMatch[1],
        description: listMatch[2],
      });
      continue;
    }

    // Single slug (no extra info)
    if (/^\S+$/.test(trimmed)) {
      items.push({ slug: trimmed, description: '' });
    }
  }
  return items;
}

/**
 * Strip ANSI escape sequences and spinner lines from CLI output to get clean text.
 * Spinner animation characters are removed entirely, while result indicators
 * (✔ / ✖) are stripped but their content is preserved.
 */
function cleanOutput(raw: string): string {
  // eslint-disable-next-line no-control-regex
  const ansiPattern = /\x1b\[[0-9;]*[a-zA-Z]/g;
  return raw
    .replace(ansiPattern, '') // strip ANSI codes
    .split('\n')
    .map((line) => {
      const t = line.trim();
      if (!t) return '';
      // Strip spinner animation lines entirely (ora progress indicators)
      if (/^[⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏] /.test(t) || /^- /.test(t)) {
        return '';
      }
      // Strip result prefix (✔ / ✖) but preserve the content after it
      if (/^[✔✖] /.test(t)) {
        return t.slice(2).trim();
      }
      return t;
    })
    .filter(Boolean)
    .join('\n')
    .trim();
}

/** Subcommands whose output should be parsed into structured item lists. */
const LIST_SUBCOMMANDS = new Set(['search', 'list']);

/** Cache: once confirmed installed, skip future checks within the session. */
let clawhubAvailable: boolean | null = null;

/** @internal Reset cache – exposed for testing only. */
export function _resetClawhubCache(): void {
  clawhubAvailable = null;
}

/**
 * Ensures clawhub CLI is available.
 * Returns:
 *  - 'ready': clawhub is installed
 *  - 'not_installed': not installed and needs user confirmation
 *  - 'install_failed': installation was attempted but failed
 */
async function ensureClawhub(
  context: CommandContext,
  doInstall: boolean,
): Promise<'ready' | 'not_installed' | 'install_failed'> {
  if (clawhubAvailable === true) {
    return 'ready';
  }

  // 1. Check user-local install first (no PATH dependency)
  try {
    _deps.execSync(`"${CLAWHUB_LOCAL_BIN}" -V`, { stdio: 'pipe' });
    clawhubAvailable = true;
    return 'ready';
  } catch {
    // not locally installed
  }

  // 2. Fall back to system PATH
  try {
    _deps.execSync('clawhub -V', { stdio: 'pipe' });
    clawhubAvailable = true;
    return 'ready';
  } catch {
    // not installed
  }

  if (!doInstall) {
    return 'not_installed';
  }

  // User confirmed – proceed with installation
  context.ui.addItem(
    {
      type: MessageType.INFO,
      text: t('Installing clawhub to {{dir}} …', { dir: CLAWHUB_PREFIX_DIR }),
    },
    Date.now(),
  );

  const installResult = await installClawhubProcess();
  if (!installResult.success) {
    context.ui.addItem(
      clawhubOutputItem({
        title: 'Clawhub',
        text: t(
          'Failed to install clawhub: {{error}}\nPlease install manually: npm install --prefix {{dir}} clawhub',
          { error: installResult.output, dir: CLAWHUB_PREFIX_DIR },
        ),
        isError: true,
      }),
      Date.now(),
    );
    return 'install_failed';
  }

  context.ui.addItem(
    { type: MessageType.INFO, text: t('clawhub installed successfully.') },
    Date.now(),
  );
  clawhubAvailable = true;
  return 'ready';
}

/**
 * Ensures clawhub is installed, executes the given clawhub subcommand,
 * and renders output via the ClawhubOutputBox component.
 */
async function execClawhub(
  context: CommandContext,
  rawArgs: string,
): Promise<SlashCommandActionReturn | void> {
  // 1. Check clawhub availability
  const status = await ensureClawhub(context, !!context.overwriteConfirmed);
  if (status === 'install_failed') {
    return;
  }
  if (status === 'not_installed') {
    return {
      type: 'confirm_action',
      prompt: React.createElement(
        Text,
        null,
        t('clawhub CLI is not installed. Install it now?'),
      ),
      originalInvocation: {
        raw: context.invocation?.raw || `/clawhub ${rawArgs}`,
      },
    };
  }

  // 2. Parse subcommand and build args
  const parts = rawArgs.trim().split(/\s+/).filter(Boolean);
  if (parts.length === 0) {
    context.ui.addItem(
      clawhubOutputItem({
        title: 'Clawhub',
        text: [
          t('Usage: /clawhub <subcommand> [args]'),
          '',
          t('Subcommands:'),
          t('Search skills in the registry (subcommand help)'),
          t('Install a skill (subcommand help)'),
          t('Uninstall a skill (subcommand help)'),
          t('Update skills (subcommand help)'),
          t('List installed skills (subcommand help)'),
          t('View details (subcommand help)'),
          t('Login (subcommand help)'),
          t('Show identity (subcommand help)'),
        ].join('\n'),
      }),
      Date.now(),
    );
    return;
  }

  const subcommand = parts[0];
  const fullArgs = [...parts];

  // Append --dir for commands that require it
  if (subcommand && COMMANDS_NEED_DIR.has(subcommand)) {
    if (!fullArgs.includes('--dir')) {
      fullArgs.push('--dir', SKILLS_DIR);
    }

    if (!fullArgs.includes('--registry')) {
      fullArgs.push('--registry', getClawhubRegistry(context));
    }
  }

  // Avoid interactive prompts blocking the CLI
  if (!fullArgs.includes('--no-input') && !fullArgs.includes('--yes')) {
    fullArgs.push('--no-input');
  }

  // 3. Execute
  const result = await runClawhubProcess(fullArgs);
  const rawOutput = [result.stdout, result.stderr]
    .filter(Boolean)
    .join('\n')
    .trim();
  const titleKey = SUBCOMMAND_TITLE_KEYS[subcommand ?? ''];
  const title = titleKey ? t(titleKey) : `Clawhub ${subcommand}`;

  if (result.code !== 0) {
    let errorText =
      cleanOutput(rawOutput) ||
      t('clawhub exited with code {{code}}.', { code: String(result.code) });
    if (rawOutput.includes('Rate limit exceeded')) {
      errorText +=
        '\n\n' +
        t('Hint: Run `clawhub login` to authenticate and bypass rate limits.');
    }
    context.ui.addItem(
      clawhubOutputItem({ title, text: errorText, isError: true }),
      Date.now(),
    );
    return;
  }

  // 4. Render output
  if (subcommand && LIST_SUBCOMMANDS.has(subcommand)) {
    const items = parseResultItems(rawOutput);
    if (items.length > 0) {
      context.ui.addItem(clawhubOutputItem({ title, items }), Date.now());
    } else {
      context.ui.addItem(
        clawhubOutputItem({
          title,
          text: cleanOutput(rawOutput) || t('No results.'),
        }),
        Date.now(),
      );
    }
  } else {
    context.ui.addItem(
      clawhubOutputItem({
        title,
        text: cleanOutput(rawOutput) || t('Command completed successfully.'),
      }),
      Date.now(),
    );
  }
}

// ── Subcommand definitions ──────────────────────────────────────────

const searchSubCommand: SlashCommand = {
  name: 'search',
  get description() {
    return t('Search skills in the registry');
  },
  kind: CommandKind.BUILT_IN,
  action: (ctx, args) => execClawhub(ctx, `search ${args}`),
};

const installSubCommand: SlashCommand = {
  name: 'install',
  get description() {
    return t('Install a skill');
  },
  kind: CommandKind.BUILT_IN,
  action: (ctx, args) => execClawhub(ctx, `install ${args}`),
};

const uninstallSubCommand: SlashCommand = {
  name: 'uninstall',
  get description() {
    return t('Uninstall a skill');
  },
  kind: CommandKind.BUILT_IN,
  action: (ctx, args) => execClawhub(ctx, `uninstall ${args} --yes`),
};

const updateSubCommand: SlashCommand = {
  name: 'update',
  get description() {
    return t('Update skill(s). Usage: update <slug> | update --all');
  },
  kind: CommandKind.BUILT_IN,
  action: (ctx, args) => execClawhub(ctx, `update ${args}`),
};

const listSubCommand: SlashCommand = {
  name: 'list',
  get description() {
    return t('List installed skills');
  },
  kind: CommandKind.BUILT_IN,
  action: (ctx, args) => execClawhub(ctx, `list ${args}`),
};

const inspectSubCommand: SlashCommand = {
  name: 'inspect',
  get description() {
    return t('View skill details');
  },
  kind: CommandKind.BUILT_IN,
  action: (ctx, args) => execClawhub(ctx, `inspect ${args}`),
};

const loginSubCommand: SlashCommand = {
  name: 'login',
  get description() {
    return t('Login to clawhub');
  },
  kind: CommandKind.BUILT_IN,
  action: (ctx, args) => {
    const token = args.trim();
    if (!token) {
      ctx.ui.addItem(
        clawhubOutputItem({
          title: t('Clawhub Login'),
          text: t('Usage: /clawhub login <token>'),
          isError: true,
        }),
        Date.now(),
      );
      return;
    }
    return execClawhub(ctx, `login --token ${token}`);
  },
};

const whoamiSubCommand: SlashCommand = {
  name: 'whoami',
  get description() {
    return t('Show current identity');
  },
  kind: CommandKind.BUILT_IN,
  action: (ctx, args) => execClawhub(ctx, `whoami ${args}`),
};

// ── Main command ────────────────────────────────────────────────────

export const clawhubCommand: SlashCommand = {
  name: 'clawhub',
  get description() {
    return t('Manage skills via clawhub CLI');
  },
  kind: CommandKind.BUILT_IN,
  subCommands: [
    searchSubCommand,
    installSubCommand,
    uninstallSubCommand,
    updateSubCommand,
    listSubCommand,
    inspectSubCommand,
    loginSubCommand,
    whoamiSubCommand,
  ],
  action: (ctx, args) => execClawhub(ctx, args),
};
