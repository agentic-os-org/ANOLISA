/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import type {
  SlashCommand,
  SlashCommandActionReturn,
  CommandContext,
  MessageActionReturn,
} from './types.js';
import { CommandKind } from './types.js';
import { t } from '../../i18n/index.js';
import type { HookRegistryEntry, HookRegistry } from '@copilot-shell/core';

function getHookName(hook: HookRegistryEntry): string {
  return hook.config.name || hook.config.command || '';
}

function getKnownHookNames(registry: HookRegistry): Set<string> {
  return new Set(registry.getAllHooks().map(getHookName).filter(Boolean));
}

function resolveHookNames(args: string, registry: HookRegistry): string[] {
  const trimmed = args.trim();
  if (!trimmed) return [];

  const knownNames = getKnownHookNames(registry);

  // Full args matches a known name (preserves names with spaces, e.g. "echo test")
  if (knownNames.has(trimmed)) {
    return [trimmed];
  }

  // Split on commas; for each segment, use as-is if it matches a known name,
  // otherwise split on whitespace to support "hook-a hook-b"
  const result: string[] = [];
  for (const segment of trimmed.split(',')) {
    const part = segment.trim();
    if (!part) continue;
    if (knownNames.has(part)) {
      result.push(part);
    } else {
      for (const token of part.split(/\s+/)) {
        if (token) result.push(token);
      }
    }
  }

  return [...new Set(result)];
}

function toggleHooks(
  registry: HookRegistry,
  names: string[],
  enabled: boolean,
): MessageActionReturn {
  const verb = enabled ? t('Enabled') : t('Disabled');
  const pastParticiple = enabled ? t('enabled') : t('disabled');
  const knownNames = getKnownHookNames(registry);
  const foundNames = names.filter((name) => knownNames.has(name));
  const unknownNames = names.filter((name) => !knownNames.has(name));

  if (foundNames.length === 0) {
    return {
      type: 'message',
      messageType: 'error',
      content: t('No matching hooks found: {{names}}', {
        names: unknownNames.join(', '),
      }),
    };
  }

  for (const name of foundNames) {
    registry.setHookEnabled(name, enabled);
  }

  const successMessage =
    foundNames.length === 1
      ? t('Hook "{{name}}" has been {{action}} for this session.', {
          name: foundNames[0]!,
          action: pastParticiple,
        })
      : t('{{verb}} {{count}} hooks for this session: {{names}}', {
          verb,
          count: String(foundNames.length),
          names: foundNames.join(', '),
        });

  const unknownMessage =
    unknownNames.length > 0
      ? ` ${t('Unknown hooks: {{names}}', { names: unknownNames.join(', ') })}`
      : '';

  return {
    type: 'message',
    messageType: 'info',
    content: `${successMessage}${unknownMessage}`,
  };
}

/**
 * Format hook source for display
 */
function formatHookSource(source: string): string {
  switch (source) {
    case 'project':
      return 'Project';
    case 'user':
      return 'User';
    case 'system':
      return 'System';
    case 'extensions':
      return 'Extension';
    default:
      return source;
  }
}

/**
 * Format hook status for display
 */
function formatHookStatus(enabled: boolean): string {
  return enabled ? '✓ Enabled' : '✗ Disabled';
}

