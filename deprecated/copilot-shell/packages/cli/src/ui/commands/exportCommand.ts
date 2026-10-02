/**
 * @license
 * Copyright 2025 Qwen
 * SPDX-License-Identifier: Apache-2.0
 */

import * as fs from 'node:fs/promises';
import path from 'node:path';
import {
  type CommandContext,
  type SlashCommand,
  type MessageActionReturn,
  CommandKind,
} from './types.js';
import { SessionService } from '@copilot-shell/core';
import {
  collectSessionData,
  normalizeSessionData,
  toMarkdown,
  toHtml,
  toJson,
  toJsonl,
  generateExportFilename,
  type ExportSessionData,
} from '../utils/export/index.js';
import { t } from '../../i18n/index.js';

/**
 * Format type for export.
 */
type ExportFormat = 'md' | 'html' | 'json' | 'jsonl';

/**
 * Format configuration mapping.
 */
const FORMAT_CONFIG: Record<
  ExportFormat,
  {
    formatter: (data: ExportSessionData) => string;
    successKey: string;
  }
> = {
  md: {
    formatter: toMarkdown,
    successKey: 'Session exported to markdown: {{filename}}',
  },
  html: {
    formatter: toHtml,
    successKey: 'Session exported to HTML: {{filename}}',
  },
  json: {
    formatter: toJson,
    successKey: 'Session exported to JSON: {{filename}}',
  },
  jsonl: {
    formatter: toJsonl,
    successKey: 'Session exported to JSONL: {{filename}}',
  },
};

/**
 * Generic export action - exports session to specified format.
 * @param context Command context
 * @param format Export format (md, html, json, jsonl)
 */
async function exportSessionAction(
  context: CommandContext,
  format: ExportFormat,
): Promise<MessageActionReturn> {
  const { services } = context;
  const { config } = services;

  if (!config) {
    return {
      type: 'message',
      messageType: 'error',
      content: t('Configuration not available.'),
    };
  }

  const cwd = config.getWorkingDir() || config.getProjectRoot();
  if (!cwd) {
    return {
      type: 'message',
      messageType: 'error',
      content: t('Could not determine current working directory.'),
    };
  }

  try {
    // Load the current session
    const sessionService = new SessionService(cwd);
    const sessionData = await sessionService.loadLastSession();

    if (!sessionData) {
      return {
        type: 'message',
        messageType: 'error',
        content: t('No active session found to export.'),
      };
    }

    const { conversation } = sessionData;

    // Collect and normalize export data (SSOT)
    const exportData = await collectSessionData(conversation, config);
    const normalizedData = normalizeSessionData(
      exportData,
      conversation.messages,
      config,
    );

    // Get format configuration
    const { formatter, successKey } = FORMAT_CONFIG[format];

    // Generate content from SSOT
    const content = formatter(normalizedData);

    const filename = generateExportFilename(format);
    const filepath = path.join(cwd, filename);

    // Write to file
    await fs.writeFile(filepath, content, 'utf-8');

    return {
      type: 'message',
      messageType: 'info',
      content: t(successKey, { filename }),
    };
  } catch (error) {
    return {
      type: 'message',
      messageType: 'error',
      content: t('Failed to export session: {{error}}', {
        error: error instanceof Error ? error.message : String(error),
      }),
    };
  }
}

/**
 * Action for the 'md' subcommand - exports session to markdown.
 */
async function exportMarkdownAction(
  context: CommandContext,
): Promise<MessageActionReturn> {
  return exportSessionAction(context, 'md');
}

/**
 * Action for the 'html' subcommand - exports session to HTML.
 */
async function exportHtmlAction(
  context: CommandContext,
): Promise<MessageActionReturn> {
  return exportSessionAction(context, 'html');
}

/**
 * Action for the 'json' subcommand - exports session to JSON.
 */
async function exportJsonAction(
  context: CommandContext,
): Promise<MessageActionReturn> {
  return exportSessionAction(context, 'json');
}

/**
 * Action for the 'jsonl' subcommand - exports session to JSONL.
 */
async function exportJsonlAction(
  context: CommandContext,
): Promise<MessageActionReturn> {
  return exportSessionAction(context, 'jsonl');
}

/**
 * Default export action - exports session to markdown format.
 * This is the default behavior when `/export` is called without a subcommand.
 */
async function exportDefaultAction(
  context: CommandContext,
): Promise<MessageActionReturn> {
  return exportSessionAction(context, 'md');
}

/**
 * Export command for exporting session history to various formats.
 * Default behavior: export to markdown format.
 */
export const exportCommand: SlashCommand = {
  name: 'export',
  kind: CommandKind.BUILT_IN,
  get description() {
    return t('Export current session message history to a file');
  },
  action: exportDefaultAction,
  subCommands: [
    {
      name: 'md',
      kind: CommandKind.BUILT_IN,
      get description() {
        return t('Export to Markdown format');
      },
      action: exportMarkdownAction,
    },
    {
      name: 'html',
      kind: CommandKind.BUILT_IN,
      get description() {
        return t('Export to HTML format');
      },
      action: exportHtmlAction,
    },
    {
      name: 'json',
      kind: CommandKind.BUILT_IN,
      get description() {
        return t('Export to JSON format');
      },
      action: exportJsonAction,
    },
    {
      name: 'jsonl',
      kind: CommandKind.BUILT_IN,
      get description() {
        return t('Export to JSONL format');
      },
      action: exportJsonlAction,
    },
  ],
};