const listCommand: SlashCommand = {
  name: 'list',
  get description() {
    return t('List all configured hooks');
  },
  kind: CommandKind.BUILT_IN,
  action: async (
    context: CommandContext,
    _args: string,
  ): Promise<MessageActionReturn> => {
    const { config } = context.services;
    if (!config) {
      return {
        type: 'message',
        messageType: 'error',
        content: t('Config not loaded.'),
      };
    }

    const hookSystem = config.getHookSystem();
    if (!hookSystem) {
      return {
        type: 'message',
        messageType: 'info',
        content: t(
          'Hooks are not enabled. Enable hooks in settings to use this feature.',
        ),
      };
    }

    const registry = hookSystem.getRegistry();
    const allHooks = registry.getAllHooks();

    if (allHooks.length === 0) {
      return {
        type: 'message',
        messageType: 'info',
        content: t(
          'No hooks configured. Add hooks in your settings.json file.',
        ),
      };
    }

    // Group hooks by event
    const hooksByEvent = new Map<string, HookRegistryEntry[]>();
    for (const hook of allHooks) {
      const eventName = hook.eventName;
      if (!hooksByEvent.has(eventName)) {
        hooksByEvent.set(eventName, []);
      }
      hooksByEvent.get(eventName)!.push(hook);
    }

    let output = `**Configured Hooks (${allHooks.length} total)**\n\n`;

    for (const [eventName, hooks] of hooksByEvent) {
      output += `### ${eventName}\n`;
      for (const hook of hooks) {
        const name = getHookName(hook) || 'unnamed';
        const source = formatHookSource(hook.source);
        const status = formatHookStatus(hook.enabled);
        const matcher = hook.matcher ? ` (matcher: ${hook.matcher})` : '';
        output += `- **${name}** [${source}] ${status}${matcher}\n`;
      }
      output += '\n';
    }

    return {
      type: 'message',
      messageType: 'info',
      content: output,
    };
  },
};

const enableCommand: SlashCommand = {
  name: 'enable',
  get description() {
    return t('Enable disabled hooks');
  },
  kind: CommandKind.BUILT_IN,
  action: async (
    context: CommandContext,
    args: string,
  ): Promise<SlashCommandActionReturn> => {
    const { config } = context.services;
    if (!config) {
      return {
        type: 'message',
        messageType: 'error',
        content: t('Config not loaded.'),
      };
    }

    const hookSystem = config.getHookSystem();
    if (!hookSystem) {
      return {
        type: 'message',
        messageType: 'error',
        content: t('Hooks are not enabled.'),
      };
    }

    const registry = hookSystem.getRegistry();
    const names = resolveHookNames(args, registry);
    if (names.length === 0) {
      const disabledNames = [
        ...new Set(
          registry
            .getAllHooks()
            .filter((h) => !h.enabled)
            .map(getHookName)
            .filter(Boolean),
        ),
      ];
      if (disabledNames.length === 0) {
        return {
          type: 'message',
          messageType: 'info',
          content: t('No disabled hooks to enable.'),
        };
      }
      return {
        type: 'multi_select_hooks',
        hookNames: disabledNames,
        title: t('Select hooks to enable'),
        onSelected: (selected) => toggleHooks(registry, selected, true),
      };
    }

    return toggleHooks(registry, names, true);
  },
  completion: async (context: CommandContext, partialArg: string) => {
    const { config } = context.services;
    if (!config) return [];

    const hookSystem = config.getHookSystem();
    if (!hookSystem) return [];

    const registry = hookSystem.getRegistry();
    const allHooks = registry.getAllHooks();
    const parts = partialArg.split(/\s+/);
    const currentPartial = parts[parts.length - 1] ?? '';
    const alreadyTyped = new Set(parts.slice(0, -1));

    const disabledHookNames = allHooks
      .filter((hook) => !hook.enabled)
      .map((hook) => getHookName(hook))
      .filter(
        (name) =>
          name && name.startsWith(currentPartial) && !alreadyTyped.has(name),
      );
    return [...new Set(disabledHookNames)];
  },
};

const disableCommand: SlashCommand = {
  name: 'disable',
  get description() {
    return t('Disable active hooks');
  },
  kind: CommandKind.BUILT_IN,
  action: async (
    context: CommandContext,
    args: string,
  ): Promise<SlashCommandActionReturn> => {
    const { config } = context.services;
    if (!config) {
      return {
        type: 'message',
        messageType: 'error',
        content: t('Config not loaded.'),
      };
    }

    const hookSystem = config.getHookSystem();
    if (!hookSystem) {
      return {
        type: 'message',
        messageType: 'error',
        content: t('Hooks are not enabled.'),
      };
    }

    const registry = hookSystem.getRegistry();
    const names = resolveHookNames(args, registry);
    if (names.length === 0) {
      const enabledNames = [
        ...new Set(
          registry
            .getAllHooks()
            .filter((h) => h.enabled)
            .map(getHookName)
            .filter(Boolean),
        ),
      ];
      if (enabledNames.length === 0) {
        return {
          type: 'message',
          messageType: 'info',
          content: t('No enabled hooks to disable.'),
        };
      }
      return {
        type: 'multi_select_hooks',
        hookNames: enabledNames,
        title: t('Select hooks to disable'),
        onSelected: (selected) => toggleHooks(registry, selected, false),
      };
    }

    return toggleHooks(registry, names, false);
  },
  completion: async (context: CommandContext, partialArg: string) => {
    const { config } = context.services;
    if (!config) return [];

    const hookSystem = config.getHookSystem();
    if (!hookSystem) return [];

    const registry = hookSystem.getRegistry();
    const allHooks = registry.getAllHooks();
    const parts = partialArg.split(/\s+/);
    const currentPartial = parts[parts.length - 1] ?? '';
    const alreadyTyped = new Set(parts.slice(0, -1));

    const enabledHookNames = allHooks
      .filter((hook) => hook.enabled)
      .map((hook) => getHookName(hook))
      .filter(
        (name) =>
          name && name.startsWith(currentPartial) && !alreadyTyped.has(name),
      );
    return [...new Set(enabledHookNames)];
  },
};

function buildHelpMessage(): string {
  const subcommands = [
    { name: 'list', description: t('List all configured hooks') },
    { name: 'enable', description: t('Enable disabled hooks') },
    { name: 'disable', description: t('Disable active hooks') },
  ];

  let output = `**${t('Manage Cosh hooks')}**\n\n`;
  output += `${t('Usage')}: /hooks <${t('subcommand')}>\n\n`;
  output += `${t('Available subcommands')}:\n`;
  for (const cmd of subcommands) {
    output += `  ${cmd.name.padEnd(9)} - ${cmd.description}\n`;
  }
  return output;
}

export const hooksCommand: SlashCommand = {
  name: 'hooks',
  get description() {
    return t('Manage Cosh hooks');
  },
  kind: CommandKind.BUILT_IN,
  subCommands: [listCommand, enableCommand, disableCommand],
  action: async (
    _context: CommandContext,
    args: string,
  ): Promise<SlashCommandActionReturn> => {
    // If no subcommand provided, show help
    if (!args.trim()) {
      return {
        type: 'message',
        messageType: 'info',
        content: buildHelpMessage(),
      };
    }

    const [subcommand, ...rest] = args.trim().split(/\s+/);
    const subArgs = rest.join(' ');

    let result: SlashCommandActionReturn | void;
    switch (subcommand.toLowerCase()) {
      case 'list':
        result = await listCommand.action?.(_context, subArgs);
        break;
      case 'enable':
        result = await enableCommand.action?.(_context, subArgs);
        break;
      case 'disable':
        result = await disableCommand.action?.(_context, subArgs);
        break;
      default:
        return {
          type: 'message',
          messageType: 'error',
          content: t(
            'Unknown subcommand: {{cmd}}. Available: list, enable, disable',
            {
              cmd: subcommand,
            },
          ),
        };
    }
    return result ?? { type: 'message', messageType: 'info', content: '' };
  },
  completion: async (context: CommandContext, partialArg: string) => {
    const subcommands = ['list', 'enable', 'disable'];
    const parts = partialArg.split(/\s+/);

    if (parts.length <= 1) {
      // Complete subcommand
      return subcommands.filter((cmd) => cmd.startsWith(partialArg));
    }

    // Complete subcommand arguments
    const [subcommand, ...rest] = parts;
    const subArgs = rest.join(' ');

    switch (subcommand.toLowerCase()) {
      case 'enable':
        return enableCommand.completion?.(context, subArgs) ?? [];
      case 'disable':
        return disableCommand.completion?.(context, subArgs) ?? [];
      default:
        return [];
    }
  },
};
